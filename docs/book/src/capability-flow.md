# Capability Flow

> **Migration note:** this page documents the current `initialize` capability
> path. The target downstream contract is MCP `2026-07-28`, where
> `server/discover` replaces this client-facing lifecycle.

This page explains where MCP capabilities come from during `initialize`, how
the gateway stores them, and what the downstream client sees today.

## Short Answer

Upstream capabilities come from each backend's own `InitializeResult`. The
gateway captures them after opening the RMCP client service for that backend,
stores them with the backend transport state, and then separately builds one
downstream `InitializeResult` for the client.

Today, the downstream response is gateway-defined. It is not a direct pass
through of one backend; it is a gateway-aware merge of backend capability
families the gateway can route.

## Initialize Sequence

`McpService::initialize` in
`crates/contextforge-gateway-rs-lib/src/gateway/mcp_service/initialization.rs`
runs this capability-related flow:

1. The gateway validates the call and resolves the selected virtual host.
2. It iterates over `virtual_host.backends` and starts one upstream RMCP client
   service per configured backend.
3. Each upstream service performs that backend's `initialize` handshake.
4. After the service is running, the gateway reads the backend-advertised
   capabilities from `rs.peer().peer_info().capabilities`.
5. It stores those capabilities inside `BackendTransportService` together with
   the running backend service.
6. It returns a separate downstream `InitializeResult` to the caller.

The code path that reads the upstream capabilities is:

```rust,ignore
let server_capabilities = running_service
    .as_ref()
    .and_then(|rs| rs.peer().peer_info().as_ref().map(|pi| pi.capabilities.clone()));
```

Those values are then stored here:

```rust,ignore
BackendTransportService::from((server_capabilities, running_service.map(Arc::new)))
```

## Where The Capabilities Come From

The source of truth is the upstream backend server. For example, a backend test
server can return:

```rust,ignore
InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
```

When the gateway connects to that backend, RMCP exposes the backend's peer
information and the gateway copies `peer_info().capabilities` into local
transport state.

## What The Client Sees Today

The downstream client does not receive a backend-specific capability object. The
gateway returns one virtual-server capability set from `merge_and_build_capabilities(...)`.

At the time of writing, that function behaves like this:

1. It checks each upstream capability family independently.
2. It advertises a downstream family when at least one backend advertises that
   family and the gateway supports routing it.
3. It preserves `resources.subscribe` when at least one backend advertises it,
   because the gateway routes subscribe/unsubscribe and forwards resource update
   notifications for active subscriptions.
4. It does not advertise `listChanged` sub-capabilities yet, because the gateway
   does not currently emit downstream list-changed notifications when upstream
   lists or control-plane allow maps change.
5. If no backend reports supported capabilities, it returns
   `ServerCapabilities::default()`.

This means the downstream capability response is a gateway policy over upstream
capabilities, not a literal copy of any one backend.

## Should This Use One Backend Or A Merge?

Do not initialize the downstream capability object from just one backend entry.

Reasons:

- The gateway fronts multiple backends but presents one downstream MCP server.
- `virtual_host.backends` is a `HashMap`, so a "first backend wins" choice is
  not a stable contract.
- List methods already merge results across backends, so selecting one backend's
  capabilities would under-report or over-report behavior depending on which
  backend happened to be chosen.

If the goal is to make the downstream capability response more accurate, prefer
merge semantics over single-backend selection.

## What Kind Of Merge?

The safe rule is not "copy one backend" and not even "blind union of every
field". The safe rule is:

Advertise only capabilities that the gateway can support correctly end-to-end
for the downstream client.

That usually means a gateway-aware merge such as:

- enable a top-level capability when at least one backend supports it and the
  gateway has a correct routing/merge story for that method family
- keep a capability disabled when the gateway cannot yet preserve the backend's
  semantics across multiple backends
- merge subfields only when their meaning still holds after namespacing,
  routing, filtering, and partial backend failure handling

Examples:

- `tools`, `prompts`, and `resources` fit the current gateway model because the
  gateway already merges list calls and routes targeted calls.
- More specific sub-capabilities should only be surfaced if the gateway really
  preserves their behavior in the aggregated downstream view.

## Recommendation

If you are deciding between these two approaches:

- "use one capability object from the vector"
- "merge backend capabilities into one downstream capability object"

choose merge.

But implement it as a gateway-supported merge, not as a raw backend union.
