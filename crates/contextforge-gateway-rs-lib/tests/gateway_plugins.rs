mod support;

use std::sync::{Arc, Mutex as StdMutex};

use contextforge_gateway_rs_cpex::CpexRuntimeRegistry;
use cpex_core::cmf::Role;
use cpex_core::hooks::types::cmf_hook_names;
use rmcp::{
    ClientHandler,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientRequest, ErrorCode, Implementation,
        InitializeRequestParams, LoggingMessageNotificationParam, Meta, NumberOrString, ProgressNotificationParam,
        ProgressToken, Request, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RoleClient},
};
use serde_json::Value;

use support::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, REWRITTEN_SUM_A, REWRITTEN_SUM_B, RunningGateway, TestPlugin,
    error_code, runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, start_gateway,
    start_gateway_with_json_backend_responses, sum_request, text, token,
};

type Recorded<T> = Arc<StdMutex<Vec<T>>>;

#[derive(Clone, Default)]
struct RecordingClient {
    progress: Recorded<ProgressNotificationParam>,
    messages: Recorded<LoggingMessageNotificationParam>,
}

impl ClientHandler for RecordingClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("recording-test-client", "0.1.0"),
        )
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

async fn call_progress_sum(
    gateway: &RunningGateway,
    user: &str,
) -> (CallToolResult, Recorded<ProgressNotificationParam>, Recorded<LoggingMessageNotificationParam>) {
    let (result, progress, messages) = send_progress_sum(gateway, user).await;
    wait_for_event_count(&progress, 4).await;
    wait_for_event_count(&messages, 4).await;
    (result, progress, messages)
}

async fn send_progress_sum(
    gateway: &RunningGateway,
    user: &str,
) -> (CallToolResult, Recorded<ProgressNotificationParam>, Recorded<LoggingMessageNotificationParam>) {
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let messages = Arc::clone(&client.messages);
    let service = gateway.connect_with_handler(user, client).await;
    let request = CallToolRequestParams::new(format!("{}-progress_sum", gateway.backend_name));
    let mut options = PeerRequestOptions::no_options();
    options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::String("package-progress".into()))));
    let handle = service
        .send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), options)
        .await
        .expect("progress_sum request is sent");

    let ServerResult::CallToolResult(result) = handle.await_response().await.expect("progress_sum call succeeds")
    else {
        panic!("expected call tool result");
    };
    (result, progress, messages)
}

async fn wait_for_event_count<T>(events: &StdMutex<Vec<T>>, expected: usize) {
    for _ in 0..50 {
        if events.lock().expect("events lock poisoned").len() >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {expected} recorded events");
}

fn raw_mcp_request(
    client: &reqwest::Client,
    gateway: &RunningGateway,
    user: &str,
    session_id: Option<&str>,
    body: &Value,
) -> reqwest::RequestBuilder {
    let mut request = client
        .post(gateway.gateway_url())
        .bearer_auth(token(user))
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::ACCEPT, "application/json, text/event-stream")
        .json(body);
    if let Some(session_id) = session_id {
        request = request.header("Mcp-Session-Id", session_id).header("MCP-Protocol-Version", "2025-11-25");
    }
    request
}

fn sse_data_values(body: &str) -> Vec<Value> {
    let values = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(|data| serde_json::from_str(data).expect("SSE data is JSON"))
        .collect::<Vec<_>>();
    if !values.is_empty() {
        return values;
    }
    let body = body.trim();
    if body.is_empty() { Vec::new() } else { vec![serde_json::from_str(body).expect("JSON response body")] }
}

fn assert_raw_progress_stream(body: &str, response_id: i64, progress_token: i64) {
    let messages = sse_data_values(body);
    let progress_count = messages
        .iter()
        .filter(|message| {
            message.get("method").and_then(Value::as_str) == Some("notifications/progress")
                && message.pointer("/params/progressToken").and_then(Value::as_i64) == Some(progress_token)
        })
        .count();
    assert_eq!(4, progress_count, "unexpected progress events in body: {body}");
    let result = messages
        .iter()
        .find(|message| message.get("id").and_then(Value::as_i64) == Some(response_id))
        .unwrap_or_else(|| panic!("missing response id {response_id} in body: {body}"));
    assert_eq!(Some("completed 4 packages"), result.pointer("/result/content/0/text").and_then(Value::as_str));
}

