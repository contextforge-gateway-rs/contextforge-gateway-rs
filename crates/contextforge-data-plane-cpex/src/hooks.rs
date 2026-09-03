use std::{any::Any, sync::Arc};

use rmcp::{
    ErrorData,
    model::{CallToolRequestParams, GetPromptRequestParams},
};
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

pub struct ResourcePreFetchResult {
    pub state: ResourceHookState,
}

/// Opaque state connecting a resource pre-fetch hook to its post-fetch hook.
pub struct ResourceHookState(Option<RuntimeHookState>);

impl ResourceHookState {
    pub(crate) fn active(state: RuntimeHookState) -> Self {
        Self(Some(state))
    }

    pub(crate) fn into_inner(self) -> Option<RuntimeHookState> {
        self.0
    }
}

impl ResourcePreFetchResult {
    pub fn unchanged() -> Self {
        Self { state: ResourceHookState(None) }
    }

    pub(crate) fn with_post_state(state: RuntimeHookState) -> Self {
        Self { state: ResourceHookState::active(state) }
    }
}

pub(crate) fn invalid_resource_hook_state_error() -> ErrorData {
    ErrorData::internal_error("Resource post-hook state is missing or invalid", None)
}

impl PromptPreFetchResult {
    pub fn unchanged() -> Self {
        Self { arguments: PromptArgumentsUpdate::Unchanged, state: None }
    }
}
