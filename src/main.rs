use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use heliondb::server::quic::QuicServer;
use heliondb::storage::engine::DatabaseEngine;

use std::collections::HashMap;

#[derive(Parser, Debug)]
#[command(name = "heliondb", version, about = "SQL database with QUIC transport")]
struct Cli {
    /// Data directory for WAL and checkpoint files
    #[arg(long, default_value = "./data")]
    data_dir: String,

    /// QUIC listen address
    #[arg(long, default_value = "127.0.0.1:9613")]
    listen: String,

    /// TLS certificate file (PEM)
    #[arg(long)]
    cert: Option<String>,

    /// TLS private key file (PEM)
    #[arg(long)]
    key: Option<String>,

    /// Durability mode: async (fast) or sync (safe)
    #[arg(long, default_value = "async")]
    durability: String,

    /// Default storage engine for new tables
    #[arg(long, default_value = "disk")]
    default_engine: String,

    /// Named databases (comma-separated, e.g. "mydb=/path/to/db,analytics=/path/to/analytics")
    #[arg(long)]
    databases: Option<String>,

    /// Default database for connections that don't specify one
    #[arg(long, default_value = "default")]
    default_database: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install ring CryptoProvider (rustls)");

    let cli = Cli::parse();

    info!("HelionDB v{} starting...", env!("CARGO_PKG_VERSION"));
    info!("Listen address: quic://{}", cli.listen);

    // Open database(s)
    let databases: HashMap<String, DatabaseEngine> = if let Some(db_config) = &cli.databases {
        // Parse comma-separated name=path pairs
        let mut dbs = HashMap::new();
        for entry in db_config.split(',') {
            let entry = entry.trim();
            if let Some((name, path)) = entry.split_once('=') {
                let name = name.trim().to_string();
                let path = path.trim();
                info!("Opening database '{}' at {}", name, path);
                let engine =
                    DatabaseEngine::open_with_default_engine(path.as_ref(), &cli.default_engine)
                        .await?;
                dbs.insert(name, engine);
            }
        }
        dbs
    } else {
        // Single default database
        info!("Data directory: {}", cli.data_dir);
        let engine =
            DatabaseEngine::open_with_default_engine(cli.data_dir.as_ref(), &cli.default_engine)
                .await?;
        let mut dbs = HashMap::new();
        dbs.insert("default".to_string(), engine);
        dbs
    };

    info!(
        "Database engines initialized ({} databases)",
        databases.len()
    );

    // Start QUIC server
    let server = QuicServer::with_databases(
        databases,
        &cli.default_database,
        &cli.listen,
        cli.cert,
        cli.key,
    );

    server.start().await?;

    Ok(())
}
