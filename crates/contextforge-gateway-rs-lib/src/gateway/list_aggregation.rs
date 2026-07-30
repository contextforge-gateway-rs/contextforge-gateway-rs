use std::{collections::HashMap, future::Future};

use contextforge_gateway_rs_apis::user_store::VirtualHost;
use rmcp::{
    ErrorData,
    model::{
        ErrorCode, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
        Prompt, Resource, ResourceTemplate, Tool,
    },
};
use tracing::{info, warn};

use super::{
    backend_transports::{McpClientService, ServiceHolder},
    identifier_routing::{exposed_tool_name, prefixed_name},
};

/// Per-backend cursor state encoded as the gateway's opaque cursor token.
/// Only backends that still have pages appear in `backends`; absent == exhausted.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct GatewayCursor {
    pub(super) backends: HashMap<String, String>,
}

/// Decode an incoming gateway cursor (raw JSON, opaque to MCP clients).
/// `None` means first page; an undecodable value returns `-32602 Invalid params`.
pub(super) fn decode_gateway_cursor(raw: Option<&str>) -> Result<GatewayCursor, ErrorData> {
    let Some(raw) = raw else { return Ok(GatewayCursor::default()) };
    serde_json::from_str(raw)
        .map_err(|_| ErrorData::new(ErrorCode::INVALID_PARAMS, "invalid cursor", None))
}

/// Build the next gateway cursor from backends that returned a `next_cursor`.
/// Returns `None` (= no more pages) when all backends are exhausted.
fn encode_next_cursor(backends: HashMap<String, String>) -> Option<String> {
    if backends.is_empty() {
        return None;
    }
    Some(
        serde_json::to_string(&GatewayCursor { backends })
            .expect("GatewayCursor is always serializable"),
    )
}

/// Fans a paginated list request out to every connected backend concurrently, logs each response,
/// and returns the `(backend_name, result)` pairs that succeeded.
pub(super) async fn fan_out_list<R, E, F, Fut, C>(
    backends: Vec<ServiceHolder>,
    op: &str,
    item_count: C,
    call: F,
) -> Vec<(String, R)>
where
    F: Fn(String, McpClientService) -> Fut,
    Fut: Future<Output = Result<R, E>>,
    C: Fn(&R) -> usize,
    E: std::fmt::Debug,
{
    let tasks = backends.into_iter().map(|service_holder| {
        let call = &call;
        async move {
            let response = match service_holder.running_service {
                Some(service) => Some(call(service_holder.name.clone(), service).await),
                None => None,
            };
            (service_holder.name, response)
        }
    });

    futures::future::join_all(tasks)
        .await
        .into_iter()
        .filter_map(|(name, response)| {
            log_backend_response(op, &name, response.as_ref(), &item_count);
            match response {
                Some(Ok(response)) => Some((name, response)),
                _ => None,
            }
        })
        .collect()
}

fn log_backend_response<T, E: std::fmt::Debug>(
    kind: &str,
    name: &str,
    response: Option<&Result<T, E>>,
    item_count: impl Fn(&T) -> usize,
) {
    match response {
        Some(Ok(response)) => info!("{kind}: backend {name} completed ({} items)", item_count(response)),
        Some(Err(error)) => warn!("{kind}: backend {name} {error:?}"),
        None => info!("{kind}: backend {name} unavailable"),
    }
}

