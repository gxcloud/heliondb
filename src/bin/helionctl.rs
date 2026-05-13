use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use heliondb::client::conn::ClientConn;
use heliondb::client::output::print_result;
use heliondb::client::repl::Repl;

#[derive(Parser, Debug)]
#[command(name = "helionctl", version, about = "HelionDB SQL shell")]
struct Args {
    #[arg(long, short, default_value = "127.0.0.1:9613", help = "Server address")]
    host: String,

    #[arg(
        long,
        short,
        help = "Username [env: HELIONDB_USER]",
        env = "HELIONDB_USER",
        default_value = "helion"
    )]
    user: String,

    #[arg(
        long,
        short,
        help = "Password [env: HELIONDB_PASSWORD]",
        env = "HELIONDB_PASSWORD"
    )]
    password: String,

    #[arg(long, default_value = "heliondb.local", help = "TLS server name (SNI)")]
    server_name: String,

    #[arg(
        long,
        help = "Skip TLS certificate verification (for self-signed certs)"
    )]
    insecure: bool,

    #[arg(help = "Optional SQL to execute as a single query and exit")]
    sql: Option<String>,
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

    let args = Args::parse();

    let conn = ClientConn::connect(
        &args.host,
        &args.server_name,
        &args.user,
        &args.password,
        args.insecure,
    )
    .await?;

    info!("Connected to HelionDB at {} as '{}'", args.host, args.user);

    match args.sql {
        Some(sql) => {
            // Single query mode
            match conn.query(&sql).await {
                Ok(result) => {
                    print_result(&result, false);
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
            conn.close().await;
        }
        None => {
            // Interactive REPL
            let mut repl = Repl::new(conn);
            repl.run().await;
        }
    }

    Ok(())
}
