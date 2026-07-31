//! End-to-end CPEX resource hook coverage for `resources/templates/list`.
//!
//! Template listings fan out to every backend, so the pre hook runs once before fan-out as a deny
//! gate and the post hook sees the merged, namespaced listing. Exposure is read-only: MCP
//! `ResourceTemplate` metadata (`title`, `mime_type`, `icons`, `annotations`, `_meta`) has no CMF
//! equivalent to be rebuilt from, so a modified payload is ignored rather than partially applied.
//!
//! `resources/subscribe` and `resources/unsubscribe` are routed paths instead: the pre hook runs
//! after routing and may deny or rewrite the target URI. Neither has a post hook, because both
//! return an empty acknowledgement with nothing to inspect.

mod support;

use std::sync::Arc;

use cpex::cpex_core::hooks::types::cmf_hook_names;
use rmcp::model::{
    ErrorCode, ReadResourceRequestParams, ReadResourceResult, ResourceContents, SubscribeRequestParams,
    UnsubscribeRequestParams,
};

use support::{
    RESOURCE_DESCRIPTION, RESOURCE_POST_DENY_ERROR_CODE, RESOURCE_PRE_DENY_ERROR_CODE, RESOURCE_TEXT, RESOURCE_URI,
    REWRITTEN_RESOURCE_URI, ResourceBehavior, ResourceTestPlugin, TEMPLATE_DESCRIPTION, TEMPLATE_URI, TEST_USER_ID,
    error_code, runtime_with_resource_plugin, start_gateway,
};

const SUBSCRIBED_URI: &str = "report://42/summary";