async fn start_raw_mcp_session(client: &reqwest::Client, gateway: &RunningGateway, user: &str) -> String {
    let initialize = raw_mcp_request(
        client,
        gateway,
        user,
        None,
        &serde_json::json!({
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "raw-test-client", "version": "0.1.0" }
            },
            "jsonrpc": "2.0",
            "id": 0
        }),
    )
    .send()
    .await
    .expect("initialize request is sent");
    assert!(initialize.status().is_success(), "initialize failed: {initialize:?}");
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .expect("initialize response has MCP session id")
        .to_str()
        .expect("MCP session id is valid")
        .to_owned();
    let _initialize_body = initialize.text().await.expect("initialize body is read");

    let initialized = raw_mcp_request(
        client,
        gateway,
        user,
        Some(&session_id),
        &serde_json::json!({ "method": "notifications/initialized", "jsonrpc": "2.0" }),
    )
    .send()
    .await
    .expect("initialized notification is sent");
    assert!(initialized.status().is_success(), "initialized notification failed: {initialized:?}");
    let _initialized_body = initialized.text().await.expect("initialized body is read");

    session_id
}

async fn read_concurrent_raw_progress_streams(
    first: reqwest::RequestBuilder,
    second: reqwest::RequestBuilder,
) -> (String, String) {
    let (first, second) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async { tokio::join!(first.send(), second.send()) })
            .await
            .expect("both raw progress requests receive response headers");
    let first = first.expect("first raw progress request succeeds");
    let second = second.expect("second raw progress request succeeds");
    assert!(first.status().is_success(), "first raw progress request failed: {first:?}");
    assert!(second.status().is_success(), "second raw progress request failed: {second:?}");

    let (first_body, second_body) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async { tokio::join!(first.text(), second.text()) })
            .await
            .expect("both raw progress streams complete");
    (first_body.expect("first raw progress body is read"), second_body.expect("second raw progress body is read"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_progress_calls_forward_each_token_without_plugins() {
    let gateway = start_gateway("admin@example.com", false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let messages = Arc::clone(&client.messages);
    let service = gateway.connect_with_handler("admin@example.com", client).await;
    let request = CallToolRequestParams::new(format!("{}-progress_sum", gateway.backend_name));

    let mut first_options = PeerRequestOptions::no_options();
    first_options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::Number(1))));
    let first = service
        .send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request.clone())), first_options)
        .await
        .expect("first progress_sum request is sent");

    let mut second_options = PeerRequestOptions::no_options();
    second_options.meta = Some(Meta::with_progress_token(ProgressToken(NumberOrString::Number(2))));
    let second = service
        .send_cancellable_request(ClientRequest::CallToolRequest(Request::new(request)), second_options)
        .await
        .expect("second progress_sum request is sent");

    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(first.await_response(), second.await_response())
    })
    .await
    .expect("both concurrent progress_sum calls complete");

    let ServerResult::CallToolResult(first) = first.expect("first progress_sum call succeeds") else {
        panic!("expected first call tool result");
    };
    let ServerResult::CallToolResult(second) = second.expect("second progress_sum call succeeds") else {
        panic!("expected second call tool result");
    };
    assert_eq!("completed 4 packages", text(&first));
    assert_eq!("completed 4 packages", text(&second));

    wait_for_event_count(&progress, 8).await;
    wait_for_event_count(&messages, 8).await;
    let progress = progress.lock().expect("progress lock poisoned");
    let first_count = progress
        .iter()
        .filter(|notification| notification.progress_token == ProgressToken(NumberOrString::Number(1)))
        .count();
    let second_count = progress
        .iter()
        .filter(|notification| notification.progress_token == ProgressToken(NumberOrString::Number(2)))
        .count();
    assert_eq!(4, first_count);
    assert_eq!(4, second_count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_streamable_http_concurrent_progress_calls_complete_without_plugins() {
    let gateway = start_gateway("admin@example.com", false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = reqwest::Client::new();
    let session_id = start_raw_mcp_session(&client, &gateway, "admin@example.com").await;

    let tool_name = format!("{}-progress_sum", gateway.backend_name);
    let first = raw_mcp_request(
        &client,
        &gateway,
        "admin@example.com",
        Some(&session_id),
        &serde_json::json!({
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": {},
                "_meta": { "progressToken": 1 }
            },
            "jsonrpc": "2.0",
            "id": 2
        }),
    );
    let second = raw_mcp_request(
        &client,
        &gateway,
        "admin@example.com",
        Some(&session_id),
        &serde_json::json!({
            "method": "tools/call",
            "params": {
                "name": format!("{}-progress_sum", gateway.backend_name),
                "arguments": {},
                "_meta": { "progressToken": 2 }
            },
            "jsonrpc": "2.0",
            "id": 3
        }),
    );
    let (first_body, second_body) = read_concurrent_raw_progress_streams(first, second).await;

    assert_raw_progress_stream(&first_body, 2, 1);
    assert_raw_progress_stream(&second_body, 3, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_streamable_http_rewrites_backend_generated_progress_tokens() {
    let gateway = start_gateway("admin@example.com", false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = reqwest::Client::new();
    let session_id = start_raw_mcp_session(&client, &gateway, "admin@example.com").await;

    let first = raw_mcp_request(
        &client,
        &gateway,
        "admin@example.com",
        Some(&session_id),
        &serde_json::json!({
            "method": "tools/call",
            "params": {
                "name": format!("{}-progress_counter_tokens", gateway.backend_name),
                "arguments": {},
                "_meta": { "progressToken": 10 }
            },
            "jsonrpc": "2.0",
            "id": 2
        }),
    );
    let second = raw_mcp_request(
        &client,
        &gateway,
        "admin@example.com",
        Some(&session_id),
        &serde_json::json!({
            "method": "tools/call",
            "params": {
                "name": format!("{}-progress_counter_tokens", gateway.backend_name),
                "arguments": {},
                "_meta": { "progressToken": 20 }
            },
            "jsonrpc": "2.0",
            "id": 3
        }),
    );
    let (first_body, second_body) = read_concurrent_raw_progress_streams(first, second).await;

    assert_raw_progress_stream(&first_body, 2, 10);
    assert_raw_progress_stream(&second_body, 3, 20);
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
async fn post_hook_can_modify_stream_progress_and_message_notifications() {
    let plugin =
        Arc::new(TestPlugin::new("post-stream", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let (result, progress, messages) = call_progress_sum(&gateway, "admin@example.com").await;

    assert_eq!("completed 4 packages", text(&result));
    let progress = progress.lock().expect("progress lock poisoned");
    assert_eq!(Some("plugin:package 4/4"), progress.last().and_then(|notification| notification.message.as_deref()));
    let messages = messages.lock().expect("messages lock poisoned");
    assert_eq!(
        Some("message"),
        messages.last().and_then(|notification| notification.data.get("plugin")).and_then(Value::as_str)
    );

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(9, observations.post_calls);
    let first_id = observations.post_tool_call_ids.first().expect("post call id");
    assert!(observations.post_tool_call_ids.iter().all(|id| id == first_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn json_response_mode_forwards_backend_progress_and_message_notifications() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway_with_json_backend_responses("admin@example.com", true, runtime).await;
    let (result, progress, messages) = call_progress_sum(&gateway, "admin@example.com").await;

    assert_eq!("post:completed 4 packages", text(&result));
    assert_eq!(4, progress.lock().expect("progress lock poisoned").len());
    assert_eq!(4, messages.lock().expect("messages lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_deny_drops_stream_notifications_without_failing_call() {
    let plugin =
        Arc::new(TestPlugin::new("post-stream-deny", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_deny());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let (result, progress, messages) = send_progress_sum(&gateway, "admin@example.com").await;

    assert_eq!("completed 4 packages", text(&result));
    for _ in 0..50 {
        if observations.lock().expect("observations lock poisoned").post_calls >= 9 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(9, observations.post_calls);
    assert!(progress.lock().expect("progress lock poisoned").is_empty());
    assert!(messages.lock().expect("messages lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn downstream_cancellation_is_relayed_to_backend() {
    let gateway = start_gateway("admin@example.com", true, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = gateway.connect("admin@example.com").await;

    let request = CallToolRequestParams::new(format!("{}-wait_for_cancellation", gateway.backend_name));
    let handle = service
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(request)),
            PeerRequestOptions::no_options(),
        )
        .await
        .expect("wait_for_cancellation request is sent");
    wait_for_event_count(&gateway.backend_state.calls, 1).await;

    handle.cancel(Some("client gave up".to_owned())).await.expect("cancellation is sent");
    wait_for_event_count(&gateway.backend_state.cancellations, 1).await;
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
