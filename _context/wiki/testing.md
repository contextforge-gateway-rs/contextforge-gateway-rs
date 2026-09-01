# Testing

## Verification Rings

- **Workspace checks** — code compiles and unit behavior holds.
- **In-repo integration tests** — MCP routing against mock backends.
- **`cf-integration` harness** — full control-plane publication and external-dataplane request path end to end.
- **Load and benchmark** — see [Performance](performance.md).

## Workspace Validation

CI runs these on every change; run them locally before pushing:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```

Use `cargo test` when nextest is unavailable. For wiki changes, also run `mdbook build _context/wiki` and `mdbook test _context/wiki`.

Protocol-sensitive tests and fixtures must cover MCP `2026-07-28` and `2025-11-25` in all four incoming-client/selected-backend combinations. The same-version paths are supported directly; the two cross-version paths are best effort and tests must cover both successful adaptation and explicit failure for semantics that cannot be translated without state. Every case must prove request independence: no required `Mcp-Session-Id`, session affinity, or retained backend transport. Keep `2026-07-28` coverage for `server/discover` and required per-request client metadata, and retain `initialize` coverage as a stateless compatibility request. SSE remains outside the external-dataplane contract.

## In-Repo Integration Tests

`crates/contextforge-data-plane-lib/tests/` exercises the gateway against in-process mock MCP backends (shared helpers live in `tests/support/`):

| Test file | Covers |
| --- | --- |
| `gateway_list_tools.rs` | List fanout, prefixing, and merged output. |
| `gateway_prompts.rs` | Prompt listing and prefixed `get_prompt` routing. |
| `gateway_resource_templates.rs` | Template fanout with prefixed names and URI templates, plus `read_resource` round-trips. |
| `gateway_plugins.rs` | Request-scoped parameter-header validation/forwarding, CPEX pre/post tool hooks around `call_tool` and stream events, and prompt hooks around `get_prompt`. |

These run in `cargo nextest run` with no Docker dependencies.

## MCP Conformance

[`cf-integration`](https://crates.io/crates/cf-integration)
owns the official fixture, control-plane registration, Compose topology, server
and client runners, result rendering, and transactional baseline handling. This
repository keeps only the CI invocation, Make targets, and expected findings.

Apply the `run-conformance` label to a pull request to run the **Conformance**
Actions workflow. It runs the modern client and modern server eras through the
external dataplane. Selecting that lane also runs the fixture-direct server leg
and the scoped external-dataplane client leg:

```bash
cargo binstall cf-integration@0.1.0 --no-confirm
make conformance
```

The Make target tests the committed data-plane `HEAD`. It rejects tracked
uncommitted changes because the CLI clones the selected repository and commit
into `.integration/`. To use another local CLI binary:

```bash
CF_INTEGRATION=/path/to/cf-integration \
  make conformance
```

Update every selected baseline atomically only after all operational work and
baseline evaluation succeeds:

```bash
make conformance-bless
```

Baselines are partitioned beneath
`tests/conformance/baselines/<client-version>/<server-era>/`. Server findings
use `fixture-direct.yml` and `external-data-plane.yml`; scoped client findings
use `client/external-data-plane.yml`. Operational failures are always failures
and cannot be blessed. Runtime checkouts, logs, results, and reports remain
beneath `.integration/`.
