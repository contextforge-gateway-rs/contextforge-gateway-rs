# MCP Capability Allocation

This table assigns the
[MCP 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)
protocol surfaces across the ContextForge control plane, dataplane, upstream
backends, and MCP host.

The control plane owns descriptive and durable control state. The dataplane owns
targeted execution, validation, and streaming. Upstream backends remain
authoritative for backend-created durable state, while the MCP host owns user
interaction and MCP App rendering.

`Essential` identifies the initial scope. Advanced capabilities remain `TBD`;
their recommended allocation records the intended direction without committing
them to the initial implementation.

| MCP surface | Primary owner | Status | Recommended behavior |
| --- | --- | --- | --- |
| `server/discover` | Control plane | Essential | Produce the merged logical-server identity, instructions, capabilities, extensions, TTL, and cache scope. |
| `tools/list` | Control plane | Essential | Serve a materialized, policy-filtered catalog with stable names and composite pagination. Avoid synchronous fanout on every request. |
| `tools/call` | Dataplane | Essential | Route one target, validate input and output, execute plugins, stream progress, and propagate cancellation and multi-round-trip requests (MRTR). |
| `resources/list` | Control plane | Essential | Catalog resources, filter them by authorization, and merge them deterministically. |
| `resources/templates/list` | Control plane | Essential | Catalog URI templates and their metadata. |
| `resources/read` | Dataplane | Essential | Route and stream potentially large, dynamic, or private content on the hot path. |
| `prompts/list` | Control plane | Essential | Catalog and policy-filter prompt definitions. |
| `prompts/get` | Dataplane | Essential | Render prompts that may be dynamic, expensive, private, or require MRTR. |
| `completion/complete` | Dataplane | Essential | Route interactive, latency-sensitive completion requests using control-plane mappings. |
| `subscriptions/listen` | Dataplane | TBD | Maintain long-lived streams and multiplex upstream notifications. |
| List-change events | Control plane to dataplane | TBD | Generate normalized catalog invalidations in the control plane and deliver them through active dataplane subscriptions. |
| Resource subscriptions | Dataplane | TBD | Subscribe upstream and relay content-change notifications without durable subscription state. |
| Progress and cancellation | Dataplane | TBD | Keep progress on the originating response stream and immediately propagate cancellation upstream. |
| MRTR | Dataplane and MCP host | TBD | Transparently carry `InputRequiredResult`, `requestState`, and retry responses in the dataplane; perform user or model interaction in the host. |
| Tasks | Dataplane routing; backend state | TBD | Route `tasks/get`, `tasks/update`, and `tasks/cancel`; keep durable task state in the originating backend. The control plane owns enablement and policy. |
| OAuth and OIDC endpoints | Control plane | TBD | Own authorization-server discovery, login, consent, registration, token issuance, refresh, and step-up. |
| Access-token enforcement | Dataplane | TBD | Verify signature, expiry, audience, scope, and resource policy on every request. Never forward a downstream bearer token upstream. |
| MCP Apps metadata and assets | Control plane | TBD | Validate and version app manifests, UI assets, CSP, permissions, tool visibility, and origin-server associations. |
| MCP Apps asset delivery and calls | Dataplane | TBD | Serve `ui://` resources and route app-originated tool calls without executing UI HTML. |
| MCP Apps iframe and UI | MCP host | TBD | Own iframe sandboxing, CSP enforcement, `postMessage`, `ui/initialize`, permission prompts, and user consent. |
| Pagination | Control plane for list methods | TBD | Own opaque composite cursors and stable catalog snapshots. |
| Caching | Split | TBD | Cache discovery and lists in the control plane. Cache targeted resource reads in the dataplane only when authorization and `cacheScope` permit it. |
| Stdio adapters | Connector or control-plane tier | TBD | Keep process lifecycle and local adapter management out of the Rust dataplane. |
| Deprecated and legacy MCP | Control plane only | TBD | Do not add legacy sessions, SSE, Roots, Sampling, or Logging to the modern dataplane. |
