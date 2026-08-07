# Plugins And Policy

Plugins are a policy boundary, not just middleware. They can inspect and mutate
payloads, so the gateway keeps the supported hook surface narrow and explicit.

## Runtime Enablement

Runtime plugins are disabled by default. When enabled, the binary creates a
CPEX runtime registry and the runtime initializes it before serving traffic.

Plugin configuration is loaded from Redis at:

```text
ContextForgeGatewayRuntimePluginConfig
```

The runtime registry builds an initialized immutable plugin manager from that
configuration. Reloading swaps the manager instead of mutating a live one.

## Built-In Demo Factories

The optional `test-plugins` feature compiles three demo factories from the
independently hosted `cpex-plugins-rs` repository. Redis configuration activates
factories already present in the binary; it never loads new Rust code into a
running process.

Start the lightweight dependencies:

```bash
docker compose -f docker/docker-compose-local.yaml up -d redis gateway-one gateway-two
```

Register the payload-marker configuration before starting the data plane:

```bash
docker compose -f docker/docker-compose-local.yaml exec -T redis \
  redis-cli SET ContextForgeGatewayRuntimePluginConfig '{
    "version": 1,
    "cpex": {
      "plugins": [
        {
          "name": "payload-marker",
          "kind": "contextforge/payload-marker",
          "hooks": ["cmf.tool_post_invoke"]
        }
      ]
    }
  }'
```

Build and run with the demo factories and runtime execution enabled:

```bash
cargo run -p contextforge-data-plane \
  --features 'contextforge-data-plane-lib/with_tools,test-plugins' \
  --bin contextforge-data-plane -- \
  --address 127.0.0.1:8001 \
  --redis-address 127.0.0.1 \
  --redis-port 6379 \
  --redis-mode plain-text \
  --token-verification-public-key assets/jwt.key.pub \
  --token-verification-private-key assets/jwt.key \
  --upstream-connection-mode plain-text-or-tls \
  --runtime-plugins-enabled true
```

Startup should log successful CPEX initialization. The payload marker appends
`[cpex:payload-marker]` to successful tool results. The supported hook path is
also covered by:

```bash
cargo nextest run --locked -p contextforge-data-plane-lib --test gateway_plugins
```

## Supported Hooks

The supported surface is deliberately narrow:

```text
cmf.tool_pre_invoke
cmf.tool_post_invoke
```

The gateway rejects route-based plugin selection, plugin directories, global
policies/defaults, non-tool hooks, and plugin conditions. Those features need
clear behavior for streaming, failures, timeouts, backpressure, context
propagation, and observability before they belong on the hot path.

## Tool Call Behavior

For `call_tool`, the pre hook runs after backend routing has selected the
backend and stripped the public prefix. The hook sees the backend name, routed
tool name, and arguments. It can:

- leave arguments unchanged
- replace arguments
- deny the call

After the upstream backend returns, the post hook can:

- leave the result unchanged
- rewrite the result payload
- deny the response

Hook state is carried across the upstream call so pre and post hooks can share
CPEX context for the same logical tool call.

## Boundary Rules

Plugin execution must not poison shared gateway state. A plugin denial becomes
an MCP error. Soft plugin errors are logged. Unsupported plugin configuration
fails validation before the runtime is accepted.

Future hook expansion should define behavior for streaming/SSE, cancellation,
timeouts, backpressure, and telemetry before adding new hook points.
