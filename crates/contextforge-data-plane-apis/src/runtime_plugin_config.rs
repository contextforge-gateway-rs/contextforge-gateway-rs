use cpex::cpex_core::config::CpexConfig;
use serde::{Deserialize, Serialize};

pub const RUNTIME_PLUGIN_CONFIG_KEY: &str = "ContextForgeGatewayRuntimePluginConfig";
pub const RUNTIME_PLUGIN_CONFIG_VERSION: u8 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimePluginConfigDocument {
    pub version: u8,
    pub revision: String,
    pub cpex: CpexConfig,
}
