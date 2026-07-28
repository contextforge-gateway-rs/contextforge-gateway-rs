# MCP Behavior

This section describes what the gateway exposes as an MCP server and how it
maps downstream MCP calls onto configured backend MCP servers.

## Protocol Support Direction

The downstream dataplane contract is MCP `2026-07-28` over Streamable HTTP.
Modern clients use `server/discover` and provide the required client context on
each request.

Older MCP versions, legacy session initialization, and the legacy SSE transport
are not target dataplane compatibility surfaces. The external ContextForge
control plane serves those clients on its own routes. Legacy traffic does not
enter the Rust dataplane.

The implementation is still being migrated to this boundary. Pages that
describe `initialize`, `Mcp-Session-Id`, or local session state document
temporary current internals, not a client contract to preserve. New code,
tests, and examples should use MCP `2026-07-28`; remaining legacy paths should
be replaced or removed rather than extended.

> 📋 **Use this section for protocol behavior.** It covers the public MCP
> surface, backend fanout, namespacing, targeted routing, pagination, and
> the modern Streamable HTTP path.

| Page | What it covers |
| --- | --- |
| 📋 [MCP Method Reference](mcp-method-reference.md) | Initialize, list, call, read, and prompt behavior from the client-facing gateway point of view. |
| 🛣️ [MCP Routing Semantics](mcp-routing-semantics.md) | How backend prefixes become the public tool, resource, and prompt namespace. |
