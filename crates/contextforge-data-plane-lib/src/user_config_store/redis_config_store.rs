use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use contextforge_data_plane_apis::{User, user_store::UserConfig};
use lru_time_cache::LruCache;
use redis::{
    AsyncCommands, RedisError,
    aio::{ConnectionManager, ConnectionManagerConfig},
    cmd,
};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use super::{ConfigStoreError, UserConfigStore};
use crate::{
    common::RedisClient,
    const_values::{LRU_CACHE_ENTRIES, REDIS_RETRIES},
};

/// Cached config stamped with its insertion time.
#[derive(Clone)]
struct CachedUserConfig {
    inserted_at: Instant,
    config: UserConfig,
}

impl CachedUserConfig {
    fn new(config: UserConfig) -> Self {
        Self { inserted_at: Instant::now(), config }
    }

    fn is_fresh(&self, expiry: Duration) -> bool {
        self.inserted_at.elapsed() < expiry
    }
}

#[derive(Clone)]
pub struct RedisUserConfigStore {
    connection: ConnectionManager,
    /// `None` when caching is disabled (zero expiry): every request reads Redis.
    cache: Option<Arc<Mutex<LruCache<String, CachedUserConfig>>>>,
    cache_expiry: Duration,
}

impl RedisUserConfigStore {
    pub async fn new(redis_client: &RedisClient, cache_expiry: Duration) -> crate::Result<Self> {
        Ok(Self {
            connection: redis_client
                .get_connection_manager_with_config(
                    ConnectionManagerConfig::default().set_number_of_retries(REDIS_RETRIES),
                )
                .await
                .map_err(|error| {
                    warn!("RedisUserConfigStore::new - failed to create Redis user config connection error = {error}");
                    ConfigStoreError::InvalidConnection
                })?,
            cache: (!cache_expiry.is_zero()).then(|| {
                Arc::new(Mutex::new(LruCache::with_expiry_duration_and_capacity(cache_expiry, LRU_CACHE_ENTRIES)))
            }),
            cache_expiry,
        })
    }
}

