# MCP Method Reference

> 📋 **Reference lens:** this page lists what each MCP method does at the
> gateway today, from the client's point of view. For how identifiers are
> preserved, aliased, or namespaced, see [MCP Routing Semantics](mcp-routing-semantics.md).

> **Migration note:** the supported target uses MCP `2026-07-28`,
> `server/discover`, and per-request client context. The legacy `initialize`,
> session, and subscription paths below are implementation inventory to replace,
> not compatibility contracts.

Gateway methods fall into three groups: `initialize` creates backend sessions,
routed methods use them, and `ping` remains local to the gateway process.

## Initialize

| Aspect | Behavior |
| --- | --- |
| Required context | RMCP `DownstreamSessionId`, `UserConfig`, `VirtualHostId`, and `ContextForgeClaims`. The `Mcp-session-id` header is not required yet. |
| Fanout | One `StreamableHttpClientTransport` per configured backend in the selected virtual host, opened concurrently with `futures::future::join_all`. |
| Backend failure | Not fatal. A backend that fails to initialize is stored with no running service; list calls skip it and routed calls to it fail. |
| Stored state | The local user session mapping, plus one `BackendTransports` entry per backend keyed by principal, backend name, and downstream session id. |
| Result | `InitializeResult` with the gateway's merged capability set. A capability family is advertised when at least one initialized backend advertises it and the gateway has routing support for that family. Backend capabilities are also stored with transport state. |

## Routed List Methods

`list_tools`, `list_resources`, `list_prompts`, and `list_resource_templates`
share one fanout path:

| Aspect | Behavior |
| --- | --- |
| Fanout | Concurrent call to every connected backend in the session. |
| Identifiers | A single backend preserves upstream identifiers; multiple backends use the backend map-key prefix. Explicit control-plane tool aliases are returned exactly as configured. Resource templates apply the same single-versus-multi rule to both names and URI templates. |
| Ordering | Merged output is sorted by name. |
| Failures | Failed or unavailable backends are logged and omitted from the merged result. |
| Pagination | Cursor-based, per the [MCP spec §Pagination](https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/pagination). The gateway cursor is an opaque JSON token encoding each active backend's position. On the first request (no cursor) all backends are queried; on resume only backends with remaining pages are queried. An undecodable cursor returns `−32602 Invalid params`. See [Federated Pagination](mcp-routing-semantics.md#federated-pagination). |

## Routed Targeted Methods

Targeted calls resolve exactly one backend using an explicit tool alias, a
single-backend pass-through, or a multi-backend prefix:

| Method | Behavior |
| --- | --- |
| `call_tool` | Resolves an exact control-plane alias first, then falls back to the single-versus-multi rule. It runs the optional plugin hooks, tracks progress, forwards the backend-local tool name, and propagates downstream cancellation. |
| `read_resource` | Preserves a single-backend URI or strips a multi-backend prefix, then returns the selected backend's result. |
| `subscribe`, `unsubscribe` | Apply the same resource-URI routing, track the downstream subscription, and forward or stop forwarding matching resource-update notifications. |
| `get_prompt` | Preserves a single-backend prompt name or strips a multi-backend prefix, then returns the selected backend's result. |
| `complete` | Applies the same routing to the prompt name or resource URI in `ref` and returns the selected backend's completion result. |

Routed failures are JSON-RPC errors: an identifier that cannot select a backend
or an unavailable backend returns an internal error, and duplicate backend
matches invalidate the session; see [Failure Modes](failure-modes.md).

## Local Methods

This method passes through the same HTTP middleware but does not touch backends:

| Method | Current behavior |
| --- | --- |
| `ping` | Returns success. |

## Session Delete

A downstream `DELETE` with `Mcp-session-id` is handled by RMCP first. On a
successful response, `session_id_layer` removes the local user session mapping
and the `BackendTransports` entries for that principal and session id. See
[Session Ownership](session-ownership.md) for the cleanup rules.
