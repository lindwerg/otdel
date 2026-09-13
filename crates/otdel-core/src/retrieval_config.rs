//! Configuration of the optional halves of phase 1E: embeddings and the bounds on
//! search and answering.
//!
//! The word that matters here is **optional**. Verification and publication are
//! deterministic and need nothing configured; what this module configures is the part
//! of search that turns meaning into vectors, and the model that turns found claims into
//! a sentence. Three properties follow, and they are why this lives in the
//! dependency-free core rather than beside an HTTP client:
//!
//! **Nothing invents a vector.** Without an embedding endpoint and model there is no
//! embedding adapter with a client in it (`otdel_embed::build_provider`), no chunk gets
//! a vector, and search reports `keyword` mode with the reason. A random or hashed
//! pseudo-vector would make the neighbourhood meaningless while looking like semantic
//! search, which is worse than not having it.
//!
//! **Profiles never mix.** `block-01-spec.md` §9: embeddings of different models do not
//! share a space. The profile identifier is stored on every row and on the version, and
//! every distance query filters by it, so a changed model produces "this version has no
//! vectors of the current profile" rather than nonsense distances.
//!
//! **The key never reaches a log**, exactly as in [`crate::llm_config`]: it is an
//! [`ApiKey`], and [`EmbeddingSettings`] has a hand-written `Debug`.

use std::fmt;
use std::time::Duration;

use crate::config::{parse_u64_or, string_or, ConfigSource};
use crate::error::AppError;
use crate::llm_config::ApiKey;
use crate::secret;

/// Default endpoint, matching the one the model adapter already uses.
pub const DEFAULT_EMBEDDING_BASE_URL: &str = "https://openrouter.ai/api/v1";

const TIMEOUT_RANGE: (u64, u64) = (5, 600);
const BATCH_RANGE: (u64, u64) = (1, 256);
const MAX_INPUT_CHARS_RANGE: (u64, u64) = (200, 40_000);
const MAX_RESPONSE_BYTES_RANGE: (u64, u64) = (4_096, 64 * 1024 * 1024);
const MIN_REQUEST_INTERVAL_MS_RANGE: (u64, u64) = (0, 60_000);
const MAX_QUERY_CHARS_RANGE: (u64, u64) = (16, 4_000);
const MAX_RESULTS_RANGE: (u64, u64) = (1, 100);
const MAX_ANSWER_CLAIMS_RANGE: (u64, u64) = (1, 50);
const MAX_ANSWER_CHARS_RANGE: (u64, u64) = (200, 8_000);
const CHUNK_MAX_CHARS_RANGE: (u64, u64) = (200, 4_000);

/// Which embedding service, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingProviderKind {
    /// Switched off deliberately. Search is keyword-only and says so.
    Disabled,
    /// Any service exposing the OpenAI-compatible `/embeddings` endpoint, including
    /// OpenRouter and a locally hosted model.
    OpenAiCompatible,
}

impl EmbeddingProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            "openai_compatible" | "openai" | "openrouter" => Ok(Self::OpenAiCompatible),
            other => Err(AppError::validation(format!(
                "OTDEL_EMBEDDING_PROVIDER must be `openai_compatible` or `disabled`, got `{other}`"
            ))),
        }
    }
}

/// Whether vectors can be produced right now, and what is missing when they cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingAvailability {
    /// `OTDEL_EMBEDDING_PROVIDER=disabled`.
    Disabled,
    /// Named but not usable yet. `missing` lists the variables to set, shown as-is.
    NeedsConfiguration {
        missing: Vec<&'static str>,
    },
    Ready,
}

impl EmbeddingAvailability {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NeedsConfiguration { .. } => "needs_configuration",
            Self::Ready => "ready",
        }
    }

    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Bounds applied to every embedding call before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingLimits {
    /// Chunks sent in one request.
    pub batch_size: u32,
    /// Characters of one chunk sent for embedding; longer text is clipped, and the
    /// clipping is recorded rather than hidden.
    pub max_input_chars: u32,
    pub max_response_bytes: u64,
    pub timeout: Duration,
    pub min_request_interval: Duration,
}

