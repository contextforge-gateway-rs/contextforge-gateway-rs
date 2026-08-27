# ContextForge Data Plane

The Rust data plane for
[ContextForge](https://github.com/IBM/mcp-context-forge). It accepts modern MCP
traffic, loads control-plane-published configuration from Redis, and routes
authorized requests to configured MCP backends.

Architecture, configuration, operations, and development context lives
in the wiki under [`_context/wiki/`](_context/wiki/index.md).

## Quick Start

Build the production image and start the supported control-plane + data-plane
test stack:

```bash
make docker-prod
make compose-up
```

The stack uses the current `fast_time_server` backend and exercises config
publication through the external ContextForge control plane. See
[getting-started.md](_context/wiki/getting-started.md) for the complete smoke
test, then stop it with:

```bash
make compose-down
```

## Run the Binary from Cargo

For a lightweight host-development setup, start Redis and the MCP Rust SDK
counter and conformance fixtures:

```bash
docker compose -f docker/docker-compose-local.yaml up -d
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

Then follow [getting-started.md](_context/wiki/getting-started.md) for the local cargo dev workflow.

## Runtime CPEX Plugins

Runtime CPEX plugins are disabled by default. When enabled, the data plane loads
validated plugin configuration from Redis and supports the narrow hook surface
documented in [config.md](_context/wiki/config.md).

The optional demo plugin crates still come from their independently hosted
`cpex-plugins-rs` repository; they are unrelated to the retired MCP SDK fork.

### Experimental Secrets Detection Plugin

The bundled secrets detection CPEX plugin is experimental. It is compiled into
the data plane with `contextforge-data-plane/plugins`; Redis config only
activates plugin factories that are already present in the binary.

Activation requires all three pieces:

- Compile-time feature: `contextforge-data-plane/plugins`
- Runtime flag: `--runtime-plugins-enabled true`
- Redis config key: `ContextForgeGatewayRuntimePluginConfig`

The plugin kind is `validator/secrets-detection`. The data plane currently
wires only `cmf.tool_pre_invoke` and `cmf.tool_post_invoke`.

Example run command:

```bash
cargo run --release \
  --features contextforge-data-plane/plugins \
  -- \
  --address 0.0.0.0:8001 \
  --redis-port 6379 \
  --redis-address 127.0.0.1 \
  --token-verification-public-key assets/jwt.key.pub \
  --token-verification-private-key assets/jwt.key \
  --number-of-cpus 16 \
  --redis-mode=plain-text \
  --upstream-connection-mode=plain-text-or-tls \
  --runtime-plugins-enabled true
```


```bash
cargo run --features contextforge-data-plane-lib/with_tools \
-- \
--address 0.0.0.0:8080 \
--redis-address 127.0.0.1 \
--redis-port 6379 \
--redis-mode plain-text \
--token-verification-private-key ./assets/jwt.key \
--token-verification-public-key ./assets/jwt.key.pub \
--upstream-connection-mode plain-text-or-tls \
--tls-address 0.0.0.0:8443 \
--server-private-key ./assets/tls_key.pem \
--server-certificate ./assets/tls_certificate.pem
--runtime-plugins-enabled true
```

## Tracing and Metrics

The data plane exports OTLP traces and metrics. Local Langfuse, OTel Collector, and Prometheus overlays are documented in [config.md](_context/wiki/config.md) under "Local Telemetry Verification Stack".

## Performance Tests

Performance testing uses the control-plane Locust suite through
[`cf-integration`](https://github.com/contextforge-org/contextforge-dev-tools).
See the [performance guide](_context/wiki/performance.md) for load and baseline
runs.
