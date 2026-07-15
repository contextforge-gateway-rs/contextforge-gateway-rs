use contextforge_gateway_rs_apis::user_store::VirtualHost;
use rmcp::model::{
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, Prompt, Resource,
    ResourceTemplate, Tool,
};
use tracing::{info, warn};

use super::{
    backend_transports::{McpClientService, ServiceHolder},
    identifier_routing::{exposed_tool_name, prefixed_name},
};

/// Fans a paginated list request out to every connected backend concurrently, logs each response,
/// and returns the `(backend_name, result)` pairs that succeeded.
pub(super) async fn fan_out_list<R, E, F, Fut, C>(
    backends: Vec<ServiceHolder>,
    op: &str,
    item_count: C,
    call: F,
) -> Vec<(String, R)>
where
    F: Fn(McpClientService) -> Fut,
    Fut: std::future::Future<Output = Result<R, E>>,
    C: Fn(&R) -> usize,
    E: std::fmt::Debug,
{
    let tasks = backends.into_iter().map(|service_holder| {
        let call = &call;
        async move {
            let response = match service_holder.running_service {
                Some(service) => Some(call(service).await),
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

pub(super) fn merge_tools(tools: Vec<(String, ListToolsResult)>, virtual_host: &VirtualHost) -> Vec<Tool> {
    let mut tools = tools
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result
                .tools
                .into_iter()
                .map(|mut tool| {
                    tool.name = exposed_tool_name(virtual_host, &backend_name, &tool.name).into();
                    tool
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    tools.sort_unstable_by(|tool, other| tool.name.cmp(&other.name));
    tools
}

pub(super) fn merge_resources(
    resources: Vec<(String, ListResourcesResult)>,
    namespace_identifiers: bool,
) -> Vec<Resource> {
    let mut resources = resources
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result
                .resources
                .into_iter()
                .map(|mut resource| {
                    if namespace_identifiers {
                        resource.name = prefixed_name(&backend_name, &resource.name);
                        resource.uri = prefixed_name(&backend_name, &resource.uri);
                    }
                    resource
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    resources.sort_unstable_by(|resource, other| resource.name.cmp(&other.name));
    resources
}

pub(super) fn merge_resource_templates(
    templates: Vec<(String, ListResourceTemplatesResult)>,
    namespace_identifiers: bool,
) -> Vec<ResourceTemplate> {
    let mut templates = templates
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result.resource_templates.into_iter().map(move |mut template| {
                if namespace_identifiers {
                    template.name = prefixed_name(&backend_name, &template.name);
                    template.uri_template = prefixed_name(&backend_name, &template.uri_template);
                }
                template
            })
        })
        .collect::<Vec<_>>();
    templates.sort_unstable_by(|template, other| template.name.cmp(&other.name));
    templates
}

pub(super) fn merge_prompts(prompts: Vec<(String, ListPromptsResult)>, namespace_identifiers: bool) -> Vec<Prompt> {
    let mut prompts = prompts
        .into_iter()
        .flat_map(|(backend_name, result)| {
            result.prompts.into_iter().map(move |mut prompt| {
                if namespace_identifiers {
                    prompt.name = prefixed_name(&backend_name, &prompt.name);
                }
                prompt
            })
        })
        .collect::<Vec<_>>();
    prompts.sort_unstable_by(|prompt, other| prompt.name.cmp(&other.name));
    prompts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_backend_listings_preserve_identifiers() {
        let config_json = serde_json::json!({
            "backends": {
                "backend-id": {
                    "name": "backend",
                    "url": "http://upstream:9000/mcp",
                    "transport": "STREAMABLEHTTP",
                    "passthrough_headers": [],
                    "allowed_tool_names": ["test_simple_text"],
                    "allowed_resource_names": [],
                    "allowed_prompt_names": []
                }
            }
        });
        let virtual_host: VirtualHost = serde_json::from_value(config_json).expect("valid virtual host");
        let tools = merge_tools(
            vec![(
                "backend-id".to_owned(),
                ListToolsResult::with_all_items(vec![Tool::new("test_simple_text", "", serde_json::Map::new())]),
            )],
            &virtual_host,
        );
        let prompts = merge_prompts(
            vec![(
                "backend-id".to_owned(),
                ListPromptsResult::with_all_items(vec![Prompt::new("test_prompt", None::<String>, None)]),
            )],
            false,
        );
        let resources = merge_resources(
            vec![(
                "backend-id".to_owned(),
                ListResourcesResult::with_all_items(vec![Resource::new("test://resource", "test_resource")]),
            )],
            false,
        );
        let templates = merge_resource_templates(
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
}
