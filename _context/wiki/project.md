# Project Overview

## What this project is

`contextforge-gateway-rs` is a Rust-based MCP (Model Context Protocol) gateway — the **dataplane** component of ContextForge. It acts as a scalable, secure proxy layer that routes AI tool calls from MCP clients to one or more backend MCP servers.

It is paired with the external ContextForge control plane at [`IBM/mcp-context-forge`](https://github.com/IBM/mcp-context-forge). The two components have a strict division of responsibility:

| Layer | Owns |
| --- | --- |
| **This repo (dataplane)** | Request routing, auth enforcement, backend fan-out, session ownership |
| **Control plane** | IAM, UI, metrics storage, legacy MCP client compatibility |

The dataplane must never take on control-plane concerns.

## Goals and objectives

- Provide a **production-grade, low-latency routing layer** between MCP clients and backend MCP servers.
- Target **MCP protocol version `2026-07-28`** over Streamable HTTP as the sole downstream contract.
- Enforce a clean **dataplane/control-plane boundary** — no IAM, UI, or metrics storage logic in this repo.
- Keep config access behind the **`UserConfigStore` abstraction** (backed by Redis/MessagePack).
- Remain in the right architectural shape during early development, prioritising correctness over backward compatibility.

## Key stakeholders and users

- **Platform teams** — deploy and operate the gateway as infrastructure.
- **AI application developers** — use the gateway as the MCP proxy layer for their applications.
- **Internal contributors** — engineers evolving the dataplane toward the `2026-07-28` protocol target.

## Key modules and architecture

The architecture book at [`docs/book/src/`](../../docs/book/src/) is the authoritative reference. Key pages:

| Page | Covers |
| --- | --- |
| [`system-shape.md`](../../docs/book/src/system-shape.md) | Crate layout, pipeline shape, state ownership, module boundaries |
| [`request-flow.md`](../../docs/book/src/request-flow.md) | Startup, middleware order, fan-out, response path |
| [`mcp-routing-semantics.md`](../../docs/book/src/mcp-routing-semantics.md) | Backend prefix namespace and routing contract |
| [`authentication-and-user-config.md`](../../docs/book/src/authentication-and-user-config.md) | JWT validation, config keying, cache behavior |
| [`architectural-choices.md`](../../docs/book/src/architectural-choices.md) | Invariants and tradeoffs that must not change accidentally |

**Crate structure:**
- `contextforge-gateway-rs-lib` — all product/routing logic lives here.
- Binary crate — thin entrypoint only. Dataplane logic must not accumulate here.

**Key invariants:**
- Redis/config access goes through `UserConfigStore` only — never leak Redis details into routing code.
- The backend prefix naming contract must not change without updating merge logic, split logic, and tests.
- When behavior on the hot path changes, the matching book page must be updated in the same change.

## Active work (near-term)

- **Protocol migration**: replacing all remaining legacy MCP paths (SSE transport, `initialize`/session shims) with `2026-07-28` equivalents over Streamable HTTP.
- Legacy SSE transport and old session behavior are **being removed**, not maintained. Do not build new behavior on temporary shims.
- New tests and examples should use `server/discover`, per-request client metadata, and protocol version `2026-07-28`.

## System topology

All external traffic enters through **nginx**, which fans out to either the dataplane or the control plane:

```mermaid
flowchart LR
    client(["client"]) --> nginx["nginx"]
    nginx --> dataplane["data-plane"]
    nginx --> controlplane["control-plane"]
    dataplane --> redis["redis"]
    controlplane --> redis
    controlplane --> postgres["postgres\n(via pgbouncer)"]
    dataplane --> fastts["fast_time_server"]
```

### How the control plane publishes config to the dataplane

The control plane and dataplane do **not** communicate over HTTP. Config is exchanged exclusively through Redis:

1. The control plane runs **`dataplane_publisher.py`** — a publisher script that writes dataplane configuration (user config, backend definitions, etc.) into Redis.
2. The dataplane reads that config from Redis via the **`UserConfigStore`** abstraction (MessagePack-encoded `UserConfig`).

This means:
- The dataplane is a **pure reader** of Redis config. It never writes back to the control-plane's Redis keys.
- The control plane is the **sole writer** of dataplane config; the dataplane has no direct dependency on the control-plane process at runtime.
- Config changes from the control plane are picked up by the dataplane through normal cache refresh / Redis reads — no restart or direct RPC required.

### Per-component responsibilities

| Component | Role | Persistence |
| --- | --- | --- |
| **nginx** | TLS termination, routing fan-out | — |
| **dataplane** (`contextforge-gateway-rs`) | MCP routing, auth enforcement, fan-out to backends | Redis (read-only for config) |
| **control-plane** (`IBM/mcp-context-forge`) | IAM, UI, metrics, legacy MCP clients, config publishing | Redis (write) + PostgreSQL (via pgbouncer) |
| **redis** | Runtime config store, inter-component pub/sub channel | In-memory + persistence |
| **postgres** (via pgbouncer) | Control-plane relational store | Durable |
| **fast_time_server** | High-resolution time source used by the dataplane | — |

## External dependencies and integration points

- **Redis** — runtime config store (MessagePack-encoded `UserConfig`). Populated by `dataplane_publisher.py` on the control plane; read by the dataplane via `UserConfigStore`.
- **Control plane** (`IBM/mcp-context-forge`) — owns legacy MCP client routes and publishes dataplane config via `dataplane_publisher.py`. Does not route through this dataplane at runtime.
- **fast_time_server** — high-resolution time source consumed by the dataplane.
- **Tokio + Axum** — fixed async runtime and web framework.
