# AGENTS.md

## Start with the wiki

At the start of each task, check `_context/wiki/index.md` to decide
whether wiki context is needed before acting. Don't read the wiki in
full. Use the index and follow links only when they are relevant to
the task.

## Update the wiki

After completing a task, offer to update the wiki if the task yielded durable knowledge that could benefit future work, then wait for user approval. This includes new processes, architecture decisions, or insights that go beyond the immediate task.

When adding a new wiki page, also update:
- `_context/wiki/index.md` — add a row to the pages table
- `_context/wiki/SUMMARY.md` — add the page under the appropriate section so it appears in the published book

---

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

## Working Rules

- Most product behavior belongs in `contextforge-data-plane-lib`; avoid adding
  dataplane logic to the binary crate.
- Keep persistent config access behind `UserConfigStore`; do not push Redis
  details into routing code.
- Do not change the backend prefix naming contract without updating merge
  logic, split logic, and tests.
- This project is still early development with no external users; prefer the
  right architecture over preserving unstable APIs or compatibility surfaces.
- When behavior on the hot path changes, update the matching wiki page in the
  same change.

## Logging

- Use consistent formatted tracing messages for related events so logs are easy to grep across request paths.
- Prefer captured variables inside the message, for example `level!("method_name - event text field = {field} other_field = {other_field}")`, over structured field syntax for dataplane logs.
- Keep the method/event prefix stable and reuse the same field names/order for related events.
- Keep warning logs for unexpected conditions that likely need operator attention. Expected user/config misses should be debug or info unless they indicate a platform problem.
- Do not log tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig`, or backend credentials.
