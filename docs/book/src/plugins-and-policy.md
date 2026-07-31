# Plugins And Policy

Plugins are a policy boundary, not just middleware. They can inspect and mutate
payloads, so the gateway keeps the supported hook surface narrow and explicit.

## Runtime Enablement

Runtime plugins are disabled by default. When enabled, the binary creates a
CPEX runtime registry and the runtime initializes it before serving traffic.

Plugin configuration is loaded from Redis at:

```text
ContextForgeGatewayRuntimePluginConfig
```

The runtime registry builds an initialized immutable plugin manager from that
configuration. Reloading swaps the manager instead of mutating a live one.

## Supported Hooks

The supported surface is deliberately narrow:

```text
cmf.tool_pre_invoke
cmf.tool_post_invoke
cmf.prompt_pre_fetch
cmf.prompt_post_fetch
cmf.resource_pre_fetch
cmf.resource_post_fetch
```

The gateway rejects route-based plugin selection, plugin directories, global
policies/defaults, LLM hooks, and plugin conditions. Those features need clear
behavior for streaming, failures, timeouts, backpressure, context propagation,
and observability before they belong on the hot path.

Each MCP path checks only its own hook pair, so a plugin config that declares
only prompt hooks leaves the tool hot path untouched, and vice versa.

## Covered MCP Paths

| MCP path | Hook family | Pre hook | Post hook |
| --- | --- | --- | --- |
| `tools/call` | tool | deny, replace arguments | deny, rewrite result |
| `prompts/get` | prompt | deny, replace arguments | deny, rewrite message text |
| `resources/read` | resource | deny, rewrite URI | deny, rewrite text contents |
| `resources/subscribe` | resource | deny, rewrite URI | none |
| `resources/unsubscribe` | resource | deny, rewrite URI | none |
| `completion/complete` | prompt or resource | deny | deny, rewrite values |
| `prompts/list` | prompt | deny | deny, read-only |
| `resources/list` | resource | deny | deny, read-only |
| `resources/templates/list` | resource | deny | deny, read-only |

Every MCP method the gateway routes now has hook coverage.

CPEX defines no completion hook, so `completion/complete` runs the hooks of
whatever is being completed: a prompt reference uses the prompt hooks, a
resource or template reference uses the resource hooks.

## Hook Ordering

For routed methods — `tools/call`, `prompts/get`, `resources/read`,
`resources/subscribe`, `resources/unsubscribe`, and `completion/complete` — the
pre hook runs **after** backend routing has selected the backend and stripped the
public prefix. The hook sees the backend-local identifier and the backend name
separately, so plugins never have to understand the gateway's namespace scheme.

For fan-out methods — `prompts/list`, `resources/list`, and
`resources/templates/list` — the pre
hook runs **once before fan-out**, so a denial costs no backend traffic. The
post hook runs **once on the merged, namespaced page**, so plugins see exactly
what the client will receive. Mutating per backend before the merge would let a
plugin break the routing contract.

These methods are paginated, so the post hook observes one page per request, not
the complete set. A plugin that needs to reason across the whole listing must
accumulate across calls using CPEX context; the pre hook receives the incoming
cursor so pages can be correlated.

A denial becomes an MCP error and the backend is never called. Hook state is
carried across the upstream call so pre and post hooks share CPEX context for
the same logical request.

## Payload Shapes

| MCP path | Pre payload | Post payload |
| --- | --- | --- |
| `tools/call` | `ToolCall`, backend in `namespace` | `ToolResult` with the serialized MCP result |
| `prompts/get` | `PromptRequest`, backend in `server_id` | `PromptResult` with mapped messages |
| `prompts/list` | `PromptRequest`, **empty name**, no `server_id` | one `PromptRequest` per prompt, descriptions as text |
| `resources/templates/list` | `ResourceRef`, **empty URI** | one `ResourceRef` per template, descriptions as text |
| `resources/list` | `ResourceRef`, **empty URI** | one `ResourceRef` per resource, descriptions as text |
| `resources/read` | `Resource`, backend in `annotations.backend` | one `Resource` per content item |
| `resources/subscribe`, `resources/unsubscribe` | `Resource`, backend in `annotations.backend` | — |
| `completion/complete` (prompt ref) | `PromptRequest`, argument merged into arguments | `PromptResult` plus one text part per value |
| `completion/complete` (resource ref) | `Resource`, argument in `annotations.completion_argument` | `Resource` plus one text part per value |

