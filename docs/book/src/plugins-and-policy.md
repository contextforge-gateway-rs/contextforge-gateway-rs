# Plugins And Policy

Plugins are a policy boundary, not just middleware. They can inspect and mutate
payloads, so the gateway keeps the supported hook surface narrow and explicit.

## Runtime Enablement

Runtime plugins are disabled by default. Enablement has two stages:

1. Compile concrete Rust plugin factories into the data-plane binary with Cargo
   features.
2. Start the data plane with runtime plugins enabled and provide plugin config
   in Redis.

The runtime flag activates already-registered factories; it does not load new
Rust code into a running process. When enabled, the binary creates a CPEX
runtime registry and the runtime initializes it before serving traffic.

Plugin configuration is loaded from Redis at:

```text
ContextForgeGatewayRuntimePluginConfig
```

The runtime registry builds an initialized immutable plugin manager from that
configuration. Reloading swaps the manager instead of mutating a live one.

The bundled secrets detection plugin is experimental. Compile it into the data
plane with `contextforge-data-plane/plugins`, enable runtime plugins with
`--runtime-plugins-enabled true`, and configure the plugin kind
`validator/secrets-detection` in the Redis document.

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
cmf.prompt_pre_fetch
cmf.prompt_post_fetch
```

The gateway rejects route-based plugin selection, plugin directories, global
policies/defaults, resource and LLM hooks, and plugin conditions. Those features
need clear behavior for streaming, failures, timeouts, backpressure, context
propagation, and observability before they belong on the hot path.

Configuration validation and factory registration must agree on this list. A
hook accepted by validation but not registered by `CmfPluginFactory` leaves the
plugin loaded and silently inert.

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

## Prompt Fetch Behavior

For `get_prompt`, the pre hook runs after backend routing, so the plugin sees
the backend-local prompt name and the owning backend separately rather than the
gateway-prefixed identifier. It can leave the arguments unchanged, replace them,
or deny the fetch before the backend renders anything.

The post hook receives the rendered prompt as one CMF message per rendered MCP
message, each carrying its role and its content block: text, image, audio,
embedded resource, or resource link. A plugin can inspect or rewrite any of
them, which is what lets a policy act on a file interpolated into a prompt
rather than only on its surrounding text.

Writing plugin edits back follows three rules:

- A message the plugin left unchanged is returned exactly as the backend sent
  it, so annotations, `_meta`, and binary resource blobs survive untouched.
- A message the plugin changed is rebuilt from CMF. CMF does not model MCP
  annotations or `_meta`, so an edited message loses them.
- Edits that cannot be applied faithfully fail the call rather than falling back
  to the backend's original. A changed message count, more than one prompt
  result in the payload, a role MCP prompts cannot express, or a resource whose
  text the plugin removed all return an error. Silently restoring the backend's
  content would undo a redaction.

MCP prompt results carry no error flag, so a plugin setting `is_error` on the
CMF prompt result is rejecting the prompt rather than describing it. The gateway
turns that into an MCP error carrying the plugin's `error_message`; the rendered
content never reaches the client. This differs from tools, where `is_error` is a
field on `CallToolResult` and is forwarded as a successful response.

Binary resource blobs reach plugins by URI and MIME type but not by content:
CMF stores decoded bytes while MCP sends base64. A plugin can deny such a
message; editing one fails the write-back.

## Boundary Rules

Plugin execution must not poison shared gateway state. A plugin denial becomes
an MCP error. Soft plugin errors are logged. Unsupported plugin configuration
fails validation before the runtime is accepted.

Future hook expansion should define behavior for streaming/SSE, cancellation,
timeouts, backpressure, and telemetry before adding new hook points.
