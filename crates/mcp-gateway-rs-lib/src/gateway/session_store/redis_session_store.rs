use async_trait::async_trait;

use redis::{AsyncCommands, RedisError, cmd};

use super::{SessionMapping, SessionStoreError, UserSession, UserSessionStore};
use crate::common::RedisClient;

#[derive(Debug, Clone)]
pub struct RedisUserSessionStore {
    redis_client: RedisClient,
}
impl RedisUserSessionStore {
    pub fn new(redis_client: RedisClient) -> Self {
        Self { redis_client }
    }
}

#[async_trait]
impl UserSessionStore for RedisUserSessionStore {
    async fn get_session<'a>(&self, key: &'a UserSession) -> Result<Option<SessionMapping>, SessionStoreError> {
        let Ok(key) = rmp_serde::encode::to_vec::<UserSession>(key) else {
            return Err(SessionStoreError::DataEncoding);
        };

        let Ok(mut connection) = self.redis_client.get_multiplexed_async_connection().await else {
            return Err(SessionStoreError::InvalidConnection);
        };

        let maybe_user_config: Result<Option<Vec<u8>>, RedisError> = cmd("GET").arg(key).take().query_async(&mut connection).await;

        let Ok(Some(user_config)) = maybe_user_config else {
            return Ok(None);
        };

        let Ok(user_config) = rmp_serde::decode::from_slice::<SessionMapping>(&user_config) else {
            return Err(SessionStoreError::DataWrongFormat);
        };

        Ok(Some(user_config))
    }

    async fn set_session<'a>(&self, key: &'a UserSession, mapping: &'a SessionMapping) -> Result<(), SessionStoreError> {
        let Ok(key) = rmp_serde::encode::to_vec::<UserSession>(key) else {
            return Err(SessionStoreError::DataEncoding);
        };

        let Ok(encoded) = rmp_serde::encode::to_vec::<SessionMapping>(mapping) else {
            return Err(SessionStoreError::DataEncoding);
        };

        let Ok(mut connection) = self.redis_client.get_multiplexed_async_connection().await else {
            return Err(SessionStoreError::InvalidConnection);
        };

        if connection.set::<&[u8], &[u8], String>(&key, &encoded).await.is_ok() {
            Ok(())
        } else {
            return Err(SessionStoreError::CantWriteData);
        }
    }
}
