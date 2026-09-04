use axum::response::Response;
use http::{StatusCode, header};

pub(crate) fn unauthorized_response(message: &str) -> Response {
    custom_error(StatusCode::UNAUTHORIZED, message)
}

pub(crate) fn bad_request(message: &str) -> Response {
    custom_error(StatusCode::BAD_REQUEST, message)
}

pub(crate) fn internal_server_error(message: &str) -> Response {
    custom_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

pub(crate) fn custom_error(status: StatusCode, message: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(message.to_owned().into())
        .expect("Expecting this to work")
}
