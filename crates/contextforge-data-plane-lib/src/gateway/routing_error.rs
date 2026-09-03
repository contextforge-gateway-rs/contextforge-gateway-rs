use rmcp::{ErrorData, model::ErrorCode, service::ServiceError};
use tracing::warn;

pub(super) fn backend_forward_error(op: &str, backend_name: &str, error: &ServiceError) -> ErrorData {
    warn!("{op}: backend {backend_name} error = {error:?}");

    match error {
        ServiceError::McpError(mcp_error) => mcp_error.to_owned(),
        _ => ErrorData {
            code: ErrorCode::INTERNAL_ERROR,
            message: "Routing problem... got no responses from backends".into(),
            data: None,
        },
    }
}
