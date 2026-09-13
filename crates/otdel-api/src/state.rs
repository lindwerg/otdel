//! Shared application state.

use std::sync::Arc;

use otdel_core::config::Config;
use otdel_db::Database;
use otdel_storage::ObjectStore;

use crate::auth::LoginThrottle;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Database,
    /// Trait object so the S3 backend can replace the filesystem one later without
    /// touching a single handler.
    pub store: Arc<dyn ObjectStore>,
    pub throttle: Arc<LoginThrottle>,
}

impl AppState {
    pub fn new(config: Arc<Config>, db: Database, store: Arc<dyn ObjectStore>) -> Self {
        let throttle = Arc::new(LoginThrottle::new(config.login_throttle));
        Self {
            config,
            db,
            store,
            throttle,
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .field("store", &self.store.describe())
            .finish_non_exhaustive()
    }
}
