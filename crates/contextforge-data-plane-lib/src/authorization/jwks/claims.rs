use jsonwebtoken::{DecodingKey, Validation, decode};
use serde_json::Value;

use crate::authorization::AuthorizationClaims;

pub(super) fn decode_claims(token: &str, key: &DecodingKey, validation: &Validation) -> Option<AuthorizationClaims> {
    let claims = decode::<Value>(token, key, validation).ok()?.claims;
    let claims = claims.as_object()?;

    if claims.get("iat").is_some_and(|value| numeric_date(value).is_none())
        || claims.get("jti").is_some_and(|value| !value.is_string())
    {
        return None;
    }
    let now = i128::from(jsonwebtoken::get_current_timestamp());
    if let Some(expiration) = claims.get("exp")
        && numeric_date(expiration)? < now
    {
        return None;
    }
    if let Some(not_before) = claims.get("nbf")
        && numeric_date(not_before)? > now
    {
        return None;
    }

    let user_id = claims
        .get("woUserId")
        .and_then(non_empty_string)
        .or_else(|| claims.get("callerExt")?.as_object()?.get("userId").and_then(non_empty_string))
        .or_else(|| claims.get("idpUniqueId").and_then(non_empty_string))
        .or_else(|| claims.get("sub").and_then(non_empty_string))?;
    let tenant_id = tenant_id(claims)?;

    Some(AuthorizationClaims::new(user_id, &tenant_id))
}

fn tenant_id(claims: &serde_json::Map<String, Value>) -> Option<String> {
    let mut tenant_id = ["woTenantId", "tenantId", "tenant_id"]
        .into_iter()
        .find_map(|claim| claims.get(claim).and_then(non_empty_string))
        .map(str::to_owned);

    if let Some(crn) = mcsp_crn(claims)
        && let Some(parsed) = parse_mcsp_crn(&crn)
    {
        tenant_id = Some(parsed);
    }

    if let Some(caller_tenant) = claims
        .get("callerExt")
        .and_then(Value::as_object)
        .and_then(|caller| caller.get("tenantId"))
        .and_then(non_empty_string)
    {
        tenant_id = Some(parse_mcsp_crn(caller_tenant).unwrap_or_else(|| caller_tenant.to_owned()));
    }

    tenant_id
}

fn mcsp_crn(claims: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(crn) = claims.get("crn").and_then(non_empty_string) {
        return Some(crn.to_owned());
    }

    if let Some(crn) = claims
        .get("aud")
        .and_then(Value::as_array)
        .and_then(|audiences| audiences.iter().filter_map(Value::as_str).find(|audience| audience.contains("crn:v1:")))
    {
        return Some(crn.to_owned());
    }

    let subscription_id = claims.get("subscriptionId").and_then(non_empty_string)?;
    let instance_id = claims
        .get("aud")
        .and_then(non_empty_string)?
        .strip_prefix("SERVICE/")
        .filter(|instance_id| !instance_id.is_empty())?;
    Some(format!("crn:v1:aws-staging:public:wxo-sandbox:us-east-1:sub/{subscription_id}:{instance_id}::"))
}

fn parse_mcsp_crn(crn: &str) -> Option<String> {
    let fields = crn.split(':').collect::<Vec<_>>();
    let [scheme, version, location, scope, service, region, account, resource, "", ""] = fields.as_slice() else {
        return None;
    };
    let version = version.strip_prefix('v')?;
    let (account_kind, account_id) = account.split_once('/')?;
    if *scheme != "crn"
        || version.is_empty()
        || !version.bytes().all(|character| character.is_ascii_digit())
        || !is_word_or_hyphen(location)
        || !is_hyphenated_word(scope)
        || !is_hyphenated_word(service)
        || !is_hyphenated_word(region)
        || !is_word(account_kind)
        || !is_word_or_hyphen(account_id)
        || resource.split('-').count() != 5
        || !resource.split('-').all(is_word)
    {
        return None;
    }
    Some(format!("{account_id}_{resource}"))
}

fn is_word(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|character| character.is_ascii_alphanumeric() || character == b'_')
}

fn is_word_or_hyphen(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|character| character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-'))
}

fn is_hyphenated_word(value: &str) -> bool {
    value.split('-').all(is_word)
}

fn non_empty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

// SaaS NumericDate compatibility requires truncating JSON floating-point numbers.
#[allow(clippy::cast_possible_truncation)]
fn numeric_date(value: &Value) -> Option<i128> {
    match value {
        Value::Bool(value) => Some(i128::from(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(i128::from)
            .or_else(|| value.as_u64().map(i128::from))
            .or_else(|| value.as_f64().map(|value| value.trunc() as i128)),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}
