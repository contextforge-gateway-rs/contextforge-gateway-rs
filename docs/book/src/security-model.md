# Security Model And Trust Boundaries

> 🔒 **Trust rule:** the gateway trusts its process config and the
> control-plane-authored data in Redis. It does not trust downstream callers
> beyond a validated JWT, and it reaches backends only through configured
> URLs.

## Trust Boundaries

| Boundary | Trust level | Enforced by |
| --- | --- | --- |
| Downstream client | Untrusted. Every request must present a valid bearer JWT; the session id alone grants nothing without matching principal state. | `claims_layer`, validators, and the principal-scoped backend session keys. |
| JWT verification material | Trust anchor. The RSA public key or HMAC secret in process config decides which tokens are accepted. | Process config; loaded at startup. |
| Redis | Control-plane trust boundary. Whoever can write Redis controls routing (`UserConfig`) and, when runtime plugins are enabled, which registered hooks execute (`ContextForgeGatewayRuntimePluginConfig`). | Redis TLS/mTLS connection modes; the dataplane never writes user config in production builds. |
| Backend MCP servers | Trusted per configured URL. The gateway forwards caller traffic to them and merges their responses. | `UserConfig` backend URLs plus the upstream connection mode. |
| Plugins | Fully trusted code. Hooks run in-process and can read and mutate tool payloads. | Compiled-in factories only; Redis config activates registered factories, it cannot load new code. |

## Identity And Authorization

Authentication is bearer-JWT only:

- Accepted algorithms are `RS256/RS384/RS512` (public key configured) or
  `HS256/HS384/HS512` (shared secret configured); anything else is rejected.
- `iss` must be `mcpgateway`, `aud` must be `mcpgateway-api`, and `exp` is
  validated. There is no revocation list: a leaked token is valid until it
  expires.
- Authorization is config existence. The `sub` claim selects the caller's
  `UserConfig`; the path selects one virtual host inside it. A caller can
  never reach a backend that is not in their own config, and unknown virtual
  hosts return `404` before MCP handling.
- `jti`, `token_use`, `iat`, `teams`, `user`, and `scopes` are carried but not
  yet enforced; `token_use`, `iat`, `teams`, and `scopes` are optional, as is
  `user.full_name`. Fine-grained permissions are future policy work.

## What Compromise Means

| If this is compromised | Impact |
| --- | --- |
| JWT signing key or HMAC secret | Attacker mints tokens for any subject and reaches that subject's backends. Rotate the key and restart; there is no revocation. |
| Redis write access | Attacker rewrites routing (arbitrary backend URLs receive caller traffic) and, if runtime plugins are enabled, chooses which registered hooks run on payloads. Protect Redis with TLS/mTLS and control-plane-only write access. |
| A backend MCP server | Attacker sees the requests routed to that backend and controls its responses; the namespace prefix limits blast radius to that backend's objects. |
| The gateway process | Full compromise: it holds the decoding keys in memory and live backend sessions. |

## Transport Security

| Leg | Current posture |
| --- | --- |
| Downstream | TLS optional (`--tls-address`, no client auth — identity is the bearer token). Plain HTTP is acceptable only behind a trusted front door on a private network. |
| Upstream | HTTPS-only by default; plain HTTP must be opted into with `--upstream-connection-mode`. mTLS client identity is supported per process. |
| Redis | Plain, TLS, or mTLS via `--redis-mode`. Use TLS or mTLS anywhere Redis crosses a trust zone, because Redis is the config trust boundary. |

CORS is currently wide open (any origin, method, and header). The API is
bearer-token based and cookie-free, so cross-site request forgery does not
apply, but expect this to tighten as policy work lands.

## Local Bootstrap Helpers

The `contextforge-gateway-rs-lib/with_tools` feature compiles in
`/contextforge-rs/admin/tokens/{user}`,
`/contextforge-rs/admin/userconfigs/{user}`, and `/contextforge-rs/health`.
These routes are registered outside the authentication middleware, so token
minting and config writes are unauthenticated by design — they exist only for
local bootstrap. Production builds must not enable this feature: in a real
deployment the control plane mints tokens and writes config.

## Secrets Handling

- The HMAC secret is held as a `SecretString`; key and certificate material is
  read from disk paths at startup.
- Log hygiene is a standing rule: never log tokens, authorization headers,
  secrets, Redis key/value bytes, full `UserConfig` documents, or backend
  credentials.
