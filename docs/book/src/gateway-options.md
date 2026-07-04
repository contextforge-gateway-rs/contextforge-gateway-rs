# Configuration Reference

This page is the reference for every gateway setting. The binary parses its
configuration with `clap`, so each option has both a CLI flag and an environment
variable, and both feed the same `Config` struct. When a setting is supplied as
a flag and an environment variable at the same time, the command-line flag wins.

For the always-current list, ask the binary directly:

```bash
cargo run -p contextforge-gateway-rs --bin contextforge-gateway-rs -- --help
```

The sections below group the options by concern: listeners, JWT, Redis, upstream
transport, runtime, telemetry, and logging.

## Minimum Useful Configuration

At startup, the CLI requires Redis location and Redis connection mode:

```text
--redis-address
--redis-port
--redis-mode
```

To serve useful traffic, the process also needs:

| Need | Typical flag |
| --- | --- |
| At least one downstream listener | `--address 127.0.0.1:8001` or `--tls-address 0.0.0.0:8443` |
| JWT verification material | `--token-verification-public-key` or `--token-verification-secret` |
| A compatible upstream client mode | `--upstream-connection-mode plain-text-or-tls` for local HTTP backends |
| Runtime user config in Redis | Written by the control plane or by local bootstrap helpers |

If no token verification key or secret is configured, authenticated MCP requests
cannot pass the claims layer.

## Listener Options

| Flag | Env var | Required | Meaning |
| --- | --- | --- | --- |
| `--address <host:port>` | `CONTEXTFORGE_GATEWAY_RS_ADDRESS` | No | Plain HTTP listener. Omit it when serving only TLS. |
| `--tls-address <host:port>` | `CONTEXTFORGE_GATEWAY_RS_TLS_ADDRESS` | No | TLS listener address. Requires certificate and private key. |
| `--server-certificate <path>` | `CONTEXTFORGE_GATEWAY_RS_TLS_SERVER_CERTIFICATE` | With `--tls-address` | PEM certificate chain for downstream TLS. |
| `--server-private-key <path>` | `CONTEXTFORGE_GATEWAY_RS_TLS_SERVER_PRIVATE_KEY` | With `--tls-address` | PEM private key for downstream TLS. |

`--address` and `--tls-address` may both be configured, but they must not use
the same socket address.

## JWT Options

| Flag | Env var | Required | Meaning |
| --- | --- | --- | --- |
| `--token-verification-public-key <path>` | `CONTEXTFORGE_GATEWAY_RS_TOKEN_VERIFICATION_PUBLIC_KEY` | For RSA tokens | RSA public key used for `RS256`, `RS384`, or `RS512` tokens. |
| `--token-verification-secret <secret>` | `CONTEXTFORGE_GATEWAY_RS_TOKEN_SECRET` | For HMAC tokens | Shared secret used for `HS256`, `HS384`, or `HS512` tokens. |
| `--token-verification-private-key <path>` | `CONTEXTFORGE_GATEWAY_RS_TOKEN_VERIFICATION_PRIVATE_KEY` | Local tools only | RSA private key used by the optional local token helper. Present only when `contextforge-gateway-rs-lib/with_tools` is enabled. |

The claims layer validates issuer, audience, and expiration:

| Claim | Expected value |
| --- | --- |
| `iss` | `mcpgateway` |
| `aud` | `mcpgateway-api` |
| `exp` | Present and not expired |

The JWT `sub` claim selects the Redis user config key. The path virtual host id
then selects one virtual host inside that config.

## Redis Options

