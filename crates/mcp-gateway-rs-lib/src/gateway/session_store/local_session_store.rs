use std::sync::Arc;

use async_trait::async_trait;

use lru_cache::LruCache;

use tokio::sync::Mutex;

use super::{SessionMapping, SessionStoreError, UserSession, UserSessionStore};

#[derive(Debug, Clone)]
pub struct LocalUserSessionStore {
    cache: Arc<Mutex<LruCache<UserSession, SessionMapping>>>,
}
impl LocalUserSessionStore {
    pub fn new() -> Self {
        Self { cache: Arc::new(Mutex::new(LruCache::new(50_000))) }
    }
}

#[async_trait]
impl UserSessionStore for LocalUserSessionStore {
    async fn get_session<'a>(&self, session_key: &'a UserSession) -> Result<Option<SessionMapping>, SessionStoreError> {
        if let Some(user_session) = self.cache.lock().await.get_mut(session_key) {
            Ok(Some(user_session.clone()))
        } else {
            Ok(None)
        }
    }

    async fn set_session<'a>(
        &self,
        session_key: &'a UserSession,
        mapping: &'a SessionMapping,
    ) -> Result<(), SessionStoreError> {
        self.cache.lock().await.insert(session_key.clone(), mapping.clone());
        Ok(())
    }
}
