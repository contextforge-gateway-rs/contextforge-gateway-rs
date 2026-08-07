FROM rust:1.96.1 AS builder
WORKDIR /app

RUN <<EOF
apt update
apt install -y ca-certificates protobuf-compiler
EOF

COPY . .
RUN --mount=type=cache,id=cargo,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo fetch --locked
RUN --mount=type=cache,id=cargo,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo build --release --features "contextforge-gateway-rs-lib/with_tools contextforge-gateway-rs/plugins"

FROM debian:trixie-slim
RUN <<EOF
apt update
apt upgrade -y
apt install -y python3
EOF

WORKDIR /
COPY --from=builder /app/target/release/contextforge-gateway-rs /contextforge-gateway-rs
LABEL org.opencontainers.image.source=https://github.com/contextforge-gateway-rs/contextforge-gateway-rs
LABEL org.opencontainers.image.description="contextforge-gateway-rs - open source highly experimental dataplane for Contextforge Gateway."
ENTRYPOINT ["/contextforge-gateway-rs"]
