use std::sync::{Arc, Mutex as StdMutex};

use contextforge_data_plane_cpex::CpexRuntimeRegistry;
use cpex::cpex_core::cmf::Role;
use cpex::cpex_core::hooks::types::cmf_hook_names;
use rmcp::{
    ClientHandler,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientRequest, ContentBlock, ErrorCode,
        GetPromptRequestParams, GetPromptResult, Implementation, InitializeRequestParams, ProgressNotificationParam,
        ReadResourceRequestParams, Request, ResourceContents, Role as McpRole, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RequestHandle, RoleClient, RunningService},
};
use serde_json::{Map, Value, json};

use crate::harness::{
    BACKEND_PROMPT_IMAGE, BACKEND_PROMPT_RESOURCE, POST_DENY_ERROR_CODE, PRE_DENY_ERROR_CODE, PROMPT_ERROR_MESSAGE,
    PROMPT_POST_DENY_ERROR_CODE, PromptBehavior, PromptTestPlugin, REWRITTEN_PROMPT_RESOURCE, REWRITTEN_PROMPT_TEXT,
    REWRITTEN_PROMPT_TOPIC, REWRITTEN_SUM_A, REWRITTEN_SUM_B, RunningGateway, TEST_USER_ID, TestPlugin, error_code,
    error_parts, runtime_with_post, runtime_with_pre, runtime_with_pre_and_post, runtime_with_prompt_plugin,
    start_gateway, start_gateway_with_events, start_gateway_with_json_backend_responses,
    start_gateway_with_parameter_headers, sum_request, text, token,
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

fn client_with_parameter_headers(a: &'static str, b: &'static str) -> reqwest::Client {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_str(&format!("Bearer {}", token(TEST_USER_ID))).expect("valid auth header"),
    );
    headers.insert("Mcp-Param-A", http::HeaderValue::from_static(a));
    headers.insert("Mcp-Param-B", http::HeaderValue::from_static(b));
    reqwest::Client::builder().default_headers(headers).build().expect("client builds")
}

