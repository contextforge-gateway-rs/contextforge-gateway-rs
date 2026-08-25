use rmcp::{
    ErrorData, RoleServer,
    model::{CompleteRequestParams, CompleteResult, ErrorCode},
    service::RequestContext,
};

use super::McpService;

#[allow(clippy::unused_async)]
pub(super) async fn complete(
    _: &McpService,
    _: CompleteRequestParams,
    _: RequestContext<RoleServer>,
) -> Result<CompleteResult, ErrorData> {
    Err(ErrorData {
        code: ErrorCode::INVALID_REQUEST,
        message: "Fan out not supported at the moment. Go to control plane".into(),
        data: None,
    })
}