impl Default for EmbeddingLimits {
    fn default() -> Self {
        Self {
            batch_size: 32,
            max_input_chars: 4_000,
            max_response_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(120),
            min_request_interval: Duration::from_millis(250),
        }
    }
}

/// Everything the embedding adapter needs, with the secret kept out of `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct EmbeddingSettings {
    pub provider: EmbeddingProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<ApiKey>,
    pub limits: EmbeddingLimits,
}

impl Default for EmbeddingSettings {
    fn default() -> Self {
        Self {
            provider: EmbeddingProviderKind::OpenAiCompatible,
            base_url: DEFAULT_EMBEDDING_BASE_URL.to_owned(),
            model: String::new(),
            api_key: None,
            limits: EmbeddingLimits::default(),
        }
    }
}

impl fmt::Debug for EmbeddingSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingSettings")
            .field("provider", &self.provider.as_str())
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("availability", &self.availability().as_str())
            .field("limits", &self.limits)
            .finish()
    }
}

impl EmbeddingSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let defaults = Self::default();

        let provider = match source.get("OTDEL_EMBEDDING_PROVIDER") {
            Some(value) if !value.trim().is_empty() => EmbeddingProviderKind::parse(&value)?,
            _ => defaults.provider,
        };

        let base_url = normalise_base_url(&string_or(
            source,
            "OTDEL_EMBEDDING_BASE_URL",
            DEFAULT_EMBEDDING_BASE_URL,
        ))?;

        let model = string_or(source, "OTDEL_EMBEDDING_MODEL", "");
        if !model.is_empty()
            && (model
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace())
                || model.len() > 200)
        {
            return Err(AppError::validation(
                "OTDEL_EMBEDDING_MODEL must be a model identifier without spaces or control \
                 characters, e.g. `openai/text-embedding-3-small`",
            ));
        }

        // An empty or placeholder key is "not supplied", never a key that will fail on
        // the first call.
        let api_key = match source.get("OTDEL_EMBEDDING_API_KEY") {
            Some(value) if !value.trim().is_empty() && !secret::looks_like_placeholder(&value) => {
                let value = value.trim().to_owned();
                if value.chars().any(char::is_control) {
                    return Err(AppError::validation(
                        "OTDEL_EMBEDDING_API_KEY must not contain control characters",
                    ));
                }
                Some(ApiKey::new(value))
            }
            _ => None,
        };

        let limits = EmbeddingLimits {
            batch_size: bounded_u32(
                source,
                "OTDEL_EMBEDDING_BATCH_SIZE",
                u64::from(defaults.limits.batch_size),
                BATCH_RANGE,
            )?,
            max_input_chars: bounded_u32(
                source,
                "OTDEL_EMBEDDING_MAX_INPUT_CHARS",
                u64::from(defaults.limits.max_input_chars),
                MAX_INPUT_CHARS_RANGE,
            )?,
            max_response_bytes: bounded_u64(
                source,
                "OTDEL_EMBEDDING_MAX_RESPONSE_BYTES",
                defaults.limits.max_response_bytes,
                MAX_RESPONSE_BYTES_RANGE,
            )?,
            timeout: Duration::from_secs(bounded_u64(
                source,
                "OTDEL_EMBEDDING_TIMEOUT_SECONDS",
                defaults.limits.timeout.as_secs(),
                TIMEOUT_RANGE,
            )?),
            min_request_interval: Duration::from_millis(bounded_u64(
                source,
                "OTDEL_EMBEDDING_MIN_REQUEST_INTERVAL_MS",
                u64::try_from(defaults.limits.min_request_interval.as_millis()).unwrap_or(250),
                MIN_REQUEST_INTERVAL_MS_RANGE,
            )?),
        };

        // Two ceilings that are each legal on their own and unusable together: one
        // response carries `batch_size` vectors, so a byte cap too small for that batch
        // refuses **every** response. The failure is classified as not retryable, so
        // nothing retries and nothing is malformed — the version just silently never
        // gets vectors on a configuration the loader accepted. Roughly 8 KiB per vector
        // of ~1500 dimensions is the conservative figure.
        const BYTES_PER_VECTOR: u64 = 8 * 1024;
        let needed = u64::from(limits.batch_size).saturating_mul(BYTES_PER_VECTOR);
        if limits.max_response_bytes < needed {
            return Err(AppError::validation(format!(
                "OTDEL_EMBEDDING_MAX_RESPONSE_BYTES ({}) слишком мал для \
                 OTDEL_EMBEDDING_BATCH_SIZE ({}): одна партия из {} векторов не поместится \
                 в ответ, и каждый запрос будет отвергнут. Поднимите лимит до {needed} или \
                 уменьшите размер партии",
                limits.max_response_bytes, limits.batch_size, limits.batch_size
            )));
        }

        Ok(Self {
            provider,
            base_url,
            model,
            api_key,
            limits,
        })
    }

    pub fn availability(&self) -> EmbeddingAvailability {
        if self.provider == EmbeddingProviderKind::Disabled {
            return EmbeddingAvailability::Disabled;
        }

        let mut missing: Vec<&'static str> = Vec::new();
        if self.api_key.is_none() {
            missing.push("OTDEL_EMBEDDING_API_KEY");
        }
        if self.model.is_empty() {
            missing.push("OTDEL_EMBEDDING_MODEL");
        }
        if self.base_url.is_empty() {
            missing.push("OTDEL_EMBEDDING_BASE_URL");
        }

        if missing.is_empty() {
            EmbeddingAvailability::Ready
        } else {
            EmbeddingAvailability::NeedsConfiguration { missing }
        }
    }

    /// Identifier of the vector space these settings produce.
    ///
    /// Stored on every chunk and on the version. A version embedded under one profile is
    /// never compared with a query embedded under another: the search filters by this
    /// string first, so changing the model degrades honestly to keyword search for
    /// versions published before the change, instead of returning distances computed
    /// between two unrelated spaces (`block-01-spec.md` §9).
    pub fn profile(&self) -> Option<String> {
        self.availability()
            .is_ready()
            .then(|| format!("{}:{}", self.provider.as_str(), self.model))
    }

    /// Host of the endpoint, for the interface and the log. Never the key.
    pub fn endpoint_host(&self) -> Option<String> {
        host_of(&self.base_url)
    }

    pub fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.base_url.trim_end_matches('/'))
    }
}

