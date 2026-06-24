# Getting Started

This section is for the first working run of the gateway: start the local
dependencies, launch the Rust dataplane, seed runtime config, and send real MCP
traffic through it.

> 🚀 **Goal:** get from a clean checkout to one client-facing MCP endpoint at
> `/contextforge-rs/servers/{virtual_host_id}/mcp`.

| Page | What it covers |
| --- | --- |
| 🚀 [Run the Gateway Locally](running-the-gateway.md) | Docker services, local bootstrap helpers, user config, MCP session setup, and smoke tests. |
| ⚙️ [Configuration Reference](gateway-options.md) | Listener settings, Redis wiring, JWT verification, upstream transport, telemetry, logging, and runtime knobs. |

## What "started" means

A useful local gateway run has these pieces:

| Piece | Why it matters |
| --- | --- |
| Redis | Holds runtime `UserConfig` keyed by JWT subject. |
| Backend MCP servers | Provide the tools, resources, and prompts the gateway merges. |
| Gateway listener | Accepts downstream streamable HTTP MCP traffic. |
| JWT verification key or secret | Lets the gateway authenticate downstream requests. |
| User config | Maps the caller's JWT subject to virtual hosts and backend MCP URLs. |
| MCP session | Binds downstream session state to backend MCP client sessions. |

If one of those is missing, the gateway should fail at the boundary that owns
that fact: authentication, config lookup, virtual host resolution, session
lookup, routing, or upstream transport.
