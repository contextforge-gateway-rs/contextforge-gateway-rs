use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, Eq)]
pub struct NameAlias {
    downstream_prefixed_name: String,
    upstream_name: String,
}

impl PartialEq for NameAlias {
    fn eq(&self, other: &Self) -> bool {
        self.downstream_prefixed_name == other.downstream_prefixed_name
    }
}

impl std::hash::Hash for NameAlias {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.downstream_prefixed_name.hash(state);
    }
}

impl NameAlias {
    pub fn new(downstream_prefixed_name: String, upstream_name: String) -> Self {
        Self { downstream_prefixed_name, upstream_name }
    }
    pub fn with_downstream_prefixed_name(downstream_prefixed_name: String) -> Self {
        NameAlias { downstream_prefixed_name, upstream_name: String::new() }
    }
    pub fn get_upstream_name(&self) -> &str {
        &self.upstream_name
    }

    pub fn get_downstream_prefixed_name(&self) -> &str {
        &self.downstream_prefixed_name
    }
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
    pub tool_name_aliases: HashSet<NameAlias>,
    #[serde(default)]
    pub resource_uri_aliases: HashSet<NameAlias>,
    #[serde(default)]
    pub prompt_name_aliases: HashSet<NameAlias>,
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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct VirtualHost {
    pub backends: HashMap<String, BackendMCPGateway>,
    #[serde(default)]
    pub tools: HashMap<String, ServiceRoute>,
    #[serde(default)]
    pub resources: HashMap<String, ServiceRoute>,
    #[serde(default)]
    pub resource_templates: HashMap<String, ServiceRoute>,
    #[serde(default)]
    pub prompts: HashMap<String, ServiceRoute>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UserConfig {
    pub virtual_hosts: HashMap<String, VirtualHost>,
}
