# Runtime Configuration

Runtime configuration is the routing source of truth. The control plane owns
the durable model. The gateway consumes the model through `UserConfigStore` and
applies it on the request path.

## Model

The API crate defines the shared config shape:

```text
UserConfig
  virtual_hosts: HashMap<virtual_host_id, VirtualHost>

VirtualHost
  backends: HashMap<backend_name, BackendMCPGateway>

BackendMCPGateway
  name
  url
  transport
  passthrough_headers
  allowed_tool_names
  allowed_resource_names
  allowed_prompt_names
```

Today, the MCP dataplane primarily uses the backend map key and backend URL.
The remaining fields show the intended expansion points: transport selection,
header policy, tool/resource/prompt filters, and richer routing policy.

## Selection

Selection has two dimensions:

```text
JWT subject -> UserConfig
path virtual_host_id -> one VirtualHost inside that UserConfig
```

The JWT subject chooses the user config. The path chooses one virtual host
inside that config. The gateway should not select a backend before both facts
are known.

## Redis Adapter

The current `RedisUserConfigStore` implementation stores keys and values as
MessagePack. It encodes `User::new(jwt_subject)` as the Redis key and decodes a
`UserConfig` from the value.

The store also keeps an in-process LRU cache of decoded configs. This reduces
Redis reads on the hot path, but Redis remains an adapter detail. Routing code
should depend on `UserConfigStore` and `UserConfig`, not on Redis commands or
MessagePack serialization.

## Expected Growth

The config model is expected to grow around enforcement, not management:

- route selection across multiple MCP endpoints
- principal and virtual-host filters for tools, resources, and prompts
- backend auth and TLS material references
- request and response header pass/add/remove rules
- plugin and CPEX hook settings
- pagination and SSE behavior where protocol handling needs config
- future A2A and LLM routing/provider settings

The durable authoring workflow for those settings remains outside this repo.
