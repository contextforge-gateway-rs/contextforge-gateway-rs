mod backend_client;

mod identifier_routing;

mod mcp_call_validator;
mod mcp_service;

pub(crate) use identifier_routing::resolve_tool_route;
pub use mcp_service::McpService;
