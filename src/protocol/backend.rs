use anyhow::{bail, Result};
use bytes::{Buf, BufMut, BytesMut};
use postgres_protocol::authentication::sasl;
use postgres_protocol::authentication::md5_hash;
use postgres_protocol::message::frontend as pgfe;
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
                            let mut pw_buf = BytesMut::new();
                            pgfe::password_message(password.as_bytes(), &mut pw_buf)?;
                            stream.write_all(&pw_buf).await?;
                            stream.flush().await?;
                        }
                        5 => {
                            // MD5Password
                            if msg.payload.len() < 8 {
                                bail!("Invalid MD5 auth message");
                            }
                            let salt: [u8; 4] = msg.payload[4..8].try_into()?;
                            let hashed = md5_hash(key.user.as_bytes(), password.as_bytes(), salt);
                            let mut pw_buf = BytesMut::new();
                            pgfe::password_message(hashed.as_bytes(), &mut pw_buf)?;
                            stream.write_all(&pw_buf).await?;
                            stream.flush().await?;
                        }
                        10 => {
                            // SASL (SCRAM-SHA-256)
                            // Parse mechanism list, confirm SCRAM-SHA-256
                            let payload = &msg.payload[4..];
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

                            // Create SCRAM state machine
                            let mut scram = sasl::ScramSha256::new(
                                password.as_bytes(),
                                sasl::ChannelBinding::unsupported(),
                            );

                            // Send SASLInitialResponse with client-first message
                            let mut sasl_buf = BytesMut::new();
                            pgfe::sasl_initial_response("SCRAM-SHA-256", scram.message(), &mut sasl_buf)?;
                            stream.write_all(&sasl_buf).await?;
                            stream.flush().await?;

                            // Read SASLContinue (auth_type 11)
                            let server_first = loop {
                                let n = stream.read_buf(&mut buf).await?;
                                if n == 0 {
                                    bail!("Backend disconnected during SCRAM");
                                }
                                if let Some(msg) = try_read_message(&mut buf, false)? {
                                    if msg.msg_type == b'R' {
                                        let at = (&msg.payload[0..4]).get_i32();
                                        if at == 11 {
                                            break msg.payload[4..].to_vec();
                                        }
                                        bail!("Unexpected auth type during SCRAM: {}", at);
                                    }
                                    if msg.msg_type == b'E' {
                                        bail!(
                                            "Backend error during SCRAM: {}",
                                            parse_error_fields(&msg.payload)
                                        );
                                    }
                                }
                            };

                            // Update SCRAM with server-first message
                            scram.update(&server_first)?;

                            // Send SASLResponse with client-final message
                            let mut resp_buf = BytesMut::new();
                            pgfe::sasl_response(scram.message(), &mut resp_buf)?;
                            stream.write_all(&resp_buf).await?;
                            stream.flush().await?;

                            // Read SASLFinal (auth_type 12)
                            loop {
                                let n = stream.read_buf(&mut buf).await?;
                                if n == 0 {
                                    bail!("Backend disconnected during SCRAM final");
                                }
                                if let Some(msg) = try_read_message(&mut buf, false)? {
                                    if msg.msg_type == b'R' {
                                        let at = (&msg.payload[0..4]).get_i32();
                                        if at == 12 {
                                            scram.finish(&msg.payload[4..])?;
                                            break;
                                        }
                                        if at == 0 {
                                            break;
                                        }
                                        bail!("Unexpected auth type in SCRAM final: {}", at);
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
#[allow(dead_code)]
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
