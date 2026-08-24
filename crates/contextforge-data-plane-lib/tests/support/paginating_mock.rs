#![allow(dead_code, reason = "shared test fixture used by separate integration test targets")]

use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, model::*, service::RequestContext};

/// Backend cursor the mock uses to signal "page 2 available".
const PAGE2_CURSOR: &str = "page2";

/// A minimal backend that returns tools across two pages.
///
/// Page 1: `tool_alpha`, `tool_beta`  →  `next_cursor = Some("page2")`
/// Page 2: `tool_gamma`               →  `next_cursor = None`
pub struct PaginatingServer;

impl PaginatingServer {
    pub fn page1_tools() -> Vec<Tool> {
        vec![
            Tool::new("tool_alpha", "Alpha tool", serde_json::Map::new()),
            Tool::new("tool_beta", "Beta tool", serde_json::Map::new()),
        ]
    }

    pub fn page2_tools() -> Vec<Tool> {
        vec![Tool::new("tool_gamma", "Gamma tool", serde_json::Map::new())]
    }

    /// Sorted union of all tool names across both pages.
    pub fn all_tool_names() -> &'static [&'static str] {
        &["tool_alpha", "tool_beta", "tool_gamma"]
    }
}

impl ServerHandler for PaginatingServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> {
        std::future::ready(if request.as_ref().and_then(|r| r.cursor.as_deref()) == Some(PAGE2_CURSOR) {
            Ok(ListToolsResult::with_all_items(Self::page2_tools()))
        } else {
            let mut result = ListToolsResult::with_all_items(Self::page1_tools());
            result.next_cursor = Some(PAGE2_CURSOR.to_owned());
            Ok(result)
        })
    }
}
