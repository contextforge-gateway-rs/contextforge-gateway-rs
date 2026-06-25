# MCP Routing Semantics

The gateway presents multiple backend MCP servers as one downstream MCP server.
It does that by namespacing backend objects, fanning out list operations, and
routing exact calls back to the selected backend.

## Backend Prefixes

Backend names are part of the public namespace:

```text
backend tool "increment" from backend "gateway-one"
  -> "gateway-one-increment"

backend resource "counter" from backend "gateway-one"
  -> "gateway-one-counter"

backend prompt "summarize" from backend "research"
  -> "research-summarize"
```

The prefix is a routing contract, not a display detail. Renaming a backend
changes downstream tool, resource, and prompt names.

## List Operations

List operations fan out:

```text
list_tools      -> all connected backends -> merged sorted tools
list_resources  -> all connected backends -> merged sorted resources
list_prompts    -> all connected backends -> merged sorted prompts
```

Each successful backend result is rewritten with its backend prefix before the
merged response is returned. Failed or unavailable backends are logged and
omitted from the current merged list result.

## Routed Operations

Calls that target one object split the prefixed name:

```text
gateway-one-increment
  -> backend_name = gateway-one
  -> upstream tool name = increment
```

`call_tool`, `read_resource`, and `get_prompt` all share the same splitter.
Backend names can themselves contain `-` (as in `gateway-one`), so the splitter
does not cut on the first `-`. Instead it walks the configured backend names,
takes the first one the prefixed name starts with, and then requires a `-`
immediately after that name. That is why `gateway-one-increment` resolves to
backend `gateway-one` and tool `increment`, while a malformed name such as
`gateway-oneincrement` is rejected.

After the split, the gateway resolves exactly one connected backend service for
the principal and downstream session. Missing backends fail the call. Duplicate
matches are treated as invalid session state and trigger backend cleanup.

## Known Gaps

Pagination is not complete. `list_tools`, `list_resources`, and `list_prompts`
currently perform one backend call and return a merged response with no
downstream cursor. Full parity needs to gather all backend pages or define a
merged cursor strategy.

Streaming/SSE behavior is also still a tracked design area. The target is to
stream downstream as backend chunks arrive while preserving plugin behavior,
backpressure, cancellation, and telemetry attribution.
