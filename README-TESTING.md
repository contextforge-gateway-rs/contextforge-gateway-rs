# Testing the Full Stack Locally (Docker)

Step-by-step process to build the dataplane image, bring up the full
control-plane + dataplane stack via `make`, and drive an MCP session through
it against the bundled `fast_time_server` test backend.

See [`docs/book/src/testing.md`](docs/book/src/testing.md) for the
workspace/unit-test and `cf-integration` harness instead — this doc covers the
local `docker/docker-compose.yml` stack only.

## 1. Build the dataplane image

```bash
make docker-prod
```

Builds `dataplane:latest` from `docker/Dockerfile`. `testing-up` refuses to
start if this image doesn't exist yet.

## 2. Launch the test stack

```bash
make testing-up
```

Starts: `nginx`, `control-plane`, `redis`, `postgres`, `pgbouncer`,
`data-plane`, `fast_time_server`, `register_fast_time` (see `Makefile`).

Resource budget for this stack (`docker/docker-compose.yml` `deploy.resources`):

| Service          | CPU Limit | Mem Limit |
| ---------------- | --------- | --------- |
| nginx            | 0.5       | 256 M     |
| control-plane    | 2         | 2 G       |
| redis            | 1         | 1 G       |
| postgres         | 2         | 2 G       |
| pgbouncer        | 0.5       | 256 M     |
| data-plane       | 2         | 2 G       |
| fast_time_server | 1         | 512 M     |
| **Total**        | **9 cores** | **8 G** |

`register_fast_time` is a one-shot init container (no steady-state resources):
it logs into control-plane, registers `fast_time_server` as an upstream
gateway, and creates a virtual server with a fixed id
(`b8e3f1a2c4d5e6f7a1b2c3d4e5f6a7b8`) exposing all of its tools.

Watch it finish:

```bash
docker compose -f docker/docker-compose.yml logs -f register_fast_time
```

Look for `Fast Time Server registration complete!`.

## 3. Wait for the config to reach the dataplane

The control plane doesn't talk to the dataplane directly — it publishes
`UserConfig` snapshots into Redis (`DATAPLANE_PUBLISHER=true`), and the
dataplane reads/caches from Redis. Worst-case propagation delay:

```text
publisher interval + dataplane user-config cache expiry
```

Both default to ~60s in this stack (see
`docs/book/src/control-plane-integration.md`). After
`register_fast_time` finishes, give it up to a minute before the virtual host
resolves on the dataplane side.

## 4. Get a bearer token

Data-plane can self-issue a test JWT (RS256, signed with the same
`assets/jwt.key` the control plane also signs with) — no need to shell into
any container:

```bash
TOKEN=$(curl --silent --show-error --request GET \
  --url http://localhost:8080/contextforge-rs/admin/tokens/admin@example.com \
  --header 'accept: application/json')
```

Use `admin@example.com` — that's `PLATFORM_ADMIN_EMAIL`, the identity
`register_fast_time` used, so it's the subject the publisher's `UserConfig`
is keyed under. Token is valid for 1 hour.

## 5. Point mcp-inspector at it

```bash
npx @modelcontextprotocol/inspector
```

| Field       | Value |
| ----------- | ----- |
| URL         | `http://localhost:8080/contextforge-rs/servers/b8e3f1a2c4d5e6f7a1b2c3d4e5f6a7b8/mcp` |
| Transport   | Streamable HTTP |
| Auth token  | `$TOKEN` from step 4 |

Note the `/contextforge-rs` prefix — that's what routes to the dataplane via
nginx (`docker/nginx.conf`, `location ^~ /contextforge-rs`). Without it,
the request goes to control-plane instead and you'll get a
`{"detail": "..."}`-shaped error from mcpgateway, not the dataplane.

Run `tools/list` — you should get back the `fast_time_server` tools
(prefixed with the gateway name, e.g. `fast_time-<tool_name>`).

## 6. Tear down

```bash
make testing-down
```

Stops the stack (containers/volumes kept; rerun `testing-up` to resume).

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| `403 {"detail":"Token is invalid: User is no longer a member of the associated team"}` | Hit control-plane instead of dataplane — URL missing `/contextforge-rs` prefix. | Use `/contextforge-rs/servers/<id>/mcp`. |
| `400 "Problem occurred retrieving the configuration"` | Dataplane has no `UserConfig` yet for the token's subject (`register_fast_time`/publisher hasn't run or synced). | Re-check step 3; confirm `register_fast_time` logs completed successfully and wait out the publisher interval. |
| `tools/list` returns `{"tools": [], "resultType": "complete"}` | Dataplane resolved the `UserConfig` fine, but couldn't connect to a declared backend. | `docker compose logs data-plane \| grep -i "worker quit\|BadScheme\|backend"`. A `BadScheme` error means `CONTEXTFORGE_GATEWAY_RS_UPSTREAM_CONNECTION_MODE` isn't set to `plain-text-or-tls` for a plain-`http://` backend. |
| `401` on every request | Bad/expired token, or control-plane and dataplane are signing/verifying with different keys or algorithms. | Confirm both sides have matching `JWT_ALGORITHM`/key config — see `docs/book/src/control-plane-integration.md`. |
