# Testing

> 🧪 **Verification rings:** workspace checks prove the code compiles and unit
> behavior holds, in-repo integration tests prove MCP routing against mock
> backends, and the `cf-integration` harness proves the whole
> control-plane-to-dataplane path end to end. Load and benchmark runs have
> their own page: [Performance](performance.md).

## Workspace Validation

CI runs these on every change; run them locally before pushing:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```

Use `cargo test` when nextest is unavailable. For book changes, also run
`mdbook build docs/book` and `mdbook test docs/book`.

Protocol tests and fixtures should target MCP `2026-07-28`, use
`server/discover`, and include the required per-request client metadata. Do not
add new dataplane coverage for older protocol versions, legacy session
initialization, or SSE; those paths belong in control-plane tests and must not
route legacy traffic through the dataplane. Existing legacy-shaped tests are
migration inventory and should be replaced as the modern implementation lands.

## In-Repo Integration Tests

`crates/contextforge-data-plane-lib/tests/` exercises the gateway against
in-process mock MCP backends (shared helpers live in `tests/support/`):

| Test file | Covers |
| --- | --- |
| `gateway_list_tools.rs` | List fanout, prefixing, and merged output. |
| `gateway_prompts.rs` | Prompt listing and prefixed `get_prompt` routing. |
| `gateway_resource_templates.rs` | Template fanout with prefixed names and URI templates, plus `read_resource` round-trips. |
| `gateway_plugins.rs` | CPEX pre/post tool hooks around `call_tool` and stream events. |

These run in `cargo nextest run` with no Docker dependencies.

## Full-Stack Integration Harness

[`cf-integration`](https://github.com/contextforge-org/contextforge-dev-tools)
wires the external ContextForge control plane (`cf-controlplane`) to this
dataplane the way production intends: the stock upstream Compose stack, plus
exactly two intentional differences — nginx routes only
`/servers/{virtual_host_id}/mcp` to the dataplane (as
`/contextforge-rs/servers/{virtual_host_id}/mcp`), and the control plane runs
with `DATAPLANE_PUBLISHER=true` so virtual server configs reach the dataplane
through Redis. Because the stack otherwise matches upstream, test failures
measure dataplane behavior, not stack drift.

### Quick Start

```bash
scripts/cf-integration.sh up
```

This checks out the control plane under `.integration/mcp-context-forge`,
pulls the published dataplane image, and starts the combined stack plus a
local MCP counter backend. The admin UI is at
`http://localhost:8080/admin` (`admin@example.com` / `changeme`). A Fast Time
backend is auto-registered as a fixed virtual server, so the commands below
work with no manual UI step; backends added through the UI are published to
the dataplane by the control-plane publisher.

### Route Probe

```bash
scripts/cf-integration.sh probe
```

Verifies the public nginx-to-dataplane route end to end: a 401 negative check,
`initialize`, session reuse, `tools/list`, and `tools/call`.

### Full Test Runs

| Command | What it runs |
| --- | --- |
| `scripts/cf-integration.sh test-all` | Every live lane against the running stack, with per-test result rows and full output in a timestamped log under `.integration/test-logs/` (override with `CF_TEST_LOG_DIR`). |
| `CF_TEST_ALL_LOCUST=true scripts/cf-integration.sh test-all` | Same, plus the full Locust load run as a final lane. |
| `scripts/cf-integration.sh test-all-up` | Start or update the stack, then `test-all` without the load lane. |
| `scripts/cf-integration.sh test-all-up-load` | Start or update the stack, then `test-all` with the load lane. |

Individual lanes are `live-mcp`, `live-rbac`, `live-protocol`, and `live-all`.
`live-mcp` is the green lane: the full MCP protocol end-to-end suite passes
against this harness. Remaining failures in the other lanes measure known
dataplane feature gaps; the harness `reports/` directory keeps the current
classification.

### Control-Plane Baseline

To separate dataplane regressions from upstream behavior, the harness can run
the stock control-plane-only stack (no dataplane, no nginx split, no
publisher) with the same commands:

```bash
scripts/cf-integration.sh down                    # frees the shared host ports
scripts/cf-integration.sh controlplane-test-all   # up + live core + locust
```

Individual steps are `controlplane-up`, `controlplane-live-core`,
`controlplane-live-all`, `controlplane-locust`, and `controlplane-down`. The
baseline load run is covered in [Performance](performance.md).

### Key Settings

| Variable | Purpose |
| --- | --- |
| `CF_DATAPLANE_IMAGE` / `CF_DATAPLANE_VERSION` | Which published dataplane image the stack runs. |
| `CF_CONTROLPLANE_IMAGE` / `CF_CONTROLPLANE_REF` | Which control-plane image and git ref to use. |
| `NGINX_PORT` | Public front-door port (default `8080`). |
| `CF_TEST_LOG_DIR` | Where `test-all` writes timestamped logs. |

See the harness README for the full command and override list.
