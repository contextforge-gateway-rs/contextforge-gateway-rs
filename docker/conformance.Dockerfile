# syntax=docker/dockerfile:1

FROM rust:1.96.1 AS builder
WORKDIR /app

RUN <<EOF
apt update
apt install -y ca-certificates protobuf-compiler
EOF

COPY . .
RUN --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=contextforge-conformance-target-rust-1.96.1,target=/app/target,sharing=locked \
    cargo build --locked --package contextforge-data-plane \
        --bin contextforge-data-plane \
        --features "contextforge-data-plane-lib/with_tools contextforge-data-plane/plugins" && \
    mkdir -p /out && \
    cp target/debug/contextforge-data-plane /out/contextforge-data-plane && \
    strip /out/contextforge-data-plane

FROM debian:trixie-slim
WORKDIR /

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /out/contextforge-data-plane /contextforge-data-plane
LABEL org.opencontainers.image.source=https://github.com/contextforge-org/contextforge-data-plane
LABEL org.opencontainers.image.description="contextforge-data-plane - open source experimental data plane for ContextForge."
ENTRYPOINT ["/contextforge-data-plane"]
