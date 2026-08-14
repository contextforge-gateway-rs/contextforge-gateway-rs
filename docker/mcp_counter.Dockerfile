FROM rust:1.96.1 AS builder
ARG RMCP_VERSION=rmcp-v3.1.1
WORKDIR /tmp

RUN <<EOF
apt update
apt install -y git ca-certificates protobuf-compiler
git clone --branch "${RMCP_VERSION}" --depth 1 https://github.com/modelcontextprotocol/rust-sdk.git rust-sdk
EOF
WORKDIR /tmp/rust-sdk

RUN sed -i 's/127\.0\.0\.1:8000/0.0.0.0:5555/' examples/servers/src/counter_streamhttp.rs

RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo fetch
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry  \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo build --release --example servers_counter_streamhttp

FROM debian:trixie-slim
RUN <<EOF
apt update
apt upgrade -y
apt install -y python3
EOF

WORKDIR /
COPY --from=builder /tmp/rust-sdk/target/release/examples/servers_counter_streamhttp /servers_counter_streamhttp
LABEL org.opencontainers.image.source=https://github.com/contextforge-org/contextforge-data-plane
LABEL org.opencontainers.image.description="RMCP 3.1.1 counter server with MCP 2026-07-28 support"
ENTRYPOINT ["/servers_counter_streamhttp"]
