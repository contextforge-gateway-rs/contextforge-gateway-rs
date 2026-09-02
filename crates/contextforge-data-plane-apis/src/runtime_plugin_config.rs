use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use cpex::cpex_core::{
    config::{CpexConfig, PluginSettings},
    plugin::{OnError, PluginConfig, PluginMode},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const RUNTIME_PLUGIN_CONFIG_KEY: &str = "ContextForgeGatewayRuntimePluginConfig";
pub const RUNTIME_PLUGIN_CONFIG_VERSION: u8 = 3;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct RuntimeRevision(String);

impl RuntimeRevision {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RuntimeRevision {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err("runtime revision must not be empty".to_owned());
        }
        Ok(Self(value))
    }
}

impl From<RuntimeRevision> for String {
    fn from(value: RuntimeRevision) -> Self {
        value.0
    }
}

impl fmt::Display for RuntimeRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct RuntimePluginName(String);

impl RuntimePluginName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RuntimePluginName {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err("runtime plugin name must not be empty".to_owned());
        }
        Ok(Self(value))
    }
}

impl From<RuntimePluginName> for String {
    fn from(value: RuntimePluginName) -> Self {
        value.0
    }
}

impl fmt::Display for RuntimePluginName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Atomically published catalog of immutable runtime snapshots.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePluginConfigDocument {
    pub version: u8,
    pub snapshots: HashMap<RuntimeRevision, RuntimePluginSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RuntimePluginSnapshotWire")]
pub struct RuntimePluginSnapshot {
    plugins: Vec<RuntimePluginDefinition>,
    plugin_settings: RuntimePluginSettings,
}

impl RuntimePluginSnapshot {
    pub fn into_cpex(self) -> CpexConfig {
        CpexConfig {
            plugins: self.plugins.into_iter().map(RuntimePluginDefinition::into_cpex).collect(),
            plugin_settings: self.plugin_settings.into_cpex(),
            ..Default::default()
        }
    }

    fn new(plugins: Vec<RuntimePluginDefinition>, plugin_settings: RuntimePluginSettings) -> Result<Self, String> {
        let mut names = HashSet::new();
        if plugins.iter().any(|plugin| !names.insert(plugin.name.as_str())) {
            return Err("runtime snapshot contains duplicate plugin names".to_owned());
        }
        Ok(Self { plugins, plugin_settings })
    }
}

impl TryFrom<CpexConfig> for RuntimePluginSnapshot {
    type Error = String;

    fn try_from(config: CpexConfig) -> Result<Self, Self::Error> {
        if config.routing_enabled()
            || config.plugin_settings.fail_on_plugin_error
            || config.plugin_settings.parallel_execution_within_band
            || !config.routes.is_empty()
            || !config.plugin_dirs.is_empty()
            || !config.global.policies.is_empty()
            || !config.global.defaults.is_empty()
            || config.global.identity.is_some()
        {
            return Err("CPEX config uses fields unsupported by the dataplane".to_owned());
        }

        let plugins =
            config.plugins.into_iter().map(RuntimePluginDefinition::try_from).collect::<Result<Vec<_>, _>>()?;
        Self::new(plugins, RuntimePluginSettings::from_cpex(&config.plugin_settings))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePluginSnapshotWire {
    #[serde(default)]
    plugins: Vec<RuntimePluginDefinition>,
    #[serde(default)]
    plugin_settings: RuntimePluginSettings,
}

impl TryFrom<RuntimePluginSnapshotWire> for RuntimePluginSnapshot {
    type Error = String;

    fn try_from(value: RuntimePluginSnapshotWire) -> Result<Self, Self::Error> {
        Self::new(value.plugins, value.plugin_settings)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RuntimePluginDefinitionWire")]
struct RuntimePluginDefinition {
    name: RuntimePluginName,
    kind: String,
    description: Option<String>,
    author: Option<String>,
    version: Option<String>,
    hooks: Vec<String>,
    mode: PluginMode,
    priority: i32,
    on_error: OnError,
    capabilities: HashSet<String>,
    tags: Vec<String>,
    config: Option<serde_json::Value>,
}

impl RuntimePluginDefinition {
    fn into_cpex(self) -> PluginConfig {
        PluginConfig {
            name: self.name.into(),
            kind: self.kind,
            description: self.description,
            author: self.author,
            version: self.version,
            hooks: self.hooks,
            mode: self.mode,
            priority: self.priority,
            on_error: self.on_error,
            capabilities: self.capabilities,
            tags: self.tags,
            conditions: Vec::new(),
            config: self.config,
        }
    }
}

impl TryFrom<PluginConfig> for RuntimePluginDefinition {
    type Error = String;

    fn try_from(plugin: PluginConfig) -> Result<Self, Self::Error> {
        if !plugin.conditions.is_empty() {
            return Err("CPEX plugin conditions are unsupported by the dataplane".to_owned());
        }
        Self::try_from(RuntimePluginDefinitionWire {
            name: plugin.name,
            kind: plugin.kind,
            description: plugin.description,
            author: plugin.author,
            version: plugin.version,
            hooks: plugin.hooks,
            mode: plugin.mode,
            priority: plugin.priority,
            on_error: plugin.on_error,
            capabilities: plugin.capabilities,
            tags: plugin.tags,
            config: plugin.config,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePluginDefinitionWire {
    name: String,
    kind: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    hooks: Vec<String>,
    #[serde(default)]
    mode: PluginMode,
    #[serde(default = "default_priority")]
    priority: i32,
    #[serde(default)]
    on_error: OnError,
    #[serde(default)]
    capabilities: HashSet<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

impl TryFrom<RuntimePluginDefinitionWire> for RuntimePluginDefinition {
    type Error = String;

    fn try_from(value: RuntimePluginDefinitionWire) -> Result<Self, Self::Error> {
        if value.kind.trim().is_empty() {
            return Err("runtime plugin kind must not be empty".to_owned());
        }
        Ok(Self {
            name: value.name.try_into()?,
            kind: value.kind,
            description: value.description,
            author: value.author,
            version: value.version,
            hooks: value.hooks,
            mode: value.mode,
            priority: value.priority,
            on_error: value.on_error,
            capabilities: value.capabilities,
            tags: value.tags,
            config: value.config,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePluginSettings {
    #[serde(default = "default_plugin_timeout")]
    plugin_timeout: u64,
    #[serde(default = "default_true")]
    short_circuit_on_deny: bool,
    #[serde(default = "default_route_cache_max_entries")]
    route_cache_max_entries: usize,
}

impl Default for RuntimePluginSettings {
    fn default() -> Self {
        Self {
            plugin_timeout: default_plugin_timeout(),
            short_circuit_on_deny: true,
            route_cache_max_entries: default_route_cache_max_entries(),
        }
    }
}

impl RuntimePluginSettings {
    fn from_cpex(settings: &PluginSettings) -> Self {
        Self {
            plugin_timeout: settings.plugin_timeout,
            short_circuit_on_deny: settings.short_circuit_on_deny,
            route_cache_max_entries: settings.route_cache_max_entries,
        }
    }

    fn into_cpex(self) -> PluginSettings {
        PluginSettings {
            plugin_timeout: self.plugin_timeout,
            short_circuit_on_deny: self.short_circuit_on_deny,
            route_cache_max_entries: self.route_cache_max_entries,
            ..Default::default()
        }
    }
}

fn default_priority() -> i32 {
    100
}

fn default_plugin_timeout() -> u64 {
    30
}

fn default_true() -> bool {
    true
}

fn default_route_cache_max_entries() -> usize {
    10_000
}
