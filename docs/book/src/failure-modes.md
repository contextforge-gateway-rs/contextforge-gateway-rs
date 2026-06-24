# Failure Modes

> Status: draft. To be implemented.

This chapter will collect expected failures and the layer that owns each
response.

## To implement

- missing or malformed bearer token
- unsupported JWT algorithm or missing decoder
- expired token, wrong issuer, or wrong audience
- no user config for JWT subject
- requested virtual host absent from user config
- backend initialization failure
- missing backend transport for an established session
- malformed prefixed tool, resource, or prompt name
- duplicate backend matches and session cleanup
- Redis connection and decode failures
- plugin denial and plugin configuration rejection
- load-balanced request landing on the wrong node
