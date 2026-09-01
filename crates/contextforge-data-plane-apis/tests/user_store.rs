use contextforge_data_plane_apis::user_store::BackendMCPGateway;
use serde_json::json;

#[test]
fn backend_config_without_tool_schemas_defaults_to_empty_map() {
    let config: BackendMCPGateway = serde_json::from_value(json!({
        "name": "backend",
        "url": "http://localhost:8000/mcp",
        "mcp_protocol_version": "2026_07_28",
        "passthrough_headers": []
    }))
    .expect("backend config without tool schemas should deserialize");

    assert!(config.tool_schemas.is_empty());
}
