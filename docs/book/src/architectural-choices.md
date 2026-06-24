# Architectural Choices

This page records the choices that should stay visible as the gateway evolves.
They are not permanent constraints, but changing one should be a deliberate
architecture decision.

## Dataplane, Not Control Plane

The dataplane consumes runtime config; it does not own management state. New
features should ask:

```text
Is this hot-path enforcement, or is this a control-plane workflow?
```

If the answer is "workflow", it probably belongs outside this repo. The Rust
gateway should enforce the result of management decisions, not become the place
where those decisions are authored.

## Config Access Is Abstracted

Redis is the current transport and storage adapter. It is not the routing model.
Code that chooses backends should depend on `UserConfig`, `VirtualHost`, and
`UserConfigStore`, not on Redis commands or key encoding.

This keeps a future xDS/gRPC or other config stream possible without rewriting
MCP routing.

## Backend Names Are Public

Backend names are part of the client-visible MCP namespace. A backend named
`gateway-one` produces names such as `gateway-one-increment`.

That means backend renames are behavior changes. Any future route aliasing,
filtering, or prettier naming scheme needs an explicit migration path.

## Session State Is Local Today

Backend sessions are local process state today. Until that changes,
load-balanced deployments need sticky routing or a reinitialization strategy.
Do not design request handling as if every node can serve every session.

## Merged MCP Semantics Define The Contract

The gateway should look like one MCP server to the downstream client. Backend
identity appears through namespaced objects, but clients should not need to know
transport details, Redis state, or fanout mechanics.

This leaves room for backend filtering, policy, and routing changes without
turning backend topology into a hard client dependency.

## Plugin Boundaries Stay Explicit

Payload mutation is powerful. Plugin hooks need clear ownership, failure,
timeout, cancellation, and telemetry behavior. Avoid adding ad hoc hook logic in
the middle of routing code. Add a clear hook point or keep the behavior out of
the plugin system.
