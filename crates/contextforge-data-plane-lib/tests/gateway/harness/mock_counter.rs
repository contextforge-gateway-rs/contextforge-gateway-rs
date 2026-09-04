#![allow(clippy::pedantic)]

use std::{sync::Arc, time::Duration};

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, handler::server::wrapper::Parameters, model::*, prompt,
    prompt_handler, prompt_router, schemars, service::RequestContext, tool, tool_handler, tool_router,
};
use serde_json::json;
use tokio::sync::Mutex;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StructRequest {
    pub a: i32,
    pub b: i32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ExamplePromptArgs {
    /// A message to put in the prompt
    pub message: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct CounterAnalysisArgs {
    /// The target value you're trying to reach
    pub goal: i32,
    /// Preferred strategy: 'fast' or 'careful'
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

#[derive(Clone)]
pub struct Counter {
    counter: Arc<Mutex<i32>>,
}

#[tool_router]
impl Counter {
    pub fn new() -> Self {
        Self { counter: Arc::new(Mutex::new(0)) }
    }

    fn _create_resource_text(&self, uri: &str, name: &str) -> Resource {
        Resource::new(uri, name)
    }

    #[tool(description = "Increment the counter by 1")]
    async fn increment(&self) -> Result<CallToolResult, McpError> {
        let mut counter = self.counter.lock().await;
        *counter += 1;
        Ok(CallToolResult::success(vec![ContentBlock::text(counter.to_string())]))
    }

    #[tool(description = "Decrement the counter by 1")]
    async fn decrement(&self) -> Result<CallToolResult, McpError> {
        let mut counter = self.counter.lock().await;
        *counter -= 1;
        Ok(CallToolResult::success(vec![ContentBlock::text(counter.to_string())]))
    }

    #[tool(description = "Get the current counter value")]
    async fn get_value(&self) -> Result<CallToolResult, McpError> {
        let counter = self.counter.lock().await;
        Ok(CallToolResult::success(vec![ContentBlock::text(counter.to_string())]))
    }

    #[tool(description = "Long running task example")]
    async fn long_task(&self) -> Result<CallToolResult, McpError> {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        Ok(CallToolResult::success(vec![ContentBlock::text("Long task completed")]))
    }

    #[tool(description = "Say hello to the client")]
    fn say_hello(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text("hello")]))
    }

    #[tool(description = "Repeat what you say")]
    fn echo(&self, Parameters(object): Parameters<JsonObject>) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::Value::Object(object).to_string())]))
    }

    #[tool(description = "Calculate the sum of two numbers")]
    fn sum(&self, Parameters(StructRequest { a, b }): Parameters<StructRequest>) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text((a + b).to_string())]))
    }

    /// Returns the `Mcp-Session-Id` of the current session (streamable HTTP only).
    #[tool(description = "Get the session ID for this connection")]
    fn get_session_id(&self, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let session_id = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.headers.get("mcp-session-id"))
            .map(|v| v.to_str().unwrap_or("(non-ascii)").to_owned());

        match session_id {
            Some(id) => Ok(CallToolResult::success(vec![ContentBlock::text(id)])),
            None => {
                Ok(CallToolResult::success(vec![ContentBlock::text("no session (not running over streamable HTTP?)")]))
            },
        }
    }
}

#[prompt_router]
impl Counter {
    /// This is an example prompt that takes one required argument, message
    #[prompt(
        name = "example_prompt",
        meta = MetaObject(rmcp::object!({"meta_key": "meta_value"}))
    )]
    async fn example_prompt(
        &self,
        Parameters(args): Parameters<ExamplePromptArgs>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<Vec<PromptMessage>, McpError> {
        let prompt = format!("This is an example prompt with your message here: '{}'", args.message);
        Ok(vec![PromptMessage::new_text(Role::User, prompt)])
    }

    /// Analyze the current counter value and suggest next steps
    #[prompt(name = "counter_analysis")]
    async fn counter_analysis(
        &self,
        Parameters(args): Parameters<CounterAnalysisArgs>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let strategy = args.strategy.unwrap_or_else(|| "careful".to_owned());
        let current_value = *self.counter.lock().await;
        let difference = args.goal - current_value;

        let messages = vec![
            PromptMessage::new_text(
                Role::Assistant,
                "I'll analyze the counter situation and suggest the best approach.",
            ),
            PromptMessage::new_text(
                Role::User,
                format!(
                    "Current counter value: {}\nGoal value: {}\nDifference: {}\nStrategy preference: {}\n\nPlease analyze the situation and suggest the best approach to reach the goal.",
                    current_value, args.goal, difference, strategy
                ),
            ),
        ];

        Ok(GetPromptResult::new(messages)
            .with_description(format!("Counter analysis for reaching {} from {}", args.goal, current_value)))
    }
}

