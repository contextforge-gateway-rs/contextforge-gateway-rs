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
- `jti` and `user` are required by the accepted token shape but are not used for policy decisions. `token_use`, `iat`, `teams`, and `scopes` are optional and are also not enforced. Fine-grained permissions are future policy work.

## Authentication Responsibility Split

Authentication is coordinated across the two planes but does not require a
runtime HTTP call between them.

| Concern | Control plane (`IBM/mcp-context-forge`) | Data plane (this repository) |
| --- | --- | --- |
| Human identity | Owns login, password/SSO/OAuth flows, users, teams, management sessions, and IAM policy. | Has no user database, login flow, cookies, or IAM service. |
| Token issuance | Creates and manages API JWTs and signs them with the configured private key or HMAC secret. The API-token path emits the rich claim shape accepted by the dataplane. | Does not mint production tokens. It receives only the RSA public key or HMAC verification secret at startup. |
| Authorization material | Applies database visibility/team rules and periodically publishes a per-user `UserConfig` snapshot to Redis. | Reads that snapshot through `UserConfigStore`; it never asks the control plane to authorize a request. |
| Request authentication | Authenticates and authorizes requests served on control-plane routes, including management and legacy MCP traffic. | Independently validates every modern MCP request routed to `/contextforge-rs`. |
| Revocation and lifecycle | Owns token catalogs, revocation/blocklists, user disablement, and management-session lifecycle for its own request paths. | Does not consult the control-plane database or token blocklist. Revocation alone does not stop local JWT validation; published config can separately remove routing access. |

### Current end-to-end flow

1. A user authenticates to the control plane and creates an API token. The
   compatible API-token path sets `sub` to the user's email and includes `jti`,
   `user`, `token_use: api`, plus optional `teams` and `scopes`.
2. The control-plane `dataplane_publisher.py` reads active users, team
   membership, visibility, and enabled server/backend associations. It writes
   a filtered `UserConfig` snapshot to Redis under the same user email.
3. The MCP client sends that bearer token to the modern dataplane route. A
   control-plane browser/login session token is a management-plane credential,
   not the documented dataplane credential: its subject semantics may differ
   from the email key used by the publisher.
4. `mcp_origin_layer` performs Host/Origin DNS-rebinding checks before
   authentication.
5. `claims_layer` requires `Authorization: Bearer ...`, accepts only
   `RS256/384/512` or `HS256/384/512`, verifies the signature, issuer
   (`mcpgateway`), audience (`mcpgateway-api`), and expiration, then inserts
   `ContextForgeClaims` into the request.
6. `user_config_store_layer` uses `claims.sub` as the Redis lookup key. Missing
   config returns HTTP `400`; the dataplane does not fall back to a live
   control-plane lookup.
7. `virtual_host_config_layer` checks that the requested virtual host exists in
   that user's published config. A missing virtual host returns the
   control-plane-compatible HTTP `404`; accepted requests continue to MCP
   routing with backend session state scoped by principal and session id.

The current authorization boundary is therefore coarse: valid token plus a
published virtual host for `sub`. JWT `teams`/`scopes` and the model's
`allowed_tool_names`, `allowed_resource_names`, and `allowed_prompt_names` are
not yet enforced by dataplane routing. The control plane must publish only the
backends a user may reach, and this limitation must remain visible until
fine-grained enforcement lands. Publishing a backend currently makes every
object exposed by that backend reachable, regardless of those allowlist fields.

> **Revocation gap:** revoking one API token in the control plane does not make
> the dataplane consult that blocklist. The token continues to pass local JWT
> validation until `exp` unless the signing key is rotated and the dataplane is
> restarted. Removing the subject's published config can stop routing after the
> publisher TTL and dataplane cache expire, but removes access for every token
> belonging to that subject rather than revoking one token.

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
