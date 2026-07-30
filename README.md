# ContextForge Dataplane

Architecture, configuration, and operations documentation lives in
[The ContextForge Gateway Book](docs/book/src/SUMMARY.md) under `docs/book`.
Build it locally with `mdbook serve docs/book`; see
[docs/book/README.md](docs/book/README.md) for details.

## Running
1. Start Redis and gateways
```
docker compose -f docker/docker-compose-local.yaml up -d
```

Check Redis and backend status:
```bash
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

2. Run gateway
```bash 
    cargo run --bin contextforge-gateway-rs -- --address 0.0.0.0:8001 --redis-port 6379 --redis-address 127.0.0.1 --token-verification-public-key assets/jwt.key.pub  --token-verification-private-key assets/jwt.key --number-of-cpus 16 --redis-mode=plain-text --upstream-connection-mode=plain-text-or-tls
```

This should spin up Redis instance and two mcp-gateways: a simple counter and a conformance test server from mcp-rust-sdk

3. Get a test JWT token
```bash
USER_ID=11111111-1111-1111-1111-111111111111
USER_EMAIL=admin@example.com

curl --request GET \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/tokens/${USER_ID}?email=${USER_EMAIL}" \
  --header 'accept: application/json' \  
  --header 'content-type: application/json'
```

4. Use the token to add a test user to Redis
```bash
curl --request POST \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/${USER_ID}" \
  --header 'authorization: Bearer {{token}}' \
  --header 'content-type: application/json' \
  --data '{
  "virtual_hosts": {
    "c0ffee00f001f00lf00ldeadbeefdead": {
      "backends": {
        "gateway-one": {
          "name": "gateway-one",
          "url": "http://127.0.0.1:5555/mcp",
          "transport": "STREAMABLEHTTP",
          "passthrough_headers": [],
          "allowed_tool_names": [],
          "allowed_resource_names": [],
          "allowed_prompt_names": []
        },
        "gateway-two": {
          "name": "gateway-two",
          "url": "http://127.0.0.1:5556/mcp",
          "transport": "STREAMABLEHTTP",
          "passthrough_headers": [],
          "allowed_tool_names": [],
          "allowed_resource_names": [],
          "allowed_prompt_names": []
        }
      }
    }
  }
}'
```

6. Spin up MCP Inspector to test the calls


## Runtime CPEX Plugins

Runtime CPEX plugins are disabled by default. Enable hook execution when starting the gateway:

```bash
cargo run --release --bin contextforge-gateway-rs -- \
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

Plugin configuration is stored in Redis at key `ContextForgeGatewayRuntimePluginConfig`. The value can be JSON or MessagePack with `version: 1` and `cpex` containing the CPEX config. When runtime plugins are enabled with Redis config, this key must exist before startup. The gateway reloads that key while running, builds a new initialized CPEX runtime, and swaps the runtime registry to the new immutable `PluginManager`. The existing `PluginManager` is not mutated after initialization.

This integration currently passes only tool payloads. CPEX configs that enable route-based plugin selection, plugin directories, global policies/defaults, non-tool hooks, or plugin conditions are rejected in this PR. Redis write access to this key is a control-plane trust boundary because it controls which registered hooks run.

### Secrets Detection Spike

This spike feature registers the Rust-native secrets detection plugin from the sibling `cpex-plugins` checkout. The plugin must be compiled into the gateway before Redis config can activate it.

Build the gateway with the secrets detection factory:

```bash
cargo check -p contextforge-gateway-rs --features secrets-detection-plugin
```

Register the runtime plugin config in Redis:

```bash
docker compose -f docker/docker-compose-local.yaml exec -T redis redis-cli SET ContextForgeGatewayRuntimePluginConfig '{
  "version": 1,
  "cpex": {
    "plugins": [
      {
        "name": "secrets-detection",
        "kind": "validator/secrets-detection",
        "hooks": ["cmf.tool_pre_invoke", "cmf.tool_post_invoke"],
        "config": {
          "redact": true,
          "redaction_text": "[redacted]",
          "block_on_detection": false
        }
      }
    ]
  }
}'
```

Start the gateway with runtime plugins enabled:

```bash
cargo run --release --features secrets-detection-plugin --bin contextforge-gateway-rs -- \
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

Only `cmf.tool_pre_invoke` and `cmf.tool_post_invoke` are wired in this data-plane spike. Set `block_on_detection` to `true` to verify deny behavior before the backend receives the tool call.

### Payload Marker Demo

This demo uses the `test-plugins` feature, which includes the demo plugin crates from `cpex-plugins-rs`. The plugin must be included in the gateway build before the gateway starts. Redis runtime registration activates already-registered factories; it does not load new Rust code into a running process.

Build the gateway with the demo plugin factories:

```bash
cargo check -p contextforge-gateway-rs --features test-plugins
```

The `test-plugins` feature registers those demo plugin factories through the gateway's generic CMF factory adapter.

Start Redis and the sample MCP backends:

```bash
GATEWAY_CPU_LIMIT=1 \
GATEWAY_CPU_RESERVATION=0.5 \
GATEWAY_MEM_LIMIT=1G \
GATEWAY_MEM_RESERVATION=512M \
docker compose -f docker/docker-compose-local.yaml up -d
```

Check Redis and backend status:
```bash
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

