# Failure Modes

> 🚧 **Boundary rule:** every failure should come from the layer that owns the
> missing fact. Identity and config failures are HTTP responses before MCP
> handling; routing and backend failures are JSON-RPC errors.

## HTTP Layer Failures

These happen in middleware, before any MCP method runs:

| Failure | Response | Owning layer |
| --- | --- | --- |
| Inner path does not match `/servers/{virtual_host_id}/mcp` | `400 Bad Request` | `virtual_host_id_layer` |
| Missing `Authorization` header or non-`Bearer` scheme | `401 Unauthorized` | `claims_layer` |
| JWT cannot be decoded, uses an unsupported algorithm, or no matching decoder key/secret is configured | `401 Unauthorized` | `claims_layer` |
| Expired token, wrong issuer, or wrong audience | `401 Unauthorized` | `claims_layer` |
| No user config exists for `claims.sub`, or claims are absent | `400 Bad Request` | `user_config_store_layer` |
| Config store error other than missing data | `500 Internal Server Error` | `user_config_store_layer` |
| Virtual host id not present in the caller's config | `404 Not Found` with `{"detail":"Server not found"}` | `virtual_host_config_layer` |

## MCP Validation Failures

`InitializeCallValidator` and `AuthorizedCallValidator` re-check the request
context before method handling. These are defense-in-depth errors: in a
healthy stack the middleware has already established the context.

| Failure | JSON-RPC error |
| --- | --- |
| Missing session id, user config, virtual host id, or claims extension | Internal error (`Routing problem...`). |
| Virtual host absent from the user config | `RESOURCE_NOT_FOUND` with message `No configuration`. Normally unreachable because `virtual_host_config_layer` already returned `404`. |

## Routing Failures

| Failure | Behavior |
| --- | --- |
| Prefixed name does not start with a configured backend name plus `-` | Internal error (`wrong tool name` / `wrong resource name` / `wrong prompt name`). |
| No backend entry matches the split name | Internal error (`got no responses from backends`). |
| Backend entry exists but has no running service | Internal error. Happens when that backend failed during `initialize`. |
| More than one backend entry matches | `INVALID_REQUEST`, and the session's backend entries are removed via `cleanup_backends`. |

## Backend Session Failures

| Situation | Behavior |
| --- | --- |
| Backend unreachable during `initialize` | The backend is stored with no running service. `initialize` still succeeds with the remaining backends. |
| Backend unreachable during a routed call | The call returns an internal error; other backends are unaffected. |
| Gateway process restart | All backend session state is lost because it is local process state. Clients must re-run `initialize`. |
| Request lands on a gateway node that does not own the session | List calls return empty results and routed calls fail, because `BackendTransports` has no entries there. Stateful sessions need sticky routing; see [Session Ownership](session-ownership.md). |

## Plugin Failures

| Failure | Behavior |
| --- | --- |
| Plugin denies a tool call or response | The denial becomes an MCP error to the caller. |
| Soft plugin error | Logged; the call proceeds. |
| Invalid plugin config document (wrong version, missing `cpex` config, unsupported features) | Rejected at load. On an invalid reload, the runtime is marked failed and plugin calls return an internal MCP error until a valid config is applied. |

## Config Store Failures

| Failure | Behavior |
| --- | --- |
| Redis connection loss | The connection manager retries (configured with 1,000 retries). |
| User config missing | `400 Bad Request` from `user_config_store_layer`. |
| Redis `GET` returns an error | Currently reported by the Redis adapter as missing data, so the layer returns `400 Bad Request`. |
| User config undecodable, key encoding failure, or other non-missing store errors | `500 Internal Server Error` from `user_config_store_layer`. |

For a symptom-first version of this table, see the troubleshooting section in
[Run the Gateway Locally](running-the-gateway.md#troubleshooting).