pub(super) fn merge_tools(
    tools: Vec<(String, ListToolsResult)>,
    virtual_host: &VirtualHost,
) -> (Vec<Tool>, Option<String>) {
    let mut next_backends = HashMap::new();
    let mut merged = Vec::new();
    for (backend_name, result) in tools {
        if let Some(c) = result.next_cursor {
            next_backends.insert(backend_name.clone(), c);
        }
        for mut tool in result.tools {
            tool.name = exposed_tool_name(virtual_host, &backend_name, &tool.name).into();
            merged.push(tool);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(next_backends))
}

pub(super) fn merge_resources(
    resources: Vec<(String, ListResourcesResult)>,
    namespace_identifiers: bool,
) -> (Vec<Resource>, Option<String>) {
    let mut next_backends = HashMap::new();
    let mut merged = Vec::new();
    for (backend_name, result) in resources {
        if let Some(c) = result.next_cursor {
            next_backends.insert(backend_name.clone(), c);
        }
        for mut resource in result.resources {
            if namespace_identifiers {
                resource.name = prefixed_name(&backend_name, &resource.name);
                resource.uri = prefixed_name(&backend_name, &resource.uri);
            }
            merged.push(resource);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(next_backends))
}

pub(super) fn merge_resource_templates(
    templates: Vec<(String, ListResourceTemplatesResult)>,
    namespace_identifiers: bool,
) -> (Vec<ResourceTemplate>, Option<String>) {
    let mut next_backends = HashMap::new();
    let mut merged = Vec::new();
    for (backend_name, result) in templates {
        if let Some(c) = result.next_cursor {
            next_backends.insert(backend_name.clone(), c);
        }
        for mut template in result.resource_templates {
            if namespace_identifiers {
                template.name = prefixed_name(&backend_name, &template.name);
                template.uri_template = prefixed_name(&backend_name, &template.uri_template);
            }
            merged.push(template);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(next_backends))
}

pub(super) fn merge_prompts(
    prompts: Vec<(String, ListPromptsResult)>,
    namespace_identifiers: bool,
) -> (Vec<Prompt>, Option<String>) {
    let mut next_backends = HashMap::new();
    let mut merged = Vec::new();
    for (backend_name, result) in prompts {
        if let Some(c) = result.next_cursor {
            next_backends.insert(backend_name.clone(), c);
        }
        for mut prompt in result.prompts {
            if namespace_identifiers {
                prompt.name = prefixed_name(&backend_name, &prompt.name);
            }
            merged.push(prompt);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(next_backends))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_virtual_host(backend_id: &str) -> VirtualHost {
        let config_json = serde_json::json!({
            "backends": {
                backend_id: {
                    "name": "backend",
                    "url": "http://upstream:9000/mcp",
                    "transport": "STREAMABLEHTTP",
                    "passthrough_headers": [],
                    "allowed_tool_names": [],
                    "allowed_resource_names": [],
                    "allowed_prompt_names": []
                }
            }
        });
        serde_json::from_value(config_json).expect("valid virtual host")
    }

    #[test]
    fn single_backend_listings_preserve_identifiers() {
        let virtual_host = test_virtual_host("backend-id");
        let (tools, _) = merge_tools(
            vec![(
                "backend-id".to_owned(),
                ListToolsResult::with_all_items(vec![Tool::new("test_simple_text", "", serde_json::Map::new())]),
            )],
            &virtual_host,
        );
        let (prompts, _) = merge_prompts(
            vec![(
                "backend-id".to_owned(),
                ListPromptsResult::with_all_items(vec![Prompt::new("test_prompt", None::<String>, None)]),
            )],
            false,
        );
        let (resources, _) = merge_resources(
            vec![(
                "backend-id".to_owned(),
                ListResourcesResult::with_all_items(vec![Resource::new("test://resource", "test_resource")]),
            )],
            false,
        );
        let (templates, _) = merge_resource_templates(
            vec![(
                "backend-id".to_owned(),
                ListResourceTemplatesResult::with_all_items(vec![ResourceTemplate::new(
                    "test://template/{id}/data",
                    "test_template",
                )]),
            )],
            false,
        );

        assert_eq!("test_simple_text", tools[0].name);
        assert_eq!("test_prompt", prompts[0].name);
        assert_eq!("test_resource", resources[0].name);
        assert_eq!("test://resource", resources[0].uri);
        assert_eq!("test_template", templates[0].name);
        assert_eq!("test://template/{id}/data", templates[0].uri_template);
    }

    #[test]
    fn backend_next_cursor_is_preserved_in_gateway_cursor() {
        let virtual_host = test_virtual_host("b1");
        let mut result =
            ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);
        result.next_cursor = Some("backend-page2".to_owned());

        let (_, next_cursor) = merge_tools(vec![("b1".to_owned(), result)], &virtual_host);
        let raw = next_cursor.expect("should have a next cursor");

        let cursor: GatewayCursor = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(cursor.backends.get("b1").map(String::as_str), Some("backend-page2"));
    }

    #[test]
    fn exhausted_backends_produce_no_next_cursor() {
        let virtual_host = test_virtual_host("b1");
        // next_cursor is None → backend is exhausted
        let result = ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);

        let (_, next_cursor) = merge_tools(vec![("b1".to_owned(), result)], &virtual_host);
        assert!(next_cursor.is_none(), "no cursor when all backends exhausted");
    }

    #[test]
    fn invalid_cursor_returns_invalid_params_error() {
        let err = decode_gateway_cursor(Some("not-json!")).unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn none_cursor_returns_default_empty_gateway_cursor() {
        let cursor = decode_gateway_cursor(None).expect("None is a valid first-page indicator");
        assert!(cursor.backends.is_empty());
    }
}
