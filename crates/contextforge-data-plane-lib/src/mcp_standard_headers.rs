use base64::{Engine, prelude::BASE64_STANDARD};
use std::collections::HashSet;

use http::{HeaderMap, HeaderName, HeaderValue};
use rmcp::transport::common::http_header::{
    BASE64_HEADER_PREFIX, BASE64_HEADER_SUFFIX, HEADER_MCP_METHOD, HEADER_MCP_NAME, HEADER_MCP_PARAM_PREFIX,
    HEADER_MCP_PROTOCOL_VERSION, HEADER_SESSION_ID,
};
use serde_json::{Map, Value};

type JsonObject = Map<String, Value>;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const MIN_SAFE_INTEGER: i64 = -MAX_SAFE_INTEGER;

#[derive(Clone, Copy)]
enum ParameterType {
    Boolean,
    Integer,
    String,
}

struct ParamHeaderAnnotation {
    header_name: HeaderName,
    header_name_display: String,
    parameter_type: ParameterType,
    property_path: Vec<String>,
}

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
    for annotation in param_header_annotations(input_schema)? {
        let header_values = headers.get_all(&annotation.header_name);
        let mut header_values = header_values.iter();
        let header_value = header_values.next();
        let body_value = value_at_property_path(arguments, &annotation.property_path).filter(|value| !value.is_null());
        let property_path = annotation.property_path.join(".");

        match (header_value, body_value) {
            (None, None) => {},
            (Some(_), None) => {
                return Err(format!(
                    "unexpected {} header for absent or null `{property_path}`",
                    annotation.header_name_display
                ));
            },
            (None, Some(_)) => {
                return Err(format!("missing {} header for `{property_path}`", annotation.header_name_display));
            },
            (Some(first), Some(body_value)) => {
                let expected = parameter_value(body_value, annotation.parameter_type, &property_path)?;
                for raw in std::iter::once(first).chain(header_values) {
                    let decoded = decode_header_value(raw).map_err(|reason| {
                        format!("{} header is malformed: {reason}", annotation.header_name_display)
                    })?;
                    if !parameter_values_match(&decoded, &expected) {
                        return Err(format!(
                            "{} header `{decoded}` does not match body value `{}`",
                            annotation.header_name_display,
                            expected.display()
                        ));
                    }
                }
            },
        }
    }
    Ok(())
}

fn param_header_annotations(input_schema: &JsonObject) -> Result<Vec<ParamHeaderAnnotation>, String> {
    let mut annotations = Vec::new();
    let mut seen_headers = HashSet::new();
    visit_schema(input_schema, "$", None, &mut seen_headers, &mut annotations)?;
    Ok(annotations)
}

fn visit_schema(
    schema: &JsonObject,
    schema_path: &str,
    property_path: Option<&[String]>,
    seen_headers: &mut HashSet<String>,
    annotations: &mut Vec<ParamHeaderAnnotation>,
) -> Result<(), String> {
    if let Some(raw_header) = schema.get("x-mcp-header") {
        let Some(property_path) = property_path else {
            return Err(format!("schema `{schema_path}`: x-mcp-header is not on a statically reachable property"));
        };
        let property_path_display = property_path.join(".");
        let Value::String(header) = raw_header else {
            return Err(format!("property `{property_path_display}`: x-mcp-header must be a string"));
        };
        if header.is_empty() {
            return Err(format!("property `{property_path_display}`: x-mcp-header must not be empty"));
        }
        if !header.bytes().all(is_tchar) {
            return Err(format!(
                "property `{property_path_display}`: x-mcp-header `{header}` is not a valid HTTP token"
            ));
        }
        if !seen_headers.insert(header.to_ascii_lowercase()) {
            return Err(format!(
                "property `{property_path_display}`: duplicate x-mcp-header `{header}` (case-insensitive)"
            ));
        }
        let parameter_type = match schema.get("type").and_then(Value::as_str) {
            Some("boolean") => ParameterType::Boolean,
            Some("integer") => ParameterType::Integer,
            Some("string") => ParameterType::String,
            other => {
                return Err(format!(
                    "property `{property_path_display}`: x-mcp-header requires type string, integer, or boolean; got {other:?}"
                ));
            },
        };
        let header_name_display = format!("{HEADER_MCP_PARAM_PREFIX}{header}");
        let header_name = HeaderName::from_bytes(header_name_display.as_bytes())
            .map_err(|_| format!("property `{property_path_display}`: invalid header name `{header_name_display}`"))?;
        annotations.push(ParamHeaderAnnotation {
            header_name,
            header_name_display,
            parameter_type,
            property_path: property_path.to_vec(),
        });
    }

    for (keyword, value) in schema {
        if keyword == "properties" {
            let Some(properties) = value.as_object() else {
                reject_unreachable_annotations(value, &format!("{schema_path}.properties"))?;
                continue;
            };
            for (property, property_schema) in properties {
                let mut nested_property_path = property_path.map_or_else(Vec::new, <[String]>::to_vec);
                nested_property_path.push(property.clone());
                let nested_schema_path = format!("{schema_path}.properties.{property}");
                if let Some(property_schema) = property_schema.as_object() {
                    visit_schema(
                        property_schema,
                        &nested_schema_path,
                        Some(&nested_property_path),
                        seen_headers,
                        annotations,
                    )?;
                } else {
                    reject_unreachable_annotations(property_schema, &nested_schema_path)?;
                }
            }
        } else if keyword != "x-mcp-header" {
            reject_unreachable_annotations(value, &format!("{schema_path}.{keyword}"))?;
        }
    }
    Ok(())
}

