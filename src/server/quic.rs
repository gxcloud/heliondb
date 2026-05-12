use quinn::{Endpoint, Incoming, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::error::{HelionError, Result};
use crate::server::session::handle_connection;
use crate::storage::engine::DatabaseEngine;

pub struct QuicServer {
    engine: Arc<Mutex<DatabaseEngine>>,
    addr: String,
    cert_path: Option<String>,
    key_path: Option<String>,
}

impl QuicServer {
    pub fn new(
        engine: DatabaseEngine,
        addr: &str,
        cert_path: Option<String>,
        key_path: Option<String>,
    ) -> Self {
        QuicServer {
            engine: Arc::new(Mutex::new(engine)),
            addr: addr.to_string(),
            cert_path,
            key_path,
        }
    }

    pub async fn start(&self) -> Result<()> {
        let addr: std::net::SocketAddr = self
            .addr
            .parse()
            .map_err(|e| HelionError::Protocol(format!("Invalid listen address: {}", e)))?;

        let (cert, key) = self.load_or_generate_certs().await?;
        let server_config = self.make_server_config(cert, key)?;

        let endpoint = Endpoint::server(server_config, addr)
            .map_err(|e| HelionError::Protocol(format!("Failed to bind: {}", e)))?;

        info!("HelionDB listening on quic://{}", addr);

        while let Some(incoming) = endpoint.accept().await {
            let engine = self.engine.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(incoming, engine).await {
                    error!("Connection error: {}", e);
                }
            });
        }

        Ok(())
    }

    fn make_server_config(
        &self,
        cert: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<ServerConfig> {
        let mut transport = TransportConfig::default();
        transport.max_concurrent_bidi_streams(100u32.into());
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into().unwrap()));

        let mut server_config = ServerConfig::with_single_cert(cert, key)
            .map_err(|e| HelionError::Protocol(format!("TLS config error: {}", e)))?;
        server_config.transport_config(Arc::new(transport));

        Ok(server_config)
    }

    async fn load_or_generate_certs(
        &self,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        if let (Some(cert_path), Some(key_path)) = (&self.cert_path, &self.key_path) {
            let cert_path = Path::new(cert_path);
            let key_path = Path::new(key_path);
            if cert_path.exists() && key_path.exists() {
                let cert_bytes = tokio::fs::read(cert_path).await?;
                let key_bytes = tokio::fs::read(key_path).await?;

                let certs: Vec<CertificateDer<'static>> =
                    rustls_pemfile::certs(&mut cert_bytes.as_slice())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|e| HelionError::Io(e.to_string()))?;

                let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
                    .map_err(|e| HelionError::Io(e.to_string()))?
                    .ok_or_else(|| HelionError::Protocol("No private key found".into()))?;

                return Ok((certs, key));
            }
        }

        info!("Generating self-signed TLS certificate...");
        Self::generate_self_signed_cert()
    }

    fn generate_self_signed_cert() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>
    {
        let certified_key = rcgen::generate_simple_self_signed(vec![
            "heliondb.local".to_string(),
            "localhost".to_string(),
        ])
        .map_err(|e| HelionError::Protocol(format!("Cert generation error: {}", e)))?;

        let cert_der = certified_key.cert.der().as_ref().to_vec();
        let key_der = certified_key.key_pair.serialize_der();

        Ok((
            vec![CertificateDer::from(cert_der)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        ))
    }
}

async fn handle_incoming(incoming: Incoming, engine: Arc<Mutex<DatabaseEngine>>) -> Result<()> {
    let connecting = incoming
        .accept()
        .map_err(|e| HelionError::Protocol(format!("Accept error: {}", e)))?;

    handle_connection(connecting, engine).await
}
