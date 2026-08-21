use axum::{
    body::{Body, to_bytes},
    extract::State,
    middleware::Next,
    response::Response,
};
use contextforge_data_plane_apis::user_store::UserConfig;
use http::{Method, StatusCode, header};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, ErrorData, JsonRpcError};

use crate::{gateway::resolve_tool_route, layers::virtual_host_id::VirtualHostId, mcp_standard_headers};

pub async fn mcp_param_validation_layer(
    State(max_request_body_bytes): State<usize>,
    request: http::Request<Body>,
    next: Next,
) -> Response {
    if request.method() != Method::POST || !mcp_standard_headers::required_for(request.headers()) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, max_request_body_bytes).await else {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .body(Body::from("Payload Too Large"))
            .expect("payload-too-large response builds");
    };

    if let Some(response) = validation_error(&parts, &body) {
        return response;
    }

    next.run(http::Request::from_parts(parts, Body::from(body))).await
}

fn validation_error(parts: &http::request::Parts, body: &[u8]) -> Option<Response> {
    let message = serde_json::from_slice::<ClientJsonRpcMessage>(body).ok()?;
    let ClientJsonRpcMessage::Request(request) = message else {
        return None;
    };
    let ClientRequest::CallToolRequest(tool_call) = &request.request else {
        return None;
    };
    let user_config = parts.extensions.get::<UserConfig>()?;
    let virtual_host_id = parts.extensions.get::<VirtualHostId>()?;
    let virtual_host = user_config.virtual_hosts.get(virtual_host_id.value())?;
    let backend_names: Vec<&str> = virtual_host.backends.keys().map(String::as_str).collect();
    let (backend_name, tool_name) = resolve_tool_route(virtual_host, &tool_call.params.name, &backend_names)?;
    let tool_schema = virtual_host.backends.get(backend_name)?.tool_schemas.get(tool_name)?;
    let reason =
        mcp_standard_headers::validate_tool_params(&parts.headers, tool_call.params.arguments.as_ref(), tool_schema)
            .err()?;

    let error = JsonRpcError::new(Some(request.id), ErrorData::header_mismatch(reason, None));
    let body = serde_json::to_vec(&error).expect("JSON-RPC header mismatch serializes");
    Some(
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("header mismatch response builds"),
    )
}
