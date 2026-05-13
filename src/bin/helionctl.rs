use std::path::Path;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

use heliondb::client::cert::{generate_ca, generate_client, generate_server, write_key_pair};
use heliondb::client::conn::ClientConn;
use heliondb::client::output::print_result;
use heliondb::client::repl::Repl;

#[derive(Parser, Debug)]
#[command(name = "helionctl", version, about = "HelionDB SQL shell")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Connect and enter interactive SQL shell (default if no subcommand given)
    #[command(name = "shell", hide = true)]
    Shell(ShellArgs),

    /// Execute a single SQL query and exit
    Query(QueryArgs),

    /// Generate TLS certificates for mutual TLS (mTLS)
    Cert {
        #[command(subcommand)]
        action: CertAction,
    },
}

#[derive(clap::Args, Debug, Clone)]
struct ConnectionArgs {
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

    #[arg(long, short = 'd', default_value = "default", help = "Database name")]
    database: String,

    #[arg(
        long,
        help = "Skip TLS certificate verification (for self-signed certs)"
    )]
    insecure: bool,

    #[arg(
        long,
        help = "Client TLS certificate file (PEM) for certificate-based authentication"
    )]
    client_cert: Option<String>,

    #[arg(
        long,
        help = "Client TLS private key file (PEM) for certificate-based authentication"
    )]
    client_key: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ShellArgs {
    #[command(flatten)]
    conn: ConnectionArgs,
}

#[derive(clap::Args, Debug)]
struct QueryArgs {
    #[command(flatten)]
    conn: ConnectionArgs,

    #[arg(required = true, help = "SQL query to execute")]
    sql: String,
}

#[derive(Subcommand, Debug)]
enum CertAction {
    /// Generate a self-signed CA certificate and key
    Ca {
        #[arg(
            long,
            default_value = "HelionDB Root CA",
            help = "Common Name for the CA"
        )]
        cn: String,

        #[arg(
            long,
            default_value = "ca.pem",
            help = "Output path for the CA certificate (PEM)"
        )]
        out: String,

        #[arg(
            long,
            default_value = "ca-key.pem",
            help = "Output path for the CA private key (PEM)"
        )]
        key_out: String,
    },
    /// Generate a server certificate signed by a CA
    Server {
        #[arg(long, required = true, help = "Path to CA certificate (PEM)")]
        ca: String,

        #[arg(long, required = true, help = "Path to CA private key (PEM)")]
        ca_key: String,

        #[arg(
            long,
            default_value = "heliondb.local",
            help = "Common Name for the server certificate"
        )]
        cn: String,

        #[arg(
            long,
            help = "Additional SANs (Subject Alternative Names), e.g. --san example.com"
        )]
        san: Vec<String>,

        #[arg(
            long,
            default_value = "server.pem",
            help = "Output path for the server certificate (PEM)"
        )]
        out: String,

        #[arg(
            long,
            default_value = "server-key.pem",
            help = "Output path for the server private key (PEM)"
        )]
        key_out: String,
    },
    /// Generate a client certificate signed by a CA (CN becomes database username)
    Client {
        #[arg(long, required = true, help = "Path to CA certificate (PEM)")]
        ca: String,

        #[arg(long, required = true, help = "Path to CA private key (PEM)")]
        ca_key: String,

        #[arg(
            long,
            required = true,
            help = "Common Name (used as the database username)"
        )]
        cn: String,

        #[arg(
            long,
            default_value = "client.pem",
            help = "Output path for the client certificate (PEM)"
        )]
        out: String,

        #[arg(
            long,
            default_value = "client-key.pem",
            help = "Output path for the client private key (PEM)"
        )]
        key_out: String,
    },
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

    match cli.command {
        Some(Command::Cert { action }) => match action {
            CertAction::Ca { cn, out, key_out } => {
                let kp = generate_ca(&cn)?;
                write_key_pair(Path::new(&out), Path::new(&key_out), &kp)?;
                println!("CA certificate written to {}", out);
                println!("CA key written to {}", key_out);
                println!();
                println!("WARNING: The CA private key is the root of trust for all");
                println!("certificates signed by it. Keep it secure and offline.");
                println!();
                println!("Next steps:");
                println!(
                    "  {} cert server --ca {} --ca-key {} --cn heliondb.local",
                    get_program_name(),
                    out,
                    key_out
                );
                println!(
                    "  {} cert client --ca {} --ca-key {} --cn <username>",
                    get_program_name(),
                    out,
                    key_out
                );
            }
            CertAction::Server {
                ca,
                ca_key,
                cn,
                san,
                out,
                key_out,
            } => {
                let ca_pem = std::fs::read_to_string(&ca)?;
                let ca_key_pem = std::fs::read_to_string(&ca_key)?;
                let sans = if san.is_empty() {
                    vec![cn.clone(), "localhost".to_string()]
                } else {
                    san
                };
                let kp = generate_server(&cn, &sans, &ca_pem, &ca_key_pem)?;
                write_key_pair(Path::new(&out), Path::new(&key_out), &kp)?;
                println!("Server certificate written to {}", out);
                println!("Server key written to {}", key_out);
                println!();
                println!("Configure the server:");
                println!("  [tls]");
                println!("  cert_path = \"{}\"", out);
                println!("  key_path = \"{}\"", key_out);
            }
            CertAction::Client {
                ca,
                ca_key,
                cn,
                out,
                key_out,
            } => {
                let ca_pem = std::fs::read_to_string(&ca)?;
                let ca_key_pem = std::fs::read_to_string(&ca_key)?;
                let kp = generate_client(&cn, &ca_pem, &ca_key_pem)?;
                write_key_pair(Path::new(&out), Path::new(&key_out), &kp)?;
                println!("Client certificate written to {}", out);
                println!("Client key written to {}", key_out);
                println!();
                println!("CN '{}' can be used as the database username.", cn);
                println!();
                println!("Connect with:");
                println!(
                    "  {} --user {} --client-cert {} --client-key {}",
                    get_program_name(),
                    cn,
                    out,
                    key_out
                );
            }
        },
        Some(Command::Query(args)) => {
            run_query(args.conn, &args.sql).await?;
        }
        Some(Command::Shell(args)) => {
            run_shell(args.conn).await?;
        }
        None => {
            run_shell(ConnectionArgs::default()).await?;
        }
    }

    Ok(())
}

