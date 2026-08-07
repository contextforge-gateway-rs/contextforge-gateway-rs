# Failure Modes

**Rule:** failures come from the layer that owns the missing fact. Identity/config failures are HTTP responses before MCP handling; routing/backend failures are JSON-RPC errors.

## HTTP Layer (middleware, before MCP)

| Failure | Response | Layer |
| --- | --- | --- |
| Path doesn't match `/servers/{id}/mcp` | `400` | `virtual_host_id_layer` |
| Missing `Authorization` / non-`Bearer` scheme | `401` | `claims_layer` |
| JWT undecoded, unsupported algorithm, no key | `401` | `claims_layer` |
| Expired token, wrong issuer/audience | `401` | `claims_layer` |
| No user config for `claims.sub`, or claims absent | `400` | `user_config_store_layer` |
| Config store error (not missing) | `500` | `user_config_store_layer` |
| Virtual host id absent from caller's config | `404` `{"detail":"Server not found"}` | `virtual_host_config_layer` |

## MCP Validation (defense-in-depth, normally unreachable)

| Failure | JSON-RPC error |
| --- | --- |
| Missing session id / config / vhost / claims extension | Internal error (`Routing problem...`) |
| Virtual host absent from user config | `RESOURCE_NOT_FOUND` `No configuration` |

## Routing

| Failure | Behavior |
| --- | --- |
| Prefixed name doesn't start with backend name + `-` | Internal error |
| No backend entry matches split name | Internal error (`got no responses from backends`) |
| Backend entry exists but no running service | Internal error (backend failed during initialize) |
| More than one backend entry matches | `INVALID_REQUEST`; session backend entries cleaned up |
| Undecodable pagination cursor | `-32602 Invalid params` |

## Backend Session

| Situation | Behavior |
| --- | --- |
| Backend unreachable during `initialize` | Stored with no running service; initialize still succeeds |
| Backend unreachable during routed call | Call returns internal error; other backends unaffected |
| Gateway process restart | All session state lost; clients must re-run `initialize` |
| Request lands on wrong gateway node | List returns empty; routed calls fail — need sticky routing |

## Plugins

| Failure | Behavior |
| --- | --- |
| Plugin denies call/response | Becomes MCP error to caller |
| Soft plugin error | Logged; call proceeds |
| Invalid plugin config on reload | Runtime marked failed; plugin calls return internal MCP error until valid config applied |

## Config Store (Redis)

| Failure | Behavior |
| --- | --- |
| Redis connection loss | Connection manager retries (1,000 configured) |
| User config missing | `400` from `user_config_store_layer` |
| Redis `GET` error | Reported as missing → `400` |
| Undecodable config / key encoding failure | `500` |
