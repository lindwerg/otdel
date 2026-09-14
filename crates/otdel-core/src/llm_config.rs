//! Configuration of the model adapter used by the product roles (phase 1C).
//!
//! Three properties are enforced here, and they are the reason this lives in the
//! dependency-free core crate rather than next to the HTTP client:
//!
//! **Absence of a key is a state, not an error to paper over.** The owner has not
//! supplied an OpenRouter key yet (`docs/implementation-contract.md`, "Среда"), so the
//! normal situation is [`LlmAvailability::NeedsConfiguration`] naming exactly which
//! variables are missing. Nothing in the system may substitute invented output for it:
//! the adapter built from such a configuration cannot reach the network at all
//! (`otdel_llm::build_provider`).
//!
//! **The key never reaches a log.** It is wrapped in [`ApiKey`], whose `Debug`/`Display`
//! print `<redacted>`, and [`LlmSettings`] has a hand-written `Debug` for the same
//! reason. The endpoint host is printable — it is not a secret and the owner needs to
//! see which service would be called.
//!
//! **Every call is bounded before it is made.** Input characters, pages per request,
//! requests per run, output tokens, response size and wall-clock time all have
//! validated ceilings, so a single material cannot turn into an unbounded spend
//! (`docs/block-01-spec.md` §10).

use std::fmt;
use std::time::Duration;

use crate::config::{duration_secs_or, parse_u64_or, string_or, ConfigSource};
use crate::error::AppError;
use crate::secret;

/// Default endpoint of the provider named in the implementation contract.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

const TIMEOUT_RANGE: (u64, u64) = (5, 600);
const MAX_INPUT_CHARS_RANGE: (u64, u64) = (1_000, 400_000);
const MAX_OUTPUT_TOKENS_RANGE: (u64, u64) = (256, 32_000);
const MAX_PAGES_PER_REQUEST_RANGE: (u64, u64) = (1, 50);
const MAX_REQUESTS_PER_RUN_RANGE: (u64, u64) = (1, 200);
const MAX_REQUESTS_PER_PURPOSE_RANGE: (u64, u64) = (1, 40);
const MIN_REQUEST_INTERVAL_MS_RANGE: (u64, u64) = (0, 60_000);
const MAX_RESPONSE_BYTES_RANGE: (u64, u64) = (4_096, 8 * 1024 * 1024);

/// Which wire protocol the adapter speaks.
///
/// Both variants speak the OpenAI-compatible `/chat/completions` shape; they differ
/// only in their default endpoint and in how they are described to the owner. A second
/// variant exists because the specification treats OpenRouter as *a possible* adapter,
/// not an approved obligation (`docs/block-01-spec.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProviderKind {
    /// Product roles are switched off entirely. Nothing is queued, nothing is called.
    Disabled,
    OpenRouter,
    /// Any other service exposing the OpenAI `/chat/completions` API, including a
    /// locally hosted one.
    OpenAiCompatible,
}

impl LlmProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::OpenRouter => "openrouter",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            "openrouter" => Ok(Self::OpenRouter),
            "openai_compatible" | "openai" | "compatible" => Ok(Self::OpenAiCompatible),
            other => Err(AppError::validation(format!(
                "OTDEL_LLM_PROVIDER must be `openrouter`, `openai_compatible` or `disabled`, \
                 got `{other}`"
            ))),
        }
    }
}

/// An API key that cannot be printed by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way to read the secret. Deliberately verbose at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Whether the product roles can run right now, and what is missing when they cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmAvailability {
    /// `OTDEL_LLM_PROVIDER=disabled`: the owner switched the roles off on purpose.
    Disabled,
    /// The provider is named but cannot be called yet. `missing` lists the environment
    /// variables that have to be set — it is shown to the owner as-is.
    NeedsConfiguration {
        missing: Vec<&'static str>,
    },
    Ready,
}

impl LlmAvailability {
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

/// Bounds applied to every model call, before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmLimits {
    /// Upper bound on the characters of source material put into one request.
    pub max_input_chars: u32,
    /// Upper bound on source pages put into one request.
    pub max_pages_per_request: u32,
    /// Upper bound on requests one knowledge run may make, across every purpose-specific
    /// pass. The total cap.
    pub max_requests_per_run: u32,
    /// R05.2 — upper bound on requests **one purpose-specific pass** may make.
    ///
    /// The run is five passes (inventory, facts, glossary, applications, inquiry), and
    /// this is what stops any one of them from spending the material's whole budget. A
    /// live pass over a technical catalogue returned 51 products, 6 facts and zero terms
    /// or tasks — not because the material lacked them, but because one omnibus request
    /// ran out on the cheapest section. Each pass now gets a share it cannot exceed.
    pub max_requests_per_purpose: u32,
    /// `max_tokens` sent to the provider.
    pub max_output_tokens: u32,
    /// Response body larger than this is refused without being parsed.
    pub max_response_bytes: u64,
    /// Wall-clock budget of a single request.
    pub timeout: Duration,
    /// Smallest gap between two requests of one run (a courtesy rate limit).
    pub min_request_interval: Duration,
}

