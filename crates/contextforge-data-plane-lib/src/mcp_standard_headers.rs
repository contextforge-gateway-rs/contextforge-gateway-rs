use std::collections::{HashMap, HashSet};

use base64::{Engine, prelude::BASE64_STANDARD};
use http::{HeaderMap, HeaderName, HeaderValue};
use rmcp::model::ProtocolVersion;
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

pub(crate) fn required_for(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER_MCP_PROTOCOL_VERSION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|version| version >= ProtocolVersion::STANDARD_HEADERS.as_str())
}

fn is_exact(name: &HeaderName, expected: &str) -> bool {
    name.as_str().eq_ignore_ascii_case(expected)
}

fn is_param(name: &HeaderName) -> bool {
    name.as_str()
        .get(..HEADER_MCP_PARAM_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(HEADER_MCP_PARAM_PREFIX))
}

/// Validate SEP-2243 parameter headers against a routed tool call.
pub(crate) fn validate_tool_params(
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    input_schema: &JsonObject,
) -> Result<(), String> {
    for (property, annotation) in param_header_annotations(input_schema)? {
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

/// Add SEP-2243 parameter headers for a routed upstream tool call.
pub(crate) fn insert_tool_params(
    headers: &mut HashMap<HeaderName, HeaderValue>,
    arguments: Option<&JsonObject>,
    input_schema: &JsonObject,
) -> Result<(), String> {
    for (property, annotation) in param_header_annotations(input_schema)? {
        let Some(value) = arguments.and_then(|arguments| arguments.get(&property)).and_then(primitive_to_string) else {
            continue;
        };
        let header_name = format!("{HEADER_MCP_PARAM_PREFIX}{annotation}");
        let header_name = HeaderName::from_bytes(header_name.as_bytes())
            .map_err(|error| format!("invalid parameter header name: {error}"))?;
        let header_value = HeaderValue::from_str(&encode_header_value(&value))
            .map_err(|error| format!("invalid parameter header value: {error}"))?;
        headers.insert(header_name, header_value);
    }
    Ok(())
}

fn param_header_annotations(input_schema: &JsonObject) -> Result<Vec<(String, String)>, String> {
    let Some(Value::Object(properties)) = input_schema.get("properties") else {
        return Ok(Vec::new());
    };
    let mut annotations = Vec::new();
    let mut seen = HashSet::new();
    for (property, schema) in properties {
        reject_nested_annotations(schema, property)?;
        let Some(raw) = schema.get("x-mcp-header") else {
            continue;
        };
        let Value::String(annotation) = raw else {
            return Err(format!("property `{property}`: x-mcp-header must be a string"));
        };
        if annotation.is_empty() {
            return Err(format!("property `{property}`: x-mcp-header must not be empty"));
        }
        if !annotation.chars().all(is_tchar) {
            return Err(format!("property `{property}`: x-mcp-header `{annotation}` is not a valid HTTP token"));
        }
        if !seen.insert(annotation.to_ascii_lowercase()) {
            return Err(format!("property `{property}`: duplicate x-mcp-header `{annotation}` (case-insensitive)"));
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("string" | "integer" | "boolean") => {},
            other => {
                return Err(format!(
                    "property `{property}`: x-mcp-header requires a primitive type \
                     (string/integer/boolean), got {other:?}"
                ));
            },
        }
        annotations.push((property.clone(), annotation.clone()));
    }
    Ok(annotations)
}

fn reject_nested_annotations(schema: &Value, path: &str) -> Result<(), String> {
    if let Some(Value::Object(properties)) = schema.get("properties") {
        for (property, nested_schema) in properties {
            if nested_schema.get("x-mcp-header").is_some() {
                return Err(format!(
                    "property `{path}.{property}`: x-mcp-header is not supported on nested properties"
                ));
            }
            reject_nested_annotations(nested_schema, &format!("{path}.{property}"))?;
        }
    }
    Ok(())
}

fn primitive_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn encode_header_value(value: &str) -> String {
    if requires_base64(value) {
        format!("{BASE64_HEADER_PREFIX}{}{BASE64_HEADER_SUFFIX}", BASE64_STANDARD.encode(value))
    } else {
        value.to_owned()
    }
}

fn decode_header_value(value: &str) -> Option<String> {
    match value.strip_prefix(BASE64_HEADER_PREFIX).and_then(|inner| inner.strip_suffix(BASE64_HEADER_SUFFIX)) {
        Some(inner) => String::from_utf8(BASE64_STANDARD.decode(inner).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

fn requires_base64(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let bytes = value.as_bytes();
    if matches!(bytes.first(), Some(b' ' | b'\t')) || matches!(bytes.last(), Some(b' ' | b'\t')) {
        return true;
    }
    value.chars().any(|character| !(0x20..=0x7e).contains(&(character as u32)))
        || value.starts_with(BASE64_HEADER_PREFIX) && value.ends_with(BASE64_HEADER_SUFFIX)
}

fn is_tchar(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '!' | '#' | '$' | '%' | '&' | '\'' | '*' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~')
}

#[cfg(test)]
mod tests {
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
    fn parameter_headers_round_trip_primitives_and_unsafe_values() {
        let arguments = json!({ "region": " leading snowman ☃", "count": 3, "dryRun": false });
        let arguments = arguments.as_object().expect("object arguments");
        let mut headers = HashMap::new();

        insert_tool_params(&mut headers, Some(arguments), &schema()).expect("headers are generated");
        let headers: HeaderMap = headers.into_iter().collect();

        assert!(
            headers
                .get("Mcp-Param-Region")
                .expect("region header")
                .to_str()
                .expect("header string")
                .starts_with(BASE64_HEADER_PREFIX)
        );
        validate_tool_params(&headers, Some(arguments), &schema()).expect("headers match arguments");
    }

    #[test]
    fn null_parameter_is_omitted_and_rejected_when_present() {
        let arguments = json!({ "region": null });
        let arguments = arguments.as_object().expect("object arguments");
        let mut headers = HashMap::new();

        insert_tool_params(&mut headers, Some(arguments), &schema()).expect("headers are generated");
        assert!(!headers.contains_key("Mcp-Param-Region"));

        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-region"),
            HeaderValue::from_static("unexpected"),
        )]);
        assert!(validate_tool_params(&headers, Some(arguments), &schema()).is_err());
    }
}
