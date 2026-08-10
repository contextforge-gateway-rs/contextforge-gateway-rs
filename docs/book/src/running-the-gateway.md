# Run the Gateway Locally

Use one of the two local workflows below. The end-to-end stack uses the current
Fast Time MCP backend, while the lightweight stack uses counter and conformance
test fixtures.

## Prerequisites

- Docker Compose.
- A Rust toolchain matching the workspace `rust-version` for the host workflow.
- Test keys at `assets/jwt.key` and `assets/jwt.key.pub` for the host workflow.
- Free local ports `6379`, `16379`, `5555`, `5556`, and `8001`.

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

For debugger, profiler, or rapid host-development loops, start Redis and the
counter and conformance fixtures:

```bash
docker compose -f docker/docker-compose-local.yaml up -d
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

The services are available at:

| Service | Local endpoint | Role |
| --- | --- | --- |
| `redis` | `127.0.0.1:6379` | Runtime configuration store. |
| `gateway-one` | `http://127.0.0.1:5555/mcp` | MCP Rust SDK counter fixture. |
| `gateway-two` | `http://127.0.0.1:5556/mcp` | MCP Rust SDK conformance fixture. |

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

Keep the data-plane process running. Use another terminal for the remaining
steps.

Runtime CPEX plugins need both a compile-time feature and runtime config. To
try the experimental secrets detection plugin locally, build with
`contextforge-data-plane/plugins`, start the data plane with
`--runtime-plugins-enabled true`, and write the Redis plugin config before
startup.

The local command uses `--upstream-connection-mode plain-text-or-tls` because
the sample backend URLs are plain HTTP. Without that option, the default
upstream client is HTTPS-only.

### Mint a Local Test Token

The local token helper signs an RS256 token with `assets/jwt.key`. Its `sub`
claim becomes the Redis user-config key.

```bash
USER_ID=11111111-1111-1111-1111-111111111111
USER_EMAIL=admin@example.com

TOKEN=$(curl --silent --show-error \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/tokens/${USER_ID}?email=${USER_EMAIL}")
```

### Seed Local Runtime Configuration

Create a virtual host that points at both lightweight MCP fixtures:

```bash
VIRTUAL_HOST_ID=c0ffee00f001f00df00ddeadbeefdead

curl --silent --show-error --request POST \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/${USER_ID}" \
  --header 'content-type: application/json' \
  --data '{
    "virtual_hosts": {
      "c0ffee00f001f00df00ddeadbeefdead": {
        "backends": {
          "gateway-one": {
            "name": "gateway-one",
            "url": "http://127.0.0.1:5555/mcp",
            "passthrough_headers": [],
            "allowed_tool_names": [],
            "allowed_resource_names": [],
            "allowed_prompt_names": []
          },
          "gateway-two": {
            "name": "gateway-two",
            "url": "http://127.0.0.1:5556/mcp",
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

The identity and routing relationship is:

```text
JWT subject
  -> Redis UserConfig
  -> virtual host id
  -> configured backend MCP URLs
```

### Verify Modern Protocol Discovery

Probe the client-facing route with MCP `2026-07-28`. Modern requests carry the
protocol version, client identity, and client capabilities in `_meta` on every
request:

```bash
curl --silent --show-error \
  --url "http://127.0.0.1:8001/contextforge-rs/servers/${VIRTUAL_HOST_ID}/mcp" \
  --header "authorization: Bearer ${TOKEN}" \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --header 'mcp-protocol-version: 2026-07-28' \
  --header 'mcp-method: server/discover' \
  --data '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "server/discover",
    "params": {
      "_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
          "name": "curl",
          "version": "0.1.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
      }
    }
  }'
```

The response should advertise `2026-07-28` in `supportedVersions`. This probe
validates the modern HTTP envelope plus authentication, user-config lookup, and
virtual-host selection. Use the [recommended end-to-end stack](local-docker-stack.md)
for the complete backend tool-call smoke test.

The target downstream contract is MCP `2026-07-28` over Streamable HTTP using
`server/discover` and per-request client metadata. Older protocol versions,
legacy session initialization, and SSE remain control-plane responsibilities;
see [MCP Behavior](mcp-behavior.md) and
[Control-Plane Integration](control-plane-integration.md).

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `401 Unauthorized` | Missing bearer token, invalid signature, expired token, or mismatched issuer/audience. |
| `400 Problem occurred retrieving the configuration` | Redis has no `UserConfig` for the token subject. Re-run the config POST with the same `USER_ID`. |
| `500 Problem occurred retrieving the configuration` | Redis access or stored config decoding failed; inspect data-plane and Redis logs. |
| `404` with `{"detail":"Server not found"}` | The URL virtual-host id does not exist in that user's config. |
| `400` mentioning request metadata | The MCP protocol header and `_meta` version differ, or required per-request client metadata is missing. |
| Backend calls fail | A backend URL is wrong, a fixture is down, or `--upstream-connection-mode` rejects plain HTTP. |

Stop the host process with `Ctrl-C`, then remove the lightweight dependencies:

```bash
docker compose -f docker/docker-compose-local.yaml down
```
