use std::{
    collections::{HashMap, HashSet},
    future::Future,
};

use contextforge_data_plane_apis::user_store::VirtualHost;
use contextforge_data_plane_observability::PerformanceTimer;
use rmcp::{
    ErrorData,
    model::{
        ErrorCode, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, Prompt,
        Resource, ResourceTemplate, Tool,
    },
};
use tracing::{info, warn};

use super::{
    backend_transports::{McpClientService, ServiceHolder},
    identifier_routing::{exposed_tool_name, prefixed_name},
};

/// Per-backend cursor state encoded as the gateway's opaque cursor token.
///
/// `op` names the list operation that issued this cursor (e.g. `"list_tools"`); a cursor
/// presented to a different operation is rejected with `-32602`.
///
/// Only backends that still have pages **or that failed mid-page** appear in `backends`.
/// Backends that returned `next_cursor = None` are absent (truly exhausted).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct GatewayCursor {
    pub(super) op: String,
    pub(super) backends: HashMap<String, String>,
}

impl Default for GatewayCursor {
    /// Returns the first-page sentinel. `op` is empty; it is never validated because
    /// `decode_gateway_cursor` returns this value only when `raw` is `None`.
    fn default() -> Self {
        Self { op: String::new(), backends: HashMap::new() }
    }
}

/// Decode an incoming gateway cursor (raw JSON, opaque to MCP clients).
/// `None` means first page; an undecodable value returns `-32602 Invalid params`.
/// `expected_op` must match the `op` field stored in the cursor, preventing a cursor
/// issued by one list operation from being accepted by a different one.
pub(super) fn decode_gateway_cursor(raw: Option<&str>, expected_op: &str) -> Result<GatewayCursor, ErrorData> {
    let Some(raw) = raw else { return Ok(GatewayCursor::default()) };
    let cursor: GatewayCursor =
        serde_json::from_str(raw).map_err(|_| ErrorData::new(ErrorCode::INVALID_PARAMS, "invalid cursor", None))?;
    if cursor.op != expected_op {
        return Err(ErrorData::new(ErrorCode::INVALID_PARAMS, "cursor operation mismatch", None));
    }
    Ok(cursor)
}

/// Build the next gateway cursor from backends that still have pages.
/// Returns `None` (= no more pages) when all backends are exhausted.
fn encode_next_cursor(op: &str, backends: HashMap<String, String>) -> Option<String> {
    if backends.is_empty() {
        return None;
    }
    Some(
        serde_json::to_string(&GatewayCursor { op: op.to_owned(), backends })
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
                Some(service) => {
                    let mut timer = PerformanceTimer::external_call("Routing", op);
                    let response = call(service_holder.name.clone(), service).await;
                    timer.record_result(&response);
                    Some(response)
                },
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
        Some(Ok(response)) => info!(
            component = "Routing",
            operation = kind,
            backend_name = name,
            item_count = item_count(response),
            "backend list request completed"
        ),
        Some(Err(error)) => warn!(
            component = "Routing",
            operation = kind,
            backend_name = name,
            error = ?error,
            "backend list request failed"
        ),
        None => {
            info!(component = "Routing", operation = kind, backend_name = name, "backend unavailable for list request");
        },
    }
}