#[tool_handler(meta = MetaObject(rmcp::object!({"tool_meta_key": "tool_meta_value"})))]
#[prompt_handler(meta = MetaObject(rmcp::object!({"router_meta_key": "router_meta_value"})))]
impl ServerHandler for Counter {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_completions()
                .enable_prompts()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
        .with_instructions("This server provides counter tools and prompts. Tools: increment, decrement, get_value, say_hello, echo, sum. Prompts: example_prompt (takes a message), counter_analysis (analyzes counter state with a goal).".to_owned())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            self._create_resource_text("str:////Users/to/some/path/", "cwd"),
            self._create_resource_text("memo://insights", "memo-name"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = &request.uri;
        match uri.as_str() {
            "str:////Users/to/some/path/" => {
                let cwd = "/Users/to/some/path/";
                Ok(ReadResourceResult::new(vec![ResourceContents::text(cwd, uri.clone())]).into())
            },
            "memo://insights" => {
                let memo = "Business Intelligence Memo\n\nAnalysis has revealed 5 key insights ...";
                Ok(ReadResourceResult::new(vec![ResourceContents::text(memo, uri.clone())]).into())
            },
            _ => Err(McpError::resource_not_found(
                "resource_not_found",
                Some(json!({
                    "uri": uri
                })),
            )),
        }
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        // Only backend-local references are known here; a still-namespaced name/URI won't match,
        // proving the gateway stripped the prefix before forwarding.
        let values = match &request.r#ref {
            Reference::Prompt(prompt) if prompt.name == "example_prompt" && request.argument.name == "message" => {
                vec!["hello".to_owned(), "hola".to_owned()]
            },
            Reference::Resource(resource)
                if matches!(resource.uri.as_str(), "str:////Users/to/some/path/" | "memo://insights") =>
            {
                vec![resource.uri.clone()]
            },
            _ => return Err(McpError::invalid_params("unknown completion reference", None)),
        };
        Ok(CompleteResult::new(CompletionInfo::new(values).map_err(|e| McpError::internal_error(e, None))?))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if is_known_resource_uri(&request.uri) {
            let uri = request.uri.clone();
            let peer = context.peer;
            // Keeps notifying even after unsubscribe (a rude backend): the gateway must stop
            // forwarding updates for unsubscribed URIs itself, and tests assert exactly that.
            tokio::spawn(async move {
                for _ in 0..MAX_RESOURCE_UPDATE_NOTIFICATIONS {
                    if peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.clone())).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(RESOURCE_UPDATE_NOTIFY_INTERVAL).await;
                }
            });
            Ok(())
        } else {
            Err(McpError::resource_not_found("resource_not_found", Some(json!({ "uri": request.uri }))))
        }
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if is_known_resource_uri(&request.uri) {
            Ok(())
        } else {
            Err(McpError::resource_not_found("resource_not_found", Some(json!({ "uri": request.uri }))))
        }
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let resource_templates = vec![
            ResourceTemplate::new("str:////{path}", "filesystem")
                .with_description("Read a file by absolute path")
                .with_mime_type("text/plain"),
            ResourceTemplate::new("memo://{id}", "memo")
                .with_description("Read a memo by id")
                .with_mime_type("text/plain"),
        ];
        Ok(ListResourceTemplatesResult::with_all_items(resource_templates))
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        if let Some(http_request_part) = context.extensions.get::<axum::http::request::Parts>() {
            let initialize_headers = &http_request_part.headers;
            let initialize_uri = &http_request_part.uri;
            tracing::info!(?initialize_headers, %initialize_uri, "initialize from http server");
        }
        Ok(self.get_info())
    }
}

/// The backend-local resource URIs this mock owns; subscribe/unsubscribe only succeed for these,
/// so a successful gateway call proves the namespace prefix was stripped before forwarding.
pub const KNOWN_RESOURCE_URIS: [&str; 2] = ["str:////Users/to/some/path/", "memo://insights"];

fn is_known_resource_uri(uri: &str) -> bool {
    KNOWN_RESOURCE_URIS.contains(&uri)
}

/// Interval between the resource-update notifications sent after a subscribe is accepted.
pub const RESOURCE_UPDATE_NOTIFY_INTERVAL: Duration = Duration::from_millis(10);

/// Safety cap so notify loops can't outlive a hung test run.
const MAX_RESOURCE_UPDATE_NOTIFICATIONS: usize = 1000;
