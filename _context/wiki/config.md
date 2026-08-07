# Configuration Reference

## Minimum Required Flags

```
--redis-address   --redis-port   --redis-mode
```

Plus at least: `--address` or `--tls-address`, `--token-verification-public-key` or `--token-verification-secret`.

## Key CLI Flags (env var: `CONTEXTFORGE_GATEWAY_RS_*`)

| Flag | Env suffix | Default | Note |
| --- | --- | --- | --- |
| `--address` | `ADDRESS` | — | Plain HTTP listener |
| `--tls-address` | `TLS_ADDRESS` | — | Requires cert + key |
| `--server-certificate` | `TLS_SERVER_CERTIFICATE` | — | With `--tls-address` |
| `--server-private-key` | `TLS_SERVER_PRIVATE_KEY` | — | With `--tls-address` |
| `--token-verification-public-key` | `TOKEN_VERIFICATION_PUBLIC_KEY` | — | RSA (RS256/384/512) |
| `--token-verification-secret` | `TOKEN_SECRET` | — | HMAC (HS256/384/512) |
| `--redis-address` | `REDIS_HOSTNAME` | **required** | |
| `--redis-port` | `REDIS_PORT` | **required** | |
| `--redis-mode` | `REDIS_CONNECTION_MODE` | **required** | `plain-text` \| `tls` \| `mtls` |
| `--user-config-cache-expiry-seconds` | `USER_CONFIG_CACHE_EXPIRY_SECONDS` | `60` | `0` = no cache |
| `--upstream-connection-mode` | `UPSTREAM_CONNECTION_MODE` | HTTPS-only | `plain-text-or-tls` for local HTTP backends |
| `--number-of-cpus` | `GATEWAY_CPUS` | host CPU count | Tokio worker threads |
| `--single-runtime` | `SINGLE_RUNTIME` | `true` | `false` = multi-runtime (no session affinity) |
| `--runtime-plugins-enabled` | `RUNTIME_PLUGINS_ENABLED` | `false` | Enables CPEX hooks |
| `--enable-open-telemetry` | `ENABLE_OPEN_TELEMETRY` | `false` | OTLP traces |
| `--enable-otel-metrics` | `ENABLE_OTEL_METRICS` | `false` | OTLP metrics |

## JWT Claims (validated by `claims_layer`)

| Claim | Required value |
| --- | --- |
| `iss` | `mcpgateway` |
| `aud` | `mcpgateway-api` |
| `exp` | present, not expired |
| `sub` | → selects Redis user config key |

Optional: `token_use`, `iat`, `teams`, `scopes`, `user.full_name`.

> **No revocation:** a leaked token is valid until `exp`. Rotate the signing key and restart to invalidate all outstanding tokens.

## UserConfig Shape (from `contextforge-gateway-rs-apis`)

```
UserConfig
  virtual_hosts: HashMap<String, VirtualHost>

VirtualHost
  backends: HashMap<String, BackendMCPGateway>   ← map key = routing prefix

BackendMCPGateway
  name: String
  url: Url
  transport: STREAMABLEHTTP | SSE | STDIO         ← only STREAMABLEHTTP used today
  passthrough_headers: Vec<String>                ← snapshotted at initialize; session-scoped
  add_headers: HashMap<String, String>            ← injected after passthrough
  remove_headers: Vec<String>                     ← stripped after add
  tool_name_aliases: HashMap<String, String>      ← downstream_alias → upstream_original
  allowed_tool_names: Vec<String>                 ← model exists, NOT currently enforced
  allowed_resource_names: Vec<String>             ← model exists, NOT currently enforced
  allowed_prompt_names: Vec<String>               ← model exists, NOT currently enforced
```

**Header apply order:** `passthrough_headers` → `add_headers` (override passthrough) → `remove_headers` (applied last).

**`passthrough_headers` is session-scoped.** Values are snapshotted from the `initialize` request and baked into the backend transport for the session lifetime. Post-`initialize` calls (tool calls, list calls) reuse those headers. Request-scoped propagation requires per-request transport reconstruction (future work).

