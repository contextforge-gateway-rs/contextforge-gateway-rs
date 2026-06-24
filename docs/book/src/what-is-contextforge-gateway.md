# What is ContextForge Gateway?

Welcome to the ContextForge Gateway book. `contextforge-gateway-rs` is the Rust
dataplane for ContextForge. It accepts downstream MCP streamable HTTP traffic,
authenticates the caller, loads the caller's runtime configuration, opens MCP
client sessions to configured backend MCP servers, and presents those backends
as one merged MCP server.

![Gateway overview](assets/gateway-overview.svg)

Blue arrows show request traffic. Green arrows show backend responses returning
to the gateway and the merged MCP response going back to the client.

| Layer | What happens |
| --- | --- |
| Client edge | An MCP client calls `/contextforge-rs/servers/{virtual_host_id}/mcp` with a bearer token and MCP session headers. |
| Gateway hot path | The gateway validates identity, loads runtime config, selects the virtual host, and routes MCP methods. |
| Backend edge | The gateway opens or reuses MCP client sessions to configured backend MCP servers and merges what the client sees. |

> **Key idea:** downstream clients interact with one logical MCP server. The
> gateway decides which configured backend sessions are involved.

The gateway is not the management application. It does not own the UI, IAM
lifecycle, tenant administration, durable metrics storage, or long-lived
configuration editing. Those concerns stay in the external ContextForge control
plane. This repository owns the hot request path and the runtime pieces needed
to make that path fast, observable, and enforceable.

Most operators should not think about backend MCP servers directly. They should
think about one client-facing MCP endpoint:

```text
/contextforge-rs/servers/{virtual_host_id}/mcp
```

Behind that endpoint, the gateway has three main boundaries:

- the downstream boundary, where clients connect over streamable HTTP and
  present their bearer token and MCP session id
- the configuration boundary, where the gateway turns the JWT subject into a
  runtime `UserConfig`
- the upstream boundary, where the gateway opens or reuses MCP client sessions
  to the configured backend servers

Most bugs in this service come from confusing those boundaries. A downstream
request is not allowed to pick arbitrary upstream backends. Redis is not the
routing model; it is the current config-store transport. Backend sessions are
not durable cluster state; they are local process state today.

| Boundary | Input | Output | Owner |
| --- | --- | --- | --- |
| Downstream | HTTP request, JWT, MCP headers | request extensions and downstream session id | gateway |
| Configuration | JWT subject and virtual host id | `UserConfig` and selected `VirtualHost` | control plane data, gateway enforcement |
| Upstream | selected backend names and URLs | running backend MCP client services | gateway |

## Mental model

The gateway is easiest to understand as one logical MCP server assembled from a
set of configured backend MCP servers.

On `initialize`, the gateway validates the caller, resolves the requested
virtual host, and creates upstream MCP client sessions for that virtual host's
backends. On list calls, it fans out to those backends and merges the result. On
targeted calls, it uses the backend prefix in the public tool, resource, or
prompt name to route to exactly one backend.

For a stateful MCP call to work, five facts must line up:

```text
JWT subject
  -> user config
  -> virtual host id
  -> downstream MCP session id
  -> backend session map entries
```

If any part is missing, the gateway should fail at the layer that owns that
fact: auth, config lookup, virtual host resolution, session lookup, routing, or
upstream transport.

> **Operational consequence:** stateful MCP traffic needs session affinity
> until backend session state moves out of the gateway process.

## Non-goals

This repository should not grow control-plane behavior by accident. It should
not become the admin UI, tenant management API, policy authoring system,
credential store, or durable observability backend.

It also should not expose backend topology as more than the MCP routing
contract requires. Backend names are visible in prefixed tool, resource, and
prompt names, but clients should still experience one gateway endpoint and one
logical MCP server.