impl Default for LlmLimits {
    fn default() -> Self {
        Self {
            max_input_chars: 24_000,
            max_pages_per_request: 6,
            // Five passes over a material of up to ~48 pages: 5 x 8. Raised from 8 with
            // R05.2, where 8 was one pass's worth and is now the whole run's.
            max_requests_per_run: 40,
            max_requests_per_purpose: 8,
            max_output_tokens: 4_000,
            max_response_bytes: 1024 * 1024,
            timeout: Duration::from_secs(120),
            min_request_interval: Duration::from_millis(250),
        }
    }
}

/// Everything the model adapter needs, with the secret kept out of `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct LlmSettings {
    pub provider: LlmProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<ApiKey>,
    pub limits: LlmLimits,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: LlmProviderKind::OpenRouter,
            base_url: OPENROUTER_BASE_URL.to_owned(),
            model: String::new(),
            api_key: None,
            limits: LlmLimits::default(),
        }
    }
}

impl fmt::Debug for LlmSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmSettings")
            .field("provider", &self.provider.as_str())
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("availability", &self.availability().as_str())
            .field("limits", &self.limits)
            .finish()
    }
}

impl LlmSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let defaults = Self::default();

        let provider = match source.get("OTDEL_LLM_PROVIDER") {
            Some(value) if !value.trim().is_empty() => LlmProviderKind::parse(&value)?,
            _ => defaults.provider,
        };

        let default_base = match provider {
            LlmProviderKind::OpenRouter | LlmProviderKind::Disabled => OPENROUTER_BASE_URL,
            LlmProviderKind::OpenAiCompatible => "",
        };
        let base_url = normalise_base_url(&string_or(source, "OTDEL_LLM_BASE_URL", default_base))?;

        let model = string_or(source, "OTDEL_LLM_MODEL", "");
        if !model.is_empty() && !model_looks_sane(&model) {
            return Err(AppError::validation(
                "OTDEL_LLM_MODEL must be a model identifier without spaces or control \
                 characters, e.g. `openai/gpt-4o-mini`",
            ));
        }

        // An empty or placeholder key is treated as "not supplied" rather than as a key
        // that will fail on the first call: the owner is told what to set, and no
        // request is ever attempted with it.
        let api_key = match source.get("OTDEL_LLM_API_KEY") {
            Some(value) if !value.trim().is_empty() && !secret::looks_like_placeholder(&value) => {
                let value = value.trim().to_owned();
                if value.chars().any(char::is_control) {
                    return Err(AppError::validation(
                        "OTDEL_LLM_API_KEY must not contain control characters",
                    ));
                }
                Some(ApiKey::new(value))
            }
            _ => None,
        };

        let limits = LlmLimits {
            max_input_chars: bounded_u32(
                source,
                "OTDEL_LLM_MAX_INPUT_CHARS",
                u64::from(defaults.limits.max_input_chars),
                MAX_INPUT_CHARS_RANGE,
            )?,
            max_pages_per_request: bounded_u32(
                source,
                "OTDEL_LLM_MAX_PAGES_PER_REQUEST",
                u64::from(defaults.limits.max_pages_per_request),
                MAX_PAGES_PER_REQUEST_RANGE,
            )?,
            max_requests_per_run: bounded_u32(
                source,
                "OTDEL_LLM_MAX_REQUESTS_PER_RUN",
                u64::from(defaults.limits.max_requests_per_run),
                MAX_REQUESTS_PER_RUN_RANGE,
            )?,
            max_requests_per_purpose: bounded_u32(
                source,
                "OTDEL_LLM_MAX_REQUESTS_PER_PURPOSE",
                u64::from(defaults.limits.max_requests_per_purpose),
                MAX_REQUESTS_PER_PURPOSE_RANGE,
            )?,
            max_output_tokens: bounded_u32(
                source,
                "OTDEL_LLM_MAX_OUTPUT_TOKENS",
                u64::from(defaults.limits.max_output_tokens),
                MAX_OUTPUT_TOKENS_RANGE,
            )?,
            max_response_bytes: bounded_u64(
                source,
                "OTDEL_LLM_MAX_RESPONSE_BYTES",
                defaults.limits.max_response_bytes,
                MAX_RESPONSE_BYTES_RANGE,
            )?,
            timeout: bounded_duration(
                source,
                "OTDEL_LLM_TIMEOUT_SECONDS",
                defaults.limits.timeout,
                TIMEOUT_RANGE,
            )?,
            min_request_interval: Duration::from_millis(bounded_u64(
                source,
                "OTDEL_LLM_MIN_REQUEST_INTERVAL_MS",
                u64::try_from(defaults.limits.min_request_interval.as_millis()).unwrap_or(250),
                MIN_REQUEST_INTERVAL_MS_RANGE,
            )?),
        };

        Ok(Self {
            provider,
            base_url,
            model,
            api_key,
            limits,
        })
    }

    /// What the owner is told, and what the worker checks before it queues anything.
    pub fn availability(&self) -> LlmAvailability {
        if self.provider == LlmProviderKind::Disabled {
            return LlmAvailability::Disabled;
        }

        let mut missing: Vec<&'static str> = Vec::new();
        if self.api_key.is_none() {
            missing.push("OTDEL_LLM_API_KEY");
        }
        if self.model.is_empty() {
            missing.push("OTDEL_LLM_MODEL");
        }
        if self.base_url.is_empty() {
            missing.push("OTDEL_LLM_BASE_URL");
        }

        if missing.is_empty() {
            LlmAvailability::Ready
        } else {
            LlmAvailability::NeedsConfiguration { missing }
        }
    }

    /// Host of the endpoint, for the interface and the log. Never the key.
    pub fn endpoint_host(&self) -> Option<String> {
        host_of(&self.base_url)
    }

    /// Full URL of the chat-completions endpoint.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// Reject anything that is not a plain `https://host[:port][/path]` endpoint.
