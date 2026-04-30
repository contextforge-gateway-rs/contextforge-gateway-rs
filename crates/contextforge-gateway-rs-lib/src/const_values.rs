use std::time::Duration;

pub const LRU_CACHE_ENTRIES: usize = 50_000;
pub const LRU_CACHE_EXPIRY_DURATION: Duration = Duration::from_hours(1);
pub const CONEXT_FORGE_GATEWAY_AUDIENCE: &str = "mcp-audience";
pub const MCP_SESSION_ID: &str = "mcp-session-id";
