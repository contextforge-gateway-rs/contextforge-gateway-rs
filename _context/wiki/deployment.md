# Deployment

## Checklist

1. Front door routes only `/contextforge-rs` to the dataplane.
2. JWT verification key/secret in place and rotated with the control plane's signing key.
3. Redis reachable; TLS/mTLS across trust zones; write access restricted to the control plane; `DATAPLANE_PUBLISHER=true` on the control plane.
4. Upstream connection mode matches backend URL schemes.
5. One replica per `Mcp-session-id` (single replica or sticky routing).
6. `with_tools` feature **disabled** in the production build.
7. Telemetry export pointed at the collector.
8. System limits raised: `nofile 65535`, TCP tuning (`tcp_fin_timeout=15`, widened local port range).

## Health Endpoint

**`/contextforge-rs/health` is a `with_tools` bootstrap helper only.** Production builds compile it out. Use TCP-level liveness checks or the exported metrics until a real health endpoint exists.

## nginx Front-Door Routing

Reference `docker/nginx.conf` split:
- `location ^~ /contextforge-rs` → proxies to the gateway.
- All other traffic (UI, management, SSE, legacy MCP) → control-plane.
- Upstream retries on `error timeout http_502/503/504`: 2 tries, 10-second window. Non-idempotent MCP `POST` bodies are not re-sent after they reached an upstream — only connection-stage failures retry.

## Session Affinity And Failover

Backend MCP sessions are **local process state** — see [routing.md](routing.md).

- >1 replica requires sticky routing by `Mcp-session-id`. The reference nginx config does not provide this; safe shapes today are a single replica or a front door with stickiness.
- On restart or failover, all sessions are lost. Design clients to treat session-not-found as "reinitialize", not "retry".

## Redis Availability

- Redis is required at startup and on every uncached config lookup.
- Connection manager retries 1,000 times (rather than failing fast).
- In-process cache (default 60s) rides out short Redis blips for warm subjects.
- A cold subject during a Redis outage fails at `user_config_store_layer` → `400` until Redis returns.

## Images

- CI builds `docker/Dockerfile` on every push to `main` and publishes both `ghcr.io/<owner>/contextforge-data-plane:v<version>` and `ghcr.io/<owner>/contextforge-data-plane:latest`, where `<version>` is the Cargo package version.
- **Pin the `v`-prefixed tag for reproducible deployments.** `latest` tracks `main`.
- Builder: `rust:1.96.1` in `docker/Dockerfile`.
- The reference Compose stack runs the gateway with raised limits worth copying to real deployments: `nofile 65535` and TCP tuning (`tcp_fin_timeout=15`, widened local port range).

## TLS Choices

| Leg | Options |
| --- | --- |
| Front door to gateway | Plain HTTP on a trusted private network (common shape behind nginx), or terminate TLS at the gateway with `--tls-address` plus certificate and key. Both listeners can run at once on different sockets. |
| Gateway to Redis | `--redis-mode` plain, TLS, or mTLS. Use TLS/mTLS across trust zones — Redis is the config trust boundary. |
| Gateway to backends | HTTPS-only by default; opt into plain HTTP or mTLS with `--upstream-connection-mode`. |

## Config Propagation Delay

```text
worst-case staleness = publisher interval + user-config cache expiry
```

Both default to ~60s. For functional tests, shorten the publisher interval and disable the cache. For throughput benchmarks, keep both at 60s.


## Security Posture

| Concern | Current state |
| --- | --- |
| JWT revocation | None. A leaked token is valid until `exp`. Rotate the key and restart to invalidate. |
| CORS | Wide open (any origin, method, header). Bearer-token based + cookie-free → no CSRF risk, but expect tightening as policy work lands. |
| Local bootstrap routes | `/contextforge-rs/admin/tokens/{user}`, `/admin/userconfigs/{user}`, `/health` are **outside auth middleware — unauthenticated by design.** Only exist with `with_tools`. Production builds must not enable `with_tools`. |
| Redis trust | Whoever can write Redis controls routing (arbitrary backend URLs receive caller traffic) AND which registered plugin hooks execute on payloads. Protect with TLS/mTLS and restrict write access to the control plane. |
| Downstream TLS | Optional. Plain HTTP is acceptable only behind a trusted front door on a private network. Identity is always the bearer JWT, not mTLS. |
| Plugin code | Fully trusted, in-process. Redis config activates compiled-in factories only — it cannot inject new Rust code. |