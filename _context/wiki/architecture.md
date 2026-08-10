# Architecture

## Middleware Stack Order

Tower layers execute outside-in. A request reaches MCP handlers with these extensions already set:

```
TCP/TLS listener
  -> HttpMetricsLayer
  -> TraceLayer
  -> /contextforge-rs nested router
  -> CORS layer
  -> virtual_host_id_layer       → inserts VirtualHostId           (400 on path mismatch)
  -> claims_layer                → inserts ContextForgeClaims      (401 on bad/missing JWT)
  -> session_id_layer            → inserts SessionId if present
  -> user_config_store_layer     → inserts UserConfig              (400 no config, 500 store error)
  -> virtual_host_config_layer   → rejects unknown vhost           (404 "Server not found")
  -> /servers/{virtual_host_name}/mcp RMCP service
```

MCP handlers read typed extensions — they never parse headers, paths, or Redis keys directly.

## Pipeline Shape

```
downstream request
  -> virtual host extraction → JWT validation → session extraction
  -> user config lookup → MCP handler validation
  -> request plugin hooks
  -> backend MCP call (concurrent via join_all for initialize/list)

upstream response
  -> response plugin hooks → merge/namespace/passthrough
  -> metrics, tracing, logging → downstream response
```

Order is invariant: auth/config before backend selection; request plugins before upstream; response plugins before returning.

## Module Boundaries (`contextforge-data-plane-lib`)

| Module | Owns |
| --- | --- |
| `common.rs` | CLI config shape, JWT claims, Redis config validation, `reqwest::Client` construction |
| `layers/` | HTTP request extension extraction, request-bound validation |
| `gateway/` | MCP server behavior, initialize fanout, list merging, prefixed routing, backend service state |
| `gateway/session_store/` | Local and Redis user session storage |
| `user_config_store/` | `UserConfigStore` trait, Redis-backed store |
| `transports/` | Downstream TCP and TLS listener setup |
| `tools.rs` | Local bootstrap helpers (`with_tools` feature only) |

## State Ownership

| State | Owner | Lifetime |
| --- | --- | --- |
| CLI `Config` | Binary startup + `Gateway` | Process |
| JWT decoders | `ContextForgeDataPlaneAppState` | Process |
| User config | `RedisUserConfigStore` (LRU + Redis) | Request-path consumed; control-plane authored |
| Request identity / VirtualHostId | Request extensions | One HTTP request |
| Downstream session id | RMCP + `SessionId` extension | MCP session |
| Backend RMCP services | `BackendTransports` map | Local process, per principal/backend/session |
| Local user session mapping | `LocalUserSessionStore` | Local LRU, 50k entries, 1 hour |
| Plugin manager | `CpexRuntimeRegistry` | Process, reloadable |

> **Session rule:** backend MCP services are local process state. Sticky routing required for load-balanced deployments.

## Executor Shapes

| `--single-runtime` | Shape |
| --- | --- |
| `true` (default) | One multi-thread Tokio runtime, `--number-of-cpus` workers. All connections share one `BackendTransports`. |
| `false` | One OS thread per CPU, each with its own current-thread Tokio runtime and own `BackendTransports`. `SO_REUSEPORT` spreads connections — no session affinity. **Stateful MCP sessions need `--single-runtime true`**. |

## Lock Design

Locks guard maps of handles, not I/O. Backend calls, Redis reads, and plugin hooks run outside any gateway lock. `borrow_transports()` clones `Arc<RunningService>` so the lock is not held across calls.
