# ContextForge External Dataplane — Wiki

This wiki captures durable project context and working preferences.
Check this index at the start of a task to decide whether deeper context is needed,
then follow only the links that are relevant.

## Pages

| File | What it covers |
| --- | --- |
| [getting-started.md](getting-started.md) | Full docker stack, local cargo dev, cf-integration — commands and URIs |
| [project.md](project.md) | What the project is, goals, stakeholders, key modules, crate ownership, active work |
| [preferences.md](preferences.md) | Working standards, code style, logging rules, branch naming, AI interaction preferences |
| [architecture.md](architecture.md) | Current middleware stack order, pipeline shape, module boundaries, state ownership, executor shapes |
| [routing.md](routing.md) | Stateless routing model: VirtualHost routing tables, per-request backend lifecycle, method quick reference, header forwarding, plugin hooks |
| [mcp-capability-allocation.md](mcp-capability-allocation.md) | Tentative ContextForge 2.0 target topology, ownership, state model, Phase 1-4 roadmap, and Phase 3 flows |
| [failure-modes.md](failure-modes.md) | HTTP/MCP/routing/backend/plugin failure table — exact HTTP codes and JSON-RPC errors |
| [config.md](config.md) | Key CLI flags, JWT claims, UserConfig shape, plugin config, telemetry debugging, startup validation, local observability stack |
| [deployment.md](deployment.md) | External-dataplane deployment checklist, health endpoint caveat, nginx routing, TLS choices, session affinity, Redis availability, image pinning |
| [security.md](security.md) | Trust boundaries among the control plane, built-in dataplane, and external dataplane; Origin/Host validation; transport security; secrets handling |
| [performance.md](performance.md) | Control-plane Locust load runs, benchmark settings, and built-in-dataplane baseline |
| [testing.md](testing.md) | Workspace checks, in-repo integration tests, full-stack harness lanes, settings, and control-plane baseline |

## Quick orientation

- **Repo**: `contextforge-data-plane` — the Rust ContextForge external dataplane.
- **Core invariant**: the ContextForge external dataplane is pure routing logic. No IAM, UI, or metrics storage.
- **Protocol target**: the ContextForge external dataplane supports MCP `2026-07-28` and `2025-11-25` over Streamable HTTP. Both use stateless request handling; `initialize` remains supported but does not establish external-dataplane session state. Legacy SSE paths are being removed from the external dataplane.
- **Status convention**: project, architecture, routing, and operations pages
  describe the current implementation. The page under **Upcoming** describes
  the tentative ContextForge 2.0 target and migration roadmap.
- **Architecture context**: [architecture.md](architecture.md) — read before touching the hot path. Full wiki index above.
- **Validation gate**: `cargo fmt` + `cargo clippy` + `cargo nextest` + `cargo deny` must be clean; CI also runs `cargo shear`. See [preferences.md](preferences.md) for by-change-type requirements.
- **System topology**: `client → nginx → [ContextForge built-in dataplane | ContextForge external dataplane | ContextForge control plane]`; external-dataplane config flows from the control plane via `dataplane_publisher.py` → Redis → external dataplane. See [project.md § System topology](project.md#system-topology).
