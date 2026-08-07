# Deployment Notes

> 🏗️ **Deployment lens:** the gateway is one stateless-config, stateful-session
> process behind a front door. Everything here follows from that: route only
> MCP traffic to it, keep Redis close and trusted, and give stateful sessions
> affinity.

## Front-Door Routing

The reference `docker/nginx.conf` shows the intended split:

- `location ^~ /contextforge-rs` proxies to the gateway upstream.
- Everything else — UI, management APIs, other ContextForge traffic — stays on
  the existing control-plane paths.
- The reference listener uses `backlog=4096 reuseport` and configures upstream
  retries (`error timeout http_502/503/504`, 2 tries, 10 s window). For MCP
  `POST` bodies this effectively retries only connection-stage failures:
  nginx does not re-send non-idempotent requests once they reached an
  upstream, and MCP calls are not idempotent.

There is no production health endpoint today: `/health` is a `with_tools`
bootstrap helper served at `/contextforge-rs/health`, and production builds
compile it out. Use TCP-level checks or the exported metrics for liveness
until a real health endpoint exists. (The reference nginx config's
`location = /health` predates this and does not match the gateway's route.)

The [`cf-integration`](https://github.com/contextforge-org/contextforge-dev-tools)
harness runs the same split with the stock upstream control-plane stack and
rewrites public `/servers/{id}/mcp` to `/contextforge-rs/servers/{id}/mcp`.

## TLS Choices

| Leg | Options |
| --- | --- |
| Front door to gateway | Plain HTTP on a trusted private network (the common shape behind nginx), or terminate TLS at the gateway with `--tls-address` plus certificate and key. Both listeners can run at once on different sockets. |
| Gateway to Redis | `--redis-mode` plain, TLS, or mTLS. Use TLS/mTLS across trust zones — Redis is the config trust boundary (see [Security Model](security-model.md)). |
| Gateway to backends | HTTPS-only by default; opt into plain HTTP or mTLS with `--upstream-connection-mode`. |

## Session Affinity And Failover

Backend MCP sessions are local process state
([Session Ownership](session-ownership.md)):

- More than one replica requires sticky routing by `Mcp-session-id`; the
  reference nginx config does not provide this, so today's safe shapes are a
  single replica or a front door that adds stickiness.
- On restart or failover, sessions are gone; clients must re-run
  `initialize`. Design clients to treat a session-not-found error as
  "reinitialize", not "retry".
- A Redis-backed user session store exists in code, but live backend services
  would still be process-local; a remote session story is future work.
- Inside one host, the same constraint applies to `--single-runtime false`;
  see [Concurrency And Runtime Model](concurrency-and-runtime.md).

## Redis Availability

Redis is required at startup and on every uncached config lookup. The
connection manager retries heavily (1,000 retries) rather than failing fast,
and the in-process cache (default 60 s) rides out short blips for warm
subjects. A cold subject during a Redis outage fails at config lookup; the
current Redis adapter reports failed `GET` calls as missing data, so the layer
returns `400` until Redis returns.

## Images And Sizing

- CI builds `docker/Dockerfile` (a `rust:1.96.1` builder stage) on every push
  to `main` and pushes both
  `ghcr.io/<owner>/contextforge-data-plane:v<version>` and
  `ghcr.io/<owner>/contextforge-data-plane:latest`, where `<version>` is the
  Cargo package version. There is no unprefixed version tag. Pin the
  `v`-prefixed tag for reproducible deployments; `latest` tracks `main`.
- The reference Compose stack runs the gateway with raised limits worth
  copying to real deployments: `nofile` 65535 and TCP tuning
  (`tcp_fin_timeout=15`, widened local port range) for high connection churn.
- Size CPU with `--number-of-cpus` (defaults to host CPU count) and keep the
  default single multi-thread runtime for stateful traffic. Memory scales
  with active sessions (live backend services) and the config caches (up to
  50,000 entries each).

## Deployment Checklist

1. Front door routes only `/contextforge-rs` here.
2. JWT verification key or secret in place and rotated with the control
   plane's signing key.
3. Redis reachable, TLS/mTLS across trust zones, write access restricted to
   the control plane; `DATAPLANE_PUBLISHER` enabled on the control plane.
4. Upstream connection mode matches the backend URL schemes.
5. One replica per `Mcp-session-id` (single replica or sticky routing).
6. `with_tools` disabled in the production build.
7. Telemetry export pointed at the collector; see
   [Telemetry And Diagnostics](telemetry-and-diagnostics.md).
