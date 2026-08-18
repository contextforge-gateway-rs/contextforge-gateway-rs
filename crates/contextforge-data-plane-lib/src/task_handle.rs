//! Stateless routing handles for the MCP Tasks extension.
//!
//! Handles are encrypted and authenticated because their payload contains an
//! upstream task identifier that may be a bearer token. The trusted
//! authorization context, virtual host, configuration revision, and backend
//! generation prevent replay through a different caller, policy snapshot, or
//! upstream route.

use std::{fmt, str::FromStr};

use aes_gcm_siv::{
    Aes256GcmSiv, Key, Nonce,
    aead::{Aead, Generate, KeyInit, Payload as AeadPayload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rmcp::{ErrorData, model::ErrorCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::VirtualHostId;

const HANDLE_PREFIX: &str = "cfth1";
const HANDLE_FAMILY_PREFIX: &str = "cfth";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// A validated AES-256-GCM-SIV key for task-handle protection.
///
/// The textual form is URL-safe base64 without padding and must decode to
/// exactly 32 bytes. Its [`Debug`] output is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskHandleKey(Zeroizing<[u8; KEY_LEN]>);

impl FromStr for TaskHandleKey {
    type Err = TaskHandleKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut key = Zeroizing::new([0_u8; KEY_LEN]);
        let decoded_len = URL_SAFE_NO_PAD.decode_slice(value, key.as_mut()).map_err(|_| TaskHandleKeyError)?;
        if decoded_len != KEY_LEN {
            return Err(TaskHandleKeyError);
        }
        Ok(Self(key))
    }
}

impl fmt::Debug for TaskHandleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TaskHandleKey([REDACTED])")
    }
}

/// Returned when task-handle key material is not a URL-safe base64-encoded
/// 256-bit key.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("task handle key must be URL-safe base64 without padding and decode to exactly 32 bytes")]
pub struct TaskHandleKeyError;

macro_rules! string_identifier {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier from its canonical trusted value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the canonical value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_identifier!(
    AuthorizationContextId,
    "Canonical identity for the authenticated tenant, principal, team, and scope set."
);
string_identifier!(ConfigurationRevision, "Revision of the validated effective-configuration snapshot.");
string_identifier!(BackendId, "Stable identity of a backend in effective configuration.");
string_identifier!(BackendGeneration, "Generation of a backend's routing material.");

/// Authenticated request scope to which a task handle is bound.
///
/// The authorization-context ID and configuration revision must come from the
/// verified JWT and the validated effective-configuration snapshot. Client
/// metadata and MCP params are not trusted sources for either value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TaskHandleScope<'a> {
    authorization_context_id: &'a AuthorizationContextId,
    virtual_host_id: &'a VirtualHostId,
    configuration_revision: &'a ConfigurationRevision,
}

impl<'a> TaskHandleScope<'a> {
    /// Creates a scope from trusted authorization and routing context.
    pub fn new(
        authorization_context_id: &'a AuthorizationContextId,
        virtual_host_id: &'a VirtualHostId,
        configuration_revision: &'a ConfigurationRevision,
    ) -> Self {
        Self { authorization_context_id, virtual_host_id, configuration_revision }
    }
}

/// Stable backend identity stored in a task handle.
///
/// `generation` must change when the routing key is reassigned to a different
/// upstream or when routing material changes in a way that invalidates
/// outstanding upstream task IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskHandleBackend<'a> {
    id: &'a BackendId,
    generation: &'a BackendGeneration,
}

impl<'a> TaskHandleBackend<'a> {
    /// Creates backend identity from trusted effective configuration.
    pub fn new(id: &'a BackendId, generation: &'a BackendGeneration) -> Self {
        Self { id, generation }
    }

    /// Stable backend identifier used for routing.
    pub fn id(self) -> &'a BackendId {
        self.id
    }

    /// Generation of the backend routing material.
    pub fn generation(self) -> &'a BackendGeneration {
        self.generation
    }
}

