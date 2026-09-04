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
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo nextest run --locked --workspace --all-features
cargo shear --check-test-targets --deny-warnings --locked
```

Use `cargo test` when nextest is unavailable. For wiki changes, also run `mdbook build _context/wiki` and `mdbook test _context/wiki`.

New protocol-sensitive tests target MCP `2026-07-28`, connect through
`server/discover`, and send the required per-request client metadata. A small
`compatibility` module retains the active `2025-11-25`/`initialize` cases until
that production compatibility surface is removed in a dedicated change; do not
add new behavior to that lane. Every case must remain request-independent, with
no required `Mcp-Session-Id`, session affinity, or retained backend transport.
SSE remains outside the external-dataplane contract.

## In-Repo Integration Tests

`crates/contextforge-data-plane-lib/tests/gateway.rs` is the single library
integration target. It exercises the public gateway API against in-process MCP
backends without recompiling a shared support tree for every feature file.

| Area | Covers |
| --- | --- |
| `gateway/{tools,prompts,resources,subscriptions}.rs` | Active routed operations and exact routing failures. |
| `gateway/plugins.rs` | Gateway-owned CPEX ordering, mutation, denial, progress, and prompt seams using deterministic recording plugins. Resource coverage includes direct and aliased URIs, text/blob redaction, canonical pre-hook URIs, pre/post denial, and invalid embedded prompt blobs. Concrete plugin behavior stays in each plugin crate. |
| `gateway/harness/` | Authentication, modern and compatibility clients, in-memory configuration, concrete mock backends, and owned server fixtures. |
| `gateway/future_contracts/` | Deferred fanout, pagination, TLS, completions, subscriptions, and cancellation contracts. |

`TestServer` binds `127.0.0.1:0` before spawning, uses cooperative
cancellation, and has a `Drop` fallback. `GatewayFixture` owns the gateway and
all backend servers. Tests should request the minimum topology: one virtual host
and one backend by default, with extra backends declared explicitly by the case.

The workspace currently keeps 13 ignored tests: 11 library future contracts
and two real-process Redis/binary E2E tests. Ignored tests are not dead tests:
keep them compiling, keep their intended assertions, give each a concrete
blocker reason, and list them with:

```bash
cargo nextest list --locked --workspace --all-features --run-ignored only
```

The two binary E2E tests and `tests/conformance/` remain separate infrastructure
boundaries. Active in-process tests run with no Docker or Redis dependency. Resource policy coverage belongs in this active harness, not in ignored binary tests or new legacy-client cases. Runtime unit tests verify that enabling or disabling hooks during a resource read preserves its original policy decision.

Parameter-header integration tests verify that calls without a published tool
schema skip local `Mcp-Param-*` validation and still reach the backend. Unit and
integration coverage also includes missing, malformed, unexpected, repeated,
and mismatched recognized headers; Base64 encoding; nested paths; numerically
equivalent integers; and invalid annotation names, types, duplicates, and
non-`properties` paths.

## MCP Conformance

[`cf-integration`](https://crates.io/crates/cf-integration)
owns the official fixture, control-plane registration, Compose topology, server
and client runners, result rendering, and transactional baseline handling. This
repository keeps only the CI invocation, Make targets, and expected findings.

Comment exactly `/conformance` on a pull request to run the **Conformance**
Actions workflow. Only repository owners, members, and collaborators can start
it. The workflow acknowledges the command, tests the pull request merge commit,
and reports the final result back to the pull request. It runs the modern client
and modern server eras through the external dataplane. Selecting that lane also
runs the fixture-direct server leg and the scoped external-dataplane client leg:

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
