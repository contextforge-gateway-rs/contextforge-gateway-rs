# Getting Started

## Full Docker Stack

```bash
make docker-prod    # build contextforge-data-plane:latest from docker/Dockerfile
make compose-up    # start nginx, control-plane, redis, postgres, dataplane, fast_time_server
```

Wait for `register_fast_time` to finish, then allow ~60s config propagation:

```bash
docker compose -f docker/docker-compose.yml logs -f register_fast_time
# Look for: Fast Time Server registration complete!
```

| Resource | URL |
| --- | --- |
| MCP endpoint | `http://localhost:8080/contextforge-rs/servers/{virtual_host_id}/mcp` |
| Bearer token | `GET http://localhost:8080/contextforge-rs/admin/tokens/admin@example.com` |
| fast_time_server virtual host id | `b8e3f1a2c4d5e6f7a1b2c3d4e5f6a7b8` |

> **Critical**: `/contextforge-rs` prefix → dataplane. Without it → control-plane (you'll get `{"detail":"..."}` from mcpgateway, not a dataplane response).

Teardown: `make compose-down` (stops containers; volumes kept).

## cf-integration Harness (full end-to-end)

```bash
scripts/cf-integration.sh up        # checkout control-plane, pull dataplane image, start full stack
scripts/cf-integration.sh probe     # smoke: 401 check → initialize → tools/list → tools/call
scripts/cf-integration.sh test-all  # all lanes: live-mcp, live-rbac, live-protocol
scripts/cf-integration.sh down
```

Admin UI (control-plane): `http://localhost:8080/admin` — `admin@example.com` / `changeme`

Key env overrides: `CF_DATAPLANE_IMAGE`, `CF_DATAPLANE_VERSION`, `NGINX_PORT` (default `8080`).

## Local Cargo Dev Workflow

For debugger/profiler/rapid iteration, start Redis and the counter/conformance fixtures:

```bash
docker compose -f docker/docker-compose-local.yaml up -d
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

| Service | Endpoint | Role |
| --- | --- | --- |
| `redis` | `127.0.0.1:6379` | Runtime configuration store. |
| `gateway-one` | `http://127.0.0.1:5555/mcp` | MCP Rust SDK counter fixture. |
| `gateway-two` | `http://127.0.0.1:5556/mcp` | MCP Rust SDK conformance fixture. |

Run the binary with bootstrap helpers:

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

The client-facing route is `http://127.0.0.1:8001/contextforge-rs/servers/{virtual_host_id}/mcp`.

### Mint a local test token

```bash
USER_ID=11111111-1111-1111-1111-111111111111
TOKEN=$(curl --silent --show-error \
  --url "http://127.0.0.1:8001/contextforge-rs/admin/tokens/${USER_ID}?email=admin@example.com")
```

### Seed runtime configuration

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
            "passthrough_headers": [], "allowed_tool_names": [],
            "allowed_resource_names": [], "allowed_prompt_names": []
          },
          "gateway-two": {
            "name": "gateway-two",
            "url": "http://127.0.0.1:5556/mcp",
            "passthrough_headers": [], "allowed_tool_names": [],
            "allowed_resource_names": [], "allowed_prompt_names": []
          }
        }
      }
    }
  }'
```

### Verify with mcp-inspector

```bash
npx @modelcontextprotocol/inspector
```

| Field | Value |
| --- | --- |
| URL | `http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00df00ddeadbeefdead/mcp` |
| Transport | Streamable HTTP |
| Auth token | `$TOKEN` |

### Modern protocol probe (server/discover)

```bash
curl --silent --show-error \
  --url "http://127.0.0.1:8001/contextforge-rs/servers/${VIRTUAL_HOST_ID}/mcp" \
  --header "authorization: Bearer ${TOKEN}" \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --header 'mcp-protocol-version: 2026-07-28' \
  --header 'mcp-method: server/discover' \
  --data '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"curl","version":"0.1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}'
```

### Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `401 Unauthorized` | Missing/invalid bearer token, wrong issuer/audience, or expired token. |
| `400 Problem occurred retrieving the configuration` | Redis has no `UserConfig` for the token subject. Re-run the config POST. |
| `404 {"detail":"Server not found"}` | The URL virtual-host id does not exist in the user's config. |
| `400` mentioning request metadata | MCP protocol header and `_meta` version differ, or client metadata missing. |
| Backend calls fail | Backend URL wrong, fixture down, or `--upstream-connection-mode` rejects plain HTTP. |
