# MCP Method Reference

> 📋 **Reference lens:** this page lists what each MCP method does at the
> gateway today, from the client's point of view. For how prefixed names are
> split and merged, see [MCP Routing Semantics](mcp-routing-semantics.md).

Gateway methods fall into three groups: `initialize` creates backend sessions,
routed methods use them, and a few methods remain local to the gateway process.

## Initialize

| Aspect | Behavior |
| --- | --- |
| Required context | RMCP `DownstreamSessionId`, `UserConfig`, `VirtualHostId`, and `ContextForgeClaims`. The `Mcp-session-id` header is not required yet. |
| Fanout | One `StreamableHttpClientTransport` per configured backend in the selected virtual host, opened concurrently with `futures::future::join_all`. |
| Backend failure | Not fatal. A backend that fails to initialize is stored with no running service; list calls skip it and routed calls to it fail. |
| Stored state | The local user session mapping, plus one `BackendTransports` entry per backend keyed by principal, backend name, and downstream session id. |
| Result | `InitializeResult` with the gateway's current fixed capability set: completions, prompts, resources, and tools enabled. Backend capabilities are stored with transport state but are not merged into the response yet. |

## Routed List Methods

`list_tools`, `list_resources`, `list_prompts`, and `list_resource_templates`
share one fanout path:

| Aspect | Behavior |
| --- | --- |
| Fanout | Concurrent call to every connected backend in the session. |
| Namespacing | Every returned name is prefixed with its backend name. Resource templates get both the template name and the URI template prefixed. |
| Ordering | Merged output is sorted by name. |
| Failures | Failed or unavailable backends are logged and omitted from the merged result. |
| Pagination | One backend call per request and no downstream cursor; see [Known Gaps](mcp-routing-semantics.md#known-gaps). |

## Routed Targeted Methods

`call_tool`, `read_resource`, `get_prompt`, and `complete` share the prefix
splitter and resolve exactly one backend:

| Method | Behavior |
| --- | --- |
| `call_tool` | Splits `{backend_name}-{tool_name}`, optionally runs the plugin pre hook, forwards the stripped tool name, tracks the downstream progress token, and optionally runs the plugin post hook on the result. Backend progress notifications for the tracked token are forwarded downstream, and a downstream cancellation is propagated to the backend call. |
| `read_resource` | Splits the prefixed resource name, strips the gateway prefix, and returns the single backend's result. |
| `get_prompt` | Splits the prefixed prompt name, strips the gateway prefix, and returns the single backend's result. |
| `complete` | Routes on the backend-prefixed prompt name or resource URI in `ref`, strips that prefix, and returns the selected backend's completion result. |

Routed failures are JSON-RPC errors: a malformed prefixed name or an
unavailable backend returns an internal error, and duplicate backend matches
invalidate the session; see [Failure Modes](failure-modes.md).

## Local Methods

These methods pass through the same HTTP middleware but do not touch backends:

| Method | Current behavior |
| --- | --- |
| `ping` | Returns success. |
| `subscribe`, `unsubscribe` | Mutate a local subscription set only. |

## Session Delete

A downstream `DELETE` with `Mcp-session-id` is handled by RMCP first. On a
successful response, `session_id_layer` removes the local user session mapping
and the `BackendTransports` entries for that principal and session id. See
[Session Ownership](session-ownership.md) for the cleanup rules.