/// Bounds on reading the published knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalLimits {
    /// Longest question or query accepted. A request beyond it is refused, not clipped:
    /// silently answering a different question than the one asked is worse.
    pub max_query_chars: u32,
    /// Most hits one search returns.
    pub max_results: u32,
    /// Most claims put in front of the answering model. This is the bounded context of
    /// `block-01-spec.md` §9 — the model sees these and nothing else.
    pub max_answer_claims: u32,
    /// Longest prose answer accepted from the model.
    pub max_answer_chars: u32,
    /// Longest searchable rendering of one claim.
    pub chunk_max_chars: u32,
}

impl Default for RetrievalLimits {
    fn default() -> Self {
        Self {
            max_query_chars: 500,
            max_results: 20,
            max_answer_claims: 8,
            max_answer_chars: 1_500,
            chunk_max_chars: 2_000,
        }
    }
}

/// Phase 1E configuration as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetrievalSettings {
    pub embedding: EmbeddingSettings,
    pub limits: RetrievalLimits,
}

impl RetrievalSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let defaults = RetrievalLimits::default();
        let limits = RetrievalLimits {
            max_query_chars: bounded_u32(
                source,
                "OTDEL_RETRIEVAL_MAX_QUERY_CHARS",
                u64::from(defaults.max_query_chars),
                MAX_QUERY_CHARS_RANGE,
            )?,
            max_results: bounded_u32(
                source,
                "OTDEL_RETRIEVAL_MAX_RESULTS",
                u64::from(defaults.max_results),
                MAX_RESULTS_RANGE,
            )?,
            max_answer_claims: bounded_u32(
                source,
                "OTDEL_RETRIEVAL_MAX_ANSWER_CLAIMS",
                u64::from(defaults.max_answer_claims),
                MAX_ANSWER_CLAIMS_RANGE,
            )?,
            max_answer_chars: bounded_u32(
                source,
                "OTDEL_RETRIEVAL_MAX_ANSWER_CHARS",
                u64::from(defaults.max_answer_chars),
                MAX_ANSWER_CHARS_RANGE,
            )?,
            chunk_max_chars: bounded_u32(
                source,
                "OTDEL_RETRIEVAL_CHUNK_MAX_CHARS",
                u64::from(defaults.chunk_max_chars),
                CHUNK_MAX_CHARS_RANGE,
            )?,
        };

        let embedding = EmbeddingSettings::load(source)?;

        // A question the search accepts but the embedding adapter would clip is a
        // question the vector half never actually saw, answered as if it had. The
        // request path refuses the vector half and says so when this happens; warning
        // here means the owner learns it at startup instead of from a `degraded[]` line
        // in production.
        if u64::from(limits.max_query_chars) > u64::from(embedding.limits.max_input_chars) {
            tracing::warn!(
                max_query_chars = limits.max_query_chars,
                max_input_chars = embedding.limits.max_input_chars,
                "OTDEL_RETRIEVAL_MAX_QUERY_CHARS exceeds OTDEL_EMBEDDING_MAX_INPUT_CHARS: a \
                 longer query will be answered by keyword search only, with the reason \
                 reported, rather than embedded in part"
            );
        }

        Ok(Self { embedding, limits })
    }
}

