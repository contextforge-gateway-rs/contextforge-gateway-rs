# Performance

> ⚡ **Two load paths:** `contextforge-load-test` measures the Rust dataplane
> alone, and the [`cf-integration`](https://github.com/contextforge-gateway-rs/cf-integration)
> harness measures the full nginx-to-control-plane-to-dataplane stack with
> Locust. Use the first to profile gateway changes and the second to measure
> what users would see.

## Dataplane-Only Load Testing

`crates/contextforge-load-test` is a [Goose](https://book.goose.rs/)-based
traffic driver that speaks the full streamable HTTP MCP flow against a running
gateway. Start the [local stack](running-the-gateway.md) with seeded user
config first, then:

```bash
cargo run --release --bin contextforge-load-test -- \
  --host 'http://127.0.0.1:8001' \
  -u 120 -r 40 --run-time 120s \
  --report-file report.html
```

`-u` is concurrent users, `-r` is the spawn rate per second, and
`--report-file` writes an HTML report. This measures the Rust dataplane alone,
without a control plane or front door in the path. Curated run reports live in
the repository's `reports/` directory.

## Full-Stack Load With Locust

The [cf-integration harness](testing.md#full-stack-integration-harness) runs
Locust against the public nginx route with a streamable-HTTP-aware locustfile:

| Command | What it runs |
| --- | --- |
| `scripts/cf-integration.sh smoke` | 1 user for 10 seconds — a quick sanity pass. |
| `scripts/cf-integration.sh locust` | The full load run, default 100 users for 5 minutes. |

Tune with environment variables:

```bash
LOCUST_USERS=20 LOCUST_SPAWN_RATE=5 LOCUST_RUN_TIME=2m \
  scripts/cf-integration.sh locust
```

`MCP_VIRTUAL_SERVER_ID` targets a UI-created virtual server instead of the
auto-registered Fast Time one, and `MCP_TOOL_NAMES` picks the tools to call.
Locust HTML/CSV output lands under `.integration/mcp-context-forge/reports/`;
curated run reports live in the harness `reports/` directory.

## Headless Versus Locust Web UI

The harness runs Locust headless by default (`LOCUST_MODE=headless`): a timed
run that writes the HTML and CSV reports and prints only the summary. Setting
`LOCUST_MODE` to any other value (for example `web`) switches the Locust
service to interactive mode: a master with the web UI on port `8089` and a
class picker (`LOCUST_EXPECT_WORKERS` controls the expected worker count). The
one-off `locust` command does not publish container ports, so for the web UI
start the Locust service through the stack's `testing` Compose profile — the
upstream stack maps `8089:8089` — then open `http://localhost:8089` and drive
the run from the browser.

## Benchmark Settings

The harness tunes config propagation for functional runs, not throughput:

| Variable | Functional default | Benchmark value |
| --- | --- | --- |
| `CF_DATAPLANE_PUBLISHER_INTERVAL_SECONDS` | `2` (fast config publish) | `60` (upstream default) |
| `CF_DATAPLANE_USER_CONFIG_CACHE_EXPIRY_SECONDS` | `0` (cache disabled) | `60` (upstream default) |

Restore both to `60` before measuring throughput, or the fast publish loop and
per-request Redis reads distort the numbers.

## Control-Plane Baseline Load

To compare against the stack without the dataplane in the path:

```bash
scripts/cf-integration.sh down                # frees the shared host ports
scripts/cf-integration.sh controlplane-locust
```

The baseline run defaults to the non-UI Locust class subset (health, Fast
Time, Fast Test, version/meta). `CONTROLPLANE_LOCUST_CLASSES=all` adds the
admin/UI/mutating surfaces. `LOCUST_USERS`, `LOCUST_SPAWN_RATE`, and
`LOCUST_RUN_TIME` apply here too.