fn get_program_name() -> String {
    std::env::args()
        .next()
        .unwrap_or_else(|| "helionctl".into())
}

async fn load_client_certs(
    cert_path: &Option<String>,
    key_path: &Option<String>,
) -> anyhow::Result<
    Option<(
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    )>,
> {
    match (cert_path, key_path) {
        (Some(cert_path), Some(key_path)) => {
            let cert_bytes = std::fs::read(cert_path)?;
            let key_bytes = std::fs::read(key_path)?;
            let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
                rustls_pemfile::certs(&mut cert_bytes.as_slice())
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| anyhow::anyhow!("Failed to parse client cert: {}", e))?;
            let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
                .map_err(|e| anyhow::anyhow!("Failed to parse client key: {}", e))?
                .ok_or_else(|| anyhow::anyhow!("No private key found in client key file"))?;
            Ok(Some((certs, key)))
        }
        _ => Ok(None),
    }
}

async fn run_query(args: ConnectionArgs, sql: &str) -> anyhow::Result<()> {
    let client_certs = load_client_certs(&args.client_cert, &args.client_key).await?;

    let mut conn = ClientConn::connect(
        &args.host,
        &args.server_name,
        &args.user,
        &args.password,
        &args.database,
        args.insecure,
        client_certs,
    )
    .await?;

    info!("Connected to HelionDB at {} as '{}'", args.host, args.user);

    match conn.query(sql).await {
        Ok(result) => {
            print_result(&result, false);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
    conn.close().await;
    Ok(())
}

async fn run_shell(args: ConnectionArgs) -> anyhow::Result<()> {
    let client_certs = load_client_certs(&args.client_cert, &args.client_key).await?;

    let conn = ClientConn::connect(
        &args.host,
        &args.server_name,
        &args.user,
        &args.password,
        &args.database,
        args.insecure,
        client_certs,
    )
    .await?;

    info!("Connected to HelionDB at {} as '{}'", args.host, args.user);

    let mut repl = Repl::new(conn);
    repl.run().await;
    Ok(())
}

impl Default for ConnectionArgs {
    fn default() -> Self {
        ConnectionArgs {
            host: "127.0.0.1:9613".to_string(),
            user: "helion".to_string(),
            password: String::new(),
            server_name: "heliondb.local".to_string(),
            database: "default".to_string(),
            insecure: false,
            client_cert: None,
            client_key: None,
        }
    }
}
