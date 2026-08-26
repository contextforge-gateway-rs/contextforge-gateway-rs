use contextforge_data_plane_apis::user_store::BackendMCPGateway;

#[test]
fn backend_config_without_resource_uri_allowlist_defaults_to_empty() {
    let backend: BackendMCPGateway = serde_json::from_value(serde_json::json!({
        "name": "backend",
        "url": "https://backend.example/mcp",
        "passthrough_headers": [],
        "allowed_resource_names": [],
        "allowed_prompt_names": [],
        "allowed_tool_names": []
    }))
    .expect("legacy backend config should deserialize");

    assert!(backend.allowed_resource_uris.is_empty());
}
