# Contributing To The Gateway

> 🛠️ **Contribution rule:** put behavior in the crate that owns it, keep the
> client-visible contracts stable, and update the matching book page in the
> same change.

## Where Changes Belong

| Change | Home |
| --- | --- |
| Dataplane behavior: routing, middleware, sessions, transports | `contextforge-gateway-rs-lib` — almost everything goes here. |
| Process shell: CLI flags, logging, runtime shape, exporters | `contextforge-gateway-rs` (the binary crate). Do not add dataplane logic here. |
| Shared config shapes (`UserConfig`, `User`, plugin config document) | `contextforge-gateway-rs-apis`. Regenerate the JSON schemas after any change: `cargo run -p contextforge-gateway-rs-apis` (see [Control-Plane Integration](control-plane-integration.md)). |
| Plugin integration | `contextforge-gateway-rs-cpex`. |
| Load generation | `contextforge-load-test`. |

Inside the library crate, keep the module boundaries from
[System Shape](system-shape.md#module-boundaries): config validation in
`common.rs`, extension extraction in `layers/`, MCP behavior in `gateway/`,
listeners in `transports/`, Redis details behind `UserConfigStore`.

## Changing MCP Routing

The backend prefix namespace is a client-visible contract
([MCP Routing Semantics](mcp-routing-semantics.md)). Any change to it must
update the merge logic, the split logic, and the tests in the same PR — and
the [Control-Plane Integration](control-plane-integration.md) if the
client-facing surface moves.

The project is still early, with no external users: prefer the right
architecture over preserving unstable APIs or compatibility surfaces.

## Adding Plugin Hooks

New hook points need defined behavior for failure, timeout, cancellation,
streaming, and telemetry attribution before they land on the hot path — see
[Plugins And Policy](plugins-and-policy.md). Avoid ad hoc plugin calls in the
middle of routing code.

## Branch And Pull Request Workflow

Name contribution branches `user/<user>/<pr-branch>`, where `<user>` is the
contributor's GitHub username and `<pr-branch>` is a short, kebab-case summary
of the change. For example: `user/alice/fix-session-cleanup`.

Open pull requests as drafts while work or validation is still in progress.
Mark a pull request ready for review only after its implementation,
documentation, tests, and required checks are complete.

## Validation

The pre-commit hooks run these local gates:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo deny check advisories licenses
cargo nextest run --locked --workspace
cargo build --locked --workspace
cargo bench --locked --workspace --no-run
```

CI runs the same gates and additionally runs
`cargo shear --check-test-targets --deny-warnings --locked`.

Expectations by change type:

| Change type | Minimum validation |
| --- | --- |
| Docs only | `mdbook build docs/book` and `mdbook test docs/book`. |
| Routing or session behavior | New or updated integration tests under `crates/contextforge-gateway-rs-lib/tests/` against the mock backends. |
| Config shape | Schema regeneration plus a control-plane compatibility check. |
| Plugin behavior | `gateway_plugins.rs` coverage for the new hook path. |
| Performance-sensitive paths | A [load-test run](performance.md) before and after. |

For end-to-end confidence against the real control plane, run the
[cf-integration lanes](testing.md#full-stack-integration-harness).

## Keep The Book True

Every page in this book states verifiable behavior. When a change makes a
page wrong — a flag, a status code, a lock, a boundary — fix the page in the
same PR. Stale architecture docs are worse than none.