/// The route recovered from a valid task handle.
#[derive(Clone, PartialEq, Eq)]
pub struct TaskHandleRoute {
    backend_id: BackendId,
    upstream_task_id: Zeroizing<String>,
}

impl TaskHandleRoute {
    /// Stable backend ID in the caller's current effective configuration.
    pub fn backend_id(&self) -> &BackendId {
        &self.backend_id
    }

    /// Original task identifier expected by the upstream backend.
    pub fn upstream_task_id(&self) -> &str {
        self.upstream_task_id.as_str()
    }
}

impl fmt::Debug for TaskHandleRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskHandleRoute")
            .field("backend_id", &self.backend_id.as_str())
            .field("upstream_task_id", &"[REDACTED]")
            .finish()
    }
}

/// Errors produced while encoding or decoding task handles.
///
/// Decode errors intentionally have the same display text so a caller cannot
/// distinguish a malformed handle from a valid handle outside its scope.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TaskHandleError {
    /// The protected handle could not be created.
    #[error("failed to create task handle")]
    Encode,
    /// The handle is malformed or fails authentication.
    #[error("invalid task handle")]
    Invalid,
    /// The handle belongs to another codec version.
    #[error("invalid task handle")]
    UnsupportedVersion,
    /// The handle does not belong to the authenticated request scope.
    #[error("invalid task handle")]
    WrongScope,
    /// The referenced backend and generation are not currently routable.
    #[error("invalid task handle")]
    UnavailableBackend,
}

impl From<TaskHandleError> for ErrorData {
    fn from(error: TaskHandleError) -> Self {
        match error {
            TaskHandleError::Encode => ErrorData::new(ErrorCode::INTERNAL_ERROR, "failed to create task handle", None),
            TaskHandleError::Invalid
            | TaskHandleError::UnsupportedVersion
            | TaskHandleError::WrongScope
            | TaskHandleError::UnavailableBackend => ErrorData::new(ErrorCode::INVALID_PARAMS, "invalid task ID", None),
        }
    }
}

#[derive(Serialize)]
struct TaskHandlePayload<'a> {
    authorization_context_id: &'a str,
    virtual_host_id: &'a str,
    configuration_revision: &'a str,
    backend_id: &'a str,
    backend_generation: &'a str,
    upstream_task_id: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedTaskHandlePayload {
    authorization_context_id: String,
    virtual_host_id: String,
    configuration_revision: String,
    backend_id: String,
    backend_generation: String,
    upstream_task_id: String,
}

/// Encodes and decodes versioned task handles shared across dataplane replicas.
#[derive(Clone)]
pub struct TaskHandleCodec {
    cipher: Aes256GcmSiv,
}

impl TaskHandleCodec {
    /// Creates a codec from a validated shared key.
    pub fn new(key: &TaskHandleKey) -> Self {
        let key: &Key<Aes256GcmSiv> = (&*key.0).into();
        let cipher = Aes256GcmSiv::new(key);
        Self { cipher }
    }

    /// Creates an opaque handle for one upstream task.
    pub fn encode(
        &self,
        scope: TaskHandleScope<'_>,
        backend: TaskHandleBackend<'_>,
        upstream_task_id: &str,
    ) -> Result<String, TaskHandleError> {
        let payload = TaskHandlePayload {
            authorization_context_id: scope.authorization_context_id.as_str(),
            virtual_host_id: scope.virtual_host_id.as_str(),
            configuration_revision: scope.configuration_revision.as_str(),
            backend_id: backend.id.as_str(),
            backend_generation: backend.generation.as_str(),
            upstream_task_id,
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| TaskHandleError::Encode)?);

        let nonce = Nonce::try_generate().map_err(|_| TaskHandleError::Encode)?;
        let ciphertext = self
            .cipher
            .encrypt(&nonce, AeadPayload { msg: plaintext.as_slice(), aad: HANDLE_PREFIX.as_bytes() })
            .map_err(|_| TaskHandleError::Encode)?;

