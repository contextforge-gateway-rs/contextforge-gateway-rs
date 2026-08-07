# Plugins And Policy

Plugins are a policy boundary, not just middleware. They can inspect and mutate
payloads, so the gateway keeps the supported hook surface narrow and explicit.

## Runtime Enablement

Runtime plugins are disabled by default. Enablement has two stages:

1. Compile concrete Rust plugin factories into the gateway binary with Cargo
   features.
2. Start the gateway with runtime plugins enabled and provide plugin config in
   Redis.

The runtime flag activates already-registered factories; it does not load new
Rust code into a running process. When enabled, the binary creates a CPEX
runtime registry and the runtime initializes it before serving traffic.

Plugin configuration is loaded from Redis at:

```text
ContextForgeGatewayRuntimePluginConfig
```

The runtime registry builds an initialized immutable plugin manager from that
configuration. Reloading swaps the manager instead of mutating a live one.

The bundled secrets detection plugin is experimental. Compile it into the
gateway with `contextforge-gateway-rs/plugins`, enable runtime plugins with
`--runtime-plugins-enabled true`, and configure the plugin kind
`validator/secrets-detection` in the Redis document.

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