#[async_trait]
impl UserConfigStore for RedisUserConfigStore {
    async fn get_config<'a>(&self, user_key: &'a User) -> Result<UserConfig, ConfigStoreError> {
        let subject = user_key.key();

        if let Some(cache) = &self.cache {
            if let Some(entry) = cache.lock().await.get_mut(subject) {
                if entry.is_fresh(self.cache_expiry) {
                    let virtual_hosts = entry.config.virtual_hosts.len();
                    debug!(
                        "RedisUserConfigStore::get_config - user config cache hit subject = {subject} virtual_hosts = {virtual_hosts}"
                    );
                    return Ok(entry.config.clone());
                }

                debug!("RedisUserConfigStore::get_config - user config cache entry expired subject = {subject}");
            } else {
                debug!("RedisUserConfigStore::get_config - user config cache miss subject = {subject}");
            }
        }

        let Ok(key) = rmp_serde::encode::to_vec::<User>(user_key) else {
            warn!("RedisUserConfigStore::get_config - failed to encode Redis user config key subject = {subject}");
            return Err(ConfigStoreError::DataEncoding);
        };

        let mut connection = self.connection.clone();
        let maybe_user_config: Result<Option<Vec<u8>>, RedisError> =
            cmd("GET").arg(key).take().query_async(&mut connection).await;

        let user_config = match maybe_user_config {
            Ok(Some(user_config)) => {
                let bytes = user_config.len();
                debug!(
                    "RedisUserConfigStore::get_config - loaded user config blob from Redis subject = {subject} bytes = {bytes}"
                );
                user_config
            },
            Ok(None) => {
                debug!("RedisUserConfigStore::get_config - no user config found in Redis subject = {subject}");
                return Err(ConfigStoreError::NoDataForKey);
            },
            Err(error) => {
                warn!(
                    "RedisUserConfigStore::get_config - failed to load user config from Redis subject = {subject} error = {error}"
                );
                return Err(ConfigStoreError::NoDataForKey);
            },
        };

        let user_config = match rmp_serde::decode::from_slice::<UserConfig>(&user_config) {
            Ok(user_config) => user_config,
            Err(error) => {
                warn!(
                    "RedisUserConfigStore::get_config - failed to decode Redis user config blob subject = {subject} error = {error}"
                );
                return Err(ConfigStoreError::DataWrongFormat);
            },
        };

        let virtual_hosts = user_config.virtual_hosts.len();
        debug!(
            "RedisUserConfigStore::get_config - decoded user config subject = {subject} virtual_hosts = {virtual_hosts}"
        );

        if let Some(cache) = &self.cache {
            cache.lock().await.insert(subject.to_owned(), CachedUserConfig::new(user_config.clone()));
        }
        Ok(user_config)
    }

    async fn set_config<'a>(&self, user_key: &'a User, config: &'a UserConfig) -> Result<(), ConfigStoreError> {
        let subject = user_key.key();

        let Ok(key) = rmp_serde::encode::to_vec::<User>(user_key) else {
            warn!("RedisUserConfigStore::set_config - failed to encode Redis user config key subject = {subject}");
            return Err(ConfigStoreError::DataEncoding);
        };

        let Ok(encoded) = rmp_serde::encode::to_vec::<UserConfig>(config) else {
            let virtual_hosts = config.virtual_hosts.len();
            warn!(
                "RedisUserConfigStore::set_config - failed to encode user config subject = {subject} virtual_hosts = {virtual_hosts}"
            );
            return Err(ConfigStoreError::DataEncoding);
        };

        let mut connection = self.connection.clone();

        match connection.set::<&[u8], &[u8], String>(&key, &encoded).await {
            Ok(_) => {
                let bytes = encoded.len();
                let virtual_hosts = config.virtual_hosts.len();
                debug!(
                    "RedisUserConfigStore::set_config - wrote user config to Redis subject = {subject} bytes = {bytes} virtual_hosts = {virtual_hosts}"
                );
                if let Some(cache) = &self.cache {
                    cache.lock().await.insert(subject.to_owned(), CachedUserConfig::new(config.clone()));
                }
                Ok(())
            },
            Err(error) => {
                warn!(
                    "RedisUserConfigStore::set_config - failed to write user config to Redis subject = {subject} error = {error}"
                );
                Err(ConfigStoreError::CantWriteData)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn empty_config() -> UserConfig {
        UserConfig { virtual_hosts: HashMap::new() }
    }

    fn instant_ago(duration: Duration) -> Instant {
        Instant::now().checked_sub(duration).unwrap_or_else(Instant::now)
    }

    #[test]
    fn cache_entry_is_fresh_within_expiry() {
        let entry = CachedUserConfig::new(empty_config());

        assert!(entry.is_fresh(Duration::from_mins(1)));
    }

    #[test]
    fn cache_entry_expires_by_insert_time() {
        let expiry = Duration::from_mins(1);
        let entry =
            CachedUserConfig { inserted_at: instant_ago(expiry + Duration::from_secs(1)), config: empty_config() };

        assert!(!entry.is_fresh(expiry));
    }

    #[test]
    fn cache_access_does_not_renew_freshness() {
        let expiry = Duration::from_mins(1);
        let mut cache: LruCache<String, CachedUserConfig> =
            LruCache::with_expiry_duration_and_capacity(expiry, LRU_CACHE_ENTRIES);
        cache.insert(
            "subject".to_owned(),
            CachedUserConfig { inserted_at: instant_ago(expiry + Duration::from_secs(1)), config: empty_config() },
        );

        for _ in 0..3 {
            let entry = cache.get_mut("subject").expect("entry should still be present in LruCache");
            assert!(!entry.is_fresh(expiry));
        }
    }
}
