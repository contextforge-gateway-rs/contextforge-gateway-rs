# Telemetry And Diagnostics

> 📈 **Observability lens:** the gateway emits three signals — logs, request
> traces, and HTTP metrics. This page explains how each is produced, how to
> verify them locally, and where to look when a request misbehaves.

## Signal Overview

| Signal | Produced by | Destination |
| --- | --- | --- |
| Logs | `tracing` events from layers, validators, and MCP handlers. | Console and a rotating file in the working directory. |
| Request traces | `tower_http::TraceLayer` spans around every HTTP request. | OTLP trace export when `--enable-open-telemetry` is set. |
| HTTP metrics | `axum-otel-metrics` (`HttpMetricsLayer`) request instruments. | OTLP metrics export when `--enable-otel-metrics` is set. |

All export settings are process config; see the telemetry section of the
[Configuration Reference](gateway-options.md#telemetry-options).

## Logging

Console and file logging are always on. The file appender writes
`contextforge-gateway-rs.log` (configurable with `--log-name`) in the current
working directory and rotates hourly by default (`--log-rotation`).

Three environment filters control verbosity independently:

| Env var | Default | Controls |
| --- | --- | --- |
| `RUST_LOG` | `debug` | Console events. |
| `RUST_FILE_LOG` | `debug` | File events. |
| `RUST_TRACE_LOG` | `info` | Which spans reach the OTLP trace exporter. |

Dataplane log messages use a stable `method_name - event text field = value`
shape, so boundary-specific prefixes are grep-friendly: `claims_layer`,
`user_config_store_layer`, `virtual_host_config_layer`,
`AuthorizedCallValidator::validate`, `initialize:`, `call_tool`, and so on.

## Traces

`TraceLayer` emits its request spans at `DEBUG` level.

> ⚠️ **`RUST_TRACE_LOG=debug` is required for trace export.** The default span
> filter (`info`) drops the HTTP spans before they reach the OTLP exporter, so
> nothing arrives at the trace backend.

Enable export with `--enable-open-telemetry true` plus the OTLP protocol,
endpoint, and header flags. Each handled HTTP request produces one span with
method, route, status, and latency.

## Metrics

`--enable-otel-metrics true` turns on the HTTP server instruments: request
duration histogram, active request gauge, and request/response body size
histograms, all labeled with method, status code, and `service.name`.

Metrics are pushed by a `PeriodicReader` every **30 seconds**, so allow about
35 seconds after the first request before the first data point appears
downstream.

## Local Verification Stack

A complete local pipeline ships under `docker/` as overlays on top of the
[local run](running-the-gateway.md) stack:

| Component | Role | Endpoint |
| --- | --- | --- |
| Langfuse | Trace backend and span viewer. | UI at `http://localhost:3100`, login `admin@example.com` / `changeme`, project `ContextForge Gateway (Rust)`. |
| OTel Collector | Receives OTLP from the gateway, fans traces and metrics out. | OTLP/HTTP on `:4318`, Prometheus exposition on `:8889`, raw dumps via `docker logs otel-collector`. |
| Prometheus | Scrapes the collector for browsable PromQL. | UI at `http://localhost:9090`. |

### 1. Start the stack

```bash
docker compose \
  -f docker/docker-compose-local.yaml \
  -f docker/docker-compose-langfuse.yaml \
  -f docker/docker-compose-otel-collector.yaml \
  up -d
```

Re-run with `ps` instead of `up -d` and wait until every container is healthy.

### 2. Run the gateway with export enabled

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

The `Authorization` header is Basic auth for the seeded Langfuse project keys
(`pk-lf-contextforge:sk-lf-contextforge`, base64-encoded).

### 3. Generate traffic

```bash
for i in {1..10}; do
  curl -s -o /dev/null -w "%{http_code}\n" \
    http://127.0.0.1:8001/contextforge-rs/admin/tokens/admin@example.com
done
```

Any response counts: each request is traced and recorded as a metric sample.

### 4. Inspect the data

- **Langfuse:** open `http://localhost:3100` and check the project — one span
  per request with method, route, status, and latency.
- **Prometheus:** open `http://localhost:9090`; `Status → Targets` should show
  `otel-collector:8889` as **UP**, then try the starter queries below.
- **Collector stdout:** `docker logs otel-collector --tail 200` shows raw OTLP
  trace and metric dumps.

The data path is:

```text
gateway :8001
  -> OTLP/HTTP traces  -> Langfuse :3100 (span UI)
  -> OTLP/HTTP metrics -> OTel Collector :4318
                            -> stdout dump (docker logs)
                            -> Prometheus exposition :8889 -> Prometheus :9090
```

Tear the stack down with the same three `-f` files and `down`.

## Prometheus Starter Queries

| Question | Query |
| --- | --- |
| Request count by method, status, and service | `http_server_request_duration_seconds_count` |
| p95 latency | `histogram_quantile(0.95, sum by (le) (rate(http_server_request_duration_seconds_bucket[1m])))` |
| In-flight requests | `http_server_active_requests` |
| Payload throughput | `http_server_request_body_size_bytes_sum` / `http_server_response_body_size_bytes_sum` |

## Debugging Checklist

Work down the boundaries in request order; each failure has an owning signal.
[Failure Modes](failure-modes.md) lists the exact responses per boundary.

| Symptom | Where to look |
| --- | --- |
| `401` responses | `claims_layer` log lines: missing/invalid bearer token, unsupported algorithm, or no configured decoder key. |
| `400` config responses | `user_config_store_layer` log lines and Redis content for the JWT subject. |
| `404` `Server not found` | `virtual_host_config_layer` debug line showing the requested virtual host id and how many the caller's config has. |
| MCP routing errors | `AuthorizedCallValidator::validate` debug lines, then `call_tool`/`read_resource`/`get_prompt` warns for the split and backend resolution. |
| Backend failures | `initialize:` warns for backends that failed to connect; routed-call warns name the failing backend. |
| Plugin problems | CPEX pipeline error logs; an invalid reload marks the plugin runtime failed until a valid config is applied. |

## Known Gaps

Tracked upstream, not yet implemented here:

- W3C trace-context propagation across gateway hops
  ([mcp-context-forge#4723](https://github.com/IBM/mcp-context-forge/issues/4723)).
- MCP-semantic spans with tool names and JSON-RPC method attributes
  ([mcp-context-forge#4722](https://github.com/IBM/mcp-context-forge/issues/4722)).
