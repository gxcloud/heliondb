use std::sync::Arc;

use quinn::{ClientConfig, Connection, Endpoint};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tracing::debug;

use crate::error::HelionError;
use crate::executor::ops::QueryResult;
use crate::protocol::messages::{ClientMessage, ServerMessage};
use quinn::crypto::rustls::QuicClientConfig;

/// A client connection to a HelionDB server over QUIC.
pub struct ClientConn {
    pub connection: Connection,
    pub token: u64,
    pub username: String,
}

impl ClientConn {
    /// Connect to a HelionDB server and authenticate.
    pub async fn connect(
        addr: &str,
        server_name: &str,
        username: &str,
        password: &str,
        insecure: bool,
    ) -> std::result::Result<Self, HelionError> {
        let endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| HelionError::Protocol(format!("Endpoint error: {}", e)))?;

        let tls_config = if insecure {
            let tls = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(SkipServerVerification::new())
                .with_no_client_auth();
            Arc::new(tls)
        } else {
            let root_store = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let tls = rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth();
            Arc::new(tls)
        };

        let quic_client_config = QuicClientConfig::try_from(tls_config.as_ref().clone())
            .map_err(|e| HelionError::Protocol(format!("TLS config error: {}", e)))?;
        let mut quinn_config = ClientConfig::new(Arc::new(quic_client_config));
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into().unwrap()));
        quinn_config.transport_config(Arc::new(transport));

        let server_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| HelionError::Protocol(format!("Invalid address: {}", e)))?;

        debug!("Connecting to quic://{}", addr);
        let connecting = endpoint
            .connect_with(quinn_config, server_addr, server_name)
            .map_err(|e| HelionError::Protocol(format!("Connect error: {}", e)))?;

        let connection = connecting
            .await
            .map_err(|e| HelionError::Protocol(format!("Connection failed: {}", e)))?;

        debug!("Connected, authenticating as '{}'", username);
        let token = Self::authenticate(&connection, username, password).await?;

        Ok(ClientConn {
            connection,
            token,
            username: username.to_string(),
        })
    }

    async fn authenticate(
        connection: &Connection,
        username: &str,
        password: &str,
    ) -> std::result::Result<u64, HelionError> {
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|e| HelionError::Protocol(format!("Stream error: {}", e)))?;

        let auth_msg = ClientMessage::Auth {
            username: username.to_string(),
            password: password.to_string(),
        };
        Self::send_message(&mut send, &auth_msg).await?;

        let response = Self::receive_message(&mut recv).await?;
        match response {
            ServerMessage::AuthResult {
                success: true,
                token,
                ..
            } => {
                debug!("Authenticated, token={}", token);
                Ok(token)
            }
            ServerMessage::AuthResult {
                success: false,
                error: Some(msg),
                ..
            } => Err(HelionError::Auth(msg)),
            ServerMessage::Error { message } => Err(HelionError::Auth(message)),
            _ => Err(HelionError::Auth("Unexpected auth response".into())),
        }
    }

    /// Execute a SQL query and return the result.
    pub async fn query(&self, sql: &str) -> std::result::Result<QueryResult, HelionError> {
        let (mut send, mut recv) = self
            .connection
            .open_bi()
            .await
            .map_err(|e| HelionError::Protocol(format!("Stream error: {}", e)))?;

        let query_msg = ClientMessage::Query {
            sql: sql.to_string(),
            token: self.token,
        };
        Self::send_message(&mut send, &query_msg).await?;

        let response = Self::receive_message(&mut recv).await?;
        match response {
            ServerMessage::QueryResult {
                columns,
                rows,
                error: None,
            } => {
                let count = rows.len() as u64;
                Ok(QueryResult {
                    columns,
                    column_types: vec![],
                    rows,
                    rows_affected: count,
                })
            }
            ServerMessage::QueryResult {
                error: Some(msg), ..
            } => Err(HelionError::Internal(msg)),
            ServerMessage::Error { message } => Err(HelionError::Internal(message)),
            _ => Err(HelionError::Internal("Unexpected response".into())),
        }
    }

    /// Close the connection.
    pub async fn close(&self) {
        self.connection.close(0u32.into(), b"bye");
    }

    // ── Protocol I/O ──────────────────────────────────────────────

    async fn send_message(
        send: &mut quinn::SendStream,
        msg: &ClientMessage,
    ) -> std::result::Result<(), HelionError> {
        let bytes = bincode::serialize(msg)
            .map_err(|e| HelionError::Protocol(format!("Serialize error: {}", e)))?;
        let len = bytes.len() as u32;
        send.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| HelionError::Protocol(format!("Write error: {}", e)))?;
        send.write_all(&bytes)
            .await
            .map_err(|e| HelionError::Protocol(format!("Write error: {}", e)))?;
        let _ = send.finish();
        Ok(())
    }

    async fn receive_message(
        recv: &mut quinn::RecvStream,
    ) -> std::result::Result<ServerMessage, HelionError> {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| HelionError::Protocol(format!("Read length error: {}", e)))?;
        let msg_len = u32::from_be_bytes(len_buf) as usize;

        let mut buf = vec![0u8; msg_len];
        if msg_len > 0 {
            recv.read_exact(&mut buf)
                .await
                .map_err(|e| HelionError::Protocol(format!("Read message error: {}", e)))?;
        }

        bincode::deserialize(&buf)
            .map_err(|e| HelionError::Protocol(format!("Deserialize error: {}", e)))
    }
}

/// Dummy certificate verifier that treats any certificate as valid.
#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
