FROM rust:1.96.1 AS builder
WORKDIR /app

RUN <<EOF
apt update
apt install -y ca-certificates protobuf-compiler
EOF

COPY . .
RUN --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    cargo fetch --locked
RUN --mount=type=cache,id=cargo,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=contextforge-data-plane-target,target=/app/target,sharing=locked \
    cargo build --release --features "contextforge-data-plane-lib/with_tools contextforge-data-plane/plugins" \
    && mkdir -p /out \
    && cp /app/target/release/contextforge-data-plane /out/contextforge-data-plane

FROM debian:trixie-slim AS runtime
RUN <<EOF
apt update
apt upgrade -y
apt install -y python3
EOF

WORKDIR /
LABEL org.opencontainers.image.source=https://github.com/contextforge-org/contextforge-data-plane
LABEL org.opencontainers.image.description="contextforge-data-plane - open source experimental data plane for ContextForge."
ENTRYPOINT ["/contextforge-data-plane"]

FROM runtime AS conformance-prebuilt
COPY --from=prebuilt /contextforge-data-plane /contextforge-data-plane

FROM runtime
COPY --from=builder /out/contextforge-data-plane /contextforge-data-plane
