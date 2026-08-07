FROM rust:1.96.1 AS builder
WORKDIR /tmp/

RUN <<EOF
apt update
apt install -y git ca-certificates protobuf-compiler
git config --global http.sslVerify false
# This SDK fork remains in its independently hosted legacy repository.
git clone https://github.com/contextforge-gateway-rs/mcp-rust-sdk.git rust-sdk
EOF
WORKDIR /tmp/rust-sdk
RUN <<EOF
git checkout enabling_propagation_of_new_session_id_2
# The pinned SDK example binds loopback, which Docker cannot publish.
sed -i 's/127\.0\.0\.1:8000/0.0.0.0:5555/' examples/servers/src/counter_streamhttp.rs
EOF
WORKDIR /tmp/rust-sdk/examples/servers

RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo fetch
RUN --mount=type=cache,target=/app/target \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
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
LABEL org.opencontainers.image.description="Mcp-conformance"
ENTRYPOINT ["/servers_counter_streamhttp"]
