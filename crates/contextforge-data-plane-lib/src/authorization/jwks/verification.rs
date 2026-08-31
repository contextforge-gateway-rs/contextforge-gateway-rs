use jsonwebtoken::{
    AlgorithmFamily, DecodingKey, Header, Validation,
    jwk::{Jwk, KeyOperations, PublicKeyUse},
};

use crate::authorization::{AuthorizationClaims, AuthorizationError, jwks::claims::decode_claims};

pub(super) fn validated_json_web_keys(
    jwks: impl IntoIterator<Item = Jwk>,
) -> Result<Vec<VerificationKey>, AuthorizationError> {
    let mut keys = Vec::new();
    for jwk in jwks {
        if let Some(key) = VerificationKey::from_jwk(jwk)? {
            keys.push(key);
        }
    }

    if keys.is_empty() {
        return Err(AuthorizationError::NoSupportedKeys);
    }
    Ok(keys)
}

pub(super) struct VerificationKey {
    key_id: Option<String>,
    decoding_key: DecodingKey,
}

impl VerificationKey {
    fn from_jwk(jwk: Jwk) -> Result<Option<Self>, AuthorizationError> {
        if jwk.common.public_key_use.as_ref().is_some_and(|key_use| key_use != &PublicKeyUse::Signature)
            || jwk.common.key_operations.as_ref().is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
        {
            return Ok(None);
        }

        let decoding_key = DecodingKey::from_jwk(&jwk).map_err(AuthorizationError::InvalidKey)?;
        if !matches!(decoding_key.family(), AlgorithmFamily::Rsa | AlgorithmFamily::Ec) {
            return Ok(None);
        }

        Ok(Some(Self { key_id: jwk.common.key_id, decoding_key }))
    }

    pub(super) fn matches(&self, header: &Header) -> bool {
        self.decoding_key.family() == header.alg.family()
            && header
                .kid
                .as_ref()
                .is_none_or(|header_key_id| self.key_id.as_ref().is_none_or(|key_id| key_id == header_key_id))
    }
}

pub(super) fn decode_with_keys(
    keys: &[VerificationKey],
    token: &str,
    header: &Header,
    validation: &Validation,
) -> Option<AuthorizationClaims> {
    keys.iter().filter(|key| key.matches(header)).find_map(|key| decode_claims(token, &key.decoding_key, validation))
}
