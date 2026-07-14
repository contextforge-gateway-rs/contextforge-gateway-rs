# Control-Plane Integration

> 🤝 **Provisional:** no formal contract with the control plane has been
> stipulated yet. This page is a snapshot of the current de facto integration
> surface with
> [IBM/mcp-context-forge](https://github.com/IBM/mcp-context-forge) as
> implemented today. Any row may change while the project is early; when a
> proper contract is agreed, this page should track it.

## Current Integration Surface

These are the values both sides currently rely on:

| Agreement | Value today |
| --- | --- |
| Client-facing route | `/servers/{virtual_host_id}/mcp` behaves like the legacy ContextForge MCP endpoint. The front door rewrites it to `/contextforge-rs/servers/{virtual_host_id}/mcp` on the dataplane. |
| Unknown virtual host | `404` with body `{"detail":"Server not found"}`, matching the control-plane response shape. |
| Token issuer and audience | `iss = mcpgateway`, `aud = mcpgateway-api` — the values the control plane mints. |
| Claims shape | `sub`, `jti`, `iss`, `aud`, `exp`, and `user` are required. `token_use`, `iat`, `teams`, and `scopes` are optional, as is `user.full_name`. The dataplane routes on `sub` only. |
| User config key | MessagePack-encoded `User::new(jwt_subject)` (key type plus subject). |
| User config value | MessagePack-encoded `UserConfig`; the JSON schema is generated into `schemas/user_config.json`. |
| Plugin config key | `ContextForgeGatewayRuntimePluginConfig`, JSON or MessagePack, `version: 1` with a `cpex` section. |

Changing any of these is a cross-repo change: the dataplane, the control-plane
publisher, and the integration harness all need updating together.

## Config Publishing

The control plane owns durable config and publishes runtime snapshots to
Redis. With `DATAPLANE_PUBLISHER=true`, it rewrites the dataplane's
`UserConfig` keys on an interval — every 60 seconds by default, configurable
in newer control-plane images.

Config staleness on the dataplane is bounded by two knobs:

```text
worst-case staleness = publisher interval + user config cache expiry
```

The dataplane's in-process cache defaults to 60 seconds
(`--user-config-cache-expiry-seconds`; `0` disables it and reads Redis on
every request). Functional test setups shorten the publisher interval and
disable the cache; production keeps both at 60.

## Schema Generation

`contextforge-gateway-rs-apis` is the single source of truth for the shared
config shapes. It generates the JSON Schemas the control plane can validate
against:

```bash
cargo run -p contextforge-gateway-rs-apis
```

This writes `schemas/user.json` and `schemas/user_config.json`. Regenerate and
commit them whenever `UserConfig`, `VirtualHost`, `BackendMCPGateway`, or the
`User` key type changes.

## Front-Door Split

Only MCP dataplane traffic comes to this process. The repository's reference
`docker/nginx.conf` proxies `location ^~ /contextforge-rs` to the gateway;
all UI, management API, and other ContextForge traffic stays on the existing
control-plane paths.

## Verifying The Integration

The [`cf-integration`](https://github.com/contextforge-gateway-rs/cf-integration)
harness tests exactly this surface: it runs the stock upstream control-plane
stack with the nginx split and the dataplane publisher enabled, then drives
probe, live-test, and load lanes through the public route. When the
integration surface changes, its lanes are what prove both sides still agree.
See [Testing](testing.md#full-stack-integration-harness) for the commands.
