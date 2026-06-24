# Deployment Notes

> Status: draft. To be implemented.

This chapter will describe deployment assumptions and constraints.

## To implement

- front-door routing for `/contextforge-rs/servers/{virtual_host_id}/mcp`
- keeping management/UI traffic on the external ContextForge application
- plain HTTP behind trusted proxy versus downstream TLS
- Redis availability and trust boundary
- backend URL reachability and TLS material
- sticky routing by `Mcp-session-id`
- failover behavior while backend session state is local
- future remote session-store options
- resource sizing and runtime thread choices
