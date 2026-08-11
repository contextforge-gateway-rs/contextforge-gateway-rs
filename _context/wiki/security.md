# Security Model

## Trust Boundaries

| Boundary | Trust level | Enforced by |
| --- | --- | --- |
| Downstream client | Untrusted. Every request must present a valid bearer JWT; session id alone grants nothing without matching principal state. | `claims_layer`, validators, and principal-scoped backend session keys. |
| JWT verification material | Trust anchor. The RSA public key or HMAC secret in process config decides which tokens are accepted. | Process config; loaded at startup. |
| Redis | Control-plane trust boundary. Whoever can write Redis controls routing (`UserConfig`) and, when runtime plugins are enabled, which registered hooks execute (`ContextForgeGatewayRuntimePluginConfig`). | Redis TLS/mTLS connection modes; the dataplane never writes user config in production builds. |
| Backend MCP servers | Trusted per configured URL. The gateway forwards caller traffic to them and merges their responses. | `UserConfig` backend URLs plus the upstream connection mode. |
| Plugins | Fully trusted code. Hooks run in-process and can read and mutate tool payloads. | Compiled-in factories only; Redis config activates registered factories, it cannot load new code. |

## Identity And Authorization

Authentication is bearer-JWT only:

- Accepted algorithms: `RS256/RS384/RS512` (public key configured) or `HS256/HS384/HS512` (shared secret configured). Anything else is rejected.
- `iss` must be `mcpgateway`, `aud` must be `mcpgateway-api`, and `exp` is validated.
- **No revocation list.** A leaked token is valid until it expires. Rotate the key and restart to invalidate all outstanding tokens.
- Authorization is config existence. The `sub` claim selects the caller's `UserConfig`; the path selects one virtual host inside it. A caller can never reach a backend not in their own config. Unknown virtual hosts return `404` before MCP handling.
- `jti`, `token_use`, `iat`, `teams`, `user`, and `scopes` are carried but not yet enforced. Fine-grained permissions are future policy work.

## What Compromise Means

| If this is compromised | Impact |
| --- | --- |
| JWT signing key or HMAC secret | Attacker mints tokens for any subject and reaches that subject's backends. Rotate the key and restart; no revocation exists. |
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

The gateway enforces the MCP `2026-07-28` Streamable HTTP DNS-rebinding
security requirement. `mcp_origin_layer` validates requests before JWT claims,
session creation, virtual-host lookup, and backend fanout.

### Host allowlist (`CONTEXTFORGE_GATEWAY_RS_MCP_ALLOWED_HOSTS`)

The optional comma-separated allowlist contains trusted `Host` authorities,
such as `gateway.example.com` or `gateway.example.com:8080`.

- When configured, a missing, malformed, or unlisted request authority returns
  HTTP `403` before Origin validation. The authority comes from `Host`, with an
  absolute request URI as fallback. An entry without a port matches that host
  on any port; an entry with a port matches only that port.
- When omitted (the default), Host validation is disabled. Configure it with
  the Origin allowlist for public-internet deployments.

### Origin allowlist (`CONTEXTFORGE_GATEWAY_RS_MCP_ALLOWED_ORIGINS`)

The optional comma-separated allowlist contains fully qualified browser
origins, such as `https://app.example.com` or `http://localhost:3000`.

| Configuration | `Origin` absent | `Origin` listed | `Origin` unlisted | `null` / malformed |
| --- | --- | --- | --- | --- |
| Non-empty allowlist | Accepted | Accepted | HTTP `403` | HTTP `403` |
| Omitted (default) | Accepted | HTTP `403` | HTTP `403` | HTTP `403` |

An omitted Origin allowlist is not a bypass: every request carrying `Origin`
is rejected until an allowlist is configured. There is no same-origin fallback,
because an attacker can control both `Host` and `Origin` during DNS rebinding.

Origins are validated strictly. Backslashes, userinfo (`@`), paths, queries,
fragments, and opaque origins are rejected. Comparison uses typed origin
equality with default-port normalization, so `https://app.example.com` equals
`https://app.example.com:443`, while port `8443` is distinct. Configuration
values must parse as URLs; invalid URL syntax fails CLI parsing before startup.

## Local Bootstrap Helpers (`with_tools`)

The `contextforge-data-plane-lib/with_tools` feature compiles in:
- `/contextforge-rs/admin/tokens/{user}`
- `/contextforge-rs/admin/userconfigs/{user}`
- `/contextforge-rs/health`

These routes are registered **outside the authentication middleware** — unauthenticated by design. They exist only for local bootstrap. **Production builds must not enable this feature.** In a real deployment the control plane mints tokens and writes config.

## Secrets Handling

- The HMAC secret is held as a `SecretString`; key and certificate material is read from disk paths at startup.
- Never log: tokens, authorization headers, secrets, Redis key/value bytes, full `UserConfig` documents, or backend credentials.
