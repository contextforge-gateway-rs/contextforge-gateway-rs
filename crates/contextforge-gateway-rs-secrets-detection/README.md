# ContextForge Gateway Secrets Detection

Rust CPEX secrets detection plugin for the ContextForge dataplane.

This crate is a first-class member of the `contextforge-data-plane` workspace.
It provides the `SecretsDetectionFactory` registered by the gateway binary when
the `secrets-detection-plugin` Cargo feature is enabled.

## Runtime Activation

The crate is compiled into the gateway binary. Runtime configuration still comes
from the dataplane plugin config document stored in Redis.

Example config:

```json
{
  "version": 1,
  "cpex": {
    "plugins": [
      {
        "name": "secrets-detection",
        "kind": "validator/secrets-detection",
        "hooks": ["cmf.tool_pre_invoke", "cmf.tool_post_invoke"],
        "config": {
          "redact": true,
          "redaction_text": "[redacted]",
          "block_on_detection": false
        }
      }
    ]
  }
}
```

The dataplane integration currently wires the tool-call path:

- `cmf.tool_pre_invoke`: scans tool arguments before the backend receives them.
- `cmf.tool_post_invoke`: scans tool results before the client receives them.

The crate also keeps prompt/resource stage handling for CPEX parity and future
hosts, but the current dataplane runtime config only uses the tool pre/post
hooks.

## Behavior

The scanner detects common secret-shaped values in JSON payloads and direct text
content. Depending on config, it can:

- redact detected values
- deny payloads when `block_on_detection` is enabled and the threshold is met
- apply dotted field allowlists/denylists to JSON arguments and results
- emit non-sensitive metadata from direct handlers when trace context exists

The CPEX plugin kind is:

```text
validator/secrets-detection
```

## Known CPEX 0.2.2 Gaps

- `PluginResult.metadata` is not propagated through `PluginManager`.
- A denied result cannot surface a redacted payload through `PluginManager`.

The direct handler can return both metadata and a redacted payload on block, but
those fields are lost at the manager/executor boundary in CPEX 0.2.2.

## Verification

From the workspace root:

```bash
cargo +1.96 test -p contextforge-gateway-rs-secrets-detection
cargo +1.96 check -p contextforge-gateway-rs --features secrets-detection-plugin
cargo +1.96 test -p contextforge-gateway-rs-cpex
cargo +1.96 test -p contextforge-gateway-rs-lib --test gateway_plugins -- --nocapture
cargo +1.96 test -p contextforge-gateway-rs --features secrets-detection-plugin --test secrets_detection_e2e -- --ignored --nocapture
```
