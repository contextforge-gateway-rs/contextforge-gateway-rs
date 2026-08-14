# Testing

## Verification Rings

- **Workspace checks** — code compiles and unit behavior holds.
- **In-repo integration tests** — MCP routing against mock backends.
- **`cf-integration` harness** — full control-plane-to-dataplane path end to end.
- **Load and benchmark** — see [Performance](performance.md).

## Workspace Validation

CI runs these on every change; run them locally before pushing:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo nextest run --locked --workspace
```

Use `cargo test` when nextest is unavailable. For wiki changes, also run `mdbook build _context/wiki` and `mdbook test _context/wiki`.

Protocol tests and fixtures should target MCP `2026-07-28`, use `server/discover`, and include the required per-request client metadata. Do not add new dataplane coverage for older protocol versions, legacy session initialization, or SSE; those paths belong in control-plane tests. Existing legacy-shaped tests are migration inventory and should be replaced as the modern implementation lands.

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
official runner → nginx → published `latest` dataplane → official fixture,
with the published `latest` control plane registering and publishing the
fixture through Redis. The control plane uses ephemeral SQLite, so PostgreSQL
is unnecessary. The harness lives in `tests/conformance/`.

Because this conformance CLI cannot set a bearer header, nginx adds an
ephemeral control-plane token when one is absent; there is no auth proxy or
repository-owned JavaScript. A route probe prevents control-plane fallback.
Counts appear directly in the Actions log, and `expected-failures.yml` guards
the current baseline. The job does not retain a separate conformance artifact.

## Full-Stack Integration Harness

[`cf-integration`](https://github.com/contextforge-org/contextforge-dev-tools) wires the external ContextForge control plane to this dataplane the way production intends: the stock upstream Compose stack, plus exactly two intentional differences — nginx routes only `/servers/{virtual_host_id}/mcp` to the dataplane (as `/contextforge-rs/servers/{virtual_host_id}/mcp`), and the control plane runs with `DATAPLANE_PUBLISHER=true` so virtual server configs reach the dataplane through Redis.

### Quick Start

```bash
scripts/cf-integration.sh up
```

This checks out the control plane under `.integration/mcp-context-forge`, pulls the published dataplane image, and starts the combined stack plus a local MCP counter backend. The admin UI is at `http://localhost:8080/admin` (`admin@example.com` / `changeme`). A Fast Time backend is auto-registered as a fixed virtual server, so the commands below work with no manual UI step.

### Route Probe

```bash
scripts/cf-integration.sh probe
```

Verifies the public nginx-to-dataplane route end to end: a 401 negative check, `initialize`, session reuse, `tools/list`, and `tools/call`.

### Full Test Runs

| Command | What it runs |
| --- | --- |
| `scripts/cf-integration.sh test-all` | Every live lane against the running stack, with per-test result rows and full output in a timestamped log under `.integration/test-logs/`. |
| `CF_TEST_ALL_LOCUST=true scripts/cf-integration.sh test-all` | Same, plus the full Locust load run as a final lane. |
| `scripts/cf-integration.sh test-all-up` | Start or update the stack, then `test-all` without the load lane. |
| `scripts/cf-integration.sh test-all-up-load` | Start or update the stack, then `test-all` with the load lane. |

Individual lanes: `live-mcp`, `live-rbac`, `live-protocol`, and `live-all`. `live-mcp` is the green lane: the full MCP protocol end-to-end suite passes against this harness. Remaining failures in other lanes measure known dataplane feature gaps; the harness `reports/` directory keeps the current classification.

### Control-Plane Baseline

To separate dataplane regressions from upstream behavior, the harness can run the stock control-plane-only stack (no dataplane, no nginx split, no publisher):

```bash
scripts/cf-integration.sh down                    # frees the shared host ports
scripts/cf-integration.sh controlplane-test-all   # up + live core + locust
```

Individual steps: `controlplane-up`, `controlplane-live-core`, `controlplane-live-all`, `controlplane-locust`, and `controlplane-down`. The baseline load run is covered in [Performance](performance.md).

### Key Settings

| Variable | Purpose |
| --- | --- |
| `CF_DATAPLANE_IMAGE` / `CF_DATAPLANE_VERSION` | Which published dataplane image the stack runs. |
| `CF_CONTROLPLANE_IMAGE` / `CF_CONTROLPLANE_REF` | Which control-plane image and git ref to use. |
| `NGINX_PORT` | Public front-door port (default `8080`). |
| `CF_TEST_LOG_DIR` | Where `test-all` writes timestamped logs. |
