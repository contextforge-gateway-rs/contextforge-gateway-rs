FROM rust:1.91.1 AS builder
WORKDIR /tmp/

RUN <<EOF
apt update
apt install -y git ca-certificates protobuf-compiler
git config --global http.sslVerify false
git clone https://github.com/contextforge-gateway-rs/contextforge-gateway-rs
EOF
WORKDIR /tmp/contextforge-gateway-rs
COPY .cargo/config.toml ./.cargo/config.toml
RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo fetch --locked
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry  \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo build  --release

FROM debian:trixie-slim
RUN <<EOF
apt update
apt upgrade -y
apt install -y python3
EOF

WORKDIR /
COPY --from=builder /tmp/contextforge-gateway-rs/target/release/contextforge-gateway-rs /contextforge-gateway-rs
LABEL org.opencontainers.image.source=https://github.com/contextforge-gateway-rs/contextforge-gateway-rs
LABEL org.opencontainers.image.description="Mcp-gateway-rs - open source highly experimental dataplane for Contextforge Gateway."
ENTRYPOINT ["/contextforge-gateway-rs"]
