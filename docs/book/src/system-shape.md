# System Shape

ContextForge has a broad control-plane surface. That surface is good at
management workflows: users, teams, configuration, credentials, policies,
catalogs, and administrative APIs. The Rust gateway has a narrower job. It sits
on the traffic path between MCP clients and MCP servers, so its design is biased
toward predictable latency, limited allocation, explicit ownership, and a small
set of state transitions.

The gateway consumes the runtime result of control-plane workflows. It should
not become the place where those workflows live.

## Control Plane Boundary

The external ContextForge control plane owns:

- management APIs and UI
- user and administrator workflows
- credential and policy authoring
- persistent configuration ownership
- durable observability storage
- customer IAM lifecycle

The Rust dataplane owns:

- downstream listener setup
- JWT validation and request context extraction
- runtime config lookup
- MCP session fanout and routing
- request and response policy hooks
- upstream transport setup
- request-path telemetry emission

This split lets the front door route only MCP traffic to the Rust service while
leaving the rest of ContextForge on the existing application:

```text
/contextforge-rs/servers/{virtual_host_id}/mcp
```

From a client point of view, the endpoint behaves like one ContextForge MCP
server. Internally, it is a focused proxy and fanout dataplane.

## Workspace Layers

The repository is a Cargo workspace with these main responsibilities:

```text
crates/contextforge-gateway-rs
  process shell: CLI config, logging, runtime setup, dependency assembly

crates/contextforge-gateway-rs-lib
  dataplane library: Axum stack, middleware, config lookup, MCP routing,
  sessions, upstream clients, downstream transports

crates/contextforge-gateway-rs-apis
  shared contract: user config model and schema generation

crates/contextforge-gateway-rs-cpex
  plugin integration: supported CPEX hook config and tool payload adaptation

crates/contextforge-load-test
  performance harness: end-to-end MCP traffic driver
```

The binary crate is intentionally thin. `main.rs` parses `Config`, initializes
logging, builds the Tokio runtime shape, optionally initializes the plugin
runtime registry, and constructs `Gateway`. Product behavior belongs in
`contextforge-gateway-rs-lib`.

## Pipeline Shape

The intended gateway shape is a bidirectional pipeline:

```text
downstream request
  -> authentication and authorization
  -> runtime config lookup
  -> session extraction
  -> virtual host selection
  -> request plugin hooks
  -> backend MCP call

upstream response
  -> response plugin hooks
  -> merge, namespace, or pass through
  -> metrics, tracing, and logging
  -> downstream response
```

The current code implements the MCP subset of that shape. Authentication
happens before configuration lookup. Configuration lookup happens before backend
selection. Plugins run around tool calls where payload access is currently
supported. Telemetry wraps the HTTP surface with `tower_http` tracing and
`axum-otel-metrics`.

## Reusable Shell

The gateway should keep protocol-neutral concerns reusable. Authentication,
configuration ingestion, TLS handling, plugin execution, telemetry, and session
strategy should not become MCP-only ideas unless the protocol requires it.
Future A2A or model-provider routing should be able to reuse the same shell
instead of growing a parallel stack.
