use clap::Parser;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use heliondb::config::Config;
use heliondb::metrics::{serve_metrics, MetricsRegistry};
use heliondb::server::quic::QuicServer;
use heliondb::storage::engine::DatabaseEngine;

#[derive(Parser, Debug)]
#[command(name = "heliondb", version, about = "SQL database with QUIC transport")]
struct Cli {
    /// Path to TOML configuration file [env: HELIONDB_CONFIG]
    #[arg(long, env = "HELIONDB_CONFIG")]
    config: Option<String>,

    /// Data directory for WAL and checkpoint files (overrides config file)
    #[arg(long)]
    data_dir: Option<String>,

    /// QUIC listen address (overrides config file)
    #[arg(long)]
    listen: Option<String>,

    /// TLS certificate file (PEM) (overrides config file)
    #[arg(long)]
    cert: Option<String>,

    /// TLS private key file (PEM) (overrides config file)
    #[arg(long)]
    key: Option<String>,

    /// Durability mode: async (fast) or sync (safe) (overrides config file)
    #[arg(long)]
    durability: Option<String>,

    /// Default storage engine for new tables (overrides config file)
    #[arg(long)]
    default_engine: Option<String>,

    /// Named databases (comma-separated, e.g. "mydb=/path/to/db") (overrides config file)
    #[arg(long)]
    databases: Option<String>,

    /// Default database for connections that don't specify one (overrides config file)
    #[arg(long)]
    default_database: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install ring CryptoProvider (rustls)");

    let cli = Cli::parse();

    // Load configuration
    let cfg = Config::load(cli.config.as_deref());

    // Merge: CLI overrides config file
    let data_dir = cli.data_dir.unwrap_or(cfg.storage.data_dir.clone());
    let listen = cli.listen.unwrap_or(cfg.server.listen.clone());
    let default_engine = cli
        .default_engine
        .unwrap_or(cfg.storage.default_engine.clone());
    let default_database = cli
        .default_database
        .unwrap_or(cfg.server.default_database.clone());
    let durability = cli.durability.unwrap_or(cfg.storage.durability.clone());
    let cert_path = cli.cert.or(cfg.tls.cert_path.clone());
    let key_path = cli.key.or(cfg.tls.key_path.clone());
    let checkpoint_interval = cfg.storage.checkpoint_interval_seconds;
    let max_streams = cfg.server.max_concurrent_streams;
    let idle_timeout = cfg.server.idle_timeout_seconds;

    info!("HelionDB v{} starting...", env!("CARGO_PKG_VERSION"));
    info!("Listen address: quic://{}", listen);

    // Open database(s)
    let databases: HashMap<String, DatabaseEngine> = if let Some(db_config) = &cli.databases {
        // Parse comma-separated name=path pairs from CLI (highest priority)
        let mut dbs = HashMap::new();
        for entry in db_config.split(',') {
            let entry = entry.trim();
            if let Some((name, path)) = entry.split_once('=') {
                let name = name.trim().to_string();
                let path = path.trim();
                info!("Opening database '{}' at {}", name, path);
                let engine = DatabaseEngine::open_with_durability(
                    path.as_ref(),
                    &default_engine,
                    checkpoint_interval,
                    &durability,
                )
                .await?;
                dbs.insert(name, engine);
            }
        }
        dbs
    } else if !cfg.database.is_empty() {
        // Use databases from config file
        let mut dbs = HashMap::new();
        for db_cfg in &cfg.database {
            let engine = db_cfg
                .engine
                .as_deref()
                .unwrap_or(&default_engine)
                .to_string();
            info!(
                "Opening database '{}' at {} (engine: {})",
                db_cfg.name, db_cfg.path, engine
            );
            let engine = DatabaseEngine::open_with_durability(
                db_cfg.path.as_ref(),
                &engine,
                checkpoint_interval,
                &durability,
            )
            .await?;
            dbs.insert(db_cfg.name.clone(), engine);
        }
        dbs
    } else {
        // Single default database
        info!("Data directory: {}", data_dir);
        let engine = DatabaseEngine::open_with_durability(
            data_dir.as_ref(),
            &default_engine,
            checkpoint_interval,
            &durability,
        )
        .await?;
        let mut dbs = HashMap::new();
        dbs.insert("default".to_string(), engine);
        dbs
    };

    info!(
        "Database engines initialized ({} databases)",
        databases.len()
    );

    // Wrap databases in Arc for signal handler access
    let databases: HashMap<String, Arc<DatabaseEngine>> = databases
        .into_iter()
        .map(|(name, engine)| (name, Arc::new(engine)))
        .collect();
    let shutdown_token = CancellationToken::new();

    // Graceful shutdown on SIGINT/SIGTERM
    {
        let token = shutdown_token.clone();
        let dbs = databases.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            info!("Shutdown signal received, initiating graceful shutdown...");

            #[cfg(unix)]
            {
                // Also listen for SIGTERM
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
                if let Some(ref mut s) = term {
                    s.recv().await;
                }
            }

            token.cancel();
            for (name, engine) in dbs.iter() {
                info!("Shutting down database '{}'...", name);
                if let Err(e) = engine.shutdown().await {
                    error!("Error shutting down database '{}': {}", name, e);
                }
            }
            info!("Shutdown complete.");
            std::process::exit(0);
        });
    }

    // Start Prometheus metrics HTTP endpoint if configured
    let metrics_registry = MetricsRegistry::new();
    if cfg.observability.metrics_port > 0 {
        let reg = metrics_registry.clone();
        let metrics_addr = format!("0.0.0.0:{}", cfg.observability.metrics_port);
        tokio::spawn(async move {
            if let Err(e) = serve_metrics(reg, &metrics_addr).await {
                error!("Metrics server error: {}", e);
            }
        });
    }

    // Start QUIC server
    let server = QuicServer::with_databases_and_cancel(
        databases,
        &default_database,
        &listen,
        cert_path,
        key_path,
        cfg.tls.client_ca_cert_path.clone(),
        cfg.tls.client_cert_required,
        max_streams,
        idle_timeout,
        shutdown_token,
    );

    server.start().await?;

    Ok(())
}