fn reject_unreachable_annotations(value: &Value, schema_path: &str) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            if object.contains_key("x-mcp-header") {
                return Err(format!("schema `{schema_path}`: x-mcp-header is not on a statically reachable property"));
            }
            for (key, nested) in object {
                reject_unreachable_annotations(nested, &format!("{schema_path}.{key}"))?;
            }
        },
        Value::Array(array) => {
            for (index, nested) in array.iter().enumerate() {
                reject_unreachable_annotations(nested, &format!("{schema_path}[{index}]"))?;
            }
        },
        _ => {},
    }
    Ok(())
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
        )
}

fn value_at_property_path<'a>(arguments: Option<&'a JsonObject>, property_path: &[String]) -> Option<&'a Value> {
    let (first, rest) = property_path.split_first()?;
    let mut value = arguments?.get(first)?;
    for property in rest {
        value = value.as_object()?.get(property)?;
    }
    Some(value)
}

enum ParameterValue<'a> {
    Boolean(bool),
    Integer(i64),
    String(&'a str),
}

impl ParameterValue<'_> {
    fn display(&self) -> String {
        match self {
            Self::Boolean(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::String(value) => (*value).to_owned(),
        }
    }
}

fn parameter_value<'a>(
    value: &'a Value,
    parameter_type: ParameterType,
    property_path: &str,
) -> Result<ParameterValue<'a>, String> {
    match (parameter_type, value) {
        (ParameterType::Boolean, Value::Bool(value)) => Ok(ParameterValue::Boolean(*value)),
        (ParameterType::Integer, Value::Number(value)) => {
            let integer = value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| parse_integer_header_value(&value.to_string()))
                .filter(|value| (MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
                .ok_or_else(|| {
                    format!(
                        "body value for `{property_path}` must be an integer between {MIN_SAFE_INTEGER} and {MAX_SAFE_INTEGER}"
                    )
                })?;
            Ok(ParameterValue::Integer(integer))
        },
        (ParameterType::String, Value::String(value)) => Ok(ParameterValue::String(value)),
        (ParameterType::Boolean, _) => Err(format!("body value for `{property_path}` must be a boolean")),
        (ParameterType::Integer, _) => Err(format!(
            "body value for `{property_path}` must be an integer between {MIN_SAFE_INTEGER} and {MAX_SAFE_INTEGER}"
        )),
        (ParameterType::String, _) => Err(format!("body value for `{property_path}` must be a string")),
    }
}

fn parameter_values_match(header_value: &str, body_value: &ParameterValue<'_>) -> bool {
    match body_value {
        ParameterValue::Boolean(value) => header_value == value.to_string(),
        ParameterValue::Integer(value) => {
            parse_integer_header_value(header_value).is_some_and(|header| header == *value)
        },
        ParameterValue::String(value) => header_value == *value,
    }
}

