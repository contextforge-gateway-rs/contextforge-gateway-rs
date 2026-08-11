# AGENTS.md

## Wiki usage

- Start each task at `_context/wiki/index.md`; read only relevant pages.
- Write for agents: terse, factual, compact, and optimized for retrieval.
  Preserve exact contracts, invariants, commands, failures, and current limits;
  omit filler, repetition, promotional prose, and generic explanations.
- Update existing pages instead of duplicating knowledge; remove stale guidance.
- Update the matching wiki page with every hot-path behavior change.
- For other durable knowledge, offer a wiki update and wait for approval.

When adding a page, update both navigation files:

- `_context/wiki/index.md` — add a row to the pages table
- `_context/wiki/SUMMARY.md` — add the page under the appropriate section

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