/// Merges tool listings from multiple backends into a single sorted list.
///
/// `op` is the list operation name used to tag the outgoing cursor.
/// `incoming_cursor` is used to re-emit the prior cursor position for any backend
/// that was expected to continue but failed on this page (transient failure ≠ exhaustion).
pub(super) fn merge_tools(
    tools: Vec<(String, ListToolsResult)>,
    virtual_host: &VirtualHost,
    incoming_cursor: &GatewayCursor,
    op: &str,
) -> (Vec<Tool>, Option<String>) {
    let mut next_backends: HashMap<String, String> = HashMap::new();
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut merged = Vec::new();
    for (backend_name, result) in &tools {
        succeeded.insert(backend_name.as_str());
        if let Some(c) = &result.next_cursor {
            next_backends.insert(backend_name.clone(), c.clone());
        }
    }
    // Preserve cursor for backends that were expected to continue but failed this page.
    for (backend_name, prior_cursor) in &incoming_cursor.backends {
        if !succeeded.contains(backend_name.as_str()) {
            next_backends.entry(backend_name.clone()).or_insert_with(|| prior_cursor.clone());
        }
    }
    for (backend_name, result) in tools {
        for mut tool in result.tools {
            tool.name = exposed_tool_name(virtual_host, &backend_name, &tool.name).into();
            merged.push(tool);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(op, next_backends))
}

/// Merges resource listings from multiple backends into a single sorted list.
/// See `merge_tools` for the `incoming_cursor` / `op` contract.
pub(super) fn merge_resources(
    resources: Vec<(String, ListResourcesResult)>,
    namespace_identifiers: bool,
    incoming_cursor: &GatewayCursor,
    op: &str,
) -> (Vec<Resource>, Option<String>) {
    let mut next_backends: HashMap<String, String> = HashMap::new();
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut merged = Vec::new();
    for (backend_name, result) in &resources {
        succeeded.insert(backend_name.as_str());
        if let Some(c) = &result.next_cursor {
            next_backends.insert(backend_name.clone(), c.clone());
        }
    }
    for (backend_name, prior_cursor) in &incoming_cursor.backends {
        if !succeeded.contains(backend_name.as_str()) {
            next_backends.entry(backend_name.clone()).or_insert_with(|| prior_cursor.clone());
        }
    }
    for (backend_name, result) in resources {
        for mut resource in result.resources {
            if namespace_identifiers {
                resource.name = prefixed_name(&backend_name, &resource.name);
                resource.uri = prefixed_name(&backend_name, &resource.uri);
            }
            merged.push(resource);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(op, next_backends))
}

/// Merges resource-template listings from multiple backends into a single sorted list.
/// See `merge_tools` for the `incoming_cursor` / `op` contract.
pub(super) fn merge_resource_templates(
    templates: Vec<(String, ListResourceTemplatesResult)>,
    namespace_identifiers: bool,
    incoming_cursor: &GatewayCursor,
    op: &str,
) -> (Vec<ResourceTemplate>, Option<String>) {
    let mut next_backends: HashMap<String, String> = HashMap::new();
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut merged = Vec::new();
    for (backend_name, result) in &templates {
        succeeded.insert(backend_name.as_str());
        if let Some(c) = &result.next_cursor {
            next_backends.insert(backend_name.clone(), c.clone());
        }
    }
    for (backend_name, prior_cursor) in &incoming_cursor.backends {
        if !succeeded.contains(backend_name.as_str()) {
            next_backends.entry(backend_name.clone()).or_insert_with(|| prior_cursor.clone());
        }
    }
    for (backend_name, result) in templates {
        for mut template in result.resource_templates {
            if namespace_identifiers {
                template.name = prefixed_name(&backend_name, &template.name);
                template.uri_template = prefixed_name(&backend_name, &template.uri_template);
            }
            merged.push(template);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(op, next_backends))
}

/// Merges prompt listings from multiple backends into a single sorted list.
/// See `merge_tools` for the `incoming_cursor` / `op` contract.
pub(super) fn merge_prompts(
    prompts: Vec<(String, ListPromptsResult)>,
    namespace_identifiers: bool,
    incoming_cursor: &GatewayCursor,
    op: &str,
) -> (Vec<Prompt>, Option<String>) {
    let mut next_backends: HashMap<String, String> = HashMap::new();
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut merged = Vec::new();
    for (backend_name, result) in &prompts {
        succeeded.insert(backend_name.as_str());
        if let Some(c) = &result.next_cursor {
            next_backends.insert(backend_name.clone(), c.clone());
        }
    }
    for (backend_name, prior_cursor) in &incoming_cursor.backends {
        if !succeeded.contains(backend_name.as_str()) {
            next_backends.entry(backend_name.clone()).or_insert_with(|| prior_cursor.clone());
        }
    }
    for (backend_name, result) in prompts {
        for mut prompt in result.prompts {
            if namespace_identifiers {
                prompt.name = prefixed_name(&backend_name, &prompt.name);
            }
            merged.push(prompt);
        }
    }
    merged.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    (merged, encode_next_cursor(op, next_backends))
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
            &GatewayCursor::default(),
            "list_tools",
        );
        let (prompts, _) = merge_prompts(
            vec![(
                "backend-id".to_owned(),
                ListPromptsResult::with_all_items(vec![Prompt::new("test_prompt", None::<String>, None)]),
            )],
            false,
            &GatewayCursor::default(),
            "list_prompts",
        );
        let (resources, _) = merge_resources(
            vec![(
                "backend-id".to_owned(),
                ListResourcesResult::with_all_items(vec![Resource::new("test://resource", "test_resource")]),
            )],
            false,
            &GatewayCursor::default(),
            "list_resources",
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
            &GatewayCursor::default(),
            "list_resource_templates",
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
        let mut result = ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);
        result.next_cursor = Some("backend-page2".to_owned());

        let (_, next_cursor) =
            merge_tools(vec![("b1".to_owned(), result)], &virtual_host, &GatewayCursor::default(), "list_tools");
        let raw = next_cursor.expect("should have a next cursor");

        let cursor: GatewayCursor = serde_json::from_str(&raw).expect("valid JSON");
        assert_eq!(cursor.op, "list_tools");
        assert_eq!(cursor.backends.get("b1").map(String::as_str), Some("backend-page2"));
    }

    #[test]
    fn exhausted_backends_produce_no_next_cursor() {
        let virtual_host = test_virtual_host("b1");
        // next_cursor is None → backend is exhausted
        let result = ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);

        let (_, next_cursor) =
            merge_tools(vec![("b1".to_owned(), result)], &virtual_host, &GatewayCursor::default(), "list_tools");
        assert!(next_cursor.is_none(), "no cursor when all backends exhausted");
    }

    #[test]
    fn invalid_cursor_returns_invalid_params_error() {
        let err = decode_gateway_cursor(Some("not-json!"), "list_tools").unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn none_cursor_returns_default_empty_gateway_cursor() {
        let cursor = decode_gateway_cursor(None, "list_tools").expect("None is a valid first-page indicator");
        assert!(cursor.backends.is_empty());
    }

    // Finding 1: a backend that fails mid-page keeps its prior cursor position so the
    // client can reach its remaining items on the next page.
    #[test]
    fn failed_backend_cursor_preserved_across_page() {
        let virtual_host = test_virtual_host("b1");
        // Simulate an incoming cursor where both b1 and b2 had more pages.
        let incoming = GatewayCursor {
            op: "list_tools".to_owned(),
            backends: [("b1".to_owned(), "b1-page2".to_owned()), ("b2".to_owned(), "b2-page2".to_owned())].into(),
        };
        // b1 succeeds and is done (no next_cursor); b2 fails (absent from results).
        let b1_result = ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);
        let (_, next_cursor) = merge_tools(vec![("b1".to_owned(), b1_result)], &virtual_host, &incoming, "list_tools");

        let raw = next_cursor.expect("b2 failure must produce a next cursor to retry");
        let cursor: GatewayCursor = serde_json::from_str(&raw).expect("valid JSON");
        assert!(!cursor.backends.contains_key("b1"), "b1 exhausted itself — must not appear");
        assert_eq!(cursor.backends.get("b2").map(String::as_str), Some("b2-page2"), "b2 prior cursor preserved");
    }

    // Finding 2: a cursor issued by one list operation must be rejected when presented
    // to a different operation, returning -32602.
    #[test]
    fn cross_operation_cursor_rejected() {
        let virtual_host = test_virtual_host("b1");
        let mut result = ListToolsResult::with_all_items(vec![Tool::new("t1", "", serde_json::Map::new())]);
        result.next_cursor = Some("page2".to_owned());
        let (_, next_cursor) =
            merge_tools(vec![("b1".to_owned(), result)], &virtual_host, &GatewayCursor::default(), "list_tools");
        let raw = next_cursor.expect("cursor present");

        // Presenting a list_tools cursor to list_prompts must fail.
        let err = decode_gateway_cursor(Some(&raw), "list_prompts").unwrap_err();
        assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        // Presenting it to its own operation succeeds.
        assert!(decode_gateway_cursor(Some(&raw), "list_tools").is_ok());
    }
}
