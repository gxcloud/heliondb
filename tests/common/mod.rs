

use heliondb::error::{HelionError, Result};
use heliondb::executor::ops::{execute, execute_as, QueryResult};
use heliondb::server::quic::QuicServer;
use heliondb::sql::parser::parse;
use heliondb::sql::planner::plan;
use heliondb::storage::engine::DatabaseEngine;
use heliondb::client::conn::ClientConn;
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Set up an in-memory database engine in a temp directory.
/// Must be called from within a tokio runtime context.
pub async fn setup() -> (DatabaseEngine, TempDir) {
    let dir = TempDir::new().unwrap();
    let engine = DatabaseEngine::open(dir.path()).await.unwrap();
    (engine, dir)
}

/// Execute SQL against the engine via the library pipeline.
pub async fn exec(engine: &DatabaseEngine, sql: &str) -> QueryResult {
    let stmts = parse(sql).unwrap();
    let mut last_result = None;
    for stmt in &stmts {
        let tables = engine.get_tables().await;
        let plan = plan(stmt, &tables).unwrap();
        last_result = Some(execute(engine, &plan).await.unwrap());
    }
    last_result.unwrap()
}

/// Execute SQL as a specific user.
pub async fn exec_as(engine: &DatabaseEngine, sql: &str, user: &str) -> Result<QueryResult> {
    let stmts = parse(sql)?;
    let mut last_result = None;
    for stmt in &stmts {
        let tables = engine.get_tables().await;
        let p = plan(stmt, &tables)?;
        last_result = Some(execute_as(engine, &p, Some(user)).await?);
    }
    last_result.ok_or_else(|| HelionError::Internal("No statements executed".into()))
}

// ── TestServer: in-process QUIC server for e2e tests ─────

pub struct TestServer {
    pub server_handle: JoinHandle<Result<()>>,
    pub addr: std::net::SocketAddr,
    pub default_password: String,
    _dir: TempDir,
}

impl TestServer {
    /// Create a new test server on an ephemeral port.
    /// Ensures the crypto provider is installed and a default user exists.
    pub async fn new() -> Self {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();

        let dir = TempDir::new().unwrap();
        let password = "test-password-123".to_string();
        std::env::set_var("HELION_PASSWORD", &password);
        let engine = DatabaseEngine::open(dir.path()).await.unwrap();
        std::env::remove_var("HELION_PASSWORD");

        let server = QuicServer::new(engine, "127.0.0.1:0", None, None);
        let (addr, server_handle) = server.start_background().await.unwrap();

        TestServer {
            server_handle,
            addr,
            default_password: password,
            _dir: dir,
        }
    }

    /// Create a TestClient connected and authenticated to this server.
    pub async fn connect(&self, user: &str) -> ClientConn {
        ClientConn::connect(
            &self.addr.to_string(),
            "localhost",
            user,
            &self.default_password,
            "default",
            true,  // insecure (self-signed cert)
            None,  // no client cert
        )
        .await
        .unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server_handle.abort();
    }
}
