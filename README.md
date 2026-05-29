# ContextForge Dataplane


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
curl --request GET \
  --url http://127.0.0.1:8001/contextforge-rs/admin/tokens/admin@example.com \
  --header 'accept: application/json' \  
  --header 'content-type: application/json'
```

4. Use the token to add a test user to Redis
```bash
curl --request POST \
  --url http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/admin@example.com \
  --header 'authorization: Bearer {{token}}' \
  --header 'content-type: application/json' \
  --data '{
  "virtualHosts": {
      "c0ffee00f001f00lf00ldeadbeefdead": {
        "backends": {
          "gateway-one": {
            "url": "http://127.0.0.1:5555/mcp"
          },
          "gateway-two": {
            "url": "http://127.0.0.1:5556/mcp"
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

### Payload Marker Demo

This demo uses [`cpex-payload-marker`](https://github.com/contextforge-gateway-rs/cpex-plugins-rs/tree/07af215bc9f00a6c3cd6d4838479518569607581/crates/cpex-payload-marker). The plugin must be included in the gateway build before the gateway starts. Redis runtime registration activates already-registered factories; it does not load new Rust code into a running process.

Build the gateway with the demo plugin factories:

```bash
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo check -p contextforge-gateway-rs --features test-plugins
```

The `test-plugins` feature includes those demo plugin crates and registers their factories through the gateway's generic CMF factory adapter.

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
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo run --release --features test-plugins --bin contextforge-gateway-rs -- \
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
CARGO_NET_GIT_FETCH_WITH_CLI=true cargo run --release --features test-plugins --bin contextforge-gateway-rs -- \
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
TOKEN=$(curl --silent --show-error --request GET \
  --url http://127.0.0.1:8001/contextforge-rs/admin/tokens/admin@example.com \
  --header 'accept: application/json' \
  --header 'content-type: application/json')
```

Create the gateway user config:

```bash
curl --silent --show-error --request POST \
  --url http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/admin@example.com \
  --header "authorization: Bearer ${TOKEN}" \
  --header 'content-type: application/json' \
  --data '{
    "virtualHosts": {
      "c0ffee00f001f00lf00ldeadbeefdead": {
        "backends": {
          "gateway-one": { "url": "http://127.0.0.1:5555/mcp" }
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

## Tracing & Metrics (Langfuse + OTel Collector + Prometheus)

Issue [#4721](https://github.com/IBM/mcp-context-forge/issues/4721) adds OTLP
**traces** and **metrics** to the Rust dataplane. A local verification stack
ships under `docker/` so the same release binary can be exercised end-to-end
without any external services.

The stack consists of three overlays composed on top of `docker-compose-local.yaml`:

| Component       | Role                                                    | UI / endpoint                                  |
| --------------- | ------------------------------------------------------- | ---------------------------------------------- |
| Langfuse        | Trace backend (OTLP/HTTP receiver, span viewer)         | http://localhost:3100 (`admin@example.com` / `admin`) |
| OTel Collector  | Receives OTLP from the gateway, fans out traces + metrics | OTLP/HTTP `:4318`, Prometheus exposition `:8889`, stdout via `docker logs` |
| Prometheus      | Scrapes the collector's `/metrics` for browsable PromQL | http://localhost:9090                          |

### 1. Bring up the verification stack

```bash
docker compose \
  -f docker/docker-compose-local.yaml \
  -f docker/docker-compose-langfuse.yaml \
  -f docker/docker-compose-otel-collector.yaml \
  up -d
```

Wait for all containers to become healthy:

```bash
docker compose \
  -f docker/docker-compose-local.yaml \
  -f docker/docker-compose-langfuse.yaml \
  -f docker/docker-compose-otel-collector.yaml \
  ps
```

### 2. Run the gateway with traces and metrics enabled

```bash
RUST_TRACE_LOG=debug \
cargo run --release --bin contextforge-gateway-rs -- \
  --address 0.0.0.0:8001 \
  --redis-port 6379 --redis-address 127.0.0.1 --redis-mode=plain-text \
  --token-verification-public-key assets/jwt.key.pub \
  --number-of-cpus 4 \
  --upstream-connection-mode=plain-text-or-tls \
  --enable-open-telemetry true \
  --enable-otel-metrics true \
  --otlp-protocol http-protobuf \
  --otlp-endpoint  http://127.0.0.1:3100/api/public/otel/v1/traces \
  --otlp-headers   "Authorization=Basic cGstbGYtY29udGV4dGZvcmdlOnNrLWxmLWNvbnRleHRmb3JnZQ==" \
  --otlp-metrics-endpoint http://127.0.0.1:4318/v1/metrics \
  --otlp-service-name contextforge-gateway-rs
```

Relevant flags (all also configurable via environment variables — see `--help`):

| Flag                          | Env var                                            | Purpose                                                       |
| ----------------------------- | -------------------------------------------------- | ------------------------------------------------------------- |
| `--enable-open-telemetry`     | `CONTEXTFORGE_GATEWAY_RS_ENABLE_OPEN_TELEMETRY`    | Turn on the OTel tracer pipeline.                             |
| `--otlp-endpoint`             | `OTEL_EXPORTER_OTLP_ENDPOINT`                      | Trace destination (Langfuse OTLP/HTTP URL here).              |
| `--otlp-headers`              | `OTEL_EXPORTER_OTLP_HEADERS`                       | Auth header for Langfuse (Basic auth, base64 of `pk:sk`).     |
| `--enable-otel-metrics`       | `CONTEXTFORGE_GATEWAY_RS_ENABLE_OTEL_METRICS`      | Turn on the OTel meter pipeline (added in #4721).             |
| `--otlp-metrics-endpoint`     | `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`              | Metrics destination (Collector OTLP/HTTP `/v1/metrics`).      |
| `--otlp-service-name`         | `OTEL_SERVICE_NAME`                                | `service.name` resource attribute on every span and metric.   |

> `RUST_TRACE_LOG=debug` is required: the `tower_http::TraceLayer` emits
> `DEBUG`-level spans, and the default filter (`info`) would drop them before
> they ever reach the OTLP exporter — no spans would land in Langfuse.

### 3. Generate traffic

```bash
for i in {1..10}; do
  curl -s -o /dev/null -w "%{http_code}\n" \
    http://127.0.0.1:8001/contextforge-rs/admin/tokens/admin@example.com
done
```

A `404` response is expected without configured users; the request is still
traced and counted as a metric sample.

### 4. Inspect the data

* **Langfuse — traces:** open http://localhost:3100, log in, project
  `contextforge`. Each curl produces one span (HTTP method, route, status,
  latency).
* **Prometheus — metrics:** open http://localhost:9090.
  * `Status → Targets` should show `otel-collector:8889` as **UP**.
  * Try these queries in the `Graph` tab:
    * `http_server_request_duration_count` — request count, broken down by
      `http_request_method`, `http_response_status_code`, and `service_name`.
    * `histogram_quantile(0.95, sum by (le) (rate(http_server_request_duration_bucket[1m])))` — p95 latency.
    * `http_server_active_requests` — gauge of in-flight requests.
    * `http_server_request_body_size_sum` / `http_server_response_body_size_sum` — payload throughput.
* **Collector stdout:** `docker logs otel-collector --tail 200` for raw OTLP
  dumps (both traces and metrics, via the `logging` exporter).

Metrics are exported by the gateway every 30 s (one `PeriodicReader` tick), so
allow ~35 s after the first request before the first data point appears in
Prometheus.

### Architecture

```
ContextForge Gateway (release binary, :8001)
        │
        │  OTLP/HTTP (protobuf)
        │
        ├──► :3100 ── Langfuse  ──► trace UI
        │
        └──► :4318 ── OTel Collector
                          │
                          ├──► stdout (logging exporter, docker logs)
                          │
                          └──► :8889 ── Prometheus ──► PromQL UI :9090
```

### Out of scope (tracked separately)

* W3C trace-context propagation across gateway hops — issue
  [#4723](https://github.com/IBM/mcp-context-forge/issues/4723).
* MCP-semantic spans (tool names, JSON-RPC method attributes) — issue
  [#4722](https://github.com/IBM/mcp-context-forge/issues/4722).

### Tear down

```bash
docker compose \
  -f docker/docker-compose-local.yaml \
  -f docker/docker-compose-langfuse.yaml \
  -f docker/docker-compose-otel-collector.yaml \
  down
```

## Performance Tests

As above and then run:
```bash
cargo run --release --bin contextforge-load-test -- --host 'http://127.0.0.1:8001' -r 40 -u 120 --run-time 120s --report-file report.html

```

[Performance reports](./reports)
