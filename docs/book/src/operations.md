# Operations

This section covers runtime behavior after the gateway is serving traffic:
plugins, diagnostics, failure modes, tests, load tests, and deployment
constraints.

> 🧪 **Use this section when operating or verifying the gateway.** It focuses
> on plugin hooks, observability, failure boundaries, load testing, and
> deployment assumptions.

| Page | What it covers |
| --- | --- |
| 🧩 [Plugins And Policy](plugins-and-policy.md) | Where request and response plugins fit, and why body access changes the runtime model. |
| 📈 [Telemetry And Diagnostics](telemetry-and-diagnostics.md) | Signals needed to debug authentication, config lookup, routing, upstream calls, and merged results. |
| 🚧 [Failure Modes](failure-modes.md) | Expected failures by boundary, including auth, Redis, virtual hosts, backend sessions, and upstream transport. |
| 🧪 [Testing](testing.md) | Workspace checks, in-repo integration tests, and the cf-integration full-stack test lanes. |
| ⚡ [Performance](performance.md) | Dataplane-only load testing, full-stack Locust runs, headless versus web UI, and benchmark settings. |
| 🏗️ [Deployment Notes](deployment-notes.md) | Front-door routing, GitHub Pages publication, session affinity, and cluster constraints. |
