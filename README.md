# ContextForge Data Plane

The Rust data plane for
[ContextForge](https://github.com/IBM/mcp-context-forge). It accepts modern MCP
traffic, loads control-plane-published configuration from Redis, and routes
authorized requests to configured MCP backends.

Architecture, configuration, operations, and development documentation lives
in [The ContextForge Data Plane Book](docs/book/src/SUMMARY.md). Build it locally
with `mdbook serve docs/book`; see [docs/book/README.md](docs/book/README.md).

## Quick Start

Build the production image and start the supported control-plane + data-plane
test stack:

```bash
make docker-prod
make testing-up
```

The stack uses the current `fast_time_server` backend and exercises config
publication through the external ContextForge control plane. Follow
[Local Docker Stack](docs/book/src/local-docker-stack.md) for the complete smoke
test, then stop it with:

```bash
make testing-down
```

## Run the Binary from Cargo

For a lightweight host-development setup, start Redis and the MCP Rust SDK
counter and conformance fixtures:

```bash
docker compose -f docker/docker-compose-local.yaml up -d
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

Then follow [Run the Gateway Locally](docs/book/src/running-the-gateway.md).

## Runtime CPEX Plugins

Runtime CPEX plugins are disabled by default. When enabled, the data plane loads
validated plugin configuration from Redis and supports the narrow hook surface
documented in [Plugins And Policy](docs/book/src/plugins-and-policy.md).

The optional demo plugin crates still come from their independently hosted
`cpex-plugins-rs` repository; they are unrelated to the retired MCP SDK fork.

## Tracing and Metrics

The data plane exports OTLP traces and metrics. The local Langfuse,
OpenTelemetry Collector, and Prometheus overlays are documented in
[Telemetry And Diagnostics](docs/book/src/telemetry-and-diagnostics.md).

## Performance Tests

With a configured data plane running on port `8001`:

```bash
cargo run --release --bin contextforge-load-test -- \
  --host http://127.0.0.1:8001 \
  -r 40 -u 120 --run-time 120s --report-file report.html
```

Existing benchmark results are under [reports](reports).
