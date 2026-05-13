use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use heliondb::server::quic::QuicServer;
use heliondb::storage::engine::DatabaseEngine;

#[derive(Parser, Debug)]
#[command(
    name = "heliondb",
    version,
    about = "In-memory SQL database with QUIC transport"
)]
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
    #[arg(long, default_value = "memory")]
    default_engine: String,
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
    info!("Data directory: {}", cli.data_dir);
    info!("Listen address: quic://{}", cli.listen);

    // Open database engine (creates or replays WAL)
    let engine =
        DatabaseEngine::open_with_default_engine(cli.data_dir.as_ref(), &cli.default_engine)
            .await?;

    info!("Database engine initialized");

    // Start QUIC server
    let server = QuicServer::new(engine, &cli.listen, cli.cert, cli.key);

    server.start().await?;

    Ok(())
}
