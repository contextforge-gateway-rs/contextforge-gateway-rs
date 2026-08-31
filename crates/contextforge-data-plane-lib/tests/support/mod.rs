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

#[cfg(feature = "with_tools")]
use std::{path::PathBuf, str::FromStr};

pub(crate) use auth::token;
pub(crate) use client::{
    CLIENT_CONNECT_TIMEOUT, TEST_POLL_INTERVAL, connect_client, connect_client_with_handler,
    connect_client_with_protocol, connect_modern_client, create_client, create_tls_client, modern_client_info,
};
use contextforge_data_plane_lib::{Config, RedisConnectionMode};
pub(crate) use plugin::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, PROMPT_ERROR_MESSAGE, PROMPT_POST_DENY_ERROR_CODE, PromptBehavior,
    PromptTestPlugin, REWRITTEN_PROMPT_RESOURCE, REWRITTEN_PROMPT_TEXT, REWRITTEN_PROMPT_TOPIC, REWRITTEN_SUM_A,
    REWRITTEN_SUM_B, TestPlugin, TestPluginFactory,
};
pub(crate) use plugin_gateway::{
    BACKEND_PROMPT_IMAGE, BACKEND_PROMPT_RESOURCE, RunningGateway, start_gateway, start_gateway_with_events,
    start_gateway_with_json_backend_responses, start_gateway_with_parameter_headers,
};
pub(crate) use runtime::{runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, runtime_with_prompt_plugin};
pub(crate) use test_gateways::{
    ListToolsGatewaySettings, create_gateway_with_four_counters, create_gateway_with_four_legacy_counters,
    create_ports, create_tls_gateway_with_four_tls_counters, plaintext_config,
};
pub(crate) use tool::{error_code, error_parts, sum_request, text};
pub(crate) use user_config_store::MemoryUserConfigStore;

pub fn create_default_config() -> Config {
    Config {
        address: None,
        jwks_url: "http://127.0.0.1:8080/".parse().expect("should work"),
        jwks_ca_cert_path: None,
        enable_open_telemetry: None,
        otlp_endpoint: None,
        otlp_protocol: None,
        otlp_headers: None,
        otlp_service_name: None,
        enable_otel_metrics: None,
        otlp_metrics_endpoint: None,
        mcp_standard_header_max_count: 10,
        mcp_standard_header_max_value_bytes: 4096,
        mcp_standard_header_max_total_bytes: 4096,
        number_of_cpus: None,
        single_runtime: None,
        runtime_plugins_enabled: None,
        tls_address: None,
        server_private_key: None,
        server_certificate: None,
        upstream_connection_mode: None,
        upstream_private_key: None,
        upstream_certificate: None,
        upstream_trust_bundle: None,
        user_config_cache_expiry_seconds: 10,
        redis_address: String::new(),
        redis_port: 0,
        redis_mode: RedisConnectionMode::PlainText,
        redis_tls_trust_bundle: None,
        redis_tls_client_private_key: None,
        redis_tls_client_certificate: None,
        log_name: None,
        log_rotation: None,
        mcp_allowed_origins: None,
        mcp_allowed_hosts: None,

        #[cfg(feature = "with_tools")]
        token_verification_private_key: PathBuf::from_str("./assets/jwt.key").expect("This should work"),
    }
}
