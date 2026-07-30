mod support;

use std::sync::{Arc, Mutex as StdMutex};

use contextforge_gateway_rs_cpex::CpexRuntimeRegistry;
use cpex::cpex_core::cmf::Role;
use cpex::cpex_core::config::CpexConfig;
use cpex::cpex_core::hooks::types::cmf_hook_names;
use rmcp::{
    ClientHandler,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientRequest, ErrorCode, Implementation,
        InitializeRequestParams, ProgressNotificationParam, Request, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RequestHandle, RoleClient, RunningService},
};
use serde_json::{Map, Value, json};

use support::{
    POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, REWRITTEN_SUM_A, REWRITTEN_SUM_B, RunningGateway, TEST_USER_ID,
    TestPlugin, error_code, runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, start_gateway,
    start_gateway_with_json_backend_responses, sum_request, text, token,
};

type Recorded<T> = Arc<StdMutex<Vec<T>>>;

#[derive(Clone, Default)]
struct RecordingClient {
    progress: Recorded<ProgressNotificationParam>,
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
}

async fn call_progress_sum(
    gateway: &RunningGateway,
    user: &str,
) -> (CallToolResult, Recorded<ProgressNotificationParam>) {
    let (result, progress) = send_progress_sum(gateway, user).await;
    wait_for_event_count(&progress, 4).await;
    (result, progress)
}

async fn send_progress_sum(
    gateway: &RunningGateway,
    user: &str,
) -> (CallToolResult, Recorded<ProgressNotificationParam>) {
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let service = gateway.connect_with_handler(user, client).await;
    let handle = send_progress_call(&service, "progress_sum").await;

    let ServerResult::CallToolResult(result) = handle.await_response().await.expect("progress_sum call succeeds")
    else {
        panic!("expected call tool result");
    };
    (result, progress)
}

/// Starts a `tools/call` and returns the in-flight request handle without
/// awaiting it.
async fn send_progress_call(
    service: &RunningService<RoleClient, RecordingClient>,
    tool_name: &str,
) -> RequestHandle<RoleClient> {
    let request = CallToolRequestParams::new(tool_name.to_owned());
    service
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(request)),
            PeerRequestOptions::no_options(),
        )
        .await
        .expect("progress request is sent")
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

fn raw_tool_call(tool_name: &str, request_id: i64, progress_token: &str) -> Value {
    serde_json::json!({
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": {},
            "_meta": { "progressToken": progress_token }
        },
        "jsonrpc": "2.0",
        "id": request_id
    })
}

fn fake_aws_access_key(suffix: &str) -> String {
    ["AKIA", suffix].concat()
}

fn sum_request_with_secret(secret_field: &str, secret: String) -> CallToolRequestParams {
    let mut request = sum_request("sum", 1, 2);
    request
        .arguments
        .as_mut()
        .expect("sum request has arguments")
        .insert(secret_field.to_owned(), Value::String(secret));
    request
}

fn reflect_text_request(text: String) -> CallToolRequestParams {
    CallToolRequestParams::new("reflect_text")
        .with_arguments(Map::from_iter([("text".to_owned(), Value::String(text))]))
}