| Flag | Env var | Required | Meaning |
| --- | --- | --- | --- |
| `--redis-address <host>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_HOSTNAME` | Yes | Redis host name or IP. |
| `--redis-port <port>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_PORT` | Yes | Redis port. |
| `--redis-mode <mode>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_CONNECTION_MODE` | Yes | Redis connection mode: `plain-text`, `tls`, or `mtls`. |
| `--redis-tls-trust-bundle <path>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_TRUST_BUNDLE` | TLS and mTLS | PEM trust bundle for Redis TLS. |
| `--redis-tls-client-certificate <path>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_CLIENT_CERTIFICATE` | mTLS | PEM client certificate for Redis mTLS. |
| `--redis-tls-client-private-key <path>` | `CONTEXTFORGE_GATEWAY_RS_REDIS_TLS_REDIS_CLIENT_PRIVATE_KEY` | mTLS | PEM client private key for Redis mTLS. |
| `--user-config-cache-expiry-seconds <n>` | `CONTEXTFORGE_GATEWAY_RS_USER_CONFIG_CACHE_EXPIRY_SECONDS` | No, default `60` | Expiry for the in-process user config cache in front of Redis. `0` disables caching and reads Redis on every request. |

Local Compose exposes plain Redis on `127.0.0.1:6379`, so local runs normally
use:

```bash
--redis-address 127.0.0.1 \
--redis-port 6379 \
--redis-mode plain-text
```

Runtime config values are MessagePack encoded. Redis is the current transport
for config, not the routing model itself.

## Upstream MCP Transport Options

| Flag | Env var | Required | Meaning |
| --- | --- | --- | --- |
| `--upstream-connection-mode <mode>` | `CONTEXTFORGE_GATEWAY_RS_UPSTREAM_CONNECTION_MODE` | No | Controls whether backend MCP URLs may be HTTP, HTTPS, or mTLS. |
| `--upstream-trust-bundle <path>` | `CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_TRUST_BUNDLE` | No | Additional PEM trust bundle for HTTPS upstreams. |
| `--upstream-certificate <path>` | `CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_CERTIFICATE` | mTLS modes | PEM client certificate for upstream mTLS. |
| `--upstream-private-key <path>` | `CONTEXTFORGE_GATEWAY_RS_TLS_UPSTREAM_PRIVATE_KEY` | mTLS modes | PEM client private key for upstream mTLS. |

Connection modes:

| Mode | Behavior |
| --- | --- |
| omitted or `tls-only` | HTTPS upstreams only. This is the safe default. |
| `plain-text-or-tls` | Allows HTTP and HTTPS upstream URLs. Use this for the local Compose backends. |
| `plain-text-or-m-tls` | Allows HTTP and HTTPS with client identity configured. |
| `mtls-only` | Requires HTTPS and uses the configured client certificate and key. |

If an upstream backend URL is `http://...` and the mode is omitted, calls fail
before reaching that backend because the reqwest client is HTTPS-only.

## Runtime Options

| Flag | Env var | Default | Meaning |
| --- | --- | --- | --- |
| `--number-of-cpus <n>` | `CONTEXTFORGE_GATEWAY_RS_GATEWAY_CPUS` | Host CPU count | Worker thread count for the Tokio runtime shape. |
| `--single-runtime <bool>` | `CONTEXTFORGE_GATEWAY_RS_SINGLE_RUNTIME` | `true` | `true` uses one multi-thread runtime. `false` starts multiple current-thread runtimes. |
| `--runtime-plugins-enabled <bool>` | `CONTEXTFORGE_GATEWAY_RS_RUNTIME_PLUGINS_ENABLED` | `false` | Enables CPEX runtime plugin execution and Redis plugin config loading. |

When runtime plugins are enabled, plugin config is read from Redis key
`ContextForgeGatewayRuntimePluginConfig`. That key is a control-plane trust
boundary because it decides which registered hooks run.

## Telemetry Options

| Flag | Env var | Default | Meaning |
| --- | --- | --- | --- |
| `--enable-open-telemetry <bool>` | `CONTEXTFORGE_GATEWAY_RS_ENABLE_OPEN_TELEMETRY` | `false` | Enables trace export. |
| `--enable-otel-metrics <bool>` | `CONTEXTFORGE_GATEWAY_RS_ENABLE_OTEL_METRICS` | `false` | Enables HTTP server metric export. |
| `--otlp-protocol <protocol>` | `CONTEXTFORGE_GATEWAY_RS_OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | `grpc` or `http-protobuf`. |
| `--otlp-endpoint <uri>` | `CONTEXTFORGE_GATEWAY_RS_OTEL_EXPORTER_OTLP_ENDPOINT` | Protocol-specific | Trace export endpoint. |
| `--otlp-metrics-endpoint <uri>` | `CONTEXTFORGE_GATEWAY_RS_OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | Protocol-specific | Metrics export endpoint. |
| `--otlp-headers <headers>` | `CONTEXTFORGE_GATEWAY_RS_OTEL_EXPORTER_OTLP_HEADERS` | none | Comma-separated `key=value` headers for OTLP export. |
| `--otlp-service-name <name>` | `CONTEXTFORGE_GATEWAY_RS_OTEL_SERVICE_NAME` | `CONTEXTFORGE-GATEWAY-RS` | OpenTelemetry `service.name`. |

