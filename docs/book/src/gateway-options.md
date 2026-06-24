# Gateway Options

> Status: draft. To be implemented.

This chapter will mirror rustc's command-line reference style. It should list
each gateway flag, its environment variable, whether it is required, and what
runtime behavior it controls.

## To implement

- listener flags: `--address`, `--tls-address`, server certificate and key
- JWT flags: public key, HMAC secret, issuer/audience assumptions
- Redis flags: host, port, connection mode, TLS and mTLS material
- upstream flags: connection mode, trust bundle, client certificate and key
- runtime flags: CPU count, single-runtime mode, plugin enablement
- telemetry flags: traces, metrics, OTLP endpoints, protocol, headers, service name
- logging flags: log name and rotation
- examples for common flag sets
