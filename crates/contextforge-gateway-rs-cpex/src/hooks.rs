use std::{any::Any, sync::Arc};

use std::collections::HashMap;

use rmcp::model::{ArgumentInfo, CallToolRequestParams, GetPromptRequestParams};
use serde_json::{Map, Value};

pub type RuntimeHookError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type RuntimeHookState = Arc<dyn Any + Send + Sync + 'static>;

#[derive(Debug)]
pub enum ToolArgumentsUpdate {
    Unchanged,
    Replace(Option<Map<String, Value>>),
}

impl ToolArgumentsUpdate {
    pub fn apply_to_request(self, request: &mut CallToolRequestParams, routed_tool_name: &str) {
        request.name = routed_tool_name.to_owned().into();
        if let Self::Replace(arguments) = self {
            request.arguments = arguments;
        }
    }
}

pub struct ToolPreCallResult {
    pub arguments: ToolArgumentsUpdate,
    pub state: Option<RuntimeHookState>,
}

impl ToolPreCallResult {
    pub fn unchanged() -> Self {
        Self { arguments: ToolArgumentsUpdate::Unchanged, state: None }
    }
}

#[derive(Debug)]
pub enum PromptArgumentsUpdate {
    Unchanged,
    Replace(Option<Map<String, Value>>),
}

impl PromptArgumentsUpdate {
    pub fn apply_to_request(self, request: &mut GetPromptRequestParams, routed_prompt_name: &str) {
        routed_prompt_name.clone_into(&mut request.name);
        if let Self::Replace(arguments) = self {
            request.arguments = arguments;
        }
    }
}

pub struct PromptPreFetchResult {
    pub arguments: PromptArgumentsUpdate,
    pub state: Option<RuntimeHookState>,
}

impl PromptPreFetchResult {
    pub fn unchanged() -> Self {
        Self { arguments: PromptArgumentsUpdate::Unchanged, state: None }
    }
}

/// Which CPEX hook family a `completion/complete` request dispatches to.
///
/// CPEX defines no completion hook, so a completion runs the hooks of the thing being completed:
/// a prompt reference uses the prompt hooks, a resource or template reference uses the resource
/// hooks. Plugins tell a completion apart from a plain fetch by the `gateway-completion-*`
/// correlation id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionTarget {
    Prompt,
    Resource,
}

/// A routed `completion/complete` request as the hooks see it.
pub struct CompletionRequest<'a> {
    /// Which hook family this reference dispatches to.
    pub target: CompletionTarget,
    /// Backend-local prompt name or resource URI, with the namespace prefix already stripped.
    pub identifier: &'a str,
    /// Backend that owns the reference.
    pub backend_name: &'a str,
    /// The argument being completed.
    pub argument: &'a ArgumentInfo,
    /// Argument values the client already resolved, when it supplied any.
    pub context: Option<&'a HashMap<String, String>>,
}

/// Outcome of the resource pre hook on `resources/subscribe` and `resources/unsubscribe`. These
/// paths return an empty acknowledgement, so there is no post hook and no state to carry.
#[derive(Debug)]
pub enum ResourceUriUpdate {
    Unchanged,
    Replace(String),
}

/// Outcome of the resource pre hook on `resources/read`, which unlike the subscription paths does
/// have a post hook, so it carries state forward.
pub struct ResourcePreFetchResult {
    pub uri: ResourceUriUpdate,
    pub state: Option<RuntimeHookState>,
}

impl ResourcePreFetchResult {
    pub fn unchanged() -> Self {
        Self { uri: ResourceUriUpdate::Unchanged, state: None }
    }
}

impl ResourceUriUpdate {
    /// Resolves the URI the gateway should forward to the backend and track the subscription
    /// under. Both must be the same value, or a rewritten subscription would be tracked under a
    /// URI the backend never notifies about.
    pub fn apply_to(self, routed_uri: String) -> String {
        match self {
            Self::Unchanged => routed_uri,
            Self::Replace(uri) => uri,
        }
    }
}
