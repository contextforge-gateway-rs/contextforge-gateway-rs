FROM rust:1.91.1 AS builder
WORKDIR /tmp/

RUN <<EOF
apt update
apt install -y git ca-certificates protobuf-compiler
git config --global http.sslVerify false
git clone https://github.com/mcp-gateway-rs/mcp-rust-sdk.git rust-sdk
EOF
WORKDIR /tmp/rust-sdk
RUN git checkout enabling_propagation_of_new_session_id_2 
WORKDIR /tmp/rust-sdk/conformance

RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git \
    cargo fetch
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
COPY --from=builder /tmp/rust-sdk/target/release/conformance-server /conformance-server
LABEL org.opencontainers.image.source=https://github.com/contextforge-gateway-rs/contextforge-gateway-rs
LABEL org.opencontainers.image.description="Mcp-conformance"
ENTRYPOINT ["/conformance-server"]
