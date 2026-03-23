use anyhow::{bail, Result};
use bytes::{Buf, BufMut, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::debug;

use super::messages::*;
use super::PoolKey;

/// Connect to a backend Postgres server and authenticate.
pub async fn connect_backend(
    key: &PoolKey,
    password: &str,
    extra_params: &[(String, String)],
) -> Result<TcpStream> {
    let addr = format!("{}:{}", key.host, key.port);
    let mut stream = TcpStream::connect(&addr).await?;
    stream.set_nodelay(true)?;

    debug!("Connected to backend {}", addr);

    // Send startup message
    let startup = build_startup_message(&key.user, &key.database, extra_params);
    stream.write_all(&startup).await?;
    stream.flush().await?;

    // Read auth response
    let mut buf = BytesMut::with_capacity(4096);
    let mut authenticated = false;

    while !authenticated {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            bail!("Backend disconnected during auth");
        }

        while let Some(msg) = try_read_message(&mut buf, false)? {
            match msg.msg_type {
                b'R' => {
                    // Authentication message
                    if msg.payload.len() < 4 {
                        bail!("Invalid auth message");
                    }
                    let auth_type = (&msg.payload[0..4]).get_i32();
                    match auth_type {
                        0 => {
                            // AuthenticationOk
                            debug!("Backend auth OK");
                            authenticated = true;
                        }
                        3 => {
                            // CleartextPassword
                            let mut pw_msg = BytesMut::new();
                            pw_msg.put_u8(b'p');
                            pw_msg.put_i32((password.len() + 5) as i32);
                            pw_msg.extend_from_slice(password.as_bytes());
                            pw_msg.put_u8(0);
                            stream.write_all(&pw_msg).await?;
                            stream.flush().await?;
                        }
                        5 => {
                            // MD5Password
                            if msg.payload.len() < 8 {
                                bail!("Invalid MD5 auth message");
                            }
                            let salt = &msg.payload[4..8];
                            let md5_password = compute_md5_password(&key.user, password, salt);
                            let mut pw_msg = BytesMut::new();
                            pw_msg.put_u8(b'p');
                            pw_msg.put_i32((md5_password.len() + 5) as i32);
                            pw_msg.extend_from_slice(md5_password.as_bytes());
                            pw_msg.put_u8(0);
                            stream.write_all(&pw_msg).await?;
                            stream.flush().await?;
                        }
                        10 => {
                            // SASL (SCRAM-SHA-256)
                            handle_scram_auth(&mut stream, &mut buf, &msg, password).await?;
                        }
                        _ => {
                            bail!("Unsupported auth type: {}", auth_type);
                        }
                    }
                }
                b'E' => {
                    // ErrorResponse
                    let error_text = parse_error_fields(&msg.payload);
                    bail!("Backend error: {}", error_text);
                }
                b'S' | b'K' | b'Z' => {
                    // ParameterStatus, BackendKeyData, ReadyForQuery
                    if msg.msg_type == b'Z' {
                        // ReadyForQuery means we're done with startup
                        return Ok(stream);
                    }
                }
                b'N' => {
                    // NoticeResponse — ignore during startup
                }
                _ => {
                    debug!(
                        "Unexpected message during backend startup: '{}'",
                        msg.msg_type as char
                    );
                }
            }
        }
    }

    // Continue reading until ReadyForQuery
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            bail!("Backend disconnected after auth");
        }
        while let Some(msg) = try_read_message(&mut buf, false)? {
            if msg.msg_type == b'Z' {
                return Ok(stream);
            }
            if msg.msg_type == b'E' {
                let error_text = parse_error_fields(&msg.payload);
                bail!("Backend error after auth: {}", error_text);
            }
        }
    }
}

/// Compute MD5 password hash as Postgres expects it.
fn compute_md5_password(user: &str, password: &str, salt: &[u8]) -> String {
    let inner = format!("{:x}", md5::compute(format!("{}{}", password, user)));
    let mut salted = inner.as_bytes().to_vec();
    salted.extend_from_slice(salt);
    format!("md5{:x}", md5::compute(salted))
}

