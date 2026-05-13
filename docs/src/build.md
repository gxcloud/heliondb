# Build Guide

## Prerequisites

- **Rust 1.75+** — Install via [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  rustup default stable
  ```
- **System packages** — Required for the `ring` cryptography crate (used by `quinn`/`rustls`):
  - Debian/Ubuntu: `apt install pkg-config libssl-dev`
  - Fedora: `dnf install pkg-config openssl-devel`
  - macOS: already satisfied via Xcode Command Line Tools

## Build Profiles

```bash
# Debug build (fast iteration, unoptimized)
cargo build

# Release build (optimized, recommended for deployment)
cargo build --release

# Build only the library (no binary)
cargo build -p heliondb --lib
```

The release binary is at `./target/release/heliondb`.

## Build the Docs (mdBook)

```bash
# Install mdBook
cargo install mdbook --locked

# Build the docs site
mdbook build docs

# Serve locally with live reload (http://localhost:3000)
mdbook serve docs --open
```

The built site goes to `docs/book/`. The CI publishes this to GitHub Pages.

## Running Tests

```bash
# All tests (unit + integration)
cargo test

# Integration tests only
cargo test --test integration

# Doc tests (examples in library documentation)
cargo test --doc

# A specific test
cargo test test_pk_auto_index_enforces_uniqueness

# Run tests with full output (no capture)
cargo test -- --nocapture
```

### Test Structure

- **Unit tests** — Co-located with source code in `#[cfg(test)]` modules (e.g., `src/executor/eval.rs` has 30+ unit tests for expression evaluation)
- **Integration tests** — `tests/integration.rs` (38 end-to-end tests covering the full pipeline, WAL recovery, permissions, MVCC, and indexes)
- **Doc tests** — Embedded in documentation comments in `src/lib.rs`

### Test Patterns

Integration tests use a shared `setup()` helper that creates a `DatabaseEngine` in a temporary directory. Two convenience functions simplify test writing:

```rust
// Execute SQL as a no-permission-check user
let result = exec(&engine, "SELECT * FROM users").await;

// Execute SQL as a specific user (permission checks enabled)
let result = exec_as(&engine, "SELECT * FROM users", "alice").await;
```

## Code Quality

```bash
# Lint (deny warnings — matches CI)
cargo clippy --all-targets -- -D warnings

# Format check
cargo fmt --all --check

# Auto-format
cargo fmt

# Security audit (install: cargo install cargo-audit)
cargo audit
```

The CI pipeline enforces all three — clippy warnings are errors, formatting must be correct, and `cargo check` must pass for all targets.

## CI Pipeline

```text
┌─────────┐   ┌──────────┐   ┌───────────┐   ┌────────────┐
│  Check  │ → │   Test   │ → │   Build   │ → │  Coverage  │
│ clippy  │   │  cargo   │   │  release  │   │  tarpaulin │
│  fmt    │   │  test    │   │  binary   │   │  Codecov   │
│ check   │   │  doctest │   │ artifact  │   │            │
└─────────┘   └──────────┘   └───────────┘   └────────────┘
                                              │
                                              ▼
                                        ┌────────────┐
                                        │    Docs    │
                                        │  mdBook →  │
                                        │  Pages     │
                                        └────────────┘
```

Four CI jobs run in parallel after check passes:

| Job | What it does |
|-----|-------------|
| **Check** | `cargo check`, `cargo clippy -- -D warnings`, `cargo fmt --check` |
| **Test** | `cargo test --all-targets`, `cargo test --doc` |
| **Build** | `cargo build --release`, uploads binary as artifact (7-day retention) |
| **Coverage** | `cargo tarpaulin --out Xml --engine Llvm`, uploads to Codecov |
| **Docs** | Builds mdBook, deploys to GitHub Pages (on push to `main` only) |

## Coverage

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Xml --engine Llvm
```

Coverage reports are uploaded to Codecov. The CI job uses `fail_ci_if_error: false` so coverage failures don't block the pipeline.

## Profiling

For performance work:

```bash
# Build with debug symbols in release
cargo build --release

# Basic profiling with perf (Linux)
perf record --call-graph dwarf ./target/release/heliondb
perf report

# Flamegraph
cargo install flamegraph
cargo flamegraph --bin heliondb -- --data-dir /tmp/bench
```

## Cross-Compilation

HelionDB is standard Rust + minimal system dependencies, so cross-compilation works well:

```bash
# Add target (example: aarch64-unknown-linux-gnu)
rustup target add aarch64-unknown-linux-gnu

# Install cross-linker (example for ARM on Debian)
apt install gcc-aarch64-linux-gnu

# Build
cargo build --release --target aarch64-unknown-linux-gnu
```

Set `CARGO_TARGET_<ARCH>_LINKER` if your cross-linker has a non-standard name.
