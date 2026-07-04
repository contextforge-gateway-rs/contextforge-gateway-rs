# Architecture

This section explains how the gateway is put together and why the main
boundaries exist.

> 🧭 **Read this when changing the hot path.** The architecture pages keep
> request handling, config lookup, backend sessions, transports, and
> control-plane boundaries explicit.

The pages are ordered for a first read: start at the top for the big picture,
then work down into each boundary. If you are changing one area, jump straight
to its page.

| Page | What it covers |
| --- | --- |
| 🧭 [System Shape](system-shape.md) | The gateway's role in ContextForge, its crate layout, and the line between dataplane and control plane. |
| 🔀 [Request Flow](request-flow.md) | The ordered path from downstream HTTP request to backend MCP call and merged response. |
| 🧵 [Concurrency And Runtime Model](concurrency-and-runtime.md) | Executor shapes, shared state and locks, fanout, cancellation, and the allocator. |
| 🔐 [Authentication And User Config Lookup](authentication-and-user-config.md) | How JWT claims, Redis-backed user config, and virtual host selection combine before routing. |
| 🔒 [Security Model And Trust Boundaries](security-model.md) | What the gateway trusts, what compromise of each boundary means, and transport security posture. |
| 🗂️ [Runtime Configuration](runtime-configuration.md) | The current `UserConfig` model, MessagePack Redis persistence, cache behavior, and expected growth. |
| 🤝 [Control-Plane Integration](control-plane-integration.md) | The current, still-provisional integration surface: Redis keys, schemas, token shape, and route parity. |
| 🔌 [Backend Connections And Transports](backend-connections-and-transports.md) | Downstream listeners, upstream RMCP transports, config-store transport, and TLS direction. |
| 🧵 [Session Ownership](session-ownership.md) | How backend services are keyed, shared, cleaned up, and constrained by local process ownership. |
| 🧱 [Architectural Choices](architectural-choices.md) | The main tradeoffs behind dataplane scope, namespacing, config boundaries, and future protocols. |