Default endpoints:

| Protocol | Traces | Metrics |
| --- | --- | --- |
| `grpc` | `http://127.0.0.1:4317` | `http://127.0.0.1:4317` |
| `http-protobuf` | `http://127.0.0.1:4318/v1/traces` | `http://127.0.0.1:4318/v1/metrics` |

`RUST_TRACE_LOG` controls which spans reach the OTLP trace layer. The HTTP
trace layer emits debug-level spans, so local trace verification usually needs:

```bash
RUST_TRACE_LOG=debug
```

## Logging Options

| Flag or env var | Default | Meaning |
| --- | --- | --- |
| `--log-name` / `CONTEXTFORGE_GATEWAY_LOG_NAME` | `contextforge-gateway-rs.log` | File log name in the current working directory. |
| `--log-rotation` / `CONTEXTFORGE_GATEWAY_LOG_ROTATION` | `hourly` | Rotation mode: `minutely`, `hourly`, `daily`, or `never`. |
| `RUST_LOG` | `debug` | Console event filter. |
| `RUST_FILE_LOG` | `debug` | File event filter. |
| `RUST_TRACE_LOG` | `info` | OpenTelemetry span filter. |

## Common Flag Sets

### Local HTTP Gateway And Plain Redis

```bash
--address 127.0.0.1:8001 \
--redis-address 127.0.0.1 \
--redis-port 6379 \
--redis-mode plain-text \
--token-verification-public-key assets/jwt.key.pub \
--upstream-connection-mode plain-text-or-tls
```

### Downstream TLS Listener

```bash
--tls-address 0.0.0.0:8443 \
--server-certificate assets/contextforgeCA/contextforge-server.cert.pem \
--server-private-key assets/contextforgeCA/contextforge-server.key.pem
```

You may combine this with `--address` to expose both HTTP and HTTPS listeners.

### HMAC Token Verification

```bash
--token-verification-secret "${CONTEXTFORGE_GATEWAY_RS_TOKEN_SECRET}"
```

Use this only when downstream JWTs are signed with an `HS*` algorithm. RSA
tokens need `--token-verification-public-key`.

### Redis TLS

```bash
--redis-address 127.0.0.1 \
--redis-port 16379 \
--redis-mode tls \
--redis-tls-trust-bundle assets/contextforgeCA/contextforge.intermediate.ca-chain.cert.pem
```

### Upstream mTLS

```bash
--upstream-connection-mode mtls-only \
--upstream-trust-bundle assets/contextforgeCA/contextforge.intermediate.ca-chain.cert.pem \
--upstream-certificate assets/contextforgeCA/contextforge-client.cert.pem \
--upstream-private-key assets/contextforgeCA/contextforge-client.key.pem
```

The certificate and key paths must point to PEM files accepted by reqwest.

## Startup Validation

The gateway fails fast for these invalid combinations:

| Invalid combination | Reason |
| --- | --- |
| `--tls-address` without server cert or key | Downstream TLS cannot be configured partially. |
| Same socket for `--address` and `--tls-address` | The process cannot bind both listeners to the same address. |
| `--redis-mode tls` without trust bundle | Redis TLS needs a root certificate bundle. |
| `--redis-mode mtls` without trust bundle, client cert, or client key | Redis mTLS needs all three pieces. |
| mTLS upstream mode without upstream cert and key | The reqwest identity cannot be built. |
| Plain HTTP backend with default upstream mode | Default upstream mode is HTTPS-only. |
