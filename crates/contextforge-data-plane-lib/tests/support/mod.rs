#![allow(dead_code, unused_imports, reason = "shared CPEX test fixture is used by separate integration test targets")]

mod auth;
mod client;
pub(crate) mod mock_counter;
pub(crate) mod paginating_mock;
mod plugin;
mod plugin_gateway;
mod runtime;
mod test_gateways;
mod tool;
mod user_config_store;

pub(crate) const TEST_USER_ID: &str = "11111111-1111-1111-1111-111111111111";
pub(crate) const TEST_USER_EMAIL: &str = "admin@example.com";

pub(crate) use auth::token;
pub(crate) use client::{
    CLIENT_CONNECT_TIMEOUT, TEST_POLL_INTERVAL, connect_client, connect_client_with_handler,
    connect_client_with_protocol, connect_modern_client, create_client, create_tls_client, modern_client_info,
};
pub(crate) use plugin::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, PROMPT_ERROR_MESSAGE, PROMPT_POST_DENY_ERROR_CODE, PromptBehavior,
    PromptTestPlugin, REWRITTEN_PROMPT_RESOURCE, REWRITTEN_PROMPT_TEXT, REWRITTEN_PROMPT_TOPIC, REWRITTEN_SUM_A,
    REWRITTEN_SUM_B, TestPlugin, TestPluginFactory,
};
pub(crate) use plugin_gateway::{
    BACKEND_PROMPT_IMAGE, BACKEND_PROMPT_RESOURCE, RunningGateway, start_gateway, start_gateway_with_events,
    start_gateway_with_json_backend_responses,
};
pub(crate) use runtime::{runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, runtime_with_prompt_plugin};
pub(crate) use test_gateways::{
    ListToolsGatewaySettings, create_gateway_with_four_counters,
    create_gateway_with_four_counters_and_enabled_filtering, create_ports, create_tls_gateway_with_four_tls_counters,
    plaintext_config,
};
pub(crate) use tool::{error_code, error_parts, sum_request, text};
pub(crate) use user_config_store::MemoryUserConfigStore;