Register the payload marker config in Redis before starting the enabled gateway:

```bash
docker compose -f docker/docker-compose-local.yaml exec -T redis redis-cli SET ContextForgeGatewayRuntimePluginConfig '{
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

Run only one gateway process on port `8001` at a time. Stop the current gateway with `Ctrl-C` before switching between disabled and enabled plugin runs.

Start the gateway with runtime plugins disabled for a baseline run:

```bash
cargo run --release --features test-plugins --bin contextforge-gateway-rs -- \
  --address 0.0.0.0:8001 \
  --redis-port 6379 \
  --redis-address 127.0.0.1 \
  --token-verification-public-key assets/jwt.key.pub \
  --token-verification-private-key assets/jwt.key \
  --number-of-cpus 16 \
  --redis-mode=plain-text \
  --upstream-connection-mode=plain-text-or-tls \
  --runtime-plugins-enabled false
```

Start the gateway with runtime plugins enabled for the marker run:

```bash
cargo run --release --features test-plugins --bin contextforge-gateway-rs -- \
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

Get a token:

```bash
USER_ID=11111111-1111-1111-1111-111111111111
USER_EMAIL=admin@example.com

TOKEN=$(curl --silent --show-error --request GET \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/tokens/${USER_ID}?email=${USER_EMAIL}" \
  --header 'accept: application/json' \
  --header 'content-type: application/json')
```

Create the gateway user config:

```bash
curl --silent --show-error --request POST \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/${USER_ID}" \
  --header "authorization: Bearer ${TOKEN}" \
  --header 'content-type: application/json' \
  --data '{
    "virtual_hosts": {
      "c0ffee00f001f00lf00ldeadbeefdead": {
        "backends": {
          "gateway-one": {
            "name": "gateway-one",
            "url": "http://127.0.0.1:5555/mcp",
            "transport": "STREAMABLEHTTP",
            "passthrough_headers": [],
            "allowed_tool_names": [],
            "allowed_resource_names": [],
            "allowed_prompt_names": []
          }
        }
      }
    }
  }'
```

Open an MCP session:

```bash
INIT_HEADERS=$(mktemp)
```

```bash
curl --silent --show-error \
  --dump-header "${INIT_HEADERS}" \
  --url http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00lf00ldeadbeefdead/mcp \
  --header "authorization: Bearer ${TOKEN}" \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --data '{
    "jsonrpc": "2.0",
    "id": 0,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-11-25",
      "capabilities": {},
      "clientInfo": { "name": "curl", "version": "0.1.0" }
    }
  }'
```

```bash
SESSION_ID=$(awk 'tolower($1) == "mcp-session-id:" { gsub("\r", "", $2); print $2 }' "${INIT_HEADERS}")
```

```bash
curl --silent --show-error \
  --url http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00lf00ldeadbeefdead/mcp \
  --header "authorization: Bearer ${TOKEN}" \
  --header "mcp-session-id: ${SESSION_ID}" \
  --header 'mcp-protocol-version: 2025-11-25' \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized"}'
```

Send a tool request:

```bash
curl --silent --show-error \
  --url http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00lf00ldeadbeefdead/mcp \
  --header "authorization: Bearer ${TOKEN}" \
  --header "mcp-session-id: ${SESSION_ID}" \
  --header 'mcp-protocol-version: 2025-11-25' \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --data '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
      "name": "gateway-one-say_hello",
      "arguments": {}
    }
  }'
```

With `--runtime-plugins-enabled false`, the response content should include only the backend tool result:

```text
hello
```

With `--runtime-plugins-enabled true`, the response content should include the backend tool result plus an additional text part:

```text
[cpex:payload-marker]
```

## Tracing & Metrics

The gateway exports OTLP traces and metrics
(issue [#4721](https://github.com/IBM/mcp-context-forge/issues/4721)), and a
local verification stack (Langfuse + OTel Collector + Prometheus) ships as
compose overlays under `docker/`. The full walkthrough — flags, stack setup,
starter PromQL queries, and a debugging checklist — lives in the book:
[Telemetry And Diagnostics](docs/book/src/telemetry-and-diagnostics.md).

## Performance Tests

As above and then run:
```bash
cargo run --release --bin contextforge-load-test -- --host 'http://127.0.0.1:8001' -r 40 -u 120 --run-time 120s --report-file report.html

```

[Performance reports](./reports)
