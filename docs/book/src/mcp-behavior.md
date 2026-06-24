# MCP Behavior

This section describes what the gateway exposes as an MCP server and how it
maps downstream MCP calls onto configured backend MCP servers.

> 📋 **Use this section for protocol behavior.** It covers the public MCP
> surface, backend fanout, namespacing, targeted routing, pagination, and
> streaming gaps.

| Page | What it covers |
| --- | --- |
| 📋 [MCP Method Reference](mcp-method-reference.md) | Initialize, list, call, read, and prompt behavior from the client-facing gateway point of view. |
| 🛣️ [MCP Routing Semantics](mcp-routing-semantics.md) | How backend prefixes become the public tool, resource, and prompt namespace. |
