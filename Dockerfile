FROM rust:1.75-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY tests/ tests/

RUN cargo build --release --bin heliondb

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/heliondb /usr/local/bin/heliondb

EXPOSE 9613/udp

USER 1000:1000

ENTRYPOINT ["heliondb"]
