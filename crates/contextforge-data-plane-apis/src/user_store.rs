use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub enum IntegrationType {
    #[serde(rename = "REST")]
    Rest,
    #[default]
    #[serde(rename = "MCP")]
    Mcp,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BackendMCPGateway {
    pub name: String,
    pub url: url::Url,
    pub mcp_protocol_version: rmcp::model::ProtocolVersion,
    /// Header names copied from the downstream request onto the upstream connection.
    pub passthrough_headers: Vec<String>,
    /// Static headers injected onto the upstream connection (override passthrough).
    #[serde(default)]
    pub add_headers: HashMap<String, String>,
    /// Header names stripped from the upstream connection (applied last).
    #[serde(default)]
    pub remove_headers: Vec<String>,
    #[serde(default)]
    pub completion: HashMap<String, String>,
    /// Input schemas keyed by the original upstream tool name.
    pub tool_schemas: HashMap<String, serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ServiceRoute {
    pub backend_name: String,
    pub upstream_name: String,
}

pub type DownstreamBackendName = String;
pub type DownstreamToolName = String;
pub type DownstreamResourceName = String;
pub type DownstreamResourceTemplateName = String;
pub type DownstreamPromptName = String;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct VirtualHost {
    pub backends: HashMap<DownstreamBackendName, BackendMCPGateway>,
    #[serde(default)]
    pub tools: HashMap<DownstreamToolName, ServiceRoute>,
    #[serde(default)]
    pub resources: HashMap<DownstreamResourceName, ServiceRoute>,
    #[serde(default)]
    pub resource_templates: HashMap<DownstreamResourceTemplateName, ServiceRoute>,
    #[serde(default)]
    pub prompts: HashMap<DownstreamPromptName, ServiceRoute>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UserConfig {
    pub virtual_hosts: HashMap<String, VirtualHost>,
}