/// Handle SCRAM-SHA-256 authentication.
async fn handle_scram_auth(
    stream: &mut TcpStream,
    buf: &mut BytesMut,
    initial_msg: &PgMessage,
    password: &str,
) -> Result<()> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    // Parse SASL mechanisms from the initial message
    let payload = &initial_msg.payload[4..]; // skip auth type
    let mechanisms: Vec<&str> = payload
        .split(|&b| b == 0)
        .filter_map(|s| {
            if s.is_empty() {
                None
            } else {
                std::str::from_utf8(s).ok()
            }
        })
        .collect();

    if !mechanisms.contains(&"SCRAM-SHA-256") {
        bail!("Server requires SASL but doesn't support SCRAM-SHA-256");
    }

    // Generate client nonce
    let client_nonce: String = (0..24)
        .map(|_| {
            let idx = rand::random::<u8>() % 62;
            let c = if idx < 10 {
                (b'0' + idx) as char
            } else if idx < 36 {
                (b'A' + idx - 10) as char
            } else {
                (b'a' + idx - 36) as char
            };
            c
        })
        .collect();

    // Client first message
    let client_first_bare = format!("n=,r={}", client_nonce);
    let client_first = format!("n,,{}", client_first_bare);

    // Send SASLInitialResponse
    let mechanism = b"SCRAM-SHA-256\0";
    let mut sasl_msg = BytesMut::new();
    sasl_msg.put_u8(b'p');
    let payload_len = 4 + mechanism.len() + 4 + client_first.len();
    sasl_msg.put_i32((payload_len + 4) as i32);
    sasl_msg.extend_from_slice(mechanism);
    sasl_msg.put_i32(client_first.len() as i32);
    sasl_msg.extend_from_slice(client_first.as_bytes());
    stream.write_all(&sasl_msg).await?;
    stream.flush().await?;

    // Read server first message (AuthenticationSASLContinue)
    let server_first = loop {
        let n = stream.read_buf(buf).await?;
        if n == 0 {
            bail!("Backend disconnected during SCRAM");
        }
        if let Some(msg) = try_read_message(buf, false)? {
            if msg.msg_type == b'R' {
                let auth_type = (&msg.payload[0..4]).get_i32();
                if auth_type == 11 {
                    // SASLContinue
                    break std::str::from_utf8(&msg.payload[4..])?.to_string();
                }
                bail!("Unexpected auth type during SCRAM: {}", auth_type);
            }
            if msg.msg_type == b'E' {
                bail!(
                    "Backend error during SCRAM: {}",
                    parse_error_fields(&msg.payload)
                );
            }
        }
    };

    // Parse server first message
    let mut server_nonce = String::new();
    let mut salt_b64 = String::new();
    let mut iterations = 0u32;
    for part in server_first.split(',') {
        if let Some(val) = part.strip_prefix("r=") {
            server_nonce = val.to_string();
        } else if let Some(val) = part.strip_prefix("s=") {
            salt_b64 = val.to_string();
        } else if let Some(val) = part.strip_prefix("i=") {
            iterations = val.parse()?;
        }
    }

    if !server_nonce.starts_with(&client_nonce) {
        bail!("SCRAM: server nonce doesn't start with client nonce");
    }

    let salt = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &salt_b64)?;

    // Derive keys
    let salted_password = pbkdf2_sha256(password.as_bytes(), &salt, iterations);

    let mut client_key_mac =
        Hmac::<Sha256>::new_from_slice(&salted_password).expect("HMAC key size");
    client_key_mac.update(b"Client Key");
    let client_key = client_key_mac.finalize().into_bytes();

    let stored_key = Sha256::digest(&client_key);

    let channel_binding = "c=biws"; // base64("n,,")
    let client_final_without_proof = format!("{},r={}", channel_binding, server_nonce);

    let auth_message = format!(
        "{},{},{}",
        client_first_bare, server_first, client_final_without_proof
    );

    let mut client_sig_mac = Hmac::<Sha256>::new_from_slice(&stored_key).expect("HMAC key size");
    client_sig_mac.update(auth_message.as_bytes());
    let client_signature = client_sig_mac.finalize().into_bytes();

    let mut client_proof = [0u8; 32];
    for i in 0..32 {
        client_proof[i] = client_key[i] ^ client_signature[i];
    }

    let proof_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &client_proof);
    let client_final = format!("{},p={}", client_final_without_proof, proof_b64);

    // Send SASLResponse
    let mut sasl_resp = BytesMut::new();
    sasl_resp.put_u8(b'p');
    sasl_resp.put_i32((client_final.len() + 4) as i32);
    sasl_resp.extend_from_slice(client_final.as_bytes());
    stream.write_all(&sasl_resp).await?;
    stream.flush().await?;

    // Read server final (AuthenticationSASLFinal)
    loop {
        let n = stream.read_buf(buf).await?;
        if n == 0 {
            bail!("Backend disconnected during SCRAM final");
        }
        if let Some(msg) = try_read_message(buf, false)? {
            if msg.msg_type == b'R' {
                let auth_type = (&msg.payload[0..4]).get_i32();
                if auth_type == 12 {
                    // SASLFinal — verify server signature
                    let server_final = std::str::from_utf8(&msg.payload[4..])?;
                    if let Some(sig) = server_final.strip_prefix("v=") {
                        let mut server_key_mac = Hmac::<Sha256>::new_from_slice(&salted_password)
                            .expect("HMAC key size");
                        server_key_mac.update(b"Server Key");
                        let server_key = server_key_mac.finalize().into_bytes();

                        let mut server_sig_mac =
                            Hmac::<Sha256>::new_from_slice(&server_key).expect("HMAC key size");
                        server_sig_mac.update(auth_message.as_bytes());
                        let expected_sig = server_sig_mac.finalize().into_bytes();

                        let expected_b64 = base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            &expected_sig,
                        );
                        if sig != expected_b64 {
                            bail!("SCRAM: server signature mismatch");
                        }
                    }
                    return Ok(());
                }
                if auth_type == 0 {
                    return Ok(());
                }
                bail!("Unexpected auth type in SCRAM final: {}", auth_type);
            }
            if msg.msg_type == b'E' {
                bail!(
                    "Backend error during SCRAM: {}",
                    parse_error_fields(&msg.payload)
                );
            }
        }
    }
}