fn resource_texts(result: &ReadResourceResult) -> Vec<String> {
    result
        .contents
        .iter()
        .filter_map(|contents| match contents {
            ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_hooks_are_skipped_when_no_plugin_is_configured() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "resource-allow",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, false, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.list_resource_templates(None).await.expect("templates are listed");

    assert_eq!(1, result.resource_templates.len());
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn template_list_pre_hook_marks_a_listing_with_an_empty_uri() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-pre",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_resource_templates(None).await.expect("templates are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    // Empty URI is the listing marker that separates this from a single-resource fetch.
    assert_eq!(Some(""), observations.pre_uri.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn template_list_post_hook_sees_merged_uris_names_and_descriptions() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-post",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_resource_templates(None).await.expect("templates are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(vec![TEMPLATE_URI.to_owned()], observations.post_uris);
    assert_eq!(vec!["report".to_owned()], observations.post_names);
    assert_eq!(vec![TEMPLATE_DESCRIPTION.to_owned()], observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn template_list_post_hook_modifications_are_not_applied() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-mutate",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::RewriteListUris,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.list_resource_templates(None).await.expect("templates are listed");

    // Read-only exposure: MCP template metadata cannot be rebuilt from CMF content parts.
    assert_eq!(
        vec![TEMPLATE_URI.to_owned()],
        result.resource_templates.iter().map(|template| template.uri_template.clone()).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_template_list_pre_hook_never_fans_out_to_backends() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-pre-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.list_resource_templates(None).await.expect_err("template list pre deny is an error");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert_eq!(0, *gateway.backend_state.template_lists.lock().expect("backend template lists lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_template_list_post_hook_returns_error_after_fan_out() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-post-deny",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::DenyPost,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.list_resource_templates(None).await.expect_err("template list post deny is an error");

    assert_eq!(ErrorCode(RESOURCE_POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, *gateway.backend_state.template_lists.lock().expect("backend template lists lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn template_list_pre_and_post_hooks_share_one_request_id() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "template-both",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_resource_templates(None).await.expect("templates are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
    assert!(observations.pre_request_id.is_some());
    assert_eq!(observations.pre_request_id, observations.post_request_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn subscribe_pre_hook_sees_routed_uri_and_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "subscribe-pre",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("subscribe succeeds");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(Some(SUBSCRIBED_URI), observations.pre_uri.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_backend.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn subscribe_has_no_post_hook() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "subscribe-post",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("subscribe succeeds");
    service.unsubscribe(UnsubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("unsubscribe succeeds");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(2, observations.pre_calls);
    // Both paths return an empty acknowledgement, so there is nothing for a post hook to see.
    assert_eq!(0, observations.post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn subscribe_pre_hook_rewrites_the_uri_reaching_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "subscribe-rewrite",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::RewriteUri,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("subscribe succeeds");

    let subscriptions = gateway.backend_state.subscriptions.lock().expect("backend subscriptions lock poisoned");
    assert_eq!(vec![REWRITTEN_RESOURCE_URI.to_owned()], *subscriptions);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn unsubscribe_pre_hook_rewrites_the_uri_reaching_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "unsubscribe-rewrite",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::RewriteUri,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("subscribe succeeds");
    service.unsubscribe(UnsubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("unsubscribe succeeds");

    // Subscribe and unsubscribe rewrite to the same URI, so the tracking key matches on both sides.
    let unsubscriptions = gateway.backend_state.unsubscriptions.lock().expect("backend lock poisoned");
    assert_eq!(vec![REWRITTEN_RESOURCE_URI.to_owned()], *unsubscriptions);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn denied_subscribe_pre_hook_never_reaches_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "subscribe-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error =
        service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect_err("subscribe deny is an error");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.subscriptions.lock().expect("backend subscriptions lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn denied_unsubscribe_pre_hook_never_reaches_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "unsubscribe-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service
        .unsubscribe(UnsubscribeRequestParams::new(SUBSCRIBED_URI))
        .await
        .expect_err("unsubscribe deny is an error");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.unsubscriptions.lock().expect("backend lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_resource_pre_hook_sees_routed_uri_and_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-pre",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.read_resource(ReadResourceRequestParams::new(RESOURCE_URI)).await.expect("read succeeds");

    assert_eq!(1, result.contents.len());
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(Some(RESOURCE_URI), observations.pre_uri.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_backend.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_resource_post_hook_rewrites_text_returned_to_the_client() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-post",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::RewriteContent,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.read_resource(ReadResourceRequestParams::new(RESOURCE_URI)).await.expect("read succeeds");

    assert_eq!(vec![format!("redacted:{RESOURCE_TEXT}")], resource_texts(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(vec![RESOURCE_TEXT.to_owned()], observations.post_contents);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_resource_pre_hook_rewrites_the_uri_reaching_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-rewrite-uri",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::RewriteUri,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.read_resource(ReadResourceRequestParams::new(RESOURCE_URI)).await.expect("read succeeds");

    let reads = gateway.backend_state.reads.lock().expect("backend reads lock poisoned");
    assert_eq!(vec![REWRITTEN_RESOURCE_URI.to_owned()], *reads);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_read_resource_pre_hook_never_reaches_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-pre-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error =
        service.read_resource(ReadResourceRequestParams::new(RESOURCE_URI)).await.expect_err("read deny errors");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.reads.lock().expect("backend reads lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_read_resource_post_hook_errors_after_the_backend_ran() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-post-deny",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::DenyPost,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error =
        service.read_resource(ReadResourceRequestParams::new(RESOURCE_URI)).await.expect_err("read post deny errors");

    assert_eq!(ErrorCode(RESOURCE_POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, gateway.backend_state.reads.lock().expect("backend reads lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_list_post_hook_sees_merged_uris_and_descriptions() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "resource-list-post",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_resources(None).await.expect("resources are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(vec![RESOURCE_URI.to_owned()], observations.post_uris);
    assert_eq!(vec![RESOURCE_DESCRIPTION.to_owned()], observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_list_post_hook_modifications_are_not_applied() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "resource-list-mutate",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::RewriteListUris,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.list_resources(None).await.expect("resources are listed");

    assert_eq!(
        vec![RESOURCE_URI.to_owned()],
        result.resources.iter().map(|resource| resource.uri.clone()).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_resource_list_pre_hook_never_fans_out_to_backends() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "resource-list-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.list_resources(None).await.expect_err("resource list deny errors");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert_eq!(0, *gateway.backend_state.resource_lists.lock().expect("backend resource lists lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_resource_post_hook_removing_text_fails_closed() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "read-delete",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::DeleteContent,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service
        .read_resource(ReadResourceRequestParams::new(RESOURCE_URI))
        .await
        .expect_err("removing text must not fall back to the backend content");

    // Restoring the original here would return content a redaction plugin stripped.
    assert_eq!(ErrorCode::INVALID_PARAMS, error_code(error));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[expect(deprecated, reason = "legacy RMCP subscription coverage; listen migration is deferred")]
async fn unsubscribe_uses_the_uri_recorded_at_subscribe_not_a_fresh_rewrite() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "sub-stateful",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::RewriteUriPerCall,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.subscribe(SubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("subscribe succeeds");
    service.unsubscribe(UnsubscribeRequestParams::new(SUBSCRIBED_URI)).await.expect("unsubscribe succeeds");

    // The plugin rewrites to a different URI on every call. Unsubscribe must use the URI recorded
    // at subscribe time, or the backend subscription is orphaned.
    let subscribed = gateway.backend_state.subscriptions.lock().expect("backend lock poisoned").clone();
    let unsubscribed = gateway.backend_state.unsubscriptions.lock().expect("backend lock poisoned").clone();
    assert_eq!(1, subscribed.len());
    assert_eq!(subscribed, unsubscribed, "unsubscribe addressed a different URI than subscribe");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_hooks_do_not_run_for_prompt_or_tool_requests() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "resource-only",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.call_tool(support::sum_request("sum", 1, 2)).await.expect("tool call succeeds");
    service.list_prompts(None).await.expect("prompts are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}