async fn runtime_with_secrets_detection(hooks: Vec<&'static str>, plugin_config: Value) -> Arc<CpexRuntimeRegistry> {
    let mut runtime = CpexRuntimeRegistry::default();
    runtime
        .register_factory(secrets_detection_rust::KIND, Box::new(secrets_detection_rust::SecretsDetectionFactory))
        .expect("secrets detection factory registers");
    let config: CpexConfig = serde_json::from_value(json!({
        "plugins": [{
            "name": "secrets-detection",
            "kind": secrets_detection_rust::KIND,
            "hooks": hooks,
            "config": plugin_config,
        }]
    }))
    .expect("secrets detection CPEX config parses");
    runtime.apply_config(Some(config)).await.expect("secrets detection runtime applies");
    Arc::new(runtime)
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

fn assert_raw_progress_stream(body: &str, response_id: i64, progress_token: &str) {
    let messages = sse_data_values(body);
    let progress = messages
        .iter()
        .filter(|message| message.get("method").and_then(Value::as_str) == Some("notifications/progress"))
        .collect::<Vec<_>>();
    assert_eq!(4, progress.len(), "unexpected progress events in body: {body}");
    assert!(
        progress
            .iter()
            .all(|message| message.pointer("/params/progressToken").and_then(Value::as_str) == Some(progress_token)),
        "progress events with foreign tokens in body: {body}"
    );
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
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let service = gateway.connect_with_handler(TEST_USER_ID, client).await;
    let tool_name = "progress_sum";

    let first = send_progress_call(&service, tool_name).await;
    let first_progress_token = first.progress_token.clone();
    let second = send_progress_call(&service, tool_name).await;
    let second_progress_token = second.progress_token.clone();
    assert_ne!(first_progress_token, second_progress_token);

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
    let progress = progress.lock().expect("progress lock poisoned");
    let first_count =
        progress.iter().filter(|notification| notification.progress_token == first_progress_token).count();
    let second_count =
        progress.iter().filter(|notification| notification.progress_token == second_progress_token).count();
    assert_eq!(4, first_count);
    assert_eq!(4, second_count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_streamable_http_concurrent_progress_calls_complete_without_plugins() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = reqwest::Client::new();
    let session_id = start_raw_mcp_session(&client, &gateway, TEST_USER_ID).await;

    let tool_name = "progress_sum";
    let first = raw_mcp_request(
        &client,
        &gateway,
        TEST_USER_ID,
        Some(&session_id),
        &raw_tool_call(tool_name, 2, "downstream-first"),
    );
    let second = raw_mcp_request(
        &client,
        &gateway,
        TEST_USER_ID,
        Some(&session_id),
        &raw_tool_call(tool_name, 3, "downstream-second"),
    );
    let (first_body, second_body) = read_concurrent_raw_progress_streams(first, second).await;

    assert_raw_progress_stream(&first_body, 2, "downstream-first");
    assert_raw_progress_stream(&second_body, 3, "downstream-second");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backend_generated_progress_tokens_are_dropped() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let service = gateway.connect_with_handler(TEST_USER_ID, client).await;

    let tool_name = "progress_counter_tokens";
    let handle = send_progress_call(&service, tool_name).await;

    let ServerResult::CallToolResult(result) =
        handle.await_response().await.expect("progress_counter_tokens call succeeds")
    else {
        panic!("expected call tool result");
    };
    assert_eq!("completed 4 packages", text(&result));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        progress.lock().expect("progress lock poisoned").is_empty(),
        "backend-generated progress tokens must not be forwarded"
    );
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

    let gateway = start_gateway(TEST_USER_ID, false, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

    assert_eq!("3", text(&result));
    assert_eq!(0, pre_observations.lock().expect("observations lock poisoned").pre_calls);
    assert_eq!(0, post_observations.lock().expect("observations lock poisoned").post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn secrets_detection_pre_hook_redacts_tool_arguments_before_backend_call() {
    let runtime = runtime_with_secrets_detection(
        vec![cmf_hook_names::TOOL_PRE_INVOKE],
        json!({
            "redact": true,
            "redaction_text": "[redacted]",
            "block_on_detection": false,
        }),
    )
    .await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;

    let result = service
        .call_tool(sum_request_with_secret("credential", fake_aws_access_key("1111111111111111")))
        .await
        .expect("secret argument is redacted and call succeeds");

    assert_eq!("3", text(&result));
    let backend_calls = gateway.backend_state.calls.lock().expect("backend calls lock poisoned");
    assert_eq!(1, backend_calls.len());
    assert_eq!(
        Some(&Value::from("[redacted]")),
        backend_calls[0].args.as_ref().and_then(|args| args.get("credential"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn secrets_detection_pre_hook_blocks_tool_arguments_before_backend_call() {
    let runtime = runtime_with_secrets_detection(
        vec![cmf_hook_names::TOOL_PRE_INVOKE],
        json!({
            "redact": false,
            "block_on_detection": true,
        }),
    )
    .await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;

    let error = service
        .call_tool(sum_request_with_secret("credential", fake_aws_access_key("2222222222222222")))
        .await
        .expect_err("secret argument blocks the call");

    assert_eq!(ErrorCode::INVALID_REQUEST, error_code(error));
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn secrets_detection_post_hook_redacts_tool_result_before_client_response() {
    let runtime = runtime_with_secrets_detection(
        vec![cmf_hook_names::TOOL_POST_INVOKE],
        json!({
            "redact": true,
            "redaction_text": "[redacted]",
            "block_on_detection": false,
        }),
    )
    .await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;

    let result = service
        .call_tool(reflect_text_request(fake_aws_access_key("3333333333333333")))
        .await
        .expect("secret result is redacted and call succeeds");

    assert_eq!("[redacted]", text(&result));
    assert_eq!(1, gateway.backend_state.calls.lock().expect("backend calls lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn secrets_detection_pre_hook_respects_field_allowlist() {
    let runtime = runtime_with_secrets_detection(
        vec![cmf_hook_names::TOOL_PRE_INVOKE],
        json!({
            "redact": true,
            "redaction_text": "[redacted]",
            "block_on_detection": false,
            "field_allowlist": ["credential"],
        }),
    )
    .await;
    let gateway = start_gateway("admin@example.com", true, runtime).await;
    let service = gateway.connect("admin@example.com").await;
    let ignored_secret = fake_aws_access_key("4444444444444444");
    let mut request = sum_request_with_secret("credential", fake_aws_access_key("5555555555555555"));
    request
        .arguments
        .as_mut()
        .expect("sum request has arguments")
        .insert("ignored".to_owned(), Value::String(ignored_secret.clone()));

    let result = service.call_tool(request).await.expect("allowed field is redacted");

    assert_eq!("3", text(&result));
    let backend_calls = gateway.backend_state.calls.lock().expect("backend calls lock poisoned");
    let args = backend_calls[0].args.as_ref().expect("backend call has args");
    assert_eq!(Some(&Value::from("[redacted]")), args.get("credential"));
    assert_eq!(Some(&Value::from(ignored_secret)), args.get("ignored"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_hook_modifies_backend_arguments_without_rerouting_tool() {
    let plugin = Arc::new(TestPlugin::new("pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_pre(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

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

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

    assert_eq!("post:3", text(&result));
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.post_calls);
    assert_eq!(Some("sum".to_owned()), observations.post_payload_name);
    assert_eq!(Some("3".to_owned()), observations.post_result_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_can_modify_stream_progress_notifications() {
    let plugin =
        Arc::new(TestPlugin::new("post-stream", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let (result, progress) = call_progress_sum(&gateway, TEST_USER_ID).await;

    assert_eq!("completed 4 packages", text(&result));
    let progress = progress.lock().expect("progress lock poisoned");
    assert_eq!(Some("plugin:package 4/4"), progress.last().and_then(|notification| notification.message.as_deref()));

    let observations = observations.lock().expect("observations lock poisoned");
    // four progress notifications plus the final tool result
    assert_eq!(5, observations.post_calls);
    let first_id = observations.post_tool_call_ids.first().expect("post call id");
    assert!(observations.post_tool_call_ids.iter().all(|id| id == first_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn json_response_mode_forwards_backend_progress_notifications() {
    let plugin = Arc::new(TestPlugin::new("post", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_rewrite());
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway_with_json_backend_responses(TEST_USER_ID, true, runtime).await;
    let (result, progress) = call_progress_sum(&gateway, TEST_USER_ID).await;

    assert_eq!("post:completed 4 packages", text(&result));
    assert_eq!(4, progress.lock().expect("progress lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_hook_deny_drops_progress_notifications_without_failing_call() {
    let plugin =
        Arc::new(TestPlugin::new("post-stream-deny", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_stream_event_deny());
    let observations = plugin.observations();
    let runtime = runtime_with_post(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let (result, progress) = send_progress_sum(&gateway, TEST_USER_ID).await;

    assert_eq!("completed 4 packages", text(&result));
    // four denied progress notifications plus the final tool result
    for _ in 0..50 {
        if observations.lock().expect("observations lock poisoned").post_calls >= 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(5, observations.post_calls);
    assert!(progress.lock().expect("progress lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn downstream_cancellation_is_relayed_to_backend() {
    let gateway = start_gateway(TEST_USER_ID, true, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = gateway.connect(TEST_USER_ID).await;

    let request = CallToolRequestParams::new("wait_for_cancellation");
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

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

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

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

    assert_eq!("3", text(&result));
    assert_eq!(1, post_observations.lock().expect("observations lock poisoned").post_calls);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_and_post_denials_return_plugin_error_codes() {
    let pre_plugin = Arc::new(TestPlugin::new("pre-deny", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_deny());
    let runtime = runtime_with_pre(pre_plugin).await;
    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.call_tool(sum_request("sum", 1, 2)).await.unwrap_err();
    assert_eq!(ErrorCode(PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());

    let post_plugin = Arc::new(TestPlugin::new("post-deny", vec![cmf_hook_names::TOOL_POST_INVOKE]).with_post_deny());
    let runtime = runtime_with_post(post_plugin).await;
    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.call_tool(sum_request("sum", 1, 2)).await.unwrap_err();
    assert_eq!(ErrorCode(POST_DENY_ERROR_CODE), error_code(error));
    assert_eq!(1, gateway.backend_state.calls.lock().expect("backend calls lock poisoned").len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_hook_invalid_arguments_return_invalid_params() {
    let plugin =
        Arc::new(TestPlugin::new("invalid-args", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_invalid_pre_args());
    let runtime = runtime_with_pre(plugin).await;
    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let error = service.call_tool(sum_request("sum", 1, 2)).await.unwrap_err();

    assert_eq!(ErrorCode::INVALID_PARAMS, error_code(error));
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());
}
