# Testing And Load Testing

> Status: draft. To be implemented.

This chapter will document how to verify behavior before changing the gateway.

## To implement

- `cargo fmt --all --check`
- `cargo clippy --locked --workspace --all-targets -- -D warnings`
- `cargo nextest run --locked --workspace`
- `cargo test` fallback when nextest is unavailable
- integration test support modules and mock MCP servers
- focused tests for routing, prompts, resources, plugins, and auth layers
- local docker stack requirements
- `contextforge-load-test` usage and report output
- what qualifies as enough validation for docs-only, routing, config, and plugin changes
