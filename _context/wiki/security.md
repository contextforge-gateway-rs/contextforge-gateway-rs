# Security Model

## Trust Boundaries

| Boundary | Trust level | Enforced by |
| --- | --- | --- |
| Downstream client | Untrusted. Every request must present a valid bearer JWT; session id alone grants nothing without matching principal state. | `claims_layer`, validators, and principal-scoped backend session keys. |
| JWT verification material | Trust anchor. The RSA public key or HMAC secret in process config decides which tokens are accepted. | Process config; loaded at startup. |
| Redis | Control-plane trust boundary. Whoever can write Redis controls routing (`UserConfig`) and, when runtime plugins are enabled, which registered hooks execute (`ContextForgeGatewayRuntimePluginConfig`). | Redis TLS/mTLS connection modes; the dataplane never writes user config in production builds. |
| Backend MCP servers | Trusted per configured URL. The gateway forwards caller traffic to them and merges their responses. | `UserConfig` backend URLs plus the upstream connection mode. |
| Plugins | Fully trusted code. Hooks run in-process and can read and mutate tool payloads. | Compiled-in factories only; Redis config activates registered factories, it cannot load new code. |

## Authentication And Authorization

| Plane | Current responsibility |
| --- | --- |
| Control plane | Owns login/SSO, users, teams, IAM, API-token issuance and revocation, and legacy routes. `dataplane_publisher.py` writes visibility-filtered `UserConfig` snapshots to Redis by user email. |
| Data plane | Has no IAM or user database. It verifies modern MCP bearer JWTs locally, loads `UserConfig` by `sub`, and requires the requested virtual host to exist. No runtime control-plane call occurs. |

Request path: control-plane API token (`sub` = email) → Origin check →
`claims_layer` → Redis config lookup → virtual-host check → RMCP Host check →
MCP routing.
Browser/login session tokens are management-plane credentials, not the
dataplane contract.

- JWT validation accepts `RS256/384/512` or `HS256/384/512` and requires a valid
  signature, `iss=mcpgateway`, `aud=mcpgateway-api`, and `exp`. `jti` and `user`
  are required fields; `token_use`, `iat`, `teams`, and `scopes` are optional.
- Failures: bad/missing JWT → `401`; no user config → `400`; unavailable virtual
  host → `404`.
- Authorization is currently coarse: valid JWT plus published virtual host.
  JWT scopes/teams and object allowlists are not enforced; publishing a backend
  exposes all objects returned by it.
- This coarse current behavior does not meet the tentative Phase 3 target. The
  target requires principal- and isolation-bound snapshots, per-request scope
  and compiled-RBAC enforcement, and default denial for missing or unauthorized
  entries. See [Target Authorization Invariants](mcp-capability-allocation.md#target-authorization-invariants).
- Dataplane requests do not consult the control-plane token blocklist. Revoked
  tokens pass JWT validation until `exp` or signing-key rotation/restart.
  Removing a subject's config eventually blocks all its tokens after publisher
  and cache expiry.

## What Compromise Means

| If this is compromised | Impact |
| --- | --- |
| JWT signing key or HMAC secret | Attacker mints tokens for any subject and reaches that subject's backends. Rotate the key and restart; no revocation exists. |
| Task-handle key | Attacker decrypts or forges upstream task routes. Rotate the key; outstanding handles become invalid. |
| Redis write access | Attacker rewrites routing (arbitrary backend URLs receive caller traffic) and, if runtime plugins are enabled, chooses which registered hooks run on payloads. Protect Redis with TLS/mTLS and control-plane-only write access. |
| A backend MCP server | Attacker sees requests routed to that backend and controls its responses; the namespace prefix limits blast radius to that backend's objects. |
| The gateway process | Full compromise: it holds the decoding keys in memory and live backend sessions. |

## Transport Security

| Leg | Current posture |
| --- | --- |
| Downstream | TLS optional (`--tls-address`, no client auth — identity is the bearer token). Plain HTTP is acceptable only behind a trusted front door on a private network. |
| Upstream | HTTPS-only by default; plain HTTP must be opted into with `--upstream-connection-mode`. mTLS client identity is supported per process. |
| Redis | Plain, TLS, or mTLS via `--redis-mode`. Use TLS or mTLS anywhere Redis crosses a trust zone — Redis is the config trust boundary. |

## MCP Origin and Host Validation

`mcp_origin_layer` validates Origin before authentication. RMCP validates Host
at the MCP service boundary. Together they enforce MCP `2026-07-28`
DNS-rebinding protection.

| Environment variable | Default | Contract |
| --- | --- | --- |
| `CONTEXTFORGE_GATEWAY_RS_MCP_ALLOWED_HOSTS` | Host check disabled | When set, RMCP requires request authority from `Host` (URI fallback) to match. A portless entry matches any port; an explicit port matches exactly. |
| `CONTEXTFORGE_GATEWAY_RS_MCP_ALLOWED_ORIGINS` | Only requests without `Origin` pass | A present Origin must be a strict serialized origin in the allowlist. |

Missing Origin is accepted. `null`, malformed, unlisted, or
path/query/fragment/userinfo-bearing origins are rejected with HTTP `403`.
Default ports are normalized (`https://a` equals `https://a:443`). When the Host
allowlist is configured, RMCP returns `400` for a missing or malformed authority
and `403` for an unlisted authority. There is no same-origin fallback; configure
both allowlists for public deployments.

Host validation runs only after the request reaches the RMCP service. Origin,
CORS, authentication, user-config, and virtual-host middleware can return a
response first, so the Host-specific `400` and `403` statuses apply only after
those earlier stages succeed.

`mcp_header_limits_layer` enforces configurable count, per-value byte, and
approximate request-level aggregate byte budgets for MCP standard request
headers before JWT validation or RMCP body parsing. The aggregate budget covers
all matched header names and values on one request, while the per-value budget
still caps each individual header value. That budget covers `Mcp-Method`,
`Mcp-Name`, `Mcp-Protocol-Version`, and `Mcp-Param-*`; the same guardrail also
covers the legacy/RMCP transport header `Mcp-Session-Id`. It is an
application-level guard for MCP-related headers only; non-MCP headers remain
bounded by the HTTP transport.

## Local Bootstrap Helpers (`with_tools`)

The `contextforge-data-plane-lib/with_tools` feature compiles in:
- `/contextforge-rs/admin/tokens/{user}`
- `/contextforge-rs/admin/userconfigs/{user}`
- `/contextforge-rs/health`

These routes are registered **outside the authentication middleware** — unauthenticated by design. They exist only for local bootstrap. **Production builds must not enable this feature.** In a real deployment the control plane mints tokens and writes config.

## Secrets Handling

- The HMAC secret is held as a `SecretString`; key and certificate material is read from disk paths at startup.
- The not-yet-wired task-handle codec encrypts upstream task IDs and binds them to a trusted authorization-context ID, virtual host, configuration revision, backend ID, and backend generation.
- Task handles do not replace per-request JWT validation, effective-configuration lookup, scope/RBAC checks, or current backend-generation validation.
- Never log: tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig` documents, or backend credentials.
- Treat task handles and decoded upstream task IDs as opaque secrets; do not log them. Decoded-route debug output must remain redacted.
