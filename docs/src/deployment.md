# Deployment Guide

## Prerequisites

- Linux x86_64 (other targets supported via cross-compilation)
- Minimum 256 MB RAM (512 MB+ recommended)
- ~50 MB disk for the binary; variable for data directory

## Configuration File

HelionDB can be configured via a TOML file in addition to CLI arguments. CLI arguments override values in the config file.

By default, the server looks for `heliondb.toml` in the current directory. Use `--config` or the `HELIONDB_CONFIG` environment variable to specify a different path:

```bash
heliondb --config /etc/heliondb/prod.toml
```

### Example Configuration

```toml
[server]
# QUIC listen address (default: "127.0.0.1:9613")
listen = "0.0.0.0:9613"

# Default database for connections that don't specify one (default: "default")
default_database = "default"

# Max concurrent bidirectional streams per QUIC connection (default: 100)
max_concurrent_streams = 200

# Idle timeout in seconds before closing inactive connections (default: 30)
idle_timeout_seconds = 60

[tls]
# Path to TLS certificate file (PEM). Auto-generated if not specified.
cert_path = "/etc/heliondb/cert.pem"

# Path to TLS private key file (PEM). Auto-generated if not specified.
key_path = "/etc/heliondb/key.pem"

[storage]
# Root data directory (used when no databases are explicitly configured)
data_dir = "/var/lib/heliondb"

# Default storage engine for new tables: "disk" or "memory" (default: "disk")
default_engine = "disk"

# Durability mode: "async" (fast, up to 5ms data loss) or "sync" (safe) (default: "async")
durability = "sync"

# Seconds between checkpoint snapshots (default: 60)
checkpoint_interval_seconds = 120

# Named databases. If specified, each database has its own engine, WAL, and catalog.
# If omitted, a single "default" database is created at storage.data_dir.
[[database]]
name = "default"
path = "/var/lib/heliondb/default"

[[database]]
name = "analytics"
path = "/data/analytics"
engine = "disk"
```

### Config Resolution Order

1. CLI arguments (highest priority) — e.g. `--listen`, `--data-dir`
2. Config file values — from `heliondb.toml` or `--config` path
3. Built-in defaults (lowest priority)

### Environment Variables

| Variable | Description |
|----------|-------------|
| `HELIONDB_CONFIG` | Path to TOML config file |
| `HELIONDB_USER` | Default username for helionctl |
| `HELIONDB_PASSWORD` | Default password for helionctl and admin user creation |
| `HELION_PASSWORD` | Alias for `HELIONDB_PASSWORD` |

## Docker

### Build the Image

```bash
docker build -t heliondb .
```

### Run

```bash
# Quick start
docker run -e HELION_PASSWORD=my_password -p 9613:9613/udp heliondb

# With persistent data
docker run \
  -e HELION_PASSWORD=my_password \
  -v heliondb-data:/data \
  -p 9613:9613/udp \
  heliondb
```

### Docker Compose

```bash
# Start
HELION_PASSWORD=my_password docker compose up -d

# View logs
docker compose logs -f

# Stop
docker compose down
```

## Systemd Service

Create `/etc/systemd/system/heliondb.service`:

```ini
[Unit]
Description=HelionDB SQL Database
Documentation=https://gxcloud.github.io/heliondb/
After=network.target

[Service]
Type=simple
User=heliondb
DynamicUser=yes
RuntimeDirectory=heliondb
StateDirectory=heliondb
EnvironmentFile=-/etc/heliondb/heliondb.conf
ExecStart=/usr/local/bin/heliondb --data-dir /var/lib/heliondb
Restart=always
RestartSec=5
NoNewPrivileges=yes
ProtectHome=yes
ProtectSystem=strict
PrivateTmp=yes
MemoryDenyWriteExecute=yes

[Install]
WantedBy=multi-user.target
```

Environment file (`/etc/heliondb/heliondb.conf`):

```env
HELION_PASSWORD=your_secure_password
RUST_LOG=heliondb=info
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now heliondb
sudo journalctl -u heliondb -f
```

## TLS Certificates

### Auto-Generated (Default)

On first run, HelionDB generates a self-signed certificate for `heliondb.local` and `localhost`. Suitable for development and internal networks.

### Custom PEM Certificates

```bash
./target/release/heliondb \
  --cert /etc/heliondb/cert.pem \
  --key /etc/heliondb/key.pem
```

### Let's Encrypt (Production)