/// Reject anything that is not a plain `https://host[:port][/path]` endpoint.
///
/// The same rule as the model adapter's: plain HTTP only towards loopback (a
/// self-hosted embedding model is the case that needs it), and never embedded
/// credentials, a query string or a fragment — the key belongs in a header, not in a URL
/// that ends up in a log.
fn normalise_base_url(raw: &str) -> Result<String, AppError> {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.chars().any(char::is_control) || value.contains(char::is_whitespace) {
        return Err(AppError::validation(
            "OTDEL_EMBEDDING_BASE_URL must not contain whitespace or control characters",
        ));
    }
    if value.contains('?') || value.contains('#') || value.contains('@') {
        return Err(AppError::validation(
            "OTDEL_EMBEDDING_BASE_URL must be a plain endpoint without credentials, a query \
             string or a fragment",
        ));
    }

    let rest = if let Some(rest) = value.strip_prefix("https://") {
        rest
    } else if let Some(rest) = value.strip_prefix("http://") {
        let host = rest.split('/').next().unwrap_or_default();
        let bare = host.split(':').next().unwrap_or_default();
        if !matches!(bare, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(AppError::validation(
                "OTDEL_EMBEDDING_BASE_URL may use plain http:// only for a loopback address; \
                 use https:// for anything else",
            ));
        }
        rest
    } else {
        return Err(AppError::validation(
            "OTDEL_EMBEDDING_BASE_URL must start with https:// (or http:// for loopback)",
        ));
    };

    if rest.split('/').next().unwrap_or_default().is_empty() {
        return Err(AppError::validation("OTDEL_EMBEDDING_BASE_URL has no host"));
    }

    Ok(value.to_owned())
}

fn host_of(base_url: &str) -> Option<String> {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or_default();
    (!host.is_empty()).then(|| host.to_owned())
}

// The two bounded readers below are deliberately private copies of the ones in
// `llm_config` and `research_config`: each phase's configuration owns its own, so a
// range change in one phase cannot silently move another phase's ceiling.
fn bounded_u64(
    source: &dyn ConfigSource,
    key: &str,
    default: u64,
    range: (u64, u64),
) -> Result<u64, AppError> {
    let value = parse_u64_or(source, key, default)?;
    if value < range.0 || value > range.1 {
        return Err(AppError::validation(format!(
            "{key} must be between {} and {}",
            range.0, range.1
        )));
    }
    Ok(value)
}

