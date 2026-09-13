//! Shared application state.

use std::sync::Arc;

use otdel_core::config::Config;
use otdel_db::Database;
use otdel_llm::LlmProvider;
use otdel_storage::ObjectStore;

use crate::auth::LoginThrottle;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Database,
    /// Trait object so the S3 backend can replace the filesystem one later without
    /// touching a single handler.
    pub store: Arc<dyn ObjectStore>,
    /// The model adapter — held by the API only to *describe* it. The API never calls
    /// a model: drafting happens in the worker, off the request path. Without a key
    /// this is the unconfigured adapter, and `/api/knowledge/provider` says so.
    pub llm: Arc<dyn LlmProvider>,
    pub throttle: Arc<LoginThrottle>,
}

impl AppState {
    pub fn new(config: Arc<Config>, db: Database, store: Arc<dyn ObjectStore>) -> Self {
        let throttle = Arc::new(LoginThrottle::new(config.login_throttle));
        let llm = otdel_llm::build_provider(&config.llm);
        Self {
            config,
            db,
            store,
            llm,
            throttle,
        }
    }
}

impl AppState {
    /// Replace the model adapter.
    ///
    /// The adapter is the one dependency of this server that cannot be exercised from
    /// its configuration alone: a real one needs a key and a network, and this phase
    /// must never acquire either implicitly. Tests therefore inject a scripted
    /// provider here and drive the whole path — route, validation, storage — without
    /// one.
    pub fn with_provider(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = llm;
        self
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .field("store", &self.store.describe())
            // The description carries the provider name and state, never the key.
            .field("llm", &self.llm.describe().state)
            .finish_non_exhaustive()
    }
}