        let mut protected = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        protected.extend_from_slice(&nonce);
        protected.extend_from_slice(&ciphertext);
        Ok(format!("{HANDLE_PREFIX}.{}", URL_SAFE_NO_PAD.encode(protected)))
    }

    /// Decodes a handle and verifies that it belongs to the authenticated
    /// authorization context and references the same backend generation in
    /// current effective configuration.
    pub fn decode<F>(
        &self,
        handle: &str,
        expected_scope: TaskHandleScope<'_>,
        backend_is_current: F,
    ) -> Result<TaskHandleRoute, TaskHandleError>
    where
        F: FnOnce(TaskHandleBackend<'_>) -> bool,
    {
        let (prefix, protected) = handle.split_once('.').ok_or(TaskHandleError::Invalid)?;
        if prefix != HANDLE_PREFIX {
            return if is_other_version(prefix) {
                Err(TaskHandleError::UnsupportedVersion)
            } else {
                Err(TaskHandleError::Invalid)
            };
        }

        let protected = URL_SAFE_NO_PAD.decode(protected).map_err(|_| TaskHandleError::Invalid)?;
        if protected.len() < NONCE_LEN + TAG_LEN {
            return Err(TaskHandleError::Invalid);
        }
        let (nonce_bytes, ciphertext) = protected.split_at(NONCE_LEN);
        let nonce_bytes: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| TaskHandleError::Invalid)?;
        let nonce = Nonce::from(nonce_bytes);
        let plaintext = Zeroizing::new(
            self.cipher
                .decrypt(&nonce, AeadPayload { msg: ciphertext, aad: HANDLE_PREFIX.as_bytes() })
                .map_err(|_| TaskHandleError::Invalid)?,
        );
        let payload: DecodedTaskHandlePayload =
            serde_json::from_slice(&plaintext).map_err(|_| TaskHandleError::Invalid)?;

        if payload.authorization_context_id != expected_scope.authorization_context_id.as_str()
            || payload.virtual_host_id != expected_scope.virtual_host_id.as_str()
            || payload.configuration_revision != expected_scope.configuration_revision.as_str()
        {
            return Err(TaskHandleError::WrongScope);
        }
        let backend_id = BackendId::new(payload.backend_id);
        let backend_generation = BackendGeneration::new(payload.backend_generation);
        if !backend_is_current(TaskHandleBackend::new(&backend_id, &backend_generation)) {
            return Err(TaskHandleError::UnavailableBackend);
        }

        Ok(TaskHandleRoute { backend_id, upstream_task_id: Zeroizing::new(payload.upstream_task_id) })
    }
}

