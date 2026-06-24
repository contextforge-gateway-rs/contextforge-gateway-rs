# Authentication And User Config Lookup

> Status: draft. To be implemented.

This chapter will explain the request identity boundary. Readers should leave
knowing how a bearer token becomes a selected `UserConfig`.

## To implement

- accepted JWT algorithms and configured decoder keys
- issuer, audience, and expiration validation
- `ContextForgeClaims` fields that matter to routing
- JWT subject as `User::new(subject)` config key
- Redis MessagePack lookup and in-process LRU cache
- HTTP error behavior for missing token, bad token, missing config, and Redis errors
- request extensions inserted before MCP handlers run