```bash
# Install certbot
apt install certbot

# Obtain certificate (for a public domain)
certbot certonly --standalone -d db.example.com

# Symlink or copy to HelionDB's location
ln -s /etc/letsencrypt/live/db.example.com/fullchain.pem /etc/heliondb/cert.pem
ln -s /etc/letsencrypt/live/db.example.com/privkey.pem /etc/heliondb/key.pem

# Renewal hook — restart HelionDB after cert renewal
echo "systemctl restart heliondb" > /etc/letsencrypt/renewal-hooks/post/heliondb.sh
chmod +x /etc/letsencrypt/renewal-hooks/post/heliondb.sh
```

> **Note:** The systemd `EnvironmentFile` approach doesn't support `--cert`/`--key` flags directly. Use a wrapper script or pass them in `ExecStart`.

## Durability Configuration

| Mode | Behavior | Data Loss Window | Write Latency |
|------|----------|-----------------|---------------|
| `async` (default) | WAL flushed every 5ms | Up to 5ms of writes | ~10-100µs |
| `sync` | WAL flushed on every write | None | ~200µs-2ms (fsync) |

Choose `sync` for critical data; `async` for throughput-sensitive workloads where minor data loss is acceptable.

### Checkpoint Interval

```bash
# Default: every 60 seconds
./target/release/heliondb --checkpoint-interval 300
```

Longer intervals reduce I/O but increase WAL replay time on restart. Shorter intervals speed up recovery but increase write amplification.

## Backup and Restore

### Cold Backup (Consistent Snapshot)

1. Stop the server gracefully (SIGTERM — triggers final checkpoint + WAL flush)
2. Copy the entire data directory
3. Restart

```bash
sudo systemctl stop heliondb
cp -a /var/lib/heliondb /backup/heliondb-$(date +%Y%m%d)
sudo systemctl start heliondb
```

### Restore

```bash
sudo systemctl stop heliondb
rm -rf /var/lib/heliondb
cp -a /backup/heliondb-20260513 /var/lib/heliondb
sudo systemctl start heliondb
```

### WAL Archiving (Future)

WAL archiving is not yet implemented. For now, use cold backups.

## Monitoring

### Structured Logging

HelionDB uses the `tracing` crate. Control verbosity with `RUST_LOG`:

```bash
# Default
RUST_LOG=info

# Debug (includes SQL statements, transaction details)
RUST_LOG=heliondb=debug

# Trace (all internal operations)
RUST_LOG=heliondb=trace

# JSON output for log aggregation
RUST_LOG=info
RUST_LOG_FORMAT=json  # (future)
```

### OpenTelemetry

The `tracing` subscriber can be configured to export to OpenTelemetry-compatible backends:

- **Jaeger**: Trace SQL queries and transactions end-to-end
- **Datadog**: APM integration via the Datadog Agent's OpenTelemetry endpoint
- **Grafana Tempo**: Trace storage and visualization

### Health Checks

HelionDB runs a QUIC server — standard TCP health checks won't work. Options:

- Application-level: connect via QUIC, send a lightweight query (`SELECT 1`)
- Wait for the process to bind: check `ss -uanp 'sport = 9613'`

## Security Hardening

### Network

```bash
# Listen only on localhost
./target/release/heliondb --listen 127.0.0.1:9613

# iptables: restrict QUIC (UDP 9613) to known clients
iptables -A INPUT -p udp --dport 9613 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p udp --dport 9613 -j DROP
```

### Password Management

- Always set `HELION_PASSWORD` via environment variable (not command-line args)
- Use a secrets manager or encrypted `.env` file
- Rotate passwords periodically: `ALTER USER helion WITH PASSWORD 'new_password'`
- Never commit secrets to version control

### Filesystem

```bash
# Encrypt the data directory
# Example: LUKS on a dedicated partition or file-backed loop device

# Restrict permissions
chmod 0700 /var/lib/heliondb
chown -R heliondb:heliondb /var/lib/heliondb
```

## Resource Estimation

| Workload | Memory | Disk (data) | Disk (WAL, per hour) |
|----------|--------|-------------|----------------------|
| Development / light | 256 MB | ~10 MB | ~1 MB |
| Moderate (10K rows, 10 cols) | 512 MB | ~100 MB | ~10 MB |
| Heavy (1M rows, 20 cols) | 2-4 GB | ~1 GB | ~100 MB |

WAL size depends on write volume and checkpoint interval. The WAL is trimmed implicitly by checkpoint records.

## Performance Tuning

- **Use indexes**: Queries without matching indexes fall back to full table scans. Run `EXPLAIN SELECT ...` to check.
- **Pick the right engine**: Use `memory` for hot/cache tables, `disk` for larger or persistent tables
- **Durability**: `async` mode is 10-100x faster than `sync` for writes
- **Checkpoint interval**: Tune based on acceptable recovery time (`interval * write_rate = max WAL size`)
- **Batch writes**: Use a single transaction for multiple INSERT/UPDATE/DELETE operations
