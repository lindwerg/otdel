//! Shared application state.

use std::sync::Arc;

use otdel_core::config::Config;
use otdel_db::Database;
use otdel_embed::EmbeddingProvider;
use otdel_llm::LlmProvider;
use otdel_search::{DocumentFetcher, SearchProvider};
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
    /// The phase 1D adapters, held for exactly the same reason and with exactly the same
    /// restriction: the API describes them and never calls them. Research happens in the
    /// worker, where it can be bounded, paid for and stopped — an HTTP handler that
    /// reached the internet would be an unbounded request path.
    pub search: Arc<dyn SearchProvider>,
    pub fetcher: Arc<dyn DocumentFetcher>,
    /// The phase 1E embedding adapter.
    ///
    /// Unlike the three above, this one the API *does* call — once per search, on the
    /// query text only, to put the question in the same vector space as the version.
    /// There is no way around it: a semantic search has to embed the query, and doing it
    /// in a worker would mean answering a question the caller has not asked yet.
    ///
    /// It is a deliberate, bounded exception and it fails safe. The call carries
    /// `OTDEL_EMBEDDING_TIMEOUT_SECONDS`, it is made only when a provider is configured,
    /// and a failure degrades the request to keyword search **with the reason reported**
    /// rather than turning a search into an error.
    pub embeddings: Arc<dyn EmbeddingProvider>,
    pub throttle: Arc<LoginThrottle>,
}

impl AppState {
    pub fn new(config: Arc<Config>, db: Database, store: Arc<dyn ObjectStore>) -> Self {
        let throttle = Arc::new(LoginThrottle::new(config.login_throttle));
        let llm = otdel_llm::build_provider(&config.llm);
        let search = otdel_search::build_search_provider(&config.research);
        let fetcher = otdel_search::build_fetcher(&config.research);
        let embeddings = otdel_embed::build_provider(&config.retrieval.embedding);
        Self {
            config,
            db,
            store,
            llm,
            search,
            fetcher,
            embeddings,
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

    /// Replace the phase 1D adapters.
    ///
    /// Same reasoning as [`Self::with_provider`], with one more: a real search adapter
    /// needs a key, a network *and* somebody's money. No test may acquire any of the
    /// three, so the suites inject scripted adapters here and drive the whole path —
    /// route, queue, budget, validation, storage — without them.
    pub fn with_research_adapters(
        mut self,
        search: Arc<dyn SearchProvider>,
        fetcher: Arc<dyn DocumentFetcher>,
    ) -> Self {
        self.search = search;
        self.fetcher = fetcher;
        self
    }

    /// Replace the phase 1E embedding adapter.
    ///
    /// Same reason as the others: a real one needs a key and a network, and the suite
    /// must be able to drive the whole hybrid search — the query folding, the exact half,
    /// the full-text half and the vector half — without acquiring either.
    pub fn with_embeddings(mut self, embeddings: Arc<dyn EmbeddingProvider>) -> Self {
        self.embeddings = embeddings;
        self
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .field("store", &self.store.describe())
            // Each description carries the adapter's name and state, never a key.
            .field("llm", &self.llm.describe().state)
            .field("search", &self.search.describe().state)
            .field("fetcher", &self.fetcher.describe().state)
            .field("embeddings", &self.embeddings.describe().state)
            .finish_non_exhaustive()
    }
}
