# Run the Gateway Locally

This page walks through a local run that proves the dataplane can authenticate a
caller, load runtime config from Redis, initialize backend MCP sessions, and
return merged MCP results to a downstream client.

The local flow uses the repository's Docker Compose stack for Redis and two
sample backend MCP servers:

| Service | Local port | Role |
| --- | --- | --- |
| `redis` | `6379` | Runtime config store for user config. |
| `gateway-one` | `5555` | Sample counter MCP backend. |
| `gateway-two` | `5556` | Sample conformance MCP backend. |
| `contextforge-gateway-rs` | `8001` | Rust dataplane process you run with Cargo. |

> 🧪 The token and user-config endpoints below are local bootstrap helpers. They
> are compiled with `contextforge-gateway-rs-lib/with_tools`. In a real
> deployment, the external ContextForge control plane mints tokens and writes
> config to Redis.

Run the numbered steps in order, in the same terminal. Later steps reuse shell
variables such as `${TOKEN}` and `${SESSION_ID}` that earlier steps set, so a
fresh shell will not have them.

## Prerequisites

- Rust toolchain matching the workspace `rust-version`.
- Docker Compose.
- Free local ports: `6379`, `5555`, `5556`, and `8001`.
- Test keys under `assets/`: `jwt.key` and `jwt.key.pub`.

## 1. Start Redis and Backend MCP Servers

```bash
docker compose -f docker/docker-compose-local.yaml up -d
```

Check that the local dependencies are running:

```bash
docker compose -f docker/docker-compose-local.yaml ps redis gateway-one gateway-two
```

The backends listen on the host so the gateway can reach them at
`http://127.0.0.1:5555/mcp` and `http://127.0.0.1:5556/mcp`.

## 2. Start the Gateway

For the local bootstrap flow, run the binary with the `with_tools` dependency
feature so the admin token and config endpoints are available:

```bash
cargo run -p contextforge-gateway-rs \
  --features contextforge-gateway-rs-lib/with_tools \
  --bin contextforge-gateway-rs -- \
  --address 127.0.0.1:8001 \
  --redis-address 127.0.0.1 \
  --redis-port 6379 \
  --redis-mode plain-text \
  --token-verification-public-key assets/jwt.key.pub \
  --token-verification-private-key assets/jwt.key \
  --upstream-connection-mode plain-text-or-tls \
  --number-of-cpus 4
```

Keep this process running. The gateway exposes MCP traffic under:

```text
http://127.0.0.1:8001/contextforge-rs/servers/{virtual_host_id}/mcp
```

The local command uses `--upstream-connection-mode plain-text-or-tls` because
the sample backend URLs are plain HTTP. Without that option, the default
upstream client is HTTPS-only.

## 3. Mint a Test Token

In another terminal, request a JWT for the test subject:

```bash
TOKEN=$(curl --silent --show-error \
  --url http://127.0.0.1:8001/contextforge-rs/admin/tokens/admin@example.com)

printf '%s\n' "${TOKEN}"
```

The token's `sub` claim is `admin@example.com`. The gateway uses that subject
as the Redis user-config key.

## 4. Write User Config

Seed Redis with one virtual host and two backend MCP servers:

```bash
curl --silent --show-error --request POST \
  --url http://127.0.0.1:8001/contextforge-rs/admin/userconfigs/admin@example.com \
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

The important relationship is:

```text
JWT subject admin@example.com
  -> Redis user config
  -> virtual host c0ffee00f001f00lf00ldeadbeefdead
  -> backend MCP URLs
```

## 5. Initialize an MCP Session

Open a streamable HTTP MCP session and save the returned `mcp-session-id`
header:

```bash
INIT_HEADERS=$(mktemp)

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

SESSION_ID=$(awk 'tolower($1) == "mcp-session-id:" { gsub("\r", "", $2); print $2 }' "${INIT_HEADERS}")
printf '%s\n' "${SESSION_ID}"
```

During `initialize`, the gateway opens upstream MCP client sessions to the
configured backends and stores them under the downstream session id.

Send the MCP initialized notification:

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

## 6. List Tools

```bash
curl --silent --show-error \
  --url http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00lf00ldeadbeefdead/mcp \
  --header "authorization: Bearer ${TOKEN}" \
  --header "mcp-session-id: ${SESSION_ID}" \
  --header 'mcp-protocol-version: 2025-11-25' \
  --header 'content-type: application/json' \
  --header 'accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

The response should contain tool names prefixed with their backend name, such as
`gateway-one-increment`. That prefix is the routing contract for later
`tools/call` requests.

## 7. Call a Tool

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
    "id": 2,
    "method": "tools/call",
    "params": {
      "name": "gateway-one-increment",
      "arguments": {}
    }
  }'
```

The gateway strips `gateway-one-`, forwards `increment` to the `gateway-one`
backend session, and returns the backend result to the client.

## 8. End the Session

```bash
curl --silent --show-error --request DELETE \
  --url http://127.0.0.1:8001/contextforge-rs/servers/c0ffee00f001f00lf00ldeadbeefdead/mcp \
  --header "authorization: Bearer ${TOKEN}" \
  --header "mcp-session-id: ${SESSION_ID}"
```

The gateway removes local session state and backend transports for that user
and MCP session id.

## Troubleshooting

| Symptom | Likely boundary |
| --- | --- |
| `401 Unauthorized` | Missing bearer token, invalid signature, expired token, wrong issuer, wrong audience, or no configured decoder key. |
| `400 Problem occurred retrieving the configuration` | Redis has no config for the JWT subject, or the config could not be decoded. |
| MCP `No configuration` error | The path virtual host id is not present in that user's config. |
| MCP session id is empty | The `initialize` call failed before RMCP created a downstream session. |
| Backend calls fail | Backend URL is wrong, backend process is down, or `--upstream-connection-mode` rejects the URL scheme. |
| Calls fail after gateway restart | Backend MCP session state is local process state today. Re-run `initialize`. |

## Tear Down

Stop the gateway with `Ctrl-C`, then stop local dependencies:

```bash
docker compose -f docker/docker-compose-local.yaml down
```