/// PBKDF2-HMAC-SHA256.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut result = vec![0u8; 32];
    // U1 = HMAC(password, salt || INT(1))
    let mut mac = Hmac::<Sha256>::new_from_slice(password).expect("HMAC key size");
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let u1 = mac.finalize().into_bytes();
    result.copy_from_slice(&u1);

    let mut prev = u1.to_vec();
    for _ in 1..iterations {
        let mut mac = Hmac::<Sha256>::new_from_slice(password).expect("HMAC key size");
        mac.update(&prev);
        let ui = mac.finalize().into_bytes();
        for (r, u) in result.iter_mut().zip(ui.iter()) {
            *r ^= u;
        }
        prev = ui.to_vec();
    }

    result
}

/// Parse error fields from an ErrorResponse payload.
fn parse_error_fields(payload: &[u8]) -> String {
    let mut msg = String::new();
    let mut pos = 0;
    while pos < payload.len() {
        let field_type = payload[pos];
        if field_type == 0 {
            break;
        }
        pos += 1;
        let start = pos;
        while pos < payload.len() && payload[pos] != 0 {
            pos += 1;
        }
        if field_type == b'M' {
            if let Ok(s) = std::str::from_utf8(&payload[start..pos]) {
                msg = s.to_string();
            }
        }
        pos += 1;
    }
    if msg.is_empty() {
        "unknown error".to_string()
    } else {
        msg
    }
}

/// Reset a backend connection for reuse (cleanup session state).
pub async fn reset_connection(stream: &mut TcpStream) -> Result<()> {
    // Send DISCARD ALL to reset session state
    let query = b"DISCARD ALL\0";
    let mut msg = BytesMut::new();
    msg.put_u8(b'Q');
    msg.put_i32((query.len() + 4) as i32);
    msg.extend_from_slice(query);
    stream.write_all(&msg).await?;
    stream.flush().await?;

    // Read until ReadyForQuery
    let mut buf = BytesMut::with_capacity(1024);
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            bail!("Backend disconnected during reset");
        }
        while let Some(msg) = try_read_message(&mut buf, false)? {
            if msg.msg_type == b'Z' {
                return Ok(());
            }
            if msg.msg_type == b'E' {
                // DISCARD ALL failed — connection is not reusable
                bail!("Reset failed: {}", parse_error_fields(&msg.payload));
            }
        }
    }
}

/// Check if a backend connection is healthy.
pub async fn health_check(stream: &mut TcpStream) -> Result<()> {
    // Send a simple query
    let query = b"SELECT 1\0";
    let mut msg = BytesMut::new();
    msg.put_u8(b'Q');
    msg.put_i32((query.len() + 4) as i32);
    msg.extend_from_slice(query);
    stream.write_all(&msg).await?;
    stream.flush().await?;

    let mut buf = BytesMut::with_capacity(512);
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);

    loop {
        if start.elapsed() > timeout {
            bail!("Health check timed out");
        }
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            bail!("Backend disconnected during health check");
        }
        while let Some(msg) = try_read_message(&mut buf, false)? {
            if msg.msg_type == b'Z' {
                return Ok(());
            }
            if msg.msg_type == b'E' {
                bail!("Health check failed");
            }
        }
    }
}
