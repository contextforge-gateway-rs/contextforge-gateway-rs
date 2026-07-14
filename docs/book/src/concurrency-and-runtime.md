# Concurrency And Runtime Model

> 🧵 **Execution lens:** the gateway is async Rust on Tokio with jemalloc as
> the global allocator. This page explains the two executor shapes, what
> state is shared under which locks, and where work fans out.

## Executor Shapes

`--single-runtime` selects between two models:

| Mode | Shape | When |
| --- | --- | --- |
| `true` (default) | One multi-thread Tokio runtime with `--number-of-cpus` worker threads (default: host CPU count). All connections share one runtime and one set of gateway state. | The default for all stateful MCP traffic. |
| `false` | One OS thread per CPU, each running its own current-thread Tokio runtime, each executing the full gateway stack. Listeners bind with `SO_REUSEPORT`, so the kernel spreads incoming connections across the per-thread listeners. | A shared-nothing, per-core experiment shape for throughput work. |

In multi-runtime mode, the first thread initializes the optional CPEX plugin
runtime before the others start; the current-thread builders are tuned with a
global queue interval of `1024` and `4` I/O events per tick.

> ⚠️ **Multi-runtime consequence:** each runtime thread builds its own
> `BackendTransports` map and user-session store inside `run_gateway`.
> Backend session state is therefore per-runtime-thread, and `SO_REUSEPORT`
> gives no connection affinity — later requests in a streamable HTTP session
> arrive on new connections and can land on a thread that does not own the
> session. Treat single-runtime mode as the only mode that supports stateful
> MCP sessions today; this is the in-process version of the
> [load-balancing constraint](session-ownership.md#load-balancing-consequence).

## Shared State And Locks

| State | Lock | Contention profile |
| --- | --- | --- |
| `BackendTransports` map | `Arc<tokio::sync::Mutex<HashMap<...>>>` | Locked briefly on initialize insert, per-call borrow, and cleanup. Borrowing clones `Arc<RunningService>` handles so the lock is not held across backend calls. |
| Subscription set | `Arc<tokio::sync::Mutex<HashSet<String>>>` | Local `subscribe`/`unsubscribe` only. |
| User config LRU cache | `Arc<tokio::sync::Mutex<LruCache>>` inside `RedisUserConfigStore` | One lock per config lookup on the hot path; misses add a Redis round trip. |
| User session LRU cache | Same pattern in `LocalUserSessionStore` | Initialize and delete paths. |
| JWT decoders, upstream `reqwest::Client`, process `Config` | No lock — immutable after startup, shared by `Arc`/clone. | None. |

The design rule: locks guard maps of handles, not I/O. Backend calls, Redis
reads, and plugin hooks all run outside any gateway lock.

## Fanout And Cancellation

- `initialize` opens one backend transport per configured backend
  concurrently (`futures::future::join_all`); a failed backend degrades that
  backend only.
- List methods fan out to all connected backends concurrently and merge.
- Targeted calls resolve exactly one backend service handle.
- `call_tool` watches the downstream cancellation token and forwards a cancel
  to the backend if the client gives up first; backend progress notifications
  are forwarded downstream while the call is in flight.

## Listener Behavior

The TCP listener binds with `reuseaddr`, `reuseport`, and keepalive, listens
with a backlog of `1024`, and serves Axum with graceful shutdown on `ctrl_c`.
The TLS listener accepts by hand through Rustls and serves the same router
via Hyper.

## Allocator

The binary sets `tikv_jemallocator` as the global allocator, which holds up
better than the system allocator under the many small, short-lived
allocations of per-request JSON and header processing.