fn bounded_u32(
    source: &dyn ConfigSource,
    key: &str,
    default: u64,
    range: (u64, u64),
) -> Result<u32, AppError> {
    let value = bounded_u64(source, key, default, range)?;
    u32::try_from(value).map_err(|_| AppError::validation(format!("{key} is too large")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn with_nothing_configured_embeddings_name_what_is_missing() {
        let settings = EmbeddingSettings::load(&empty()).unwrap();
        assert_eq!(
            settings.availability(),
            EmbeddingAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_EMBEDDING_API_KEY", "OTDEL_EMBEDDING_MODEL"],
            },
            "with no embedding model configured the system must say what to set instead of \
             reaching for some default service"
        );
    }

    #[test]
    fn without_a_provider_there_is_no_profile_so_nothing_can_claim_to_have_vectors() {
        let settings = EmbeddingSettings::load(&empty()).unwrap();
        assert_eq!(settings.profile(), None);
    }

    #[test]
    fn a_configured_profile_names_the_model_so_two_models_never_share_a_space() {
        let mut source = empty();
        source.insert("OTDEL_EMBEDDING_API_KEY".into(), "key-1".into());
        source.insert(
            "OTDEL_EMBEDDING_MODEL".into(),
            "openai/text-embedding-3-small".into(),
        );
        let settings = EmbeddingSettings::load(&source).unwrap();
        assert!(settings.availability().is_ready());
        assert_eq!(
            settings.profile().as_deref(),
            Some("openai_compatible:openai/text-embedding-3-small")
        );

        source.insert("OTDEL_EMBEDDING_MODEL".into(), "bge/m3".into());
        let other = EmbeddingSettings::load(&source).unwrap();
        assert_ne!(
            settings.profile(),
            other.profile(),
            "a different model must be a different vector space"
        );
    }

    #[test]
    fn a_placeholder_key_counts_as_no_key_rather_than_one_that_fails_later() {
        let mut source = empty();
        source.insert("OTDEL_EMBEDDING_API_KEY".into(), "changeme".into());
        source.insert("OTDEL_EMBEDDING_MODEL".into(), "m".into());
        let settings = EmbeddingSettings::load(&source).unwrap();
        assert!(!settings.availability().is_ready());
    }

    #[test]
    fn disabling_embeddings_is_a_state_of_its_own_not_a_missing_variable() {
        let mut source = empty();
        source.insert("OTDEL_EMBEDDING_PROVIDER".into(), "disabled".into());
        let settings = EmbeddingSettings::load(&source).unwrap();
        assert_eq!(settings.availability(), EmbeddingAvailability::Disabled);
    }

    #[test]
    fn a_plain_http_endpoint_is_refused_unless_it_is_loopback() {
        let mut source = empty();
        source.insert(
            "OTDEL_EMBEDDING_BASE_URL".into(),
            "http://embeddings.example.com/v1".into(),
        );
        assert!(EmbeddingSettings::load(&source).is_err());

        source.insert(
            "OTDEL_EMBEDDING_BASE_URL".into(),
            "http://127.0.0.1:11434/v1".into(),
        );
        let settings = EmbeddingSettings::load(&source).unwrap();
        assert_eq!(settings.endpoint_host().as_deref(), Some("127.0.0.1:11434"));
    }

    #[test]
    fn an_endpoint_carrying_credentials_is_refused_so_a_key_cannot_reach_a_log() {
        let mut source = empty();
        source.insert(
            "OTDEL_EMBEDDING_BASE_URL".into(),
            "https://user:secret@embeddings.example.com/v1".into(),
        );
        assert!(EmbeddingSettings::load(&source).is_err());
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let mut source = empty();
        source.insert(
            "OTDEL_EMBEDDING_API_KEY".into(),
            "sk-super-secret-value".into(),
        );
        source.insert("OTDEL_EMBEDDING_MODEL".into(), "m".into());
        let settings = EmbeddingSettings::load(&source).unwrap();
        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("sk-super-secret-value"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn a_limit_outside_its_range_is_refused_by_name() {
        let mut source = empty();
        source.insert("OTDEL_RETRIEVAL_MAX_RESULTS".into(), "100000".into());
        let error = RetrievalSettings::load(&source).unwrap_err();
        assert!(
            error.message.contains("OTDEL_RETRIEVAL_MAX_RESULTS"),
            "{}",
            error.message
        );
    }

    #[test]
    fn the_shipped_retrieval_limits_are_the_documented_ones() {
        let limits = RetrievalSettings::load(&empty()).unwrap().limits;
        assert_eq!(limits, RetrievalLimits::default());
    }
}
