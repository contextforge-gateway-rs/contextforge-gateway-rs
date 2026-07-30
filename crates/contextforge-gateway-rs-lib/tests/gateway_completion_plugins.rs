//! End-to-end CPEX hook coverage for `completion/complete`.
//!
//! CPEX defines no completion hook, so a completion runs the hooks of whatever is being completed:
//! a prompt reference dispatches to the prompt hooks, a resource or template reference to the
//! resource hooks. Plugins tell a completion apart from a plain fetch by the `gateway-completion-*`
//! correlation id.
//!
//! The pre hook is deny-only. The argument value is a partial user keystroke, so rewriting it would
//! return completions for text the user never typed. The post hook may rewrite the returned values,
//! but the count must match so the backend's `total` and `has_more` stay accurate.

mod support;

use std::sync::Arc;

use cpex::cpex_core::hooks::types::cmf_hook_names;
use rmcp::model::{
    ArgumentInfo, CompleteRequestParams, ErrorCode, PromptReference, Reference, ResourceTemplateReference,
};

use support::{
    COMPLETION_VALUES, PROMPT_PRE_DENY_ERROR_CODE, PromptBehavior, PromptTestPlugin, RESOURCE_POST_DENY_ERROR_CODE,
    RESOURCE_PRE_DENY_ERROR_CODE, ResourceBehavior, ResourceTestPlugin, TEMPLATE_URI, TEST_USER_ID, error_code,
    runtime_with_prompt_plugin, runtime_with_resource_plugin, start_gateway,
};

fn prompt_completion() -> CompleteRequestParams {
    CompleteRequestParams::new(Reference::Prompt(PromptReference::new("review")), ArgumentInfo::new("topic", "wea"))
}

fn resource_completion() -> CompleteRequestParams {
    CompleteRequestParams::new(
        Reference::Resource(ResourceTemplateReference::new(TEMPLATE_URI)),
        ArgumentInfo::new("id", "4"),
    )
}

fn expected_values() -> Vec<String> {
    COMPLETION_VALUES.iter().map(|value| (*value).to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_reference_completion_runs_the_prompt_hooks() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "completion-prompt",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.complete(prompt_completion()).await.expect("completion succeeds");

    assert_eq!(expected_values(), result.completion.values);
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
    assert_eq!(Some("review"), observations.pre_name.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_server_id.as_deref());
    // The correlation id marks this as a completion rather than a `prompts/get`.
    let request_id = observations.pre_request_id.clone().expect("pre hook ran");
    assert!(request_id.starts_with("gateway-completion-"), "unexpected correlation id {request_id}");
    assert_eq!(observations.pre_request_id, observations.post_request_id);
    assert_eq!(expected_values(), observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_reference_completion_post_hook_rewrites_values() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "completion-prompt-rewrite",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::RewriteText,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.complete(prompt_completion()).await.expect("completion succeeds");

    assert_eq!(vec!["redacted:alpha".to_owned(), "redacted:beta".to_owned()], result.completion.values);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_prompt_reference_completion_never_reaches_the_backend() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "completion-prompt-deny",
        vec![cmf_hook_names::PROMPT_PRE_FETCH],
        PromptBehavior::DenyPre,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.complete(prompt_completion()).await.expect_err("completion deny is an error");

    assert_eq!(ErrorCode(PROMPT_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.completions.lock().expect("backend completions lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_reference_completion_runs_the_resource_hooks() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "completion-resource",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.complete(resource_completion()).await.expect("completion succeeds");

    assert_eq!(expected_values(), result.completion.values);
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
    assert_eq!(Some(TEMPLATE_URI), observations.pre_uri.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_backend.as_deref());
    assert_eq!(expected_values(), observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_reference_completion_post_hook_rewrites_values() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "completion-resource-rewrite",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::RewriteText,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.complete(resource_completion()).await.expect("completion succeeds");

    assert_eq!(vec!["redacted:alpha".to_owned(), "redacted:beta".to_owned()], result.completion.values);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_resource_reference_completion_never_reaches_the_backend() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "completion-resource-deny",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH],
        ResourceBehavior::DenyPre,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.complete(resource_completion()).await.expect_err("completion deny is an error");

    assert_eq!(ErrorCode(RESOURCE_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.completions.lock().expect("backend completions lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_resource_reference_completion_post_hook_errors_after_the_backend_ran() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "completion-resource-post-deny",
        vec![cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::DenyPost,
    ));
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.complete(resource_completion()).await.expect_err("completion post deny is an error");

    assert_eq!(ErrorCode(RESOURCE_POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, gateway.backend_state.completions.lock().expect("backend completions lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_reference_completion_does_not_run_resource_hooks() {
    let plugin = Arc::new(ResourceTestPlugin::new(
        "completion-resource-only",
        vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH],
        ResourceBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_resource_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.complete(prompt_completion()).await.expect("completion succeeds");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn resource_reference_completion_does_not_run_prompt_hooks() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "completion-prompt-only",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.complete(resource_completion()).await.expect("completion succeeds");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}
