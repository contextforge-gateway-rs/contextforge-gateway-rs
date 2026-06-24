# Session Ownership

The current gateway keeps backend MCP sessions in local process memory. That
choice keeps the implementation simple, but it defines what load-balanced
deployments can safely do.

## Session Mapping

One downstream MCP session fans out to one upstream MCP client session per
backend. The backend transport map is keyed by:

```text
principal
backend_name
downstream_session_id
```

The downstream session id alone is not enough because two principals could
present the same value. The backend name is required because the gateway opens
multiple upstream sessions for one downstream session.

## Borrowing Services

`SessionManager` resolves services for a validated principal, session id, and
virtual host. List calls borrow all matching backend services. Routed calls
borrow the target backend service after splitting the prefixed object name.

The transport map stores shared RMCP running services. Callers should treat
service ownership and cleanup as explicit request-path concerns, because a
stale service can affect later calls in the same downstream session.

## Cleanup

On successful downstream `DELETE`, the session middleware removes the user
session mapping and clears backend transports for the same principal and
downstream session id.

The routing code also cleans up backend entries when it detects invalid session
state, such as duplicate backend matches for a single routed call.

## Load Balancing

Because backend sessions are local process state today, a random load-balanced
deployment is not safe for stateful MCP sessions. A later request with the same
`Mcp-session-id` must reach the process that owns the backend services, unless
session state has moved outside the process.

Known deployment options:

- sticky routing by `Mcp-session-id`
- remote session mapping through Redis or another external cache
- active-hot-standby behavior where clients reinitialize after failover

Do not assume any node can serve any stateful MCP session until backend session
state has an external owner.