///
/// Plain HTTP is allowed only towards the loopback interface, where there is no network
/// to intercept — that is the self-hosted case (`http://127.0.0.1:11434/v1`). Embedded
/// credentials, a query string or a fragment are refused outright: the key belongs in
/// the `Authorization` header, not in a URL that ends up in logs and error messages.
fn normalise_base_url(raw: &str) -> Result<String, AppError> {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.chars().any(char::is_control) || value.contains(char::is_whitespace) {
        return Err(AppError::validation(
            "OTDEL_LLM_BASE_URL must not contain whitespace or control characters",
        ));
    }
    if value.contains('?') || value.contains('#') {
        return Err(AppError::validation(
            "OTDEL_LLM_BASE_URL must be a plain endpoint without a query string or fragment",
        ));
    }

    let (scheme, rest) = value
        .split_once("://")
        .ok_or_else(|| AppError::validation("OTDEL_LLM_BASE_URL must start with https://"))?;
    if rest.is_empty() {
        return Err(AppError::validation("OTDEL_LLM_BASE_URL has no host"));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(AppError::validation(
            "OTDEL_LLM_BASE_URL must not embed credentials; the key is sent in the \
             Authorization header",
        ));
    }

    match scheme {
        "https" => Ok(value.to_owned()),
        "http" if is_loopback_authority(authority) => Ok(value.to_owned()),
        "http" => Err(AppError::validation(
            "OTDEL_LLM_BASE_URL may only use http:// for a loopback address \
             (127.0.0.1, ::1, localhost); use https:// for anything else",
        )),
        other => Err(AppError::validation(format!(
            "OTDEL_LLM_BASE_URL scheme `{other}` is not supported; use https://"
        ))),
    }
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = match authority.rsplit_once(':') {
        // `[::1]:8080` — the bracketed form keeps the colons of an IPv6 literal out of
        // the port split.
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn host_of(base_url: &str) -> Option<String> {
    let (_, rest) = base_url.split_once("://")?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() {
        None
    } else {
        Some(authority.to_owned())
    }
}

fn model_looks_sane(model: &str) -> bool {
    model.len() <= 200
        && !model.chars().any(|c| c.is_control() || c.is_whitespace())
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':' | '@' | '+')
        })
}

