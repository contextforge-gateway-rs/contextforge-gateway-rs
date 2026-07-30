# Working Preferences and Standards

## Validation gate — definition of "done"

A change is not done until:
1. `cargo test` passes with no failures.
2. `cargo clippy` is clean — no new warnings.
3. If the change touches the hot path, the matching book page in [`docs/book/src/`](../../docs/book/src/) is updated in the same change.

## Code style

- **Idiomatic Rust** — no unnecessary clones, heap allocations, `Arc`, or `Mutex` unless justified by the design.
- Most product behavior lives in `contextforge-gateway-rs-lib`. Do not let dataplane logic accumulate in the binary crate.
- Typed errors — propagate errors rather than swallowing them silently.
- Keep change size minimal. Every changed line must trace directly to the task at hand.

## Logging (tracing)

- Use `tracing` for all log output.
- **Prefer message-embedded fields**: `level!("method_name - event field = {val} other_field = {other}")`.
  Do **not** use structured field syntax (`, field = val`) for dataplane logs.
- Keep method/event prefixes stable and reuse the same field names and order for related events.
- `warn!` is for unexpected conditions that need operator attention. Expected user/config misses → `debug!` or `info!`.
- **Never log**: tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig`, or backend credentials.

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
- **Update the book**: when hot-path behavior changes, include the book page update in the same task.
- **No hallucination**: if something is unclear, ask rather than guess.
