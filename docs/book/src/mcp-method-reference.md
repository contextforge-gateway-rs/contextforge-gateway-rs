# MCP Method Reference

> Status: draft. To be implemented.

This chapter will be the gateway's MCP behavior reference. It should be short,
table-driven, and precise.

## To implement

- `initialize`: required context, backend fanout, stored session state, merged capabilities
- `ping`: current pass-through behavior
- `list_tools`: fanout and prefixed merge behavior
- `call_tool`: prefix split, plugin pre hook, upstream call, plugin post hook
- `list_resources` and `read_resource`: fanout and exact-backend routing
- `list_prompts` and `get_prompt`: fanout and exact-backend routing
- `list_resource_templates`, `subscribe`, `unsubscribe`, and `complete`: current support level
- `DELETE`: downstream session cleanup behavior
- current pagination and streaming gaps