CMF resource types have no backend field of their own, unlike `ToolCall` and
`PromptRequest`, so resource paths carry the backend in `Resource.annotations`
under the `backend` key.

## Telling Payloads Apart

A plugin that registers one handler for both hooks of a family needs to know
what it is looking at. Three rules cover it:

- **Pre versus post.** Prompt and resource payloads use CMF role `User` on the
  request side and `Assistant` on the response side. Tool payloads use
  `Assistant` for the call and `Tool` for the result. Do not discriminate on
  content part alone: a `prompts/list` post payload is built from
  `PromptRequest` parts, the same part a pre payload uses.
- **Listing versus routed.** Listings carry an empty identifier — an empty
  prompt name or an empty resource URI — and no backend.
- **Which MCP path.** The CPEX correlation id is prefixed by originating path:

  ```text
  gateway-tool-call-N               gateway-prompt-request-N
  gateway-prompt-list-N             gateway-resource-template-list-N
  gateway-resource-subscription-N   gateway-completion-N
  gateway-resource-read-N           gateway-resource-list-N
  ```

  This is how a prompt-reference completion is told apart from a `prompts/get`,
  which otherwise produce identically shaped payloads.

## Mutation Limits

Mutation is supported only where the gateway can apply a change without
silently dropping data:

- **Prompt messages** write back text content only. The message count must
  match what the plugin was given. `description`, `_meta`, message roles, and
  non-text blocks always come from the backend response, so a plugin that only
  observes gets a byte-identical passthrough.
- **Removing exposed text fails closed.** A text block is always exposed as a
  text part, so its absence in the returned payload means the plugin deleted it.
  The gateway rejects that with `INVALID_PARAMS` rather than restoring the
  backend's text, which would return content a redaction plugin stripped.
  Explicit deletion semantics are not supported yet; rewrite to an empty string
  instead.
- **Completion values** write back in full, but the value count must match, so
  the backend's `total` and `has_more` stay accurate.
- **Listings are read-only.** MCP `Prompt` and `ResourceTemplate` are metadata —
  `title`, `arguments`, `mime_type`, `icons`, `annotations`, `_meta` — with no
  CMF equivalent to rebuild from. A modified listing payload is ignored rather
  than partially applied. Changing which items are exposed is filtering, which
  belongs to the control plane.
- **Resource contents** write back text only, with a matching content count. Blob
  contents are exposed as a reference — URI and MIME type — but their bytes are
  withheld and never modified: MCP carries blobs base64-encoded while CMF wants
  raw bytes, and cloning arbitrary binary payloads through the plugin pipeline
  on the hot path is a memory hazard.
- **Subscription URIs** may be rewritten by the pre hook. The mapping from the
  client's URI to the rewritten upstream URI is recorded at subscribe time and
  reused afterwards — it is never recomputed. `unsubscribe` still runs its pre
  hook so a plugin can deny it, but addresses the backend with the stored URI,
  and resource-update notifications are reported to the client under the URI it
  subscribed to. Recomputing would misroute both whenever a plugin's rewrite is
  stateful or non-deterministic.
- **Completion arguments are read-only.** The argument value is a partial user
  keystroke, so rewriting it would return completions for text the user never
  typed.

A structural change the gateway cannot apply — a changed message count, text
aimed at a non-text message, a changed completion value count — is an
`INVALID_PARAMS` error rather than a partial write.

## Boundary Rules

Plugin execution must not poison shared gateway state. A plugin denial becomes
an MCP error, carrying the plugin's `proto_error_code` when it set one and
`INVALID_REQUEST` otherwise. Soft plugin errors are logged. Unsupported plugin
configuration fails validation before the runtime is accepted.

An in-flight request is pinned to the runtime that ran its pre hook, so a
configuration reload mid-request cannot hand the post hook to a different plugin
set.

Future hook expansion should define behavior for streaming/SSE, cancellation,
timeouts, backpressure, and telemetry before adding new hook points. One gap is
known and deliberate: resource-update notifications have no post-hook equivalent
to the tool path's streamed-event hook.
