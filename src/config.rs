use serde::{Deserialize, Serialize};

// ── Top-Level Configuration ──────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub database: Vec<DatabaseConfig>,
}

// ── Server Section ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_database_name")]
    pub default_database: String,
    #[serde(default = "default_max_streams")]
    pub max_concurrent_streams: u32,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_seconds: u64,
}

fn default_listen() -> String {
    "127.0.0.1:9613".to_string()
}
fn default_database_name() -> String {
    "default".to_string()
}
fn default_max_streams() -> u32 {
    100
}
fn default_idle_timeout() -> u64 {
    30
}

// ── TLS Section ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsConfig {
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    /// Path to CA certificate PEM file for verifying client certificates (mTLS).
    #[serde(default)]
    pub client_ca_cert_path: Option<String>,
    /// Require valid client certificate for all connections.
    #[serde(default)]
    pub client_cert_required: bool,
}

// ── Storage Section ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default = "default_engine")]
    pub default_engine: String,
    #[serde(default = "default_durability")]
    pub durability: String,
    #[serde(default = "default_checkpoint_interval")]
    pub checkpoint_interval_seconds: u64,
}

fn default_data_dir() -> String {
    "./data".to_string()
}
fn default_engine() -> String {
    "disk".to_string()
}
fn default_durability() -> String {
    "async".to_string()
}
fn default_checkpoint_interval() -> u64 {
    60
}

// ── Database Section ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub engine: Option<String>,
}

// ── Config Loading ───────────────────────────────────────────────────

impl Config {
    /// Load configuration from an optional TOML file path.
    ///
    /// If `path` is `Some`, reads that file. If `None`, tries `heliondb.toml`
    /// in the current directory. Falls back to `Config::default()`.
    pub fn load(path: Option<&str>) -> Self {
        let config_path = path.unwrap_or("heliondb.toml");
        match std::fs::read_to_string(config_path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(cfg) => {
                    tracing::info!("Loaded configuration from '{}'", config_path);
                    cfg
                }
                Err(e) => {
                    tracing::warn!("Failed to parse '{}': {}. Using defaults.", config_path, e);
                    Config::default()
                }
            },
            Err(_) => {
                if let Some(p) = path {
                    tracing::warn!("Config file '{}' not found. Using defaults.", p);
                }
                Config::default()
            }
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            default_database: default_database_name(),
            max_concurrent_streams: default_max_streams(),
            idle_timeout_seconds: default_idle_timeout(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            data_dir: default_data_dir(),
            default_engine: default_engine(),
            durability: default_durability(),
            checkpoint_interval_seconds: default_checkpoint_interval(),
        }
    }
}

// ── Display / Helpers ────────────────────────────────────────────────

impl std::fmt::Display for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Server:")?;
        writeln!(f, "  listen:                  {}", self.server.listen)?;
        writeln!(
            f,
            "  default_database:        {}",
            self.server.default_database
        )?;
        writeln!(
            f,
            "  max_concurrent_streams:  {}",
            self.server.max_concurrent_streams
        )?;
        writeln!(
            f,
            "  idle_timeout_seconds:    {}",
            self.server.idle_timeout_seconds
        )?;
        writeln!(f, "TLS:")?;
        writeln!(
            f,
            "  cert_path:               {}",
            self.tls.cert_path.as_deref().unwrap_or("(auto)")
        )?;
        writeln!(
            f,
            "  key_path:                {}",
            self.tls.key_path.as_deref().unwrap_or("(auto)")
        )?;
        writeln!(
            f,
            "  client_ca_cert_path:     {}",
            self.tls.client_ca_cert_path.as_deref().unwrap_or("(none)")
        )?;
        writeln!(
            f,
            "  client_cert_required:    {}",
            self.tls.client_cert_required
        )?;
        writeln!(f, "Storage:")?;
        writeln!(f, "  data_dir:                {}", self.storage.data_dir)?;
        writeln!(
            f,
            "  default_engine:          {}",
            self.storage.default_engine
        )?;
        writeln!(f, "  durability:              {}", self.storage.durability)?;
        writeln!(
            f,
            "  checkpoint_interval:     {}s",
            self.storage.checkpoint_interval_seconds
        )?;
        if self.database.is_empty() {
            writeln!(f, "Databases: (single — uses storage.data_dir)")?;
        } else {
            writeln!(f, "Databases:")?;
            for db in &self.database {
                writeln!(
                    f,
                    "  - {} -> {} (engine: {})",
                    db.name,
                    db.path,
                    db.engine.as_deref().unwrap_or(&self.storage.default_engine)
                )?;
            }
        }
        Ok(())
    }
}
