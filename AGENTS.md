# AGENTS.md

Guidance for agents working on `contextforge-data-plane`.

This repo is the Rust dataplane part of ContextForge. It must stay compatible
with the external ContextForge control plane in
`https://github.com/IBM/mcp-context-forge`, but it must not become a
control-plane, UI, IAM, or metrics-storage app.

## MCP Protocol Support

- The downstream dataplane contract targets modern MCP clients using protocol
  version `2026-07-28` over Streamable HTTP.
- Do not add new dataplane compatibility for older MCP protocol versions,
  legacy `initialize`/session behavior, or the legacy SSE transport. Replace
  remaining legacy paths with their `2026-07-28` equivalents as that migration
  proceeds.
- The external ContextForge control plane owns and serves legacy clients.
  Older MCP versions and SSE remain on control-plane routes and must not be
  routed through this dataplane.
- Tests, examples, and new protocol behavior should use `server/discover`,
  per-request client metadata, and the `2026-07-28` protocol version.
- Temporary compatibility shims in the current implementation are migration
  details, not supported client contracts. Do not build new behavior on them.

## Architecture

Architecture documentation lives in The ContextForge Data Plane Book under
[docs/book](docs/book/README.md). Read the relevant page before changing the
hot path:

| Page | Read it for |
| --- | --- |
| [What is ContextForge Data Plane?](docs/book/src/what-is-contextforge-data-plane.md) | Scope, boundaries, key terms, and the mental model. |
| [System Shape](docs/book/src/system-shape.md) | Crate layout, control-plane boundary, pipeline shape, state ownership, and module boundaries. |
| [Request Flow](docs/book/src/request-flow.md) | Startup, middleware order, initialize fanout, authorized calls, and the response path. |
| [Concurrency And Runtime Model](docs/book/src/concurrency-and-runtime.md) | Executor shapes, shared state and locks, fanout, and cancellation. |
| [Authentication And User Config Lookup](docs/book/src/authentication-and-user-config.md) | JWT validation, config keying, cache behavior, and failure responses. |
| [Security Model And Trust Boundaries](docs/book/src/security-model.md) | Trust boundaries, compromise impact, and transport security posture. |
| [Runtime Configuration](docs/book/src/runtime-configuration.md) | The `UserConfig` model, Redis/MessagePack persistence, and plugin runtime config. |
| [Control-Plane Integration](docs/book/src/control-plane-integration.md) | Redis keys, schemas, token shape, and route parity with the control plane. |
| [Backend Connections And Transports](docs/book/src/backend-connections-and-transports.md) | Downstream, upstream, and config-store transports plus TLS direction. |
| [Session Ownership](docs/book/src/session-ownership.md) | Backend session state, cleanup, and load-balancing constraints. |
| [MCP Routing Semantics](docs/book/src/mcp-routing-semantics.md) | The backend prefix namespace and routing contract. |
| [Architectural Choices](docs/book/src/architectural-choices.md) | Invariants and tradeoffs that must not change accidentally. |

The book is rendered from `docs/book/src/` and published through GitHub Pages;
see [docs/book/README.md](docs/book/README.md) for build and validation steps.

## Working Rules

- Most product behavior belongs in `contextforge-data-plane-lib`; avoid adding
  dataplane logic to the binary crate.
- Keep persistent config access behind `UserConfigStore`; do not push Redis
  details into routing code.
- Do not change the backend prefix naming contract without updating merge
  logic, split logic, and tests.
- This project is still early development with no external users; prefer the
  right architecture over preserving unstable APIs or compatibility surfaces.
- When behavior on the hot path changes, update the matching book page in the
  same change.

## Logging

- Use consistent formatted tracing messages for related events so logs are easy to grep across request paths.
- Prefer captured variables inside the message, for example `level!("method_name - event text field = {field} other_field = {other_field}")`, over structured field syntax for dataplane logs.
- Keep the method/event prefix stable and reuse the same field names/order for related events.
- Keep warning logs for unexpected conditions that likely need operator attention. Expected user/config misses should be debug or info unless they indicate a platform problem.
- Do not log tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig`, or backend credentials.
