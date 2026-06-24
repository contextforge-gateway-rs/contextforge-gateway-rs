# Architecture

This section explains how the gateway is put together and why the main
boundaries exist.

> 🧭 **Read this when changing the hot path.** The architecture pages keep
> request handling, config lookup, backend sessions, transports, and
> control-plane boundaries explicit.

| Page | What it covers |
| --- | --- |
| 🧭 [System Shape](system-shape.md) | The gateway's role in ContextForge, its crate layout, and the line between dataplane and control plane. |
| 🔀 [Request Flow](request-flow.md) | The ordered path from downstream HTTP request to backend MCP call and merged response. |
| 🔐 [Authentication And User Config Lookup](authentication-and-user-config.md) | How JWT claims, Redis-backed user config, and virtual host selection combine before routing. |
| 🗂️ [Runtime Configuration](runtime-configuration.md) | The current `UserConfig` model, MessagePack Redis persistence, cache behavior, and expected growth. |
| 🔌 [Backend Connections And Transports](backend-connections-and-transports.md) | Downstream listeners, upstream RMCP transports, config-store transport, and TLS direction. |
| 🧵 [Session Ownership](session-ownership.md) | Why backend services move in and out of the shared map, and what that means for load balancing. |
| 🧱 [Architectural Choices](architectural-choices.md) | The main tradeoffs behind dataplane scope, namespacing, config boundaries, and future protocols. |
