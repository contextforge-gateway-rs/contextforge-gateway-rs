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
| `gateway_plugins.rs` | CPEX pre/post tool hooks around `call_tool` and stream events, and prompt hooks around `get_prompt`. |

These run in `cargo nextest run` with no Docker dependencies.

## MCP Conformance CI

`.github/workflows/mcp_conformance.yml` runs the pinned official conformance
suite `0.2.0-alpha.11` with `--requirements 2026-07-28`. Its small live path is
official runner → nginx → checked-out external dataplane → fixture proxy →
official fixture, with the published `latest` Python image's control plane
registering and publishing the fixture through Redis. The backend-only proxy rewrites `Host` to
`localhost:3000`, which the official fixture's DNS-rebinding protection
requires, while leaving external-dataplane header protections unchanged. The
control plane uses ephemeral SQLite, so PostgreSQL is unnecessary. The harness lives
in `tests/conformance/`.

Because this conformance CLI cannot set a bearer header, nginx adds an
ephemeral control-plane token when one is absent; there is no auth proxy or
repository-owned JavaScript. A route probe prevents built-in-dataplane fallback.
Counts and the official fixture log appear directly in the Actions log, and
`expected-failures.yml` guards the current baseline. The job does not retain a
separate conformance artifact. `upstream-fixture-failures.yml` records the
pinned fixture's seven scored failures and one warning; its other 47 failures
are extension or pending scenarios and are already unscored. CI prints the
exact actual-versus-baseline diff, adds annotations for unexpected and stale
entries, and writes the same comparison to the job summary.

## Full-Stack Integration Harness

[`cf-integration`](https://github.com/contextforge-org/contextforge-dev-tools)
wires the ContextForge control plane, built-in dataplane, and this ContextForge
external dataplane together. The stock Python Compose stack contains both the
control plane and built-in dataplane. The harness adds two intentional
differences: nginx routes the selected `/servers/{virtual_host_id}/mcp` path to
the external dataplane as `/contextforge-rs/servers/{virtual_host_id}/mcp`, and
the control plane runs with `DATAPLANE_PUBLISHER=true` so virtual-server config
reaches the external dataplane through Redis.

### Quick Start

```bash
scripts/cf-integration.sh up
```

This checks out the Python control-plane/built-in-dataplane repository under
`.integration/mcp-context-forge`, pulls the published external-dataplane image,
and starts the combined stack plus a local MCP counter backend. The admin UI is
at `http://localhost:8080/admin` (`admin@example.com` / `changeme`). A Fast Time
backend is auto-registered as a fixed virtual server, so the commands below
work with no manual UI step.

### Route Probe

```bash
scripts/cf-integration.sh probe
```

Verifies the public nginx-to-external-dataplane route end to end: a 401 negative check, `initialize`, session reuse, `tools/list`, and `tools/call`.

### Full Test Runs

| Command | What it runs |
| --- | --- |
| `scripts/cf-integration.sh test-all` | Every live lane against the running stack, with per-test result rows and full output in a timestamped log under `.integration/test-logs/`. |
| `CF_TEST_ALL_LOCUST=true scripts/cf-integration.sh test-all` | Same, plus the full Locust load run as a final lane. |
| `scripts/cf-integration.sh test-all-up` | Start or update the stack, then `test-all` without the load lane. |
| `scripts/cf-integration.sh test-all-up-load` | Start or update the stack, then `test-all` with the load lane. |

Individual lanes: `live-mcp`, `live-rbac`, `live-protocol`, and `live-all`.
`live-mcp` is the green lane: the full MCP protocol end-to-end suite passes
against this harness. Remaining failures in other lanes measure known
external-dataplane feature gaps; the harness `reports/` directory keeps the
current classification.

### Built-In-Dataplane Baseline

To separate external-dataplane regressions from Python behavior, the harness
can run the stock `IBM/mcp-context-forge` stack. MCP traffic then uses the
ContextForge built-in dataplane; the external dataplane, nginx split, and
publisher are absent:

```bash
scripts/cf-integration.sh down                    # frees the shared host ports
scripts/cf-integration.sh controlplane-test-all   # up + live core + locust
```

Individual steps: `controlplane-up`, `controlplane-live-core`, `controlplane-live-all`, `controlplane-locust`, and `controlplane-down`. The baseline load run is covered in [Performance](performance.md).

### Key Settings

| Variable | Purpose |
| --- | --- |
| `CF_DATAPLANE_IMAGE` / `CF_DATAPLANE_VERSION` | Which published external-dataplane image the stack runs. |
| `CF_CONTROLPLANE_IMAGE` / `CF_CONTROLPLANE_REF` | Which `IBM/mcp-context-forge` Python image and git ref to use for the control plane and built-in dataplane. |
| `NGINX_PORT` | Public front-door port (default `8080`). |
| `CF_TEST_LOG_DIR` | Where `test-all` writes timestamped logs. |
