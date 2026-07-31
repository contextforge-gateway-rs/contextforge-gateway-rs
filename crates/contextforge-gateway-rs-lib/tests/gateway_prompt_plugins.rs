//! End-to-end CPEX prompt hook coverage for `prompts/get` and `prompts/list`.
//!
//! The tool equivalents live in `gateway_plugins.rs`; these prove the same contract holds on the
//! non-tool pass-through paths: hooks run after routing, a denial stops the request before the
//! backend sees it, and mutations reach the backend (pre) and the client (post).
//!
//! `prompts/list` differs deliberately. It fans out to every backend, so its pre hook runs once
//! before fan-out as a deny gate and its post hook sees the merged, namespaced listing. Exposure
//! there is read-only: MCP `Prompt` metadata has no CMF equivalent to be rebuilt from, so a
//! modified payload is ignored rather than partially applied.

mod support;

use std::sync::Arc;

use cpex::cpex_core::hooks::types::cmf_hook_names;
use rmcp::model::{ContentBlock, ErrorCode, GetPromptRequestParams, GetPromptResult};
use serde_json::{Value, json};

use support::{
    PROMPT_DESCRIPTION, PROMPT_POST_DENY_ERROR_CODE, PROMPT_PRE_DENY_ERROR_CODE, PromptBehavior, PromptTestPlugin,
    REWRITTEN_PROMPT_TOPIC, TEST_USER_ID, error_code, runtime_with_prompt_plugin, start_gateway,
};

fn review_request(topic: &str) -> GetPromptRequestParams {
    GetPromptRequestParams::new("review")
        .with_arguments(serde_json::Map::from_iter([("topic".to_owned(), json!(topic))]))
}

fn prompt_text(result: &GetPromptResult) -> String {
    result
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_hooks_are_skipped_when_no_plugin_is_configured() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-allow",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    // Runtime plugins disabled: the gateway is built without a plugin handle at all.
    let gateway = start_gateway(TEST_USER_ID, false, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!("review of weather", prompt_text(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_pre_hook_sees_routed_name_and_backend() {
    let plugin =
        Arc::new(PromptTestPlugin::new("prompt-pre", vec![cmf_hook_names::PROMPT_PRE_FETCH], PromptBehavior::Allow));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!("review of weather", prompt_text(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(Some("review"), observations.pre_name.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_server_id.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_pre_hook_rewrites_arguments_reaching_the_backend() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-pre-rewrite",
        vec![cmf_hook_names::PROMPT_PRE_FETCH],
        PromptBehavior::RewriteArguments,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!(format!("review of {REWRITTEN_PROMPT_TOPIC}"), prompt_text(&result));
    let prompt_calls = gateway.backend_state.prompts.lock().expect("backend prompts lock poisoned");
    assert_eq!("review", prompt_calls[0].name);
    assert_eq!(
        Some(&Value::from(REWRITTEN_PROMPT_TOPIC)),
        prompt_calls[0].args.as_ref().and_then(|args| args.get("topic"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_rewrites_text_returned_to_the_client() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-post",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::RewriteText,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!("redacted:review of weather", prompt_text(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(vec!["review of weather".to_owned()], observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_pre_and_post_hooks_share_one_request_id() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-both",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
    assert!(observations.pre_request_id.is_some());
    assert_eq!(observations.pre_request_id, observations.post_request_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_prompt_pre_hook_never_reaches_the_backend() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-pre-deny",
        vec![cmf_hook_names::PROMPT_PRE_FETCH],
        PromptBehavior::DenyPre,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.get_prompt(review_request("weather")).await.expect_err("prompt pre deny is an error");

    assert_eq!(ErrorCode(PROMPT_PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.prompts.lock().expect("backend prompts lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_prompt_post_hook_returns_error_after_the_backend_ran() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-post-deny",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::DenyPost,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.get_prompt(review_request("weather")).await.expect_err("prompt post deny is an error");

    assert_eq!(ErrorCode(PROMPT_POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, gateway.backend_state.prompts.lock().expect("backend prompts lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_list_pre_hook_marks_a_listing_with_an_empty_name() {
    let plugin =
        Arc::new(PromptTestPlugin::new("list-pre", vec![cmf_hook_names::PROMPT_PRE_FETCH], PromptBehavior::Allow));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.list_prompts(None).await.expect("prompts are listed");

    assert_eq!(1, result.prompts.len());
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    // Empty name is the listing marker that separates this from a `prompts/get`.
    assert_eq!(Some(""), observations.pre_name.as_deref());
    assert_eq!(None, observations.pre_server_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_list_post_hook_sees_merged_names_and_descriptions() {
    let plugin =
        Arc::new(PromptTestPlugin::new("list-post", vec![cmf_hook_names::PROMPT_POST_FETCH], PromptBehavior::Allow));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_prompts(None).await.expect("prompts are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(vec!["review".to_owned()], observations.post_prompt_names);
    assert_eq!(vec![PROMPT_DESCRIPTION.to_owned()], observations.post_texts);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_list_post_hook_modifications_are_not_applied() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "list-mutate",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::RewriteListNames,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.list_prompts(None).await.expect("prompts are listed");

    // `prompts/list` exposure is read-only: MCP prompt metadata cannot be rebuilt from CMF.
    assert_eq!(vec!["review".to_owned()], result.prompts.iter().map(|p| p.name.clone()).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_prompt_list_pre_hook_never_fans_out_to_backends() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "list-pre-deny",
        vec![cmf_hook_names::PROMPT_PRE_FETCH],
        PromptBehavior::DenyPre,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.list_prompts(None).await.expect_err("prompt list pre deny is an error");

    assert_eq!(ErrorCode(PROMPT_PRE_DENY_ERROR_CODE), error_code(error));
    assert_eq!(0, *gateway.backend_state.prompt_lists.lock().expect("backend prompt lists lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn denied_prompt_list_post_hook_returns_error_after_fan_out() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "list-post-deny",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::DenyPost,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.list_prompts(None).await.expect_err("prompt list post deny is an error");

    assert_eq!(ErrorCode(PROMPT_POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, *gateway.backend_state.prompt_lists.lock().expect("backend prompt lists lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_list_pre_and_post_hooks_share_one_request_id() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "list-both",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.list_prompts(None).await.expect("prompts are listed");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
    assert!(observations.pre_request_id.is_some());
    assert_eq!(observations.pre_request_id, observations.post_request_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_removing_text_fails_closed() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-delete",
        vec![cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::DeleteText,
    ));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service
        .get_prompt(review_request("weather"))
        .await
        .expect_err("removing text must not fall back to the backend message");

    // Restoring the original here would return text a redaction plugin stripped.
    assert_eq!(ErrorCode::INVALID_PARAMS, error_code(error));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn tool_hooks_do_not_run_for_prompt_requests() {
    let plugin = Arc::new(PromptTestPlugin::new(
        "prompt-only",
        vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        PromptBehavior::Allow,
    ));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.call_tool(support::sum_request("sum", 1, 2)).await.expect("tool call succeeds");

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls);
    assert_eq!(0, observations.post_calls);
}
