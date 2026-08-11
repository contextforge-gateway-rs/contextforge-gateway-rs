# Working Preferences and Standards

## Validation gate — definition of "done"

A change is not done until:
1. `cargo fmt --all --check` passes.
2. `cargo clippy --locked --workspace --all-targets -- -D warnings` is clean.
3. `cargo nextest run --locked --workspace` passes (fallback: `cargo test`).
4. `cargo deny check advisories licenses` passes (pre-commit + CI).
5. `cargo build --locked --workspace` succeeds.
6. If the change touches the hot path, update the matching wiki page in `_context/wiki/` in the same change.

CI additionally runs `cargo shear --check-test-targets --deny-warnings --locked`.

**By change type:**

| Change type | Minimum extra validation |
| --- | --- |
| Docs only | Run `mdbook build _context/wiki` and `mdbook test _context/wiki`; inspect affected headings, tables, and code blocks in the rendered output |
| Routing or session behavior | New/updated integration tests in `crates/contextforge-data-plane-lib/tests/` against mock backends |
| Config shape | Schema regeneration (`cargo run -p contextforge-data-plane-apis`) + control-plane compatibility check |
| Plugin behavior | `gateway_plugins.rs` coverage for the new hook path |
| Performance-sensitive paths | Load-test run before and after |

## Code style

- **Idiomatic Rust** — no unnecessary clones, heap allocations, `Arc`, or `Mutex` unless justified by the design.
- Most product behavior lives in `contextforge-data-plane-lib`. Do not let dataplane logic accumulate in the binary crate.
- Typed errors — propagate errors rather than swallowing them silently.
- Keep change size minimal. Every changed line must trace directly to the task at hand.

## Logging (tracing)

- Use `tracing` for all log output.
- Emit queryable fields with structured syntax:
  `level!(component = "Routing", operation = "call_tool", backend_name, "backend tool call completed")`.
- Console and rolling-file output are newline-delimited JSON. The formatter adds the common contract:
  `timestamp`, `service_name`, `version`, `environment`, `cluster_id`, `transaction_id`,
  `correlation_id`, `trace_id`, `span_id`, `user_id`, `log_level`, `error_code`, `message`, and `component`.
- The shared implementation lives in `contextforge-data-plane-observability`; applications explicitly initialize it.
- Request, Redis, and backend-call latency events use `event_type=PERFORMANCE` with `metric`, `outcome`, and
  `latency_ms` fields.
- `error!` and fatal events must add a stable `CFDP-*` error code, root cause, impact scope, and retryability.
  Add `http_status` and `stack_trace` when available; the formatter supplies explicit null/default values otherwise.
- Use short stable messages and stable field names. Put variable data in fields, not message text.
- `warn!` is for unexpected conditions that need operator attention. Expected user/config misses → `debug!` or `info!`.
- **Never log**: tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig`, backend credentials,
  raw user subjects, session IDs, resource URIs, progress tokens, request/response bodies, or prompt content.

## Change discipline

- Make the **minimal change** that solves the problem. No speculative refactors, no added abstractions beyond the task scope.
- Do not clean up surrounding code that is unrelated to the task.
- Do not add error handling for scenarios that cannot happen.
- Always **read relevant code before suggesting or making changes**. Never speculate about code that hasn't been opened.

## Architectural rules (non-negotiable)

- The dataplane is pure routing logic. **No IAM, UI, or metrics-storage concerns.**
- Config access goes through `UserConfigStore` only — never push Redis details into routing code.
- The backend prefix naming contract must not change without updating merge logic, split logic, and tests.
- Legacy SSE transport and old `initialize`/session behavior are being **removed** — do not build on temporary shims.
- Prefer the right architecture over backward compatibility; this project has no external users yet.

## Protocol target

- All new behavior targets MCP protocol version **`2026-07-28`** over **Streamable HTTP**.
- New tests and examples use `server/discover`, per-request client metadata, and the `2026-07-28` version.
- Do not add new compatibility for older MCP protocol versions.

## AI interaction preferences

- **Read before acting**: always investigate relevant files before making suggestions or edits.
- **Minimal scope**: stay tightly scoped to the task — no unsolicited refactors or cleanups.
- **Plan first for complex tasks**: for changes with multiple moving parts, propose the approach before implementing.
- **Run validation**: run `cargo test` and `cargo clippy` after changes and report results before declaring done.
- **Update the wiki**: when hot-path behavior changes, include the wiki page update in the same task.
- **No hallucination**: if something is unclear, ask rather than guess.


## Branch naming

Format: `user/<github-username>/<kebab-case-summary>` — e.g. `user/alice/fix-session-cleanup`.
Open PRs as draft; mark ready only when implementation, tests, and wiki updates are complete.
