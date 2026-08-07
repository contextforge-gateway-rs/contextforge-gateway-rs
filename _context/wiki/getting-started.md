# Getting Started

## Full Docker Stack

```bash
make docker-prod    # build dataplane:latest from docker/Dockerfile
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
