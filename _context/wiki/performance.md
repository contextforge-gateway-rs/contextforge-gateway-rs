# Performance And Load Testing

## Full-Stack Load (Locust via cf-integration)

Performance testing uses the control-plane Locust suite through
[`cf-integration`](https://crates.io/crates/cf-integration).
It measures the nginx → external dataplane → backend request path while the
ContextForge control plane publishes configuration.

| Command | What it runs |
| --- | --- |
| `scripts/cf-integration.sh smoke` | 1 user for 10 s — quick sanity pass. |
| `scripts/cf-integration.sh locust` | Full load run, default 100 users for 5 minutes. |

Tune with environment variables:

```bash
LOCUST_USERS=20 LOCUST_SPAWN_RATE=5 LOCUST_RUN_TIME=2m \
  scripts/cf-integration.sh locust
```

- `MCP_VIRTUAL_SERVER_ID` — target a UI-created virtual server instead of the auto-registered Fast Time one.
- `MCP_TOOL_NAMES` — pick the tools to call.
- Output: `.integration/mcp-context-forge/reports/` (HTML and CSV).

## Headless vs Web UI

The harness runs Locust headless by default (`LOCUST_MODE=headless`). Set `LOCUST_MODE=web` to switch to interactive mode (master + web UI on port `8089`). The one-off `locust` command does not publish container ports; for the web UI, start via the stack's `testing` Compose profile which maps `8089:8089`.

## Benchmark Settings

Restore both to `60` before measuring throughput — fast publish + per-request Redis reads distort numbers:

| Variable | Functional default | Benchmark value |
| --- | --- | --- |
| `CF_DATAPLANE_PUBLISHER_INTERVAL_SECONDS` | `2` (fast config publish) | `60` (upstream default) |
| `CF_DATAPLANE_USER_CONFIG_CACHE_EXPIRY_SECONDS` | `0` (cache disabled) | `60` (upstream default) |

## Built-In-Dataplane Baseline

Compare against the stock Python repository, where MCP traffic uses the
ContextForge built-in dataplane and the ContextForge external dataplane is
absent:

```bash
scripts/cf-integration.sh down                # free shared ports
scripts/cf-integration.sh controlplane-locust
```

`CONTROLPLANE_LOCUST_CLASSES=all` adds admin/UI/mutating surfaces. `LOCUST_USERS`, `LOCUST_SPAWN_RATE`, and `LOCUST_RUN_TIME` apply here too.
