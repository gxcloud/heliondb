use crate::error::Result;

pub struct QuicServer;

impl QuicServer {
    pub fn new() -> Self {
        QuicServer
    }

    pub async fn start(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_quic_server_new() {
        let server = QuicServer::new();
        assert!(server.start().await.is_ok());
    }
}
