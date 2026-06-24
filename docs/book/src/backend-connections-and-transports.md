# Backend Connections And Transports

> Status: draft. To be implemented.

This chapter will describe the three transport classes the gateway keeps
separate.

## To implement

- downstream listener transport: TCP and TLS Axum/Hyper serving
- upstream backend transport: `reqwest` plus RMCP streamable HTTP client transport
- config-store transport: Redis plain, TLS, and mTLS
- upstream TLS-only, plain-or-TLS, plain-or-mTLS, and mTLS-only modes
- host header behavior for HTTPS backend URLs
- where transport security is process config today
- which transport settings should eventually move into runtime config