fn parse_integer_header_value(value: &str) -> Option<i64> {
    let (negative, value) = match value.as_bytes().first() {
        Some(b'-') => (true, value.get(1..)?),
        Some(b'+') => (false, value.get(1..)?),
        _ => (false, value),
    };
    let (mantissa, exponent) = match value.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (value, 0),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() => (whole, fraction),
        Some(_) => return None,
        None => (mantissa, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let mut digits = String::with_capacity(whole.len().checked_add(fraction.len())?);
    digits.push_str(whole);
    digits.push_str(fraction);
    let scale = i64::try_from(fraction.len()).ok()?.checked_sub(exponent)?;
    let (integer_digits, trailing_zeroes) = if scale > 0 {
        let scale = usize::try_from(scale).ok()?;
        if scale > digits.len() {
            if digits.bytes().all(|byte| byte == b'0') {
                return Some(0);
            }
            return None;
        }
        let integer_end = digits.len() - scale;
        if !digits.as_bytes()[integer_end..].iter().all(|byte| *byte == b'0') {
            return None;
        }
        (&digits[..integer_end], 0)
    } else {
        (&*digits, usize::try_from(scale.checked_neg()?).ok()?)
    };
    let integer_digits = integer_digits.trim_start_matches('0');
    if integer_digits.is_empty() {
        return Some(0);
    }
    if integer_digits.len().checked_add(trailing_zeroes)? > MAX_SAFE_INTEGER.to_string().len() {
        return None;
    }
    let magnitude =
        integer_digits.parse::<u64>().ok()?.checked_mul(10_u64.checked_pow(u32::try_from(trailing_zeroes).ok()?)?)?;
    if magnitude > MAX_SAFE_INTEGER.unsigned_abs() {
        return None;
    }
    let magnitude = i64::try_from(magnitude).ok()?;
    if negative { magnitude.checked_neg() } else { Some(magnitude) }
}

fn decode_header_value(value: &HeaderValue) -> Result<String, &'static str> {
    let raw = value.as_bytes();
    let raw = std::str::from_utf8(raw).map_err(|_| "value is not ASCII or UTF-8")?;
    if let Some(inner) =
        raw.strip_prefix(BASE64_HEADER_PREFIX).and_then(|inner| inner.strip_suffix(BASE64_HEADER_SUFFIX))
    {
        let decoded = BASE64_STANDARD.decode(inner).map_err(|_| "sentinel contains invalid Base64")?;
        return String::from_utf8(decoded).map_err(|_| "sentinel does not contain UTF-8");
    }

    let bytes = raw.as_bytes();
    if matches!(bytes.first(), Some(b' ' | b'\t')) || matches!(bytes.last(), Some(b' ' | b'\t')) {
        return Err("plain value has leading or trailing whitespace");
    }
    if bytes.iter().any(|byte| !matches!(byte, b'\t' | 0x20..=0x7e)) {
        return Err("plain value contains characters that require Base64 encoding");
    }
    Ok(raw.to_owned())
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

    fn object(value: &Value) -> JsonObject {
        value.as_object().expect("JSON object").clone()
    }

    fn as_arguments(value: &Value) -> &JsonObject {
        value.as_object().expect("object arguments")
    }

    #[test]
    fn matching_parameter_headers_are_validated() {
        let arguments = json!({ "region": " leading snowman ☃", "count": 3, "dryRun": false });
        let encoded =
            format!("{BASE64_HEADER_PREFIX}{}{BASE64_HEADER_SUFFIX}", BASE64_STANDARD.encode(" leading snowman ☃"));
        let headers = HeaderMap::from_iter([
            (HeaderName::from_static("mcp-param-region"), HeaderValue::from_str(&encoded).expect("encoded header")),
            (HeaderName::from_static("mcp-param-count"), HeaderValue::from_static("3")),
            (HeaderName::from_static("mcp-param-dry-run"), HeaderValue::from_static("false")),
        ]);

        validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema()).expect("headers match arguments");
    }

    #[test]
    fn null_parameter_is_omitted_and_rejected_when_present() {
        let arguments = json!({ "region": null });
        validate_tool_params(&HeaderMap::new(), Some(as_arguments(&arguments)), &schema())
            .expect("null parameter needs no header");

        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-region"),
            HeaderValue::from_static("unexpected"),
        )]);
        assert!(validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema()).is_err());
    }

    #[test]
    fn missing_header_for_present_parameter_is_rejected() {
        let arguments = json!({ "region": "eu-west" });

        let error = validate_tool_params(&HeaderMap::new(), Some(as_arguments(&arguments)), &schema())
            .expect_err("present annotated parameter requires a header");

        assert_eq!("missing Mcp-Param-Region header for `region`", error);
    }

    #[test]
    fn nested_property_header_uses_the_exact_property_path() {
        let schema = object(&json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "object",
                    "properties": {
                        "region": { "type": "string", "x-mcp-header": "Region" }
                    }
                }
            }
        }));
        let arguments = json!({ "region": "wrong", "request": { "region": "eu-west" } });
        let headers =
            HeaderMap::from_iter([(HeaderName::from_static("mcp-param-region"), HeaderValue::from_static("eu-west"))]);

        validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema)
            .expect("nested header matches its exact property path");
    }

    #[test]
    fn annotations_outside_properties_only_paths_are_rejected() {
        let invalid_schemas = [
            json!({ "type": "object", "x-mcp-header": "Root" }),
            json!({
                "type": "object",
                "properties": {
                    "values": {
                        "type": "array",
                        "items": { "type": "string", "x-mcp-header": "Item" }
                    }
                }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "oneOf": [{ "type": "string", "x-mcp-header": "Choice" }]
                    }
                }
            }),
            json!({
                "type": "object",
                "$defs": { "value": { "type": "string", "x-mcp-header": "Reference" } },
                "properties": { "value": { "$ref": "#/$defs/value" } }
            }),
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "if": { "type": "string", "x-mcp-header": "Conditional" }
                    }
                }
            }),
        ];

        for invalid_schema in invalid_schemas {
            let error = validate_tool_params(&HeaderMap::new(), None, &object(&invalid_schema))
                .expect_err("unreachable annotation invalidates the schema");
            assert!(error.contains("not on a statically reachable property"), "unexpected error: {error}");
        }
    }

    #[test]
    fn annotation_names_and_types_must_meet_mcp_constraints() {
        let invalid_schemas = [
            json!({ "type": "object", "properties": { "value": {
                "type": "string", "x-mcp-header": ""
            } } }),
            json!({ "type": "object", "properties": { "value": {
                "type": "string", "x-mcp-header": "not valid"
            } } }),
            json!({ "type": "object", "properties": { "value": {
                "type": "string", "x-mcp-header": 7
            } } }),
            json!({ "type": "object", "properties": { "value": {
                "type": "number", "x-mcp-header": "Value"
            } } }),
            json!({ "type": "object", "properties": { "value": {
                "x-mcp-header": "Value"
            } } }),
        ];

        for invalid_schema in invalid_schemas {
            validate_tool_params(&HeaderMap::new(), None, &object(&invalid_schema))
                .expect_err("invalid annotation is rejected");
        }
    }

    #[test]
    fn annotation_names_are_unique_case_insensitively_across_nested_properties() {
        let schema = object(&json!({
            "type": "object",
            "properties": {
                "region": { "type": "string", "x-mcp-header": "Region" },
                "request": {
                    "type": "object",
                    "properties": {
                        "region": { "type": "string", "x-mcp-header": "region" }
                    }
                }
            }
        }));

        let error = validate_tool_params(&HeaderMap::new(), None, &schema)
            .expect_err("case-insensitive duplicate annotation is rejected");

        assert!(error.contains("duplicate x-mcp-header `region`"));
    }

    #[test]
    fn integer_headers_are_compared_numerically_within_the_safe_range() {
        assert_eq!(Some(42), parse_integer_header_value("4.2e1"));
        assert_eq!(Some(-7), parse_integer_header_value("-7.00"));
        assert_eq!(None, parse_integer_header_value("42.1"));
        assert_eq!(None, parse_integer_header_value("9007199254740991.1"));

        let arguments = json!({ "count": MAX_SAFE_INTEGER });
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-count"),
            HeaderValue::from_static("9007199254740991.0"),
        )]);

        validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema())
            .expect("numerically equivalent safe integers match");

        let arguments = json!({ "count": 42.0 });
        let headers =
            HeaderMap::from_iter([(HeaderName::from_static("mcp-param-count"), HeaderValue::from_static("4.2e1"))]);
        validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema())
            .expect("JSON numbers with an integral value satisfy an integer schema");

        let arguments = json!({ "count": 9_007_199_254_740_992_i64 });
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-count"),
            HeaderValue::from_static("9007199254740992"),
        )]);
        let error = validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema())
            .expect_err("integer outside the safe range is rejected");
        assert!(error.contains("must be an integer between"));
    }

    #[test]
    fn malformed_recognized_header_values_are_rejected() {
        let arguments = json!({ "region": "hello" });
        let invalid_base64 = HeaderMap::from_iter([(
            HeaderName::from_static("mcp-param-region"),
            HeaderValue::from_static("=?base64?%%%?="),
        )]);
        let leading_whitespace =
            HeaderMap::from_iter([(HeaderName::from_static("mcp-param-region"), HeaderValue::from_static(" hello"))]);

        validate_tool_params(&invalid_base64, Some(as_arguments(&arguments)), &schema())
            .expect_err("invalid Base64 is rejected");
        validate_tool_params(&leading_whitespace, Some(as_arguments(&arguments)), &schema())
            .expect_err("unsafe plain value is rejected");
    }

    #[test]
    fn every_repeated_recognized_header_value_must_match() {
        let arguments = json!({ "region": "eu-west" });
        let mut headers = HeaderMap::new();
        headers.append(HeaderName::from_static("mcp-param-region"), HeaderValue::from_static("eu-west"));
        headers.append(HeaderName::from_static("mcp-param-region"), HeaderValue::from_static("us-east"));

        validate_tool_params(&headers, Some(as_arguments(&arguments)), &schema())
            .expect_err("a conflicting repeated header is rejected");
    }

    #[test]
    fn unknown_parameter_headers_are_ignored_without_a_published_annotation() {
        let headers =
            HeaderMap::from_iter([(HeaderName::from_static("mcp-param-region"), HeaderValue::from_static("anything"))]);

        validate_tool_params(&headers, None, &JsonObject::new()).expect("unknown header is ignored");
    }
}
