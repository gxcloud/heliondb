use quinn::crypto::rustls::QuicServerConfig;
use quinn::{Endpoint, Incoming, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};

use crate::error::{HelionError, Result};
use crate::server::session::{handle_connection, DatabaseMap};
use crate::storage::engine::DatabaseEngine;

pub struct QuicServer {
    databases: Arc<DatabaseMap>,
    default_database: String,
    addr: String,
    cert_path: Option<String>,
    key_path: Option<String>,
    client_ca_cert_path: Option<String>,
    client_cert_required: bool,
    max_concurrent_streams: u32,
    idle_timeout_seconds: u64,
}

#[allow(clippy::too_many_arguments)]
impl QuicServer {
    pub fn new(
        engine: DatabaseEngine,
        addr: &str,
        cert_path: Option<String>,
        key_path: Option<String>,
    ) -> Self {
        let mut databases = HashMap::new();
        databases.insert("default".to_string(), Arc::new(engine));
        QuicServer {
            databases: Arc::new(databases),
            default_database: "default".to_string(),
            addr: addr.to_string(),
            cert_path,
            key_path,
            client_ca_cert_path: None,
            client_cert_required: false,
            max_concurrent_streams: 100,
            idle_timeout_seconds: 30,
        }
    }

    pub fn with_databases(
        databases: HashMap<String, DatabaseEngine>,
        default_database: &str,
        addr: &str,
        cert_path: Option<String>,
        key_path: Option<String>,
        client_ca_cert_path: Option<String>,
        client_cert_required: bool,
        max_concurrent_streams: u32,
        idle_timeout_seconds: u64,
    ) -> Self {
        let map: DatabaseMap = databases
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect();
        QuicServer {
            databases: Arc::new(map),
            default_database: default_database.to_string(),
            addr: addr.to_string(),
            cert_path,
            key_path,
            client_ca_cert_path,
            client_cert_required,
            max_concurrent_streams,
            idle_timeout_seconds,
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
        info!(
            "Databases: {} (default: {})",
            self.databases
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            self.default_database
        );
        if self.client_ca_cert_path.is_some() {
            info!(
                "Client certificate authentication: {}",
                if self.client_cert_required {
                    "required"
                } else {
                    "optional"
                }
            );
        }

        while let Some(incoming) = endpoint.accept().await {
            let databases = self.databases.clone();
            let default_db = self.default_database.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(incoming, databases, default_db).await {
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
        transport.max_concurrent_bidi_streams(self.max_concurrent_streams.into());
        transport.max_idle_timeout(Some(
            std::time::Duration::from_secs(self.idle_timeout_seconds)
                .try_into()
                .unwrap(),
        ));

        let rustls_config: rustls::ServerConfig = if let Some(ca_path) = &self.client_ca_cert_path {
            let ca_bytes = std::fs::read(ca_path).map_err(|e| HelionError::Io(e.to_string()))?;
            let ca_certs: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut ca_bytes.as_slice())
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| HelionError::Io(e.to_string()))?;

            let mut root_store = rustls::RootCertStore::empty();
            for ca_cert in ca_certs {
                root_store
                    .add(ca_cert)
                    .map_err(|e| HelionError::Protocol(format!("CA cert error: {}", e)))?;
            }

            let builder = WebPkiClientVerifier::builder(Arc::new(root_store));
            let verifier = if self.client_cert_required {
                builder
                    .build()
                    .map_err(|e| HelionError::Protocol(format!("Client verifier error: {}", e)))?
            } else {
                builder
                    .allow_unauthenticated()
                    .build()
                    .map_err(|e| HelionError::Protocol(format!("Client verifier error: {}", e)))?
            };

            rustls::ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(cert, key)
                .map_err(|e| HelionError::Protocol(format!("TLS config error: {}", e)))?
        } else {
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert, key)
                .map_err(|e| HelionError::Protocol(format!("TLS config error: {}", e)))?
        };

        let quic_server_config = QuicServerConfig::try_from(rustls_config)
            .map_err(|e| HelionError::Protocol(format!("QUIC config error: {}", e)))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_server_config));
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

async fn handle_incoming(
    incoming: Incoming,
    databases: Arc<DatabaseMap>,
    default_database: String,
) -> Result<()> {
    let connecting = incoming
        .accept()
        .map_err(|e| HelionError::Protocol(format!("Accept error: {}", e)))?;

    handle_connection(connecting, databases, default_database).await
}
