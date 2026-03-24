use anyhow::{bail, Context, Result};
use std::fs;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

use crate::config::TlsConfig;

/// Build a TLS acceptor for client-facing connections.
pub fn build_server_tls_acceptor(config: &TlsConfig) -> Result<Arc<TlsAcceptor>> {
    if config.cert_path.is_empty() || config.key_path.is_empty() {
        bail!("TLS enabled but cert_path or key_path not set");
    }

    let cert_pem = fs::read(&config.cert_path)
        .with_context(|| format!("Reading cert: {}", config.cert_path))?;
    let key_pem =
        fs::read(&config.key_path).with_context(|| format!("Reading key: {}", config.key_path))?;

    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .context("Parsing TLS certificates")?;

    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .context("Parsing TLS private key")?
        .ok_or_else(|| anyhow::anyhow!("No private key found in key file"))?;

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("Building TLS server config")?;

    Ok(Arc::new(TlsAcceptor::from(Arc::new(server_config))))
}