**Protected headers** — silently skipped in all three phases (passthrough/add/remove):

| Category | Headers |
| --- | --- |
| Body-framing | `Content-Length`, `Content-Type` |
| Hop-by-hop | `Connection`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`, `Proxy-Connection`, `TE`, `Trailer`, `Trailers`, `Transfer-Encoding`, `Upgrade` |
| RMCP-reserved | `Mcp-Session-Id`, `Accept`, `Last-Event-Id` |
| Gateway-managed | `Host` (set from backend URL host + port; never overridden by config) |

Redis storage: `MessagePack(User::new(sub))` → `MessagePack(UserConfig)`.

Schema: `schemas/user_config.json`. Regenerate after any struct change:
```bash
cargo run -p contextforge-gateway-rs-apis
```

## Plugin Config (Redis key: `ContextForgeGatewayRuntimePluginConfig`)

```
RuntimePluginConfigDocument
  version: 1
  cpex: CpexConfig
```

Supported: `cmf.tool_pre_invoke`, `cmf.tool_post_invoke` only.  
Rejected: routing-based selection, plugin dirs, global policies, other hook types.  
Reload watcher: 10-minute interval. Invalid reload → runtime marked failed.

## Startup Validation (fails fast)

| Invalid combo | Reason |
| --- | --- |
| `--tls-address` without cert or key | Rustls needs both |
| Same address for `--address` and `--tls-address` | Cannot bind same socket twice |
| `--redis-mode tls` without trust bundle | Required |
| `--redis-mode mtls` without trust bundle + client cert + key | All three required |
| mTLS upstream without cert and key | reqwest identity cannot be built |
| HTTP backend URL with default upstream mode (HTTPS-only) | Calls fail before reaching backend |

## Upstream Connection Modes

| Mode | Behavior |
| --- | --- |
| omitted / `tls-only` | HTTPS backends only (safe default) |
| `plain-text-or-tls` | HTTP or HTTPS (use for local Compose backends) |
| `plain-text-or-m-tls` | HTTP or HTTPS + client identity |
| `mtls-only` | HTTPS + client cert/key required |

## Logging Env Vars

| Var | Default | Controls |
| --- | --- | --- |
| `RUST_LOG` | `debug` | Console filter |
| `RUST_FILE_LOG` | `debug` | File filter |
| `RUST_TRACE_LOG` | `info` | OTLP span filter (`debug` for local trace verification) |


## Telemetry Debugging Notes

> **`RUST_TRACE_LOG=debug` is required for trace export.** The default (`info`) drops HTTP spans before they reach the OTLP exporter — nothing arrives at the trace backend.

Metrics are pushed by a `PeriodicReader` every **30 seconds**. Allow ~35s after the first request before data appears downstream.

**Stable log prefixes for grepping** (use these to scope log searches by boundary):

| Prefix | Boundary |
| --- | --- |
| `claims_layer` | JWT validation failures |
| `user_config_store_layer` | Config lookup / Redis errors |
| `virtual_host_config_layer` | Unknown virtual host |
| `AuthorizedCallValidator::validate` | Post-session MCP validation |
| `initialize:` | Backend session creation |
| `call_tool` | Tool routing and backend invocation |

**Debugging by symptom:**

| Symptom | Where to look |
| --- | --- |
| `401` | `claims_layer` logs: missing/invalid token, unsupported algorithm, no decoder key |
| `400` config error | `user_config_store_layer` logs + Redis content for the JWT subject |
| `404 Server not found` | `virtual_host_config_layer` debug: requested vhost id vs caller's config |
| MCP routing errors | `AuthorizedCallValidator::validate` debug, then `call_tool`/`read_resource`/`get_prompt` warns |
| Backend failures | `initialize:` warns for failed backends; routed-call warns name the failing backend |
| Plugin problems | CPEX pipeline error logs; invalid reload marks runtime failed |