# MCP Routing Semantics

The gateway presents multiple backend MCP servers as one downstream MCP server.
It fans out list operations, preserves identifiers when only one backend is
configured, and adds a backend namespace when multiple backends need to be
disambiguated. Explicit tool aliases published by the control plane take
precedence over both forms.

## Backend Prefixes

Backend map keys are part of the public namespace only for multi-backend
virtual hosts without an explicit tool alias:

```text
backend tool "increment" from backend "gateway-one"
  -> "gateway-one-increment"

backend resource "counter" from backend "gateway-one"
  -> "gateway-one-counter"

backend prompt "summarize" from backend "research"
  -> "research-summarize"
```

For a single-backend virtual host, those identifiers remain `increment`,
`counter`, and `summarize`. Resource URIs and resource-template URI templates
follow the same rule. This keeps a transparent gateway from changing the MCP
contract when no collision is possible.

For tools, `BackendMCPGateway.tool_name_aliases` maps an exact downstream alias
to the upstream original name. An alias is advertised and routed exactly as
published, including case, dots, and underscores. When no alias exists,
single-backend hosts preserve the original tool name and multi-backend hosts
fall back to `{backend-map-key}-{tool-name}`.

When a prefix is required, it is a routing contract rather than a display
detail. Changing a backend map key changes downstream identifiers for that
multi-backend virtual host.

## List Operations

List operations fan out:

```text
list_tools               -> all connected backends -> merged sorted tools
list_resources           -> all connected backends -> merged sorted resources
list_prompts             -> all connected backends -> merged sorted prompts
list_resource_templates  -> all connected backends -> merged sorted templates
```

For a single backend, each successful result is returned with its identifier
unchanged, except for explicit tool aliases. For multiple backends, resources,
prompts, and tools without aliases are rewritten with their backend map-key
prefix. Resource-template names and URI templates are both prefixed. Failed or
unavailable backends are logged and omitted from the current merged list
result.

## Routed Operations

Calls that target one object use the inverse rule. A single-backend host selects
its only backend and forwards the identifier unchanged. A multi-backend host
splits the prefixed identifier:

```text
gateway-one-increment
  -> backend_name = gateway-one
  -> upstream tool name = increment
```

`read_resource`, `subscribe`, `unsubscribe`, `get_prompt`, and `complete` share
this conditional routing helper. For `complete`, the routed value is the prompt
name or resource URI inside its `ref`. Resource-update notifications apply the
same rule on the response path, so their URI matches the URI originally exposed
to the downstream client.

`call_tool` first resolves an exact control-plane alias, then applies the same
single-versus-multi-backend fallback. Backend names can themselves contain `-`
(as in `gateway-one`), so the splitter does not cut on the first `-`. Instead it
walks the configured backend names, takes the first one the prefixed name starts
with, and then requires a `-` immediately after that name. That is why
`gateway-one-increment` resolves to backend `gateway-one` and tool `increment`,
while a malformed name such as `gateway-oneincrement` is rejected.

After selecting a route, the gateway resolves exactly one connected backend
service for the principal and downstream session. Missing backends fail the
call. Duplicate matches are treated as invalid session state and trigger
backend cleanup.

## Known Gaps

Pagination is not complete. `list_tools`, `list_resources`, `list_prompts`,
and `list_resource_templates` currently perform one backend call and return a
merged response with no downstream cursor. Full parity needs to gather all
backend pages or define a merged cursor strategy.

Streaming/SSE behavior is also still a tracked design area. The target is to
stream downstream as backend chunks arrive while preserving plugin behavior,
backpressure, cancellation, and telemetry attribution.
