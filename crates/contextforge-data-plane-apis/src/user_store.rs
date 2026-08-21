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
    /// Header names copied from the downstream request onto the upstream connection.
    pub passthrough_headers: Vec<String>,
    /// Static headers injected onto the upstream connection (override passthrough).
    #[serde(default)]
    pub add_headers: HashMap<String, String>,
    /// Header names stripped from the upstream connection (applied last).
    #[serde(default)]
    pub remove_headers: Vec<String>,
    #[serde(default)]
    pub tool_name_aliases: HashMap<String, String>,
    #[serde(default)]
    pub resource_name_aliases: HashMap<String, String>,
    #[serde(default)]
    pub prompt_name_aliases: HashMap<String, String>,
    #[serde(default)]
    pub completion: HashMap<String, String>,

    pub allowed_resource_names: Vec<String>,
    pub allowed_prompt_names: Vec<String>,
    pub allowed_tool_names: Vec<String>,
    /// Input schemas keyed by the original upstream tool name.
    #[serde(default)]
    pub tool_schemas: HashMap<String, serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct VirtualHost {
    pub backends: HashMap<String, BackendMCPGateway>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UserConfig {
    pub virtual_hosts: HashMap<String, VirtualHost>,
}