fn is_other_version(prefix: &str) -> bool {
    prefix
        .strip_prefix(HANDLE_FAMILY_PREFIX)
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // pragma: allowlist secret
    const AUTHORIZATION_CONTEXT_VALUE: &str = "tenant-a:principal-a:team-a:scope-set-a";
    const VIRTUAL_HOST_ID_VALUE: &str = "host-a";
    const CONFIGURATION_REVISION_VALUE: &str = "revision-7";
    const BACKEND_ID_VALUE: &str = "backend-a";
    const BACKEND_GENERATION_VALUE: &str = "generation-3";

    static AUTHORIZATION_CONTEXT: LazyLock<AuthorizationContextId> =
        LazyLock::new(|| AuthorizationContextId::new(AUTHORIZATION_CONTEXT_VALUE));
    static VIRTUAL_HOST_ID: LazyLock<VirtualHostId> = LazyLock::new(|| VirtualHostId::new(VIRTUAL_HOST_ID_VALUE));
    static CONFIGURATION_REVISION: LazyLock<ConfigurationRevision> =
        LazyLock::new(|| ConfigurationRevision::new(CONFIGURATION_REVISION_VALUE));
    static BACKEND_ID: LazyLock<BackendId> = LazyLock::new(|| BackendId::new(BACKEND_ID_VALUE));
    static BACKEND_GENERATION: LazyLock<BackendGeneration> =
        LazyLock::new(|| BackendGeneration::new(BACKEND_GENERATION_VALUE));

    fn codec() -> TaskHandleCodec {
        TaskHandleCodec::new(&KEY.parse().expect("test key is valid"))
    }

    fn scope<'a>(
        authorization_context_id: &'a AuthorizationContextId,
        virtual_host_id: &'a VirtualHostId,
        configuration_revision: &'a ConfigurationRevision,
    ) -> TaskHandleScope<'a> {
        TaskHandleScope::new(authorization_context_id, virtual_host_id, configuration_revision)
    }

    fn current_scope() -> TaskHandleScope<'static> {
        scope(&AUTHORIZATION_CONTEXT, &VIRTUAL_HOST_ID, &CONFIGURATION_REVISION)
    }

    fn backend<'a>(id: &'a BackendId, generation: &'a BackendGeneration) -> TaskHandleBackend<'a> {
        TaskHandleBackend::new(id, generation)
    }

    fn current_backend() -> TaskHandleBackend<'static> {
        backend(&BACKEND_ID, &BACKEND_GENERATION)
    }

    fn decode_current(
        codec: &TaskHandleCodec,
        handle: &str,
        expected_scope: TaskHandleScope<'_>,
        expected_backend: TaskHandleBackend<'_>,
    ) -> Result<TaskHandleRoute, TaskHandleError> {
        codec.decode(handle, expected_scope, |decoded_backend| {
            decoded_backend.id() == expected_backend.id()
                && decoded_backend.generation() == expected_backend.generation()
        })
    }

    #[test]
    fn arbitrary_upstream_task_ids_round_trip_without_loss() {
        let codec = codec();
        let task_ids = ["", "simple", "with/slashes?and=query", "nul\0byte", "emoji-🦀", "line\nbreak"];

        for task_id in task_ids {
            let handle = codec.encode(current_scope(), current_backend(), task_id).expect("handle encodes");
            let route = decode_current(&codec, &handle, current_scope(), current_backend()).expect("handle decodes");

            assert_eq!(route.backend_id().as_str(), BACKEND_ID_VALUE);
            assert_eq!(route.upstream_task_id(), task_id);
        }
    }

    #[test]
    fn identical_task_ids_from_different_backends_remain_isolated() {
        let codec = codec();
        let first_identity = (BackendId::new("backend-a"), BackendGeneration::new("generation-a"));
        let second_identity = (BackendId::new("backend-b"), BackendGeneration::new("generation-b"));
        let first_backend = backend(&first_identity.0, &first_identity.1);
        let second_backend = backend(&second_identity.0, &second_identity.1);
        let first_handle = codec.encode(current_scope(), first_backend, "same-id").expect("first handle encodes");
        let second_handle = codec.encode(current_scope(), second_backend, "same-id").expect("second handle encodes");

        let first_route =
            decode_current(&codec, &first_handle, current_scope(), first_backend).expect("first handle decodes");
        let second_route =
            decode_current(&codec, &second_handle, current_scope(), second_backend).expect("second handle decodes");

        assert_ne!(first_handle, second_handle);
        assert_eq!(first_route.backend_id().as_str(), "backend-a");
        assert_eq!(second_route.backend_id().as_str(), "backend-b");
    }

    #[test]
    fn handle_decodes_on_another_codec_with_the_same_key() {
        let first_replica = codec();
        let second_replica = codec();
        let handle = first_replica.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");

        let route =
            decode_current(&second_replica, &handle, current_scope(), current_backend()).expect("handle decodes");

        assert_eq!(route.upstream_task_id(), "task-42");
    }

    #[test]
    fn malformed_and_tampered_handles_fail_closed() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");
        let mut tampered = handle.into_bytes();
        let last = tampered.last_mut().expect("handle is non-empty");
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("tampered handle remains UTF-8");

        assert_eq!(
            decode_current(&codec, "not-a-handle", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
        assert_eq!(
            decode_current(&codec, "cfth1.AA", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
        assert_eq!(
            decode_current(&codec, &tampered, current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
    }

    #[test]
    fn unsupported_handle_versions_are_rejected() {
        let codec = codec();

        assert_eq!(
            decode_current(&codec, "cfth2.AA", current_scope(), current_backend()),
            Err(TaskHandleError::UnsupportedVersion)
        );
        assert_eq!(
            decode_current(&codec, "cfthx.AA", current_scope(), current_backend()),
            Err(TaskHandleError::Invalid)
        );
    }

    #[test]
    fn authorization_virtual_host_and_revision_scope_are_enforced() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");
        let other_authorization_context = AuthorizationContextId::new("tenant-b:principal-a:team-a:scope-set-a");
        let other_virtual_host_id = VirtualHostId::new("host-b");
        let other_configuration_revision = ConfigurationRevision::new("revision-8");

        for wrong_scope in [
            scope(&other_authorization_context, &VIRTUAL_HOST_ID, &CONFIGURATION_REVISION),
            scope(&AUTHORIZATION_CONTEXT, &other_virtual_host_id, &CONFIGURATION_REVISION),
            scope(&AUTHORIZATION_CONTEXT, &VIRTUAL_HOST_ID, &other_configuration_revision),
        ] {
            assert_eq!(
                decode_current(&codec, &handle, wrong_scope, current_backend()),
                Err(TaskHandleError::WrongScope)
            );
        }
    }

    #[test]
    fn removed_or_reassigned_backends_are_rejected_without_exposing_identity() {
        let codec = codec();
        let handle = codec.encode(current_scope(), current_backend(), "task-42").expect("handle encodes");

        assert!(decode_current(&codec, &handle, current_scope(), current_backend()).is_ok());
        let removed = codec.decode(&handle, current_scope(), |_| false).expect_err("removed backend is rejected");
        let reassigned_generation = BackendGeneration::new("generation-4");
        let reassigned = decode_current(&codec, &handle, current_scope(), backend(&BACKEND_ID, &reassigned_generation))
            .expect_err("reassigned backend is rejected");

        for error in [removed, reassigned] {
            assert_eq!(error, TaskHandleError::UnavailableBackend);
            assert_eq!(error.to_string(), "invalid task handle");
            assert!(!error.to_string().contains(BACKEND_ID_VALUE));
            assert!(!error.to_string().contains(BACKEND_GENERATION_VALUE));
        }
    }

    #[test]
    fn invalid_keys_and_sensitive_debug_output_are_redacted() {
        assert!("short".parse::<TaskHandleKey>().is_err());
        assert!(format!("{KEY}=").parse::<TaskHandleKey>().is_err());
        assert!(format!("{KEY}AA").parse::<TaskHandleKey>().is_err());
        let key: TaskHandleKey = KEY.parse().expect("test key is valid");
        let codec = TaskHandleCodec::new(&key);
        let handle = codec.encode(current_scope(), current_backend(), "bearer-task-id").expect("handle encodes");
        let route = decode_current(&codec, &handle, current_scope(), current_backend()).expect("handle decodes");

        assert_eq!(format!("{key:?}"), "TaskHandleKey([REDACTED])");
        assert!(!format!("{key:?}").contains(KEY));
        assert_eq!(
            format!("{route:?}"),
            "TaskHandleRoute { backend_id: \"backend-a\", upstream_task_id: \"[REDACTED]\" }"
        );
        assert!(!format!("{route:?}").contains("bearer-task-id"));
    }

    #[test]
    fn decode_errors_map_to_indistinguishable_invalid_params_errors() {
        for error in [
            TaskHandleError::Invalid,
            TaskHandleError::UnsupportedVersion,
            TaskHandleError::WrongScope,
            TaskHandleError::UnavailableBackend,
        ] {
            let protocol_error = ErrorData::from(error);

            assert_eq!(protocol_error.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(protocol_error.message, "invalid task ID");
            assert_eq!(protocol_error.data, None);
        }

        let protocol_error = ErrorData::from(TaskHandleError::Encode);
        assert_eq!(protocol_error.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(protocol_error.message, "failed to create task handle");
    }
}
