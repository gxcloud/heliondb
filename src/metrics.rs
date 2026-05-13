use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info};

// ── Metrics Registry ─────────────────────────────────────────────────

#[derive(Default)]
pub struct MetricsRegistry {
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub queries_total: AtomicU64,
    pub queries_select: AtomicU64,
    pub queries_insert: AtomicU64,
    pub queries_update: AtomicU64,
    pub queries_delete: AtomicU64,
    pub queries_ddl: AtomicU64,
    pub mvcc_conflicts_total: AtomicU64,
    pub wal_size_bytes: AtomicU64,
    pub checkpoint_duration_seconds: AtomicU64,
}

impl MetricsRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(MetricsRegistry::default())
    }
}

/// Start a Prometheus metrics HTTP server on the given address.
/// Serves Prometheus text format on GET /metrics.
pub async fn serve_metrics(
    registry: Arc<MetricsRegistry>,
    addr: &str,
) -> Result<(), crate::error::HelionError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| crate::error::HelionError::Io(e.to_string()))?;

    info!("Metrics endpoint listening on http://{}/metrics", addr);

    loop {
        let (mut stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Metrics accept error: {}", e);
                continue;
            }
        };
        let reg = registry.clone();

        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;

            let mut buf = String::new();
            buf.push_str("# HELP heliondb_connections_total Total connections established\n");
            buf.push_str("# TYPE heliondb_connections_total counter\n");
            buf.push_str(&format!(
                "heliondb_connections_total {}\n",
                reg.connections_total.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_connections_active Currently active connections\n");
            buf.push_str("# TYPE heliondb_connections_active gauge\n");
            buf.push_str(&format!(
                "heliondb_connections_active {}\n",
                reg.connections_active.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_total Total queries executed\n");
            buf.push_str("# TYPE heliondb_queries_total counter\n");
            buf.push_str(&format!(
                "heliondb_queries_total {}\n",
                reg.queries_total.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_select SELECT queries\n");
            buf.push_str("# TYPE heliondb_queries_select counter\n");
            buf.push_str(&format!(
                "heliondb_queries_select {}\n",
                reg.queries_select.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_insert INSERT queries\n");
            buf.push_str("# TYPE heliondb_queries_insert counter\n");
            buf.push_str(&format!(
                "heliondb_queries_insert {}\n",
                reg.queries_insert.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_update UPDATE queries\n");
            buf.push_str("# TYPE heliondb_queries_update counter\n");
            buf.push_str(&format!(
                "heliondb_queries_update {}\n",
                reg.queries_update.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_delete DELETE queries\n");
            buf.push_str("# TYPE heliondb_queries_delete counter\n");
            buf.push_str(&format!(
                "heliondb_queries_delete {}\n",
                reg.queries_delete.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_queries_ddl DDL queries\n");
            buf.push_str("# TYPE heliondb_queries_ddl counter\n");
            buf.push_str(&format!(
                "heliondb_queries_ddl {}\n",
                reg.queries_ddl.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_mvcc_conflicts_total MVCC conflict count\n");
            buf.push_str("# TYPE heliondb_mvcc_conflicts_total counter\n");
            buf.push_str(&format!(
                "heliondb_mvcc_conflicts_total {}\n",
                reg.mvcc_conflicts_total.load(Ordering::Relaxed)
            ));

            buf.push_str("# HELP heliondb_wal_size_bytes Current WAL file size\n");
            buf.push_str("# TYPE heliondb_wal_size_bytes gauge\n");
            buf.push_str(&format!(
                "heliondb_wal_size_bytes {}\n",
                reg.wal_size_bytes.load(Ordering::Relaxed)
            ));

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                buf.len(),
                buf
            );

            if let Err(e) = stream.write_all(response.as_bytes()).await {
                error!("Failed to write metrics response to {}: {}", peer, e);
            }
        });
    }
}