fn bounded_u64(
    source: &dyn ConfigSource,
    key: &str,
    default: u64,
    (min, max): (u64, u64),
) -> Result<u64, AppError> {
    let value = parse_u64_or(source, key, default)?;
    if value < min || value > max {
        return Err(AppError::validation(format!(
            "{key} must be between {min} and {max}"
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

fn bounded_duration(
    source: &dyn ConfigSource,
    key: &str,
    default: Duration,
    (min, max): (u64, u64),
) -> Result<Duration, AppError> {
    let value = duration_secs_or(source, key, default.as_secs())?;
    if value.as_secs() < min || value.as_secs() > max {
        return Err(AppError::validation(format!(
            "{key} must be between {min} and {max} seconds"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn without_a_key_the_product_role_reports_what_is_missing() {
        let settings = LlmSettings::load(&env(&[])).unwrap();
        assert_eq!(settings.provider, LlmProviderKind::OpenRouter);
        assert_eq!(
            settings.availability(),
            LlmAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_LLM_API_KEY", "OTDEL_LLM_MODEL"],
            }
        );
        assert!(!settings.availability().is_ready());
    }

    #[test]
    fn a_placeholder_key_is_not_a_key() {
        for placeholder in ["", "   ", "changeme", "your-api-key-here"] {
            let settings = LlmSettings::load(&env(&[
                ("OTDEL_LLM_API_KEY", placeholder),
                ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
            ]))
            .unwrap();
            assert!(
                !settings.availability().is_ready(),
                "`{placeholder}` must not count as a configured key"
            );
        }
    }

    #[test]
    fn a_complete_configuration_is_ready() {
        let settings = LlmSettings::load(&env(&[
            ("OTDEL_LLM_API_KEY", "sk-or-v1-abcdef0123456789"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
        ]))
        .unwrap();
        assert_eq!(settings.availability(), LlmAvailability::Ready);
        assert_eq!(
            settings.chat_completions_url(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(settings.endpoint_host().as_deref(), Some("openrouter.ai"));
    }

    #[test]
    fn disabled_is_its_own_state_not_a_missing_key() {
        let settings = LlmSettings::load(&env(&[("OTDEL_LLM_PROVIDER", "disabled")])).unwrap();
        assert_eq!(settings.availability(), LlmAvailability::Disabled);
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        let settings = LlmSettings::load(&env(&[
            ("OTDEL_LLM_API_KEY", "sk-or-v1-supersecretvalue"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
        ]))
        .unwrap();

        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("supersecret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("openrouter.ai"), "{rendered}");

        let key = settings.api_key.clone().unwrap();
        assert_eq!(format!("{key:?}"), "<redacted>");
        assert_eq!(format!("{key}"), "<redacted>");
        assert_eq!(key.expose(), "sk-or-v1-supersecretvalue");
    }

    #[test]
    fn insecure_and_malformed_endpoints_are_refused() {
        for url in [
            "http://api.example.com/v1",
            "ftp://example.com",
            "example.com/v1",
            "https://user:pass@example.com/v1",
            "https://example.com/v1?key=abc",
            "https://exa mple.com/v1",
        ] {
            let error = LlmSettings::load(&env(&[("OTDEL_LLM_BASE_URL", url)])).unwrap_err();
            assert!(
                error.message.contains("OTDEL_LLM_BASE_URL"),
                "`{url}` must be refused, got: {}",
                error.message
            );
        }
    }

    #[test]
    fn loopback_http_is_allowed_for_a_self_hosted_model() {
        for url in [
            "http://127.0.0.1:11434/v1",
            "http://localhost:8000/v1",
            "http://[::1]:8080/v1",
        ] {
            let settings = LlmSettings::load(&env(&[
                ("OTDEL_LLM_PROVIDER", "openai_compatible"),
                ("OTDEL_LLM_BASE_URL", url),
            ]))
            .unwrap();
            assert_eq!(settings.base_url, url);
        }
    }

    #[test]
    fn a_compatible_provider_without_an_endpoint_is_not_ready() {
        let settings = LlmSettings::load(&env(&[
            ("OTDEL_LLM_PROVIDER", "openai_compatible"),
            ("OTDEL_LLM_API_KEY", "sk-local-0123456789"),
            ("OTDEL_LLM_MODEL", "local/model"),
        ]))
        .unwrap();
        assert_eq!(
            settings.availability(),
            LlmAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_LLM_BASE_URL"],
            }
        );
    }

    #[test]
    fn limits_are_bounded_and_defaulted() {
        let settings = LlmSettings::load(&env(&[])).unwrap();
        assert_eq!(settings.limits, LlmLimits::default());

        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_MAX_INPUT_CHARS", "10")])).is_err());
        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_MAX_REQUESTS_PER_RUN", "0")])).is_err());
        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_TIMEOUT_SECONDS", "1")])).is_err());
        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_MAX_RESPONSE_BYTES", "10")])).is_err());

        let tightened = LlmSettings::load(&env(&[
            ("OTDEL_LLM_MAX_INPUT_CHARS", "5000"),
            ("OTDEL_LLM_MAX_PAGES_PER_REQUEST", "2"),
            ("OTDEL_LLM_MAX_REQUESTS_PER_RUN", "3"),
        ]))
        .unwrap();
        assert_eq!(tightened.limits.max_input_chars, 5_000);
        assert_eq!(tightened.limits.max_pages_per_request, 2);
        assert_eq!(tightened.limits.max_requests_per_run, 3);
    }

    #[test]
    fn a_model_identifier_with_spaces_is_refused() {
        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_MODEL", "gpt 4o mini")])).is_err());
        assert!(LlmSettings::load(&env(&[("OTDEL_LLM_MODEL", "openai/gpt-4o-mini")])).is_ok());
    }
}
