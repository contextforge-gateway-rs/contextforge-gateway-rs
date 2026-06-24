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
| 🧪 [Testing And Load Testing](testing-and-load-testing.md) | Unit, integration, mdBook, and load-test concerns for proving behavior before changes ship. |
| 🏗️ [Deployment Notes](deployment-notes.md) | Front-door routing, GitHub Pages publication, session affinity, and cluster constraints. |
