FROM rust:1.96.1 AS builder

ARG MCP_RUST_SDK_REV=baac607e52b9788ec20902e2c7143ba4f4786f4b

WORKDIR /tmp/rust-sdk
# Package versions follow the pinned Rust base image's Debian snapshot.
# hadolint ignore=DL3008
RUN <<EOF
apt-get update
apt-get install -y --no-install-recommends ca-certificates git protobuf-compiler
rm -rf /var/lib/apt/lists/*
git init
git remote add origin https://github.com/modelcontextprotocol/rust-sdk.git
git fetch --depth 1 origin "${MCP_RUST_SDK_REV}"
git checkout --detach FETCH_HEAD
# The upstream server binds loopback, which Docker cannot publish.
sed -i 's/127\.0\.0\.1/0.0.0.0/' conformance/src/bin/server.rs
EOF

WORKDIR /tmp/rust-sdk/conformance
RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo fetch
# Keep dependency fetching in its own cacheable layer.
# hadolint ignore=DL3059
RUN \
    --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo build --locked --release --bin conformance-server

FROM debian:trixie-slim

# Runtime packages follow the Debian base image's security updates.
# hadolint ignore=DL3008
RUN <<EOF
apt-get update
apt-get install -y --no-install-recommends ca-certificates python3
rm -rf /var/lib/apt/lists/*
EOF

COPY --from=builder /tmp/rust-sdk/target/release/conformance-server /conformance-server
LABEL org.opencontainers.image.source=https://github.com/contextforge-org/contextforge-data-plane
LABEL org.opencontainers.image.description="Official MCP Rust SDK conformance test server"
ENTRYPOINT ["/conformance-server"]
