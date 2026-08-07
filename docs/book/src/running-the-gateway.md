# Run the Gateway Locally

Use one of the two local workflows below. Both use the current Fast Time MCP
backend from `IBM/contextforge-examples`.

## Recommended End-to-End Stack

The supported smoke environment includes the external ContextForge control
plane, Redis, PostgreSQL, PgBouncer, the Rust data plane, and
`fast_time_server`:

```bash
make docker-prod
make testing-up
```

Confirm the services and one-shot registration job:

```bash
docker compose -f docker/docker-compose.yml ps -a
docker compose -f docker/docker-compose.yml logs register_fast_time
```

Continue with [Local Docker Stack](local-docker-stack.md) for token creation,
config propagation, and an MCP smoke test through the complete control-plane to
data-plane path.

Stop the stack without deleting its containers or volumes:

```bash
make testing-down
```

## Run the Rust Binary from Cargo

For debugger, profiler, or rapid host-development loops, start only Redis and
the current test backend:

```bash
docker compose -f docker/docker-compose-local.yaml up -d
docker compose -f docker/docker-compose-local.yaml ps redis fast_time_server
```

The services are available at:

| Service | Local endpoint | Role |
| --- | --- | --- |
| `redis` | `127.0.0.1:6379` | Runtime configuration store. |
| `fast_time_server` | `http://127.0.0.1:8880/mcp` | Current sample MCP backend. |

Run the binary with the local bootstrap helpers when direct token/config setup
is needed during development:

```bash
cargo run -p contextforge-data-plane \
  --features contextforge-data-plane-lib/with_tools \
  --bin contextforge-data-plane -- \
  --address 127.0.0.1:8001 \
  --redis-address 127.0.0.1 \
  --redis-port 6379 \
  --redis-mode plain-text \
  --token-verification-public-key assets/jwt.key.pub \
  --token-verification-private-key assets/jwt.key \
  --upstream-connection-mode plain-text-or-tls \
  --number-of-cpus 4
```

The client-facing route is:

```text
http://127.0.0.1:8001/contextforge-rs/servers/{virtual_host_id}/mcp
```

The target downstream contract is MCP `2026-07-28` over Streamable HTTP using
`server/discover` and per-request client metadata. Older protocol versions,
legacy session initialization, and SSE remain control-plane responsibilities;
see [MCP Behavior](mcp-behavior.md) and
[Control-Plane Integration](control-plane-integration.md).

Stop the host process with `Ctrl-C`, then remove the lightweight dependencies:

```bash
docker compose -f docker/docker-compose-local.yaml down
```