fn last_backend_request_headers(gateway: &RunningGateway) -> http::HeaderMap {
    gateway
        .backend_state
        .request_headers
        .lock()
        .expect("backend request headers lock poisoned")
        .last()
        .cloned()
        .expect("backend received a request")
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
async fn stateless_tool_call_forwards_validated_parameter_headers_unchanged() {
    let gateway =
        start_gateway_with_parameter_headers(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        client_with_parameter_headers("1", "2"),
        crate::harness::modern_client_info(),
    )
    .await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.expect("stateless tool call succeeds");

    assert_eq!("3", text(&result));
    let headers = last_backend_request_headers(&gateway);
    assert_eq!("1", headers["Mcp-Param-A"]);
    assert_eq!("2", headers["Mcp-Param-B"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stateless_tool_call_with_mismatched_parameter_header_is_rejected() {
    let gateway =
        start_gateway_with_parameter_headers(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        client_with_parameter_headers("9", "2"),
        crate::harness::modern_client_info(),
    )
    .await;
    let error = service.call_tool(sum_request("sum", 1, 2)).await.expect_err("mismatched header is rejected");
    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected backend MCP error, got {error:?}");
    };

    assert_eq!(ErrorCode::HEADER_MISMATCH, error.code);
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stateless_tool_call_without_required_parameter_headers_is_rejected() {
    let gateway =
        start_gateway_with_parameter_headers(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        crate::harness::create_client(TEST_USER_ID),
        crate::harness::modern_client_info(),
    )
    .await;

    let error = service.call_tool(sum_request("sum", 1, 2)).await.expect_err("missing annotated headers are rejected");
    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected backend MCP error, got {error:?}");
    };

    assert_eq!(ErrorCode::HEADER_MISMATCH, error.code);
    assert!(gateway.backend_state.calls.lock().expect("backend calls lock poisoned").is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stateless_tool_call_without_published_schema_reaches_backend() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        client_with_parameter_headers("1", "2"),
        crate::harness::modern_client_info(),
    )
    .await;
    let error = service.call_tool(CallToolRequestParams::new("missing_schema_tool")).await.unwrap_err();
    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected backend MCP error, got {error:?}");
    };

    assert_eq!(ErrorCode::METHOD_NOT_FOUND, error.code);
    let backend_calls = gateway.backend_state.calls.lock().expect("backend calls lock poisoned");
    assert_eq!("missing_schema_tool", backend_calls[0].tool_name);
    drop(backend_calls);
    let headers = last_backend_request_headers(&gateway);
    assert_eq!("1", headers["Mcp-Param-A"]);
    assert_eq!("2", headers["Mcp-Param-B"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stateless_tool_error_round_trips() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        crate::harness::create_client(TEST_USER_ID),
        crate::harness::modern_client_info(),
    )
    .await;
    let error = service.call_tool(CallToolRequestParams::new("missing_tool")).await.unwrap_err();
    let rmcp::service::ServiceError::McpError(error) = error else {
        panic!("expected backend MCP error, got {error:?}");
    };
    assert_eq!(ErrorCode::INVALID_PARAMS, error.code);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stateless_alias_and_namespaced_tool_names_route() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        crate::harness::create_client(TEST_USER_ID),
        crate::harness::modern_client_info(),
    )
    .await;

    let alias_result = service.call_tool(sum_request("sum", 1, 2)).await.expect("control-plane alias routes");
    let namespaced_result = service
        .call_tool(sum_request(format!("{}-sum", gateway.backend_name), 3, 4))
        .await
        .expect("namespaced tool routes");
    assert_eq!("3", text(&alias_result));
    assert_eq!("7", text(&namespaced_result));
    drop(service);
    gateway.shutdown().await.expect("gateway shuts down");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_concurrent_progress_calls_remain_request_scoped() {
    let gateway = start_gateway(TEST_USER_ID, false, Arc::new(CpexRuntimeRegistry::default())).await;
    let client = RecordingClient::default();
    let progress = Arc::clone(&client.progress);
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        crate::harness::create_client(TEST_USER_ID),
        client,
    )
    .await;
    let first = send_progress_call(&service, "progress_sum").await;
    let first_progress_token = first.progress_token.clone();
    let second = send_progress_call(&service, "progress_sum").await;
    let second_progress_token = second.progress_token.clone();
    assert_ne!(first_progress_token, second_progress_token);
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(first.await_response(), second.await_response())
    })
    .await
    .expect("both stateless calls complete");
    let ServerResult::CallToolResult(first) = first.expect("first call succeeds") else {
        panic!("expected first tool result");
    };
    let ServerResult::CallToolResult(second) = second.expect("second call succeeds") else {
        panic!("expected second tool result");
    };
    assert_eq!("completed 4 packages", text(&first));
    assert_eq!("completed 4 packages", text(&second));
    wait_for_event_count(&progress, 8).await;
    let progress = progress.lock().expect("progress lock poisoned");
    assert_eq!(4, progress.iter().filter(|event| event.progress_token == first_progress_token).count());
    assert_eq!(4, progress.iter().filter(|event| event.progress_token == second_progress_token).count());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pre_hook_rewrites_payload_without_changing_forwarded_parameter_headers() {
    let plugin = Arc::new(TestPlugin::new("pre", vec![cmf_hook_names::TOOL_PRE_INVOKE]).with_pre_rewrite());
    let observations = plugin.observations();
    let runtime = runtime_with_pre(plugin).await;

    let gateway = start_gateway_with_parameter_headers(TEST_USER_ID, true, runtime).await;
    let service = crate::harness::connect_modern_client(
        gateway.gateway_url(),
        client_with_parameter_headers("1", "2"),
        crate::harness::modern_client_info(),
    )
    .await;
    let result = service.call_tool(sum_request("sum", 1, 2)).await.unwrap();

    assert_eq!((REWRITTEN_SUM_A + REWRITTEN_SUM_B).to_string(), text(&result));
    assert_eq!("1", last_backend_request_headers(&gateway)["Mcp-Param-A"]);
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

// ---------------------------------------------------------------------------
// Prompt hooks
// ---------------------------------------------------------------------------

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
async fn prompt_pre_hook_rewrites_arguments_reaching_the_backend() {
    let plugin = Arc::new(PromptTestPlugin::new("prompt-pre", vec![cmf_hook_names::PROMPT_PRE_FETCH]));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!(format!("review of {REWRITTEN_PROMPT_TOPIC}"), prompt_text(&result));

    let prompt_calls = gateway.backend_state.prompts.lock().expect("backend prompts lock poisoned");
    assert_eq!("review", prompt_calls[0].tool_name);
    assert_eq!(
        Some(&Value::from(REWRITTEN_PROMPT_TOPIC)),
        prompt_calls[0].args.as_ref().and_then(|args| args.get("topic"))
    );

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(Some("review"), observations.pre_name.as_deref());
    assert_eq!(Some(gateway.backend_name.as_str()), observations.pre_server_id.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_rewrites_rendered_text_before_client_response() {
    let plugin = Arc::new(PromptTestPlugin::new("prompt-post", vec![cmf_hook_names::PROMPT_POST_FETCH]));
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!(REWRITTEN_PROMPT_TEXT, prompt_text(&result));

    let prompt_calls = gateway.backend_state.prompts.lock().expect("backend prompts lock poisoned");
    assert_eq!(Some(&Value::from("weather")), prompt_calls[0].args.as_ref().and_then(|args| args.get("topic")));
    drop(prompt_calls);

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(0, observations.pre_calls, "no pre hook is configured");
    assert_eq!(1, observations.post_calls);
    assert_eq!(Some("review"), observations.post_prompt_name.as_deref());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_removing_rendered_text_fails_closed() {
    let plugin = Arc::new(
        PromptTestPlugin::new("prompt-post-drop", vec![cmf_hook_names::PROMPT_POST_FETCH])
            .with_behavior(PromptBehavior::DropText),
    );
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;

    let error = service.get_prompt(review_request("weather")).await.expect_err("dropped text fails the call");
    assert_eq!(ErrorCode::INTERNAL_ERROR, error_code(error));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_rewrites_multimodal_prompt_content() {
    let plugin = Arc::new(PromptTestPlugin::new("prompt-multimodal", vec![cmf_hook_names::PROMPT_POST_FETCH]));
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let request = GetPromptRequestParams::new("review_bundle")
        .with_arguments(Map::from_iter([("topic".to_owned(), json!("weather"))]));
    let result = service.get_prompt(request).await.expect("prompt is returned");

    assert_eq!(3, result.messages.len());
    assert_eq!(REWRITTEN_PROMPT_TEXT, prompt_text(&result));

    let ContentBlock::Resource(resource) = &result.messages[1].content else {
        panic!("expected the embedded resource to survive as a resource");
    };
    let ResourceContents::TextResourceContents { text, uri, .. } = &resource.resource else {
        panic!("expected text resource contents");
    };
    assert_eq!(REWRITTEN_PROMPT_RESOURCE, text, "the plugin's resource edit must reach the client");
    assert_ne!(BACKEND_PROMPT_RESOURCE, text);
    assert_eq!("file:///app.env", uri, "identity the plugin did not touch is preserved");

    let ContentBlock::Image(image) = &result.messages[2].content else {
        panic!("expected the image to survive as an image");
    };
    assert_eq!(BACKEND_PROMPT_IMAGE, image.data, "untouched content passes through unchanged");
    assert_eq!(McpRole::Assistant, result.messages[2].role, "roles survive the round trip");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_denial_returns_plugin_error_code() {
    let plugin = Arc::new(
        PromptTestPlugin::new("prompt-post-deny", vec![cmf_hook_names::PROMPT_POST_FETCH])
            .with_behavior(PromptBehavior::Deny),
    );
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;

    let error = service.get_prompt(review_request("weather")).await.expect_err("denied prompt fails the call");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode(PROMPT_POST_DENY_ERROR_CODE), code);
    assert!(
        message.contains("prompt"),
        "a denied prompt must not be reported to the client as a denied tool call: {message}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_post_hook_error_flag_fails_the_call() {
    let plugin = Arc::new(
        PromptTestPlugin::new("prompt-post-error", vec![cmf_hook_names::PROMPT_POST_FETCH])
            .with_behavior(PromptBehavior::MarkError),
    );
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;

    let error = service.get_prompt(review_request("weather")).await.expect_err("flagged prompt fails the call");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode::INVALID_REQUEST, code);
    assert_eq!(PROMPT_ERROR_MESSAGE, message);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_hooks_run_either_side_of_the_backend_call() {
    let events: Arc<StdMutex<Vec<&'static str>>> = Arc::new(StdMutex::new(Vec::new()));
    let plugin = Arc::new(
        PromptTestPlugin::new(
            "prompt-ordering",
            vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        )
        .with_events(Arc::clone(&events)),
    );
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway_with_events(TEST_USER_ID, runtime, Arc::clone(&events)).await;
    let service = gateway.connect(TEST_USER_ID).await;
    service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!(vec!["pre", "backend", "post"], *events.lock().expect("events lock poisoned"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prompt_pre_and_post_hooks_share_gateway_call_context() {
    let plugin = Arc::new(
        PromptTestPlugin::new(
            "prompt-context",
            vec![cmf_hook_names::PROMPT_PRE_FETCH, cmf_hook_names::PROMPT_POST_FETCH],
        )
        .with_behavior(PromptBehavior::ContextRoundtrip),
    );
    let observations = plugin.observations();
    let runtime = runtime_with_prompt_plugin(plugin).await;

    let gateway = start_gateway(TEST_USER_ID, true, runtime).await;
    let service = gateway.connect(TEST_USER_ID).await;
    let result = service.get_prompt(review_request("weather")).await.expect("prompt is returned");

    assert_eq!("review of weather", prompt_text(&result));

    let observations = observations.lock().expect("observations lock poisoned");
    assert_eq!(1, observations.pre_calls);
    assert_eq!(1, observations.post_calls);
}

#[tokio::test]
async fn resource_hooks_inspect_canonical_uris_and_redact_direct_and_aliased_reads() {
    let plugin = Arc::new(
        TestPlugin::new("resource", vec![cmf_hook_names::RESOURCE_PRE_FETCH, cmf_hook_names::RESOURCE_POST_FETCH])
            .with_post_rewrite(),
    );
    let observations = plugin.observations();
    let gateway = start_gateway(TEST_USER_ID, true, runtime_with_pre(plugin).await).await;
    let service = gateway.connect(TEST_USER_ID).await;
    for uri in ["file:///password.env", "file:///password.bin"] {
        for requested in [uri.to_owned(), format!("{}-{uri}", gateway.backend_name)] {
            let result =
                service.read_resource(ReadResourceRequestParams::new(requested)).await.expect("resource is returned");
            match &result.contents[0] {
                ResourceContents::TextResourceContents { text, uri: returned, .. } => {
                    assert_eq!("post:[redacted]", text);
                    assert_eq!(uri, returned);
                },
                ResourceContents::BlobResourceContents { blob, uri: returned, .. } => {
                    use base64::{Engine as _, prelude::BASE64_STANDARD};
                    assert_eq!(b"post:[redacted]", BASE64_STANDARD.decode(blob).expect("valid base64").as_slice());
                    assert_eq!(uri, returned);
                },
                _ => panic!("expected text or blob resource"),
            }
            assert_eq!(Some(uri), observations.lock().expect("observations lock").pre_resource_uri.as_deref());
        }
    }
    let observed = observations.lock().expect("observations lock");
    assert_eq!(4, observed.pre_calls);
    assert_eq!(4, observed.post_calls);
    assert_eq!(
        ["file:///password.env", "file:///password.env", "file:///password.bin", "file:///password.bin"],
        gateway.backend_state.resources.lock().expect("resource calls lock").as_slice()
    );
}

#[tokio::test]
async fn resource_pre_hook_denies_before_the_backend_read() {
    let plugin = Arc::new(TestPlugin::new("resource", vec![cmf_hook_names::RESOURCE_PRE_FETCH]).with_pre_deny());
    let gateway = start_gateway(TEST_USER_ID, true, runtime_with_pre(plugin).await).await;
    let error = gateway
        .connect(TEST_USER_ID)
        .await
        .read_resource(ReadResourceRequestParams::new("file:///password.env"))
        .await
        .expect_err("resource policy denies");
    assert_eq!(ErrorCode(PRE_DENY_ERROR_CODE), error_code(error));
    assert!(gateway.backend_state.resources.lock().expect("resource calls lock").is_empty());
}

#[tokio::test]
async fn resource_post_hook_denies_the_backend_response() {
    let plugin = Arc::new(TestPlugin::new("resource", vec![cmf_hook_names::RESOURCE_POST_FETCH]).with_post_deny());
    let gateway = start_gateway(TEST_USER_ID, true, runtime_with_post(plugin).await).await;
    let error = gateway
        .connect(TEST_USER_ID)
        .await
        .read_resource(ReadResourceRequestParams::new("file:///password.env"))
        .await
        .expect_err("resource policy denies");
    let (_, message) = error_parts(error);
    assert!(message.contains("Plugin denied resource"), "{message}");
    assert_eq!(1, gateway.backend_state.resources.lock().expect("resource calls lock").len());
}

#[tokio::test]
async fn invalid_prompt_blob_is_rejected_before_post_policy() {
    let plugin = Arc::new(PromptTestPlugin::new("prompt", vec![cmf_hook_names::PROMPT_POST_FETCH]));
    let observations = plugin.observations();
    let gateway = start_gateway(TEST_USER_ID, true, runtime_with_prompt_plugin(plugin).await).await;
    let error = gateway
        .connect(TEST_USER_ID)
        .await
        .get_prompt(GetPromptRequestParams::new("review_invalid_blob"))
        .await
        .expect_err("invalid content fails closed");
    let (code, message) = error_parts(error);
    assert_eq!(ErrorCode::INTERNAL_ERROR, code);
    assert_eq!("Prompt response contains unsupported or invalid content", message);
    assert_eq!(0, observations.lock().expect("observations lock").post_calls);
}
