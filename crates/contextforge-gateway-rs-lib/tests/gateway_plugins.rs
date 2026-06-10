mod support;

use std::sync::{Arc, Mutex as StdMutex};

use cpex_core::cmf::Role;
use cpex_core::hooks::types::cmf_hook_names;
use rmcp::{
    ClientHandler,
    model::{
        CallToolRequestParams, ClientRequest, ErrorCode, Implementation, InitializeRequestParams,
        LoggingMessageNotificationParam, Meta, NumberOrString, ProgressNotificationParam, ProgressToken, Request,
        ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RoleClient},
};
use serde_json::Value;

use support::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, REWRITTEN_SUM_A, REWRITTEN_SUM_B, TestPlugin, error_code,
    runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, start_gateway,
    start_gateway_with_json_backend_responses, sum_request, text,
};

#[derive(Clone, Default)]
struct RecordingClient {
    progress: Arc<StdMutex<Vec<ProgressNotificationParam>>>,
    messages: Arc<StdMutex<Vec<LoggingMessageNotificationParam>>>,
}

impl ClientHandler for RecordingClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(Default::default(), Implementation::new("recording-test-client", "0.1.0"))
    }

    async fn on_progress(&self, params: ProgressNotificationParam, _context: NotificationContext<RoleClient>) {
        self.progress.lock().expect("progress lock poisoned").push(params);
    }

    async fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.messages.lock().expect("messages lock poisoned").push(params);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn disabled_runtime_does_not_invoke_registered_plugin() {
    let pre_plugin =
        Arc::new(TestPlugin::new("disabled-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
    let post_plugin =
        Arc::new(TestPlugin::new("disabled-post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let pre_observations = pre_plugin.observations();
    let post_observations = post_plugin.observations();
    let runtime = runtime_with_pre_and_post(pre_plugin, post_plugin).await;

    let gateway = start_gateway("admin@example.com", false, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let result = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();

    assert_eq!("3", text(&result));
    assert_eq!(0, pre_observations.lock().expect("observations lock poisoned").pre_calls);
    assert_eq!(0, post_observations.lock().expect("observations lock poisoned").post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_hook_modifies_backend_arguments_without_rerouting_tool() {
    let plugin = Arc::new(TestPlugin::new("pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_pre(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let result = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();

    assert_eq!((REWRITTEN_SUM_A + REWRITTEN_SUM_B).to_string(), text(&result));
    let backend_calls = gateway.backend_state.calls.lock().expect("backend calls lock poisoned");
    assert_eq!("sum", backend_calls[0].tool_name);
    assert_eq!(Some(&Value::from(REWRITTEN_SUM_A)), backend_calls[0].args.as_ref().and_then(|args| args.get("a")));

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(Some("sum".to_owned()), observations.pre_payload_name);
    assert_eq!(Some(gateway.backend_name.clone()), observations.pre_payload_namespace);
    assert_eq!(Some(Role::Assistant), observations.pre_payload_role);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_receives_backend_result_and_modifies_client_result() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let result = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();

    assert_eq!("post:3", text(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(Some("sum".to_owned()), observations.post_payload_name);
    assert_eq!(Some("3".to_owned()), observations.post_result_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn call_tool_forwards_backend_progress_and_message_notifications_before_completion() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let messages = Arc::clone(&client.messages);
    let service = gateway.connect_with_handler("admin@example.com", client).await;
    let request = CallToolRequestParams::new(format!("{}-progress_sum", gateway.backend_name));
    let mut options = PeerRequestOptions::no_options();
    options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::String("package-progress".into()))));
    let handle =
        service.send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options).await.unwrap();

    let ServerResult::CallToolResult(result) = handle.await_response().await.unwrap() else {
        panic!("expected call tool result");
    };
    wait_for_notification_count(&progress, 4).await;
    wait_for_notification_count(&messages, 4).await;

    assert_eq!("post:completed 4 packages", text(&result));
    let progress = progress.lock().expect("progress lock poisoned");
    assert_eq!(4, progress.len());
    assert_eq!(Some("package 4/4"), progress.last().and_then(|notification| notification.message.as_deref()));
    let messages = messages.lock().expect("messages lock poisoned");
    assert_eq!(4, messages.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_can_modify_stream_progress_and_message_notifications() {
    let plugin =
        Arc::new(TestPlugin::new("post-stream", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let messages = Arc::clone(&client.messages);
    let service = gateway.connect_with_handler("admin@example.com", client).await;
    let request = CallToolRequestParams::new(format!("{}-progress_sum", gateway.backend_name));
    let mut options = PeerRequestOptions::no_options();
    options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::String("package-progress".into()))));
    let handle =
        service.send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options).await.unwrap();

    let ServerResult::CallToolResult(result) = handle.await_response().await.unwrap() else {
        panic!("expected call tool result");
    };
    wait_for_notification_count(&progress, 4).await;
    wait_for_notification_count(&messages, 4).await;

    assert_eq!("completed 4 packages", text(&result));
    let progress = progress.lock().expect("progress lock poisoned");
    assert_eq!(Some("plugin:package 4/4"), progress.last().and_then(|notification| notification.message.as_deref()));
    let messages = messages.lock().expect("messages lock poisoned");
    assert_eq!(
        Some("message"),
        messages.last().and_then(|notification| notification.data.get("plugin")).and_then(Value::as_str)
    );
    assert_eq!(9, observations.lock().expect("observations lock poisoned").post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_only_stream_events_share_context_with_final_result() {
    let plugin = Arc::new(TestPlugin::new("post-stream", vec![cmf_hook_names::TOOL_POST_INVOKE]));
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let messages = Arc::clone(&client.messages);
    let service = gateway.connect_with_handler("admin@example.com", client).await;
    let request = CallToolRequestParams::new(format!("{}-progress_sum", gateway.backend_name));
    let mut options = PeerRequestOptions::no_options();
    options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::String("package-progress".into()))));
    let handle =
        service.send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options).await.unwrap();

    let ServerResult::CallToolResult(_) = handle.await_response().await.unwrap() else {
        panic!("expected call tool result");
    };
    wait_for_notification_count(&progress, 4).await;
    wait_for_notification_count(&messages, 4).await;

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(9, observations.post_calls);
    let first_id = observations.post_tool_call_ids.first().expect("post call id");
    assert!(observations.post_tool_call_ids.iter().all(|id| id == first_id));
}

async fn wait_for_notification_count<T>(notifications: &StdMutex<Vec<T>>, expected: usize) {
    for _ in 0..50 {
        if notifications.lock().expect("notifications lock poisoned").len() >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn repeated_call_tool_reuses_initialized_streamable_http_backend_session() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let first = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();
    let second = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 3, 4)).await.unwrap();

    assert_eq!("post:3", text(&first));
    assert_eq!("post:7", text(&second));
    assert_eq!(2, gateway.backend_state.calls.lock().expect("backend calls lock poisoned").len());
    assert_eq!(1, *gateway.backend_state.initialize_calls.lock().expect("backend initialize calls lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn repeated_call_tool_works_with_json_streamable_http_backend_responses() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway_with_json_backend_responses("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let first = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();
    let second = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 3, 4)).await.unwrap();

    assert_eq!("post:3", text(&first));
    assert_eq!("post:7", text(&second));
    assert_eq!(2, gateway.backend_state.calls.lock().expect("backend calls lock poisoned").len());
    assert_eq!(1, *gateway.backend_state.initialize_calls.lock().expect("backend initialize calls lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_can_return_raw_cmf_result_content() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_raw_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let result = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();

    assert_eq!("raw-post", text(&result));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_and_post_hooks_share_gateway_call_context() {
    let pre_plugin =
        Arc::new(TestPlugin::new("context-pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_context_roundtrip());
    let post_plugin =
        Arc::new(TestPlugin::new("context-post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_context_roundtrip());
    let post_observations = post_plugin.observations();
    let runtime = runtime_with_pre_and_post(pre_plugin, post_plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let result = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap();

    assert_eq!("3", text(&result));
    assert_eq!(1, post_observations.lock().expect("observations lock poisoned").post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_and_post_denials_return_plugin_error_codes() {
    let pre_plugin = Arc::new(TestPlugin::new("pre-deny", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_deny());
    let runtime = runtime_with_pre(pre_plugin).await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let error = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap_err();
    assert_eq!(ErrorCode(PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());

    let post_plugin = Arc::new(TestPlugin::new("post-deny", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_deny());
    let runtime = runtime_with_post(post_plugin).await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let error = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap_err();
    assert_eq!(ErrorCode(POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, gateway.backend_state.calls.lock().expect("backend calls lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_hook_invalid_arguments_return_invalid_params() {
    let plugin =
        Arc::new(TestPlugin::new("invalid-args", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_invalid_pre_args());
    let runtime = runtime_with_pre(plugin).await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let error = service.call_tool(sum_request(format!("{}-sum", gateway.backend_name), 1, 2)).await.unwrap_err();

    assert_eq!(ErrorCode::INVALID_PARAMS, error_code(error));
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());
}
