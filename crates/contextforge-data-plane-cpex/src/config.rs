use async_trait::async_trait;
use redis::{
    Client, RedisError,
    aio::{ConnectionManager, ConnectionManagerConfig},
    cmd,
};
use tokio::sync::Mutex;

use crate::error::GatewayPluginRuntimeError;
use contextforge_data_plane_apis::runtime_plugin_config::{
    RUNTIME_PLUGIN_CONFIG_KEY, RUNTIME_PLUGIN_CONFIG_VERSION, RuntimePluginConfigDocument, RuntimeRevision,
};
use cpex::cpex_core::config::CpexConfig;

#[async_trait]
pub(crate) trait RuntimePluginConfigStore: Send + Sync {
    async fn get_config(&self) -> Result<Option<LoadedRuntimePluginConfig>, GatewayPluginRuntimeError>;
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedRuntimePluginConfig {
    pub(crate) document: RuntimePluginConfigDocument,
    pub(crate) fingerprint: Vec<u8>,
}

impl LoadedRuntimePluginConfig {
    pub(crate) fn decode(config: Vec<u8>) -> Result<Self, GatewayPluginRuntimeError> {
        let document = decode_config_document(&config)?;
        Ok(Self { document, fingerprint: config })
    }
}

pub(crate) struct RedisRuntimePluginConfigStore {
    redis_client: Client,
    connection: Mutex<Option<ConnectionManager>>,
}

impl RedisRuntimePluginConfigStore {
    pub(crate) fn new(redis_client: Client) -> Self {
        Self { redis_client, connection: Mutex::new(None) }
    }

    async fn connection(&self) -> Result<ConnectionManager, GatewayPluginRuntimeError> {
        let mut connection = self.connection.lock().await;
        if connection.is_none() {
            *connection = Some(
                self.redis_client
                    .get_connection_manager_with_config(ConnectionManagerConfig::default())
                    .await
                    .map_err(|_| GatewayPluginRuntimeError::ConfigStoreUnavailable)?,
            );
        }
        connection.clone().ok_or(GatewayPluginRuntimeError::ConfigStoreUnavailable)
    }
}

#[async_trait]
impl RuntimePluginConfigStore for RedisRuntimePluginConfigStore {
    async fn get_config(&self) -> Result<Option<LoadedRuntimePluginConfig>, GatewayPluginRuntimeError> {
        let mut connection = self.connection().await?;

        let maybe_config: Result<Option<Vec<u8>>, RedisError> =
            cmd("GET").arg(RUNTIME_PLUGIN_CONFIG_KEY).take().query_async(&mut connection).await;
        let Some(config) = maybe_config.map_err(|_| GatewayPluginRuntimeError::ConfigStoreUnavailable)? else {
            return Ok(None);
        };

        LoadedRuntimePluginConfig::decode(config).map(Some)
    }
}

pub(crate) fn cpex_configs(
    document: &RuntimePluginConfigDocument,
) -> Result<Vec<(RuntimeRevision, CpexConfig)>, GatewayPluginRuntimeError> {
    if document.version != RUNTIME_PLUGIN_CONFIG_VERSION || document.snapshots.is_empty() {
        return Err(GatewayPluginRuntimeError::ConfigWrongFormat);
    }
    let mut configs = document
        .snapshots
        .iter()
        .map(|(revision, snapshot)| (revision.clone(), snapshot.clone().into_cpex()))
        .collect::<Vec<_>>();
    configs.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    Ok(configs)
}

pub(crate) fn decode_config_document(config: &[u8]) -> Result<RuntimePluginConfigDocument, GatewayPluginRuntimeError> {
    serde_json::from_slice::<RuntimePluginConfigDocument>(config)
        .or_else(|_| rmp_serde::decode::from_slice::<RuntimePluginConfigDocument>(config))
        .map_err(|_| GatewayPluginRuntimeError::ConfigWrongFormat)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cpex::cpex_core::config::CpexConfig;

    use contextforge_data_plane_apis::runtime_plugin_config::{
        RuntimePluginConfigDocument, RuntimePluginSnapshot, RuntimeRevision,
    };

    use super::{cpex_configs, decode_config_document};

    #[test]
    fn decode_config_document_accepts_json_bytes() {
        let document = br#"{
            "version": 3,
            "snapshots": {
                "revision-1": { "plugins": [] }
            }
        }"#;

        let document = decode_config_document(document).expect("JSON document decodes");

        let mut configs = cpex_configs(&document).expect("config version is valid");
        let (revision, config) = configs.pop().expect("one runtime snapshot");
        assert_eq!("revision-1", revision.as_str());
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn decode_config_document_accepts_messagepack_bytes() {
        let expected = RuntimePluginConfigDocument {
            version: 3,
            snapshots: HashMap::from([(
                RuntimeRevision::try_from("revision-1".to_owned()).expect("valid revision"),
                RuntimePluginSnapshot::try_from(CpexConfig::default()).expect("supported CPEX config"),
            )]),
        };
        let document = rmp_serde::to_vec_named(&expected).expect("MessagePack document encodes");

        assert!(cpex_configs(&decode_config_document(&document).expect("MessagePack document decodes")).is_ok());
    }

    #[test]
    fn decode_config_document_rejects_missing_cpex_config() {
        let error = decode_config_document(br#"{ "version": 1 }"#).expect_err("missing CPEX config is rejected");

        assert_eq!("runtime plugin config is in wrong format", error.to_string());
    }

    #[test]
    fn decode_config_document_rejects_invalid_json_bytes() {
        let error = decode_config_document(b"{not-json").expect_err("invalid JSON bytes are rejected");

        assert_eq!("runtime plugin config is in wrong format", error.to_string());
    }

    #[test]
    fn decode_config_document_rejects_unknown_or_unsupported_fields() {
        for document in [
            br#"{
                "version": 3,
                "snapshots": {"revision-1": {"plugins": [], "typo": true}}
            }"#
            .as_slice(),
            br#"{
                "version": 3,
                "snapshots": {"revision-1": {
                    "plugins": [{"name": "plugin", "kind": "test", "conditions": []}]
                }}
            }"#
            .as_slice(),
            br#"{
                "version": 3,
                "snapshots": {"revision-1": {
                    "plugins": [],
                    "plugin_settings": {"routing_enabled": false}
                }}
            }"#
            .as_slice(),
        ] {
            assert!(decode_config_document(document).is_err());
        }
    }

    #[test]
    fn decode_config_document_rejects_empty_and_duplicate_names() {
        for document in [
            br#"{
                "version": 3,
                "snapshots": {" ": {"plugins": []}}
            }"#
            .as_slice(),
            br#"{
                "version": 3,
                "snapshots": {"revision-1": {
                    "plugins": [{"name": "", "kind": "test"}]
                }}
            }"#
            .as_slice(),
            br#"{
                "version": 3,
                "snapshots": {"revision-1": {"plugins": [
                    {"name": "same", "kind": "test"},
                    {"name": "same", "kind": "test"}
                ]}}
            }"#
            .as_slice(),
        ] {
            assert!(decode_config_document(document).is_err());
        }
    }
}
