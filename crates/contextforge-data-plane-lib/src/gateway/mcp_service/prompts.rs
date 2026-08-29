use contextforge_data_plane_cpex::PromptPreFetchResult;
use rmcp::{
    ErrorData, RoleServer,
    model::{ErrorCode, GetPromptRequestParams, GetPromptResponse},
    service::RequestContext,
};
use tracing::info;

use super::McpService;
use crate::gateway::{
    mcp_call_validator::AuthorizedCallValidator, mcp_service::initialization::connect_backend_for_request,
    routing_error::backend_forward_error,
};

pub(super) async fn get_prompt(
    mcp_service: &McpService,
    request: GetPromptRequestParams,
    cx: RequestContext<RoleServer>,
) -> Result<GetPromptResponse, ErrorData> {
    let mcp_call_validator = AuthorizedCallValidator::new("get_prompt", &cx);
    let (virtual_host, _claims) = mcp_call_validator.validate_stateless()?;
    let Some((backend_name, prompt_name)) = virtual_host.prompts.get(&request.name) else {
        return Err(ErrorData {
            code: ErrorCode::INVALID_PARAMS,
            message: "Routing problem... prompt not found".into(),
            data: None,
        });
    };

    let backend_name = backend_name.to_owned();
    let prompt_name = prompt_name.to_owned();

    let backend = virtual_host.backends.get(&backend_name).ok_or_else(|| ErrorData {
        code: ErrorCode::INVALID_PARAMS,
        message: "Routing problem... backend not found".into(),
        data: None,
    })?;
    let pre_result = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.before_get_prompt(&request, &prompt_name, &backend_name).await?
    } else {
        PromptPreFetchResult::unchanged()
    };
    let mut backend_service = connect_backend_for_request(mcp_service, &backend_name, backend, &cx).await?;
    let mut routed_request = request;
    pre_result.arguments.apply_to_request(&mut routed_request, &prompt_name);
    let response = backend_service.get_prompt(routed_request).await;
    if let Err(error) = backend_service.close().await {
        tracing::warn!("get_prompt: backend cleanup failed backend_name = {backend_name} error = {error:?}");
    }
    let response = response.map_err(|error| backend_forward_error("get_prompt", &backend_name, &error))?;
    info!("get_prompt: backend {backend_name} returned {} messages", response.messages.len());
    let response = if let Some(plugin_runtime) = &mcp_service.plugin_runtime {
        plugin_runtime.after_get_prompt(&prompt_name, response, pre_result.state).await?
    } else {
        response
    };
    Ok(response.into())
}
