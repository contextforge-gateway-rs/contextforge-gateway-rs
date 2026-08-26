use base64::{Engine, prelude::BASE64_STANDARD};
use http::{HeaderMap, HeaderName};
use rmcp::transport::common::http_header::{
    BASE64_HEADER_PREFIX, BASE64_HEADER_SUFFIX, HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PARAM_PREFIX,
    HEADER_MCP_PROTOCOL_VERSION, HEADER_SESSION_ID,
};
use serde_json::{Map, Value};

type JsonObject = Map<String, Value>;

pub(crate) fn is_limited(name: &HeaderName) -> bool {
    is_exact(name, HEADER_MCP_METHOD)
        || is_exact(name, HEADER_MCP_NAME)
        || is_exact(name, HEADER_MCP_PROTOCOL_VERSION)
        || is_exact(name, HEADER_SESSION_ID)
        || is_param(name)
}

pub(crate) fn is_computed(name: &HeaderName) -> bool {
    is_exact(name, HEADER_MCP_METHOD)
        || is_exact(name, HEADER_MCP_NAME)
        || is_exact(name, HEADER_MCP_PROTOCOL_VERSION)
        || is_param(name)
}

fn is_exact(name: &HeaderName, expected: &str) -> bool {
    name.as_str().eq_ignore_ascii_case(expected)
}

pub(crate) fn is_param(name: &HeaderName) -> bool {
    name.as_str()
        .get(..HEADER_MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(HEADER_MCP_PARAM_PREFIX))
}

/// Validates SEP-2243 parameter headers against a routed tool call.
pub(crate) fn validate_tool_params(
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    input_schema: &JsonObject,
) -> Result<(), String> {
    for (property, annotation) in param_header_annotations(input_schema) {
        let header_name = format!("{HEADER_MCP_PARAM_PREFIX}{annotation}");
        let header_value = headers.get(&header_name).and_then(|value| value.to_str().ok());
        let body_value = arguments
            .and_then(|arguments| arguments.get(&property))
            .filter(|value| !value.is_null())
            .and_then(primitive_to_string);

        match (header_value, body_value) {
            (None, None) => {},
            (Some(_), None) => {
                return Err(format!("unexpected {header_name} header for absent or null `{property}`"));
            },
            (None, Some(_)) => return Err(format!("missing {header_name} header for `{property}`")),
            (Some(raw), Some(expected)) => {
                let decoded =
                    decode_header_value(raw).ok_or_else(|| format!("{header_name} header is not valid Base64"))?;
                if decoded != expected {
                    return Err(format!("{header_name} header `{decoded}` does not match body value `{expected}`"));
                }
            },
        }
    }
    Ok(())
}

fn param_header_annotations(input_schema: &JsonObject) -> Vec<(String, String)> {
    input_schema
        .get("properties")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(property, schema)| {
            schema
                .get("x-mcp-header")
                .and_then(Value::as_str)
                .filter(|annotation| !annotation.is_empty())
                .map(|annotation| (property.clone(), annotation.to_owned()))
        })
        .collect()
}

fn primitive_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn decode_header_value(value: &str) -> Option<String> {
    match value.strip_prefix(BASE64_HEADER_PREFIX).and_then(|inner| inner.strip_suffix(BASE64_HEADER_SUFFIX)) {
        Some(inner) => String::from_utf8(BASE64_STANDARD.decode(inner).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use serde_json::json;

    use super::*;

    fn schema() -> JsonObject {
        json!({
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "count": { "type": "integer", "x-mcp-header": "Count" },
                "dryRun": { "type": "boolean", "x-mcp-header": "Dry-Run" },
            },
        })
        .as_object()
        .expect("object schema")
        .clone()
    }

    #[test]
    fn matching_parameter_headers_are_validated() {
        let arguments = json!({ "region": " leading snowman ☃", "count": 3, "dryRun": false });
        let arguments = arguments.as_object().expect("object arguments");
        let encoded =
            format!("{BASE64_HEADER_PREFIX}{}{BASE64_HEADER_SUFFIX}", BASE64_STANDARD.encode(" leading snowman ☃"));
        let headers = HeaderMap::from_iter([
            (HeaderName::from_static("mcp-param-region"), HeaderValue::from_str(&encoded).expect("encoded header")),
            (HeaderName::from_static("mcp-param-count"), HeaderValue::from_static("3")),
            (HeaderName::from_static("mcp-param-dry-run"), HeaderValue::from_static("false")),
        ]);

        validate_tool_params(&headers, Some(arguments), &schema()).expect("headers match arguments");
    }

    #[test]
    fn null_parameter_is_omitted_and_rejected_when_present() {
        let arguments = json!({ "region": null });
        let arguments = arguments.as_object().expect("object arguments");
        validate_tool_params(&HeaderMap::new(), Some(arguments), &schema()).expect("null parameter needs no header");

        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-region"),
            HeaderValue::from_static("unexpected"),
        )]);
        assert!(validate_tool_params(&headers, Some(arguments), &schema()).is_err());
    }
}
