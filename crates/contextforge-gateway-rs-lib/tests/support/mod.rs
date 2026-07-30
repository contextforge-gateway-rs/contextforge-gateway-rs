#![allow(dead_code, unused_imports, reason = "shared CPEX test fixture is used by separate integration test targets")]

mod auth;
mod client;
mod list_tools_gateway;
pub(crate) mod mock_counter;
pub(crate) mod paginating_mock;
mod plugin;
mod plugin_gateway;
mod runtime;
mod tool;
mod user_config_store;

pub(crate) const TEST_USER_ID: &str = "11111111-1111-1111-1111-111111111111";
pub(crate) const TEST_USER_EMAIL: &str = "admin@example.com";

pub(crate) use auth::token;
pub(crate) use client::{
    CLIENT_CONNECT_TIMEOUT, TEST_POLL_INTERVAL, connect_client, connect_client_with_handler, create_client,
    create_tls_client,
};
pub(crate) use list_tools_gateway::{
    ListToolsGatewaySettings, create_gateway_with_four_counters, create_ports,
    create_tls_gateway_with_four_tls_counters, plaintext_config,
};
pub(crate) use plugin::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, PROMPT_POST_DENY_ERROR_CODE, PROMPT_PRE_DENY_ERROR_CODE, PromptBehavior,
    PromptTestPlugin, PromptTestPluginFactory, RESOURCE_POST_DENY_ERROR_CODE, RESOURCE_PRE_DENY_ERROR_CODE,
    REWRITTEN_PROMPT_TOPIC, REWRITTEN_RESOURCE_URI, REWRITTEN_SUM_A, REWRITTEN_SUM_B, ResourceBehavior,
    ResourceTestPlugin, ResourceTestPluginFactory, TestPlugin, TestPluginFactory,
};
pub(crate) use plugin_gateway::{
    COMPLETION_VALUES, PROMPT_DESCRIPTION, RESOURCE_DESCRIPTION, RESOURCE_TEXT, RESOURCE_URI, RunningGateway,
    TEMPLATE_DESCRIPTION, TEMPLATE_URI, start_gateway, start_gateway_with_json_backend_responses,
};
pub(crate) use runtime::{
    runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, runtime_with_prompt_plugin,
    runtime_with_resource_plugin,
};
pub(crate) use tool::{error_code, sum_request, text};
pub(crate) use user_config_store::MemoryUserConfigStore;
