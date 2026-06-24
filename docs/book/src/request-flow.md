# Request Flow

This page follows a request from process startup to MCP handling. The important
thing is not just what happens, but what state is allowed to exist at each
point.

## Startup Path

Startup begins in `crates/contextforge-gateway-rs/src/main.rs`.

```text
Config::parse()
  -> logging::init_tracing_logging
  -> runtime::Runtime::from(&config)
  -> optional CPEX runtime registry
  -> Gateway::builder()
       .with_config(config)
       .with_user_config_store_type(UserConfigStoreType::Redis)
       .with_session_manager(LocalSessionManager::default())
       .with_plugin_runtime(...)
       .build()
  -> runtime.execute(gateway, plugin_registry)
```

The runtime can run as one multi-thread Tokio runtime or as multiple
current-thread runtimes. The default is the single multi-thread runtime. The
multi-runtime mode is a performance and isolation lever, not a behavior change.

`Gateway::run_gateway` builds the HTTP stack. It creates the Redis-backed config
store, local user session store, shared backend transport map, RMCP
`StreamableHttpService`, request middleware, telemetry layers, and downstream
TCP or TLS listeners.

## Route Shape

The public route is nested under `/contextforge-rs`:

```text
/contextforge-rs
  /servers/{virtual_host_name}/mcp
```

The Axum route segment is named `virtual_host_name`, but the gateway extracts
the actual id from the path and stores it as a `VirtualHostId` extension. MCP
handlers do not parse paths directly.

## Middleware Context

Ignoring CORS preflight handling, the request middleware prepares these
extensions before RMCP handlers need them:

```text
VirtualHostId
ContextForgeClaims
SessionId
UserConfig
```

The virtual host layer extracts `{virtual_host_id}` from paths of the form
`/servers/{virtual_host_id}/mcp`.

The claims layer validates a bearer token against the configured RSA or HMAC
decoder. It enforces issuer, audience, and expiration. On success, it inserts
`ContextForgeClaims`.

The session layer reads the `Mcp-session-id` header into `SessionId` for
authorized calls after initialization. It also performs cleanup on successful
`DELETE`.

The user config layer uses the JWT subject as the user key and asks
`UserConfigStore` for the matching `UserConfig`. That lookup must happen after
claims validation and before backend selection.

## Initialize

MCP initialization turns one downstream client session into one upstream MCP
client session per configured backend.

During `initialize`, `InitializeCallValidator` requires:

```text
DownstreamSessionId from RMCP
UserConfig from middleware
VirtualHostId from middleware
ContextForgeClaims from middleware
```

The validator resolves the selected `VirtualHost` from the user's config. The
gateway then creates one `StreamableHttpClientTransport` per backend URL and
serves a `GatewayBackendClient` over that transport. Backend initialization is
done concurrently with `futures::future::join_all`.

For each initialized backend, the gateway stores the running RMCP client service
in the shared backend transport map. Later calls reuse those services instead
of recreating upstream sessions.

## Authorized Calls

After initialization, calls such as `list_tools`, `call_tool`,
`list_resources`, `read_resource`, `list_prompts`, and `get_prompt` use
`AuthorizedCallValidator`. That validator requires:

```text
SessionId
UserConfig
VirtualHostId
ContextForgeClaims
```

This keeps routing dependent on authenticated user config and the selected
virtual host, not on client-provided backend URLs or hard-coded process state.
