//! Configuration of the bounded industry researcher (phase 1D).
//!
//! Phase 1C put a model adapter behind a configuration that, without a key, has no HTTP
//! client at all. This phase does the same for the *outside world*, and adds the two
//! things a researcher needs that a model call does not: a declared set of hosts it may
//! read, and money.
//!
//! Four properties are enforced here rather than at the call site.
//!
//! **Absence of a provider is a state.** No search endpoint has been chosen for OTDEL
//! (`docs/block-01-spec.md` §3 lists Perplexity and OpenRouter as *possible* adapters,
//! not obligations), so [`SearchAvailability::NeedsConfiguration`] naming the missing
//! variables is the normal situation. Nothing substitutes an invented answer for it, and
//! the adapter built from such a configuration cannot reach a network at all
//! (`otdel_search::build_search_provider`).
//!
//! **No arbitrary URL is ever fetched.** [`ResearchSettings::allowed_hosts`] is a
//! *required* variable: with an empty allowlist the researcher is not ready, so there is
//! no configuration in which "fetch whatever the search engine returned" is possible. A
//! result outside the list is recorded as discovered-and-not-read, with the reason.
//!
//! **Every external call is priced before it is made.** The cost model here is the
//! *declared* tariff — micros per search, micros per fetch — used to reserve budget
//! before the call and settle it afterwards (`docs/block-01-spec.md` §10). It is not a
//! provider's invoice, and the documentation says so.
//!
//! **Nothing is unbounded.** Queries per plan, results per query, pages per plan, bytes
//! per page, characters stored, wall-clock time per plan, passes per plan and the pause
//! between requests all have validated ceilings.

use std::fmt;
use std::time::Duration;

use crate::config::{duration_secs_or, parse_u64_or, string_or, ConfigSource};
use crate::error::AppError;
use crate::llm_config::ApiKey;
use crate::secret;

/// Longest host name the DNS specification allows.
const MAX_HOST_CHARS: usize = 253;
/// Upper bound on entries in the allowlist — a list this long is not a decision any more.
const MAX_ALLOWED_HOSTS: usize = 200;

const MAX_QUERIES_RANGE: (u64, u64) = (1, 20);
const MAX_RESULTS_RANGE: (u64, u64) = (1, 50);
const MAX_SOURCES_RANGE: (u64, u64) = (1, 50);
const MAX_PAGE_BYTES_RANGE: (u64, u64) = (4_096, 8 * 1024 * 1024);
const MAX_PAGE_CHARS_RANGE: (u64, u64) = (1_000, 200_000);
const REQUEST_TIMEOUT_RANGE: (u64, u64) = (5, 120);
const PLAN_TIME_BUDGET_RANGE: (u64, u64) = (30, 1_800);
const MIN_REQUEST_INTERVAL_RANGE: (u64, u64) = (0, 60_000);
const MAX_PASSES_RANGE: (u64, u64) = (1, 10);
/// OpenRouter clamps the tool's own `max_results` at 25.
const OPENROUTER_MAX_RESULTS_RANGE: (u64, u64) = (1, 25);
const OPENROUTER_MAX_TOTAL_RESULTS_RANGE: (u64, u64) = (1, 200);
/// Ten currency units per single call is already absurd; beyond it a typo is likelier
/// than an intention.
const COST_RANGE: (u64, u64) = (0, 10_000_000);
/// A thousand currency units of budget. Above this the number is not a local pilot's.
const BUDGET_RANGE: (u64, u64) = (0, 1_000_000_000);

/// Which search adapter the researcher speaks to.
///
/// [`SearchProviderKind::HttpJson`] is a *shape* rather than a vendor: any endpoint — a
/// vendor API, a self-hosted SearxNG, a three-line proxy in front of either — that
/// accepts the documented JSON request and answers with the documented JSON response.
///
/// [`SearchProviderKind::OpenRouterWebSearch`] is the first *named* provider, chosen by
/// the owner: OpenRouter's official server tool `openrouter:web_search`, called through
/// the same `/chat/completions` endpoint the product roles already use. It finds links;
/// it is never allowed to become the thing that reads them. Every URL it returns goes
/// through the same allowlist, the same SSRF guard, the same `robots.txt` check and the
/// same snapshot-and-quote pipeline as any other lead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProviderKind {
    /// External research is switched off on purpose. Nothing is queued, nothing called.
    Disabled,
    /// A configured HTTPS endpoint speaking the documented JSON shape.
    HttpJson,
    /// OpenRouter's `openrouter:web_search` server tool.
    OpenRouterWebSearch,
}

impl SearchProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::HttpJson => "http_json",
            Self::OpenRouterWebSearch => "openrouter_web_search",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" | "off" | "none" => Ok(Self::Disabled),
            "http_json" | "http" | "json" => Ok(Self::HttpJson),
            "openrouter" | "openrouter_web_search" | "web_search" => Ok(Self::OpenRouterWebSearch),
            other => Err(AppError::validation(format!(
                "OTDEL_RESEARCH_PROVIDER must be `openrouter`, `http_json` or `disabled`, \
                 got `{other}`"
            ))),
        }
    }
}

/// Which engine OpenRouter is asked to run the search on.
///
/// An allowlist, not a free string. OpenRouter also exposes `perplexity` and
/// `firecrawl`; they are deliberately not here, because an engine this system has never
/// priced and never tested is not something an environment variable should be able to
/// switch on silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEngine {
    /// Let OpenRouter choose. For a model without built-in search this means Exa, and
    /// the interface says so rather than leaving the tariff a mystery.
    Auto,
    Exa,
    Parallel,
    /// The model provider's own built-in search. Priced by that provider, so its cost is
    /// only ever known from what the response reports.
    Native,
}

impl SearchEngine {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Exa => "exa",
            Self::Parallel => "parallel",
            Self::Native => "native",
        }
    }

    fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "exa" => Ok(Self::Exa),
            "parallel" => Ok(Self::Parallel),
            "native" => Ok(Self::Native),
            other => Err(AppError::validation(format!(
                "OTDEL_RESEARCH_OPENROUTER_ENGINE must be one of `auto`, `exa`, `parallel`, \
                 `native`, got `{other}`"
            ))),
        }
    }

    /// The declared tariff of this engine: `(base_micros, included_results,
    /// extra_result_micros)`.
    ///
    /// From OpenRouter's published prices for its server tool. They are defaults the
    /// owner can override, not a promise about an invoice — the same rule as everywhere
    /// else in this phase.
    const fn default_tariff(self) -> (u64, u32, u64) {
        match self {
            // $0.007 per request including ten results, then $0.001 each. `auto` is
            // priced as Exa because that is what it resolves to for every model this
            // system is likely to use, and because over-reserving is the safe direction.
            Self::Auto | Self::Exa => (7_000, 10, 1_000),
            // $0.005 per request in Parallel's default mode; results are not metered.
            Self::Parallel => (5_000, u32::MAX, 0),
            // Passed through to the model provider, which prices it in its own tokens.
            // There is no separate per-request tariff to declare.
            Self::Native => (0, u32::MAX, 0),
        }
    }
}

/// Whether the researcher can run right now, and what is missing when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAvailability {
    /// `OTDEL_RESEARCH_PROVIDER=disabled`: switched off deliberately.
    Disabled,
    /// Named but not usable yet. `missing` lists the environment variables to set; it is
    /// shown to the owner as-is.
    NeedsConfiguration {
        missing: Vec<&'static str>,
    },
    Ready,
}

impl SearchAvailability {
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

/// Hosts the researcher is allowed to read, as declared by the owner.
///
/// An entry is either an exact host (`docs.cntd.ru`) or a suffix rule written with a
/// leading dot (`.gost.ru`, which also matches `www.gost.ru`). A bare entry never
/// matches a subdomain: `example.com` in the list does not authorise
/// `evil.example.com`, because the two are not the same publisher.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAllowlist {
    entries: Vec<String>,
}

impl HostAllowlist {
    /// Parse a comma-separated list, rejecting anything that is not a bare host.
    ///
    /// A scheme, a port, a path or a `*` wildcard is refused rather than normalised:
    /// each of them means the owner was writing something other than a host, and
    /// guessing which would be the wrong thing for a security boundary.
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        let mut entries: Vec<String> = Vec::new();
        for item in raw.split(',') {
            let value = item.trim().to_ascii_lowercase();
            if value.is_empty() {
                continue;
            }
            if value.len() > MAX_HOST_CHARS {
                return Err(AppError::validation(format!(
                    "OTDEL_RESEARCH_ALLOWED_HOSTS entry `{value}` is longer than {MAX_HOST_CHARS} characters"
                )));
            }
            if value.contains("://") || value.contains('/') || value.contains('@') {
                return Err(AppError::validation(format!(
                    "OTDEL_RESEARCH_ALLOWED_HOSTS entry `{value}` must be a bare host name \
                     (`example.com` or `.example.com`), not a URL"
                )));
            }
            if value.contains(':') {
                return Err(AppError::validation(format!(
                    "OTDEL_RESEARCH_ALLOWED_HOSTS entry `{value}` must not name a port; \
                     only https on the default port is fetched"
                )));
            }
            if value.contains('*') {
                return Err(AppError::validation(format!(
                    "OTDEL_RESEARCH_ALLOWED_HOSTS entry `{value}` must not use `*`; write \
                     `.example.com` to include subdomains"
                )));
            }
            let body = value.strip_prefix('.').unwrap_or(&value);
            if body.is_empty()
                || !body.contains('.')
                || !body
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
                || body.starts_with('-')
                || body.ends_with('.')
                || body.contains("..")
            {
                return Err(AppError::validation(format!(
                    "OTDEL_RESEARCH_ALLOWED_HOSTS entry `{value}` is not a valid host name"
                )));
            }
            if !entries.contains(&value) {
                entries.push(value);
            }
        }

        if entries.len() > MAX_ALLOWED_HOSTS {
            return Err(AppError::validation(format!(
                "OTDEL_RESEARCH_ALLOWED_HOSTS lists more than {MAX_ALLOWED_HOSTS} hosts"
            )));
        }
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The list as configured, for the interface. Not a secret.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Is this host one the owner declared?
    pub fn allows(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        self.entries
            .iter()
            .any(|entry| match entry.strip_prefix('.') {
                // `.example.com` — the domain itself and anything under it.
                Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
                // `example.com` — exactly that host.
                None => &host == entry,
            })
    }
}

/// Bounds applied to one research plan, before any request is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResearchLimits {
    /// Search requests one plan may make.
    pub max_queries_per_plan: u32,
    /// Results asked of the provider per query.
    pub max_results_per_query: u32,
    /// Pages one plan may actually download.
    pub max_sources_per_plan: u32,
    /// A response body larger than this is refused without being read to the end.
    pub max_page_bytes: u64,
    /// Characters of extracted text kept per source (the stored snapshot).
    pub max_page_chars: u32,
    /// Wall-clock budget of a single search or fetch.
    pub request_timeout: Duration,
    /// Wall-clock budget of a whole plan pass.
    pub plan_time_budget: Duration,
    /// Smallest gap between two outbound requests.
    pub min_request_interval: Duration,
    /// How many times one plan may be run at all.
    pub max_passes_per_plan: u32,
}

impl Default for ResearchLimits {
    fn default() -> Self {
        Self {
            max_queries_per_plan: 3,
            max_results_per_query: 8,
            max_sources_per_plan: 6,
            max_page_bytes: 2 * 1024 * 1024,
            max_page_chars: 40_000,
            request_timeout: Duration::from_secs(20),
            plan_time_budget: Duration::from_secs(300),
            min_request_interval: Duration::from_millis(500),
            max_passes_per_plan: 2,
        }
    }
}

/// The declared tariff and the ceilings it is charged against.
///
/// These numbers are what the *owner* says a call costs, in millionths of one currency
/// unit. They are used to reserve before a call and to settle after it, so concurrent
/// plans cannot both slip past the same ceiling. They are not an invoice: the interface
/// and the documentation both call this "по объявленному тарифу".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchCosts {
    /// ISO-4217-shaped label, for display only. No conversion is ever performed.
    pub currency: String,
    pub search_micros: u64,
    pub fetch_micros: u64,
    /// Per model request made while interpreting a plan's sources.
    pub model_call_micros: u64,
    /// Ceiling for one plan.
    pub plan_budget_micros: u64,
    /// Ceiling for the whole bureau, across every plan and every worker.
    pub bureau_budget_micros: u64,
}

impl Default for ResearchCosts {
    fn default() -> Self {
        Self {
            currency: "USD".to_owned(),
            // 0.005 of a unit per search: a plausible order of magnitude for a metered
            // search API, and a number the owner is expected to replace with the real
            // one from their contract.
            search_micros: 5_000,
            // Reading a public page costs the provider nothing; the bound that matters
            // for fetching is `max_sources_per_plan`, not money.
            fetch_micros: 0,
            // Zero by default, like the fetch: the owner sets it from their model
            // contract. What matters is that the *mechanism* exists — a model call is
            // reserved for before it is made and settled after, so it appears in the
            // ledger and counts against the same ceiling as everything else.
            model_call_micros: 0,
            plan_budget_micros: 100_000,
            bureau_budget_micros: 5_000_000,
        }
    }
}

/// Everything specific to the `openrouter:web_search` adapter.
///
/// Contains no secret. The key is resolved once, at load, into
/// [`ResearchSettings::api_key`] — either the researcher's own
/// `OTDEL_RESEARCH_API_KEY` or, when that is absent, the model key the owner already
/// configured. Inheriting it is a convenience, not a second copy: the value lives in one
/// redacting [`ApiKey`], is never serialised, never logged and never written to the
/// database, and [`Self::api_key_inherited`] is the only trace of where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterSearchSettings {
    /// Base URL of the OpenRouter-compatible API, without `/chat/completions`.
    pub base_url: String,
    /// Model that runs the tool. Its own tokens are part of the bill.
    pub model: String,
    pub engine: SearchEngine,
    /// Results asked of the tool per search call.
    pub max_results: u32,
    /// Ceiling on results one plan may accumulate across all of its searches. Bounds the
    /// spend of a plan whose every query keeps finding new links.
    pub max_total_results_per_plan: u32,
    /// Declared price of one search request, covering `included_results` results.
    pub base_micros: u64,
    pub included_results: u32,
    /// Declared price of each result beyond `included_results`.
    pub extra_result_micros: u64,
    /// Declared allowance for the model tokens one search call spends reading its own
    /// results. A *forecast*: when the response reports what it really cost, the reported
    /// number replaces this one in the ledger.
    pub token_allowance_micros: u64,
    /// `true` when the key came from `OTDEL_LLM_API_KEY`. Shown to the owner so that
    /// "which key is this spending" has a visible answer.
    pub api_key_inherited: bool,
}

impl Default for OpenRouterSearchSettings {
    fn default() -> Self {
        let (base_micros, included_results, extra_result_micros) =
            SearchEngine::Auto.default_tariff();
        Self {
            base_url: crate::llm_config::OPENROUTER_BASE_URL.to_owned(),
            model: String::new(),
            engine: SearchEngine::Auto,
            max_results: 5,
            max_total_results_per_plan: 20,
            base_micros,
            included_results,
            extra_result_micros,
            // Around five results of bounded context plus a short answer, at the price of
            // a small model. Deliberately not zero: a forecast of nothing would make the
            // reservation smaller than the call and the ledger would learn about it late.
            token_allowance_micros: 3_000,
            api_key_inherited: false,
        }
    }
}

impl OpenRouterSearchSettings {
    /// Which engine will really serve the request.
    ///
    /// `auto` is not a mystery to be reported as one: OpenRouter resolves it to the
    /// model's built-in search when the model has one, and to Exa when it does not. A
    /// model like `openai/gpt-4o-mini` has none, so `auto` means Exa, and both the
    /// interface and the tariff say Exa rather than "auto, we will see".
    pub fn effective_engine(&self) -> SearchEngine {
        match self.engine {
            SearchEngine::Auto if native_search_capable(&self.model) => SearchEngine::Native,
            SearchEngine::Auto => SearchEngine::Exa,
            other => other,
        }
    }

    /// `true` when `auto` had to fall back to Exa because the model cannot search itself.
    pub fn is_exa_fallback(&self) -> bool {
        self.engine == SearchEngine::Auto && !native_search_capable(&self.model)
    }

    /// The declared price of one search returning `results` results.
    pub fn search_cost_micros(&self, results: u32) -> u64 {
        let extra = u64::from(results.saturating_sub(self.included_results));
        self.base_micros
            .saturating_add(extra.saturating_mul(self.extra_result_micros))
    }

    /// What one search call is expected to cost in total, tokens included.
    ///
    /// This is the number reserved before the call. The number *settled* after it is what
    /// the provider reported, when it reports anything.
    pub fn forecast_micros(&self) -> u64 {
        self.search_cost_micros(self.max_results)
            .saturating_add(self.token_allowance_micros)
    }

    /// Full URL of the chat-completions endpoint that carries the tool.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Host of that endpoint, for the interface and the log. Never the key.
    pub fn endpoint_host(&self) -> Option<String> {
        let (_, rest) = self.base_url.split_once("://")?;
        let authority = rest.split('/').next()?;
        (!authority.is_empty()).then(|| authority.to_owned())
    }
}

/// Does this model carry its own web search?
///
/// Conservative and deliberately small: a model is assumed to have no built-in search
/// unless its identifier says otherwise. Guessing the other way would forecast a tariff
/// of zero for a call that is really billed as an Exa search.
fn native_search_capable(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    if model.is_empty() {
        return false;
    }
    // `:online` is OpenRouter's own "this variant searches" suffix; Perplexity's models
    // and the `…-search…` variants of the big providers are the documented cases.
    model.ends_with(":online")
        || model.starts_with("perplexity/")
        || model.contains("-search")
        || model.contains("_search")
}

/// Everything the researcher needs, with the secret kept out of `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct ResearchSettings {
    pub provider: SearchProviderKind,
    /// Full URL of the search endpoint. Empty when none is configured.
    pub search_url: String,
    pub api_key: Option<ApiKey>,
    /// Header the key travels in. `None` means `Authorization: Bearer <key>`.
    pub api_key_header: Option<String>,
    pub allowed_hosts: HostAllowlist,
    pub limits: ResearchLimits,
    pub costs: ResearchCosts,
    /// Only meaningful when `provider` is [`SearchProviderKind::OpenRouterWebSearch`].
    pub openrouter: OpenRouterSearchSettings,
}

impl Default for ResearchSettings {
    fn default() -> Self {
        Self {
            provider: SearchProviderKind::HttpJson,
            search_url: String::new(),
            api_key: None,
            api_key_header: None,
            allowed_hosts: HostAllowlist::default(),
            limits: ResearchLimits::default(),
            costs: ResearchCosts::default(),
            openrouter: OpenRouterSearchSettings::default(),
        }
    }
}

impl fmt::Debug for ResearchSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResearchSettings")
            .field("provider", &self.provider.as_str())
            .field("search_host", &self.search_host())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_key_header", &self.api_key_header)
            .field("allowed_hosts", &self.allowed_hosts.entries())
            .field("availability", &self.availability().as_str())
            .field("limits", &self.limits)
            .field("costs", &self.costs)
            .field("openrouter", &self.openrouter)
            .finish()
    }
}

impl ResearchSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let provider = match source.get("OTDEL_RESEARCH_PROVIDER") {
            Some(value) if !value.trim().is_empty() => SearchProviderKind::parse(&value)?,
            _ => SearchProviderKind::HttpJson,
        };

        let search_url = normalise_endpoint(&string_or(source, "OTDEL_RESEARCH_SEARCH_URL", ""))?;

        // An empty or placeholder key counts as "not supplied": the owner is told what
        // to set, and no request is attempted with it.
        let api_key = read_key(source, "OTDEL_RESEARCH_API_KEY")?;

        // The model key is inherited *only* for the OpenRouter adapter, and only when the
        // researcher has no key of its own. It is the same account and the same endpoint
        // the product roles already call, so asking the owner to paste the value a second
        // time would add a copy to protect without adding a boundary.
        let (api_key, api_key_inherited) = match (api_key, provider) {
            (None, SearchProviderKind::OpenRouterWebSearch) => {
                (read_key(source, "OTDEL_LLM_API_KEY")?, true)
            }
            (key, _) => (key, false),
        };

        let api_key_header = match source.get("OTDEL_RESEARCH_API_KEY_HEADER") {
            Some(value) if !value.trim().is_empty() => {
                let value = value.trim().to_owned();
                if value.eq_ignore_ascii_case("authorization") {
                    None
                } else {
                    if value.len() > 80
                        || !value
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
                    {
                        return Err(AppError::validation(
                            "OTDEL_RESEARCH_API_KEY_HEADER must be a header name made of \
                             ASCII letters, digits, `-` or `_`",
                        ));
                    }
                    Some(value)
                }
            }
            _ => None,
        };

        let allowed_hosts =
            HostAllowlist::parse(&string_or(source, "OTDEL_RESEARCH_ALLOWED_HOSTS", ""))?;

        let openrouter = load_openrouter(source, api_key_inherited)?;

        let defaults = ResearchLimits::default();
        let mut limits = ResearchLimits {
            max_queries_per_plan: bounded_u32(
                source,
                "OTDEL_RESEARCH_MAX_QUERIES_PER_PLAN",
                u64::from(defaults.max_queries_per_plan),
                MAX_QUERIES_RANGE,
            )?,
            max_results_per_query: bounded_u32(
                source,
                "OTDEL_RESEARCH_MAX_RESULTS_PER_QUERY",
                u64::from(defaults.max_results_per_query),
                MAX_RESULTS_RANGE,
            )?,
            max_sources_per_plan: bounded_u32(
                source,
                "OTDEL_RESEARCH_MAX_SOURCES_PER_PLAN",
                u64::from(defaults.max_sources_per_plan),
                MAX_SOURCES_RANGE,
            )?,
            max_page_bytes: bounded_u64(
                source,
                "OTDEL_RESEARCH_MAX_PAGE_BYTES",
                defaults.max_page_bytes,
                MAX_PAGE_BYTES_RANGE,
            )?,
            max_page_chars: bounded_u32(
                source,
                "OTDEL_RESEARCH_MAX_PAGE_CHARS",
                u64::from(defaults.max_page_chars),
                MAX_PAGE_CHARS_RANGE,
            )?,
            request_timeout: bounded_duration(
                source,
                "OTDEL_RESEARCH_REQUEST_TIMEOUT_SECONDS",
                defaults.request_timeout,
                REQUEST_TIMEOUT_RANGE,
            )?,
            plan_time_budget: bounded_duration(
                source,
                "OTDEL_RESEARCH_PLAN_TIME_BUDGET_SECONDS",
                defaults.plan_time_budget,
                PLAN_TIME_BUDGET_RANGE,
            )?,
            min_request_interval: Duration::from_millis(bounded_u64(
                source,
                "OTDEL_RESEARCH_MIN_REQUEST_INTERVAL_MS",
                u64::try_from(defaults.min_request_interval.as_millis()).unwrap_or(500),
                MIN_REQUEST_INTERVAL_RANGE,
            )?),
            max_passes_per_plan: bounded_u32(
                source,
                "OTDEL_RESEARCH_MAX_PASSES_PER_PLAN",
                u64::from(defaults.max_passes_per_plan),
                MAX_PASSES_RANGE,
            )?,
        };

        // One number, not two. The OpenRouter tool is asked for exactly as many results
        // as the plan is allowed to see, and that count is what the tariff is computed
        // from; two knobs disagreeing would make the forecast wrong in a way nobody would
        // notice until the invoice.
        if provider == SearchProviderKind::OpenRouterWebSearch {
            let generic = source
                .get("OTDEL_RESEARCH_MAX_RESULTS_PER_QUERY")
                .filter(|value| !value.trim().is_empty());
            if generic.is_some() && limits.max_results_per_query != openrouter.max_results {
                return Err(AppError::validation(
                    "OTDEL_RESEARCH_MAX_RESULTS_PER_QUERY and \
                     OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS disagree; with the OpenRouter \
                     provider set only OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS, because the \
                     search tariff is computed from it",
                ));
            }
            limits.max_results_per_query = openrouter.max_results;
        }

        let cost_defaults = ResearchCosts::default();
        let currency = string_or(source, "OTDEL_RESEARCH_CURRENCY", &cost_defaults.currency)
            .to_ascii_uppercase();
        if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
            return Err(AppError::validation(
                "OTDEL_RESEARCH_CURRENCY must be a three-letter code such as USD or RUB",
            ));
        }

        // With OpenRouter the per-search price is *derived* — engine tariff for the
        // configured result count, plus the token allowance — so a flat
        // `COST_PER_SEARCH_MICROS` would be a second, silently ignored answer to the same
        // question. Saying so is better than quietly preferring one of them.
        if provider == SearchProviderKind::OpenRouterWebSearch
            && source
                .get("OTDEL_RESEARCH_COST_PER_SEARCH_MICROS")
                .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(AppError::validation(
                "OTDEL_RESEARCH_COST_PER_SEARCH_MICROS does not apply to the OpenRouter \
                 provider: the price of a search is computed from \
                 OTDEL_RESEARCH_OPENROUTER_SEARCH_BASE_MICROS, \
                 OTDEL_RESEARCH_OPENROUTER_SEARCH_EXTRA_RESULT_MICROS and \
                 OTDEL_RESEARCH_OPENROUTER_TOKEN_ALLOWANCE_MICROS",
            ));
        }

        let costs = ResearchCosts {
            currency,
            search_micros: if provider == SearchProviderKind::OpenRouterWebSearch {
                openrouter.forecast_micros()
            } else {
                bounded_u64(
                    source,
                    "OTDEL_RESEARCH_COST_PER_SEARCH_MICROS",
                    cost_defaults.search_micros,
                    COST_RANGE,
                )?
            },
            fetch_micros: bounded_u64(
                source,
                "OTDEL_RESEARCH_COST_PER_FETCH_MICROS",
                cost_defaults.fetch_micros,
                COST_RANGE,
            )?,
            model_call_micros: bounded_u64(
                source,
                "OTDEL_RESEARCH_COST_PER_MODEL_CALL_MICROS",
                cost_defaults.model_call_micros,
                COST_RANGE,
            )?,
            plan_budget_micros: bounded_u64(
                source,
                "OTDEL_RESEARCH_PLAN_BUDGET_MICROS",
                cost_defaults.plan_budget_micros,
                BUDGET_RANGE,
            )?,
            bureau_budget_micros: bounded_u64(
                source,
                "OTDEL_RESEARCH_BUDGET_MICROS",
                cost_defaults.bureau_budget_micros,
                BUDGET_RANGE,
            )?,
        };

        if costs.plan_budget_micros > costs.bureau_budget_micros {
            return Err(AppError::validation(
                "OTDEL_RESEARCH_PLAN_BUDGET_MICROS must not exceed OTDEL_RESEARCH_BUDGET_MICROS: \
                 a single plan cannot be allowed to spend more than the whole bureau",
            ));
        }

        Ok(Self {
            provider,
            search_url,
            api_key,
            api_key_header,
            allowed_hosts,
            limits,
            costs,
            openrouter,
        })
    }

    /// What the owner is told, and what the worker checks before it queues anything.
    ///
    /// The allowlist is part of readiness on purpose: a researcher with a search key and
    /// no declared hosts could search but never read anything it found, and would spend
    /// money to produce nothing.
    pub fn availability(&self) -> SearchAvailability {
        if self.provider == SearchProviderKind::Disabled {
            return SearchAvailability::Disabled;
        }

        let mut missing: Vec<&'static str> = Vec::new();
        match self.provider {
            SearchProviderKind::Disabled => {}
            SearchProviderKind::HttpJson => {
                if self.search_url.is_empty() {
                    missing.push("OTDEL_RESEARCH_SEARCH_URL");
                }
                if self.api_key.is_none() {
                    missing.push("OTDEL_RESEARCH_API_KEY");
                }
            }
            // The OpenRouter adapter needs no endpoint of its own: it calls the
            // `/chat/completions` the product roles already use. What it does need is a
            // model to run the tool on, and a key — its own or the model's.
            SearchProviderKind::OpenRouterWebSearch => {
                if self.api_key.is_none() {
                    missing.push("OTDEL_LLM_API_KEY");
                }
                if self.openrouter.model.is_empty() {
                    missing.push("OTDEL_LLM_MODEL");
                }
                if self.openrouter.base_url.is_empty() {
                    missing.push("OTDEL_LLM_BASE_URL");
                }
            }
        }
        if self.allowed_hosts.is_empty() {
            missing.push("OTDEL_RESEARCH_ALLOWED_HOSTS");
        }

        if missing.is_empty() {
            SearchAvailability::Ready
        } else {
            SearchAvailability::NeedsConfiguration { missing }
        }
    }

    /// Host of the search endpoint, for the interface and the log. Never the key.
    pub fn search_host(&self) -> Option<String> {
        if self.provider == SearchProviderKind::OpenRouterWebSearch {
            return self.openrouter.endpoint_host();
        }
        let (_, rest) = self.search_url.split_once("://")?;
        let authority = rest.split('/').next()?;
        if authority.is_empty() {
            None
        } else {
            Some(authority.to_owned())
        }
    }
}

/// Reject anything that is not a plain `https://host[:port][/path]` endpoint.
///
/// The same rule as the model adapter's, and for the same reason: the key belongs in a
/// header, not in a URL that ends up in a log. Plain HTTP is allowed only towards the
/// loopback interface, which is the self-hosted case (a local SearxNG).
fn normalise_endpoint(raw: &str) -> Result<String, AppError> {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.chars().any(char::is_control) || value.contains(char::is_whitespace) {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_SEARCH_URL must not contain whitespace or control characters",
        ));
    }
    if value.contains('?') || value.contains('#') {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_SEARCH_URL must be a plain endpoint without a query string or \
             fragment; the query and the key are sent in the body and the header",
        ));
    }

    let (scheme, rest) = value.split_once("://").ok_or_else(|| {
        AppError::validation("OTDEL_RESEARCH_SEARCH_URL must start with https://")
    })?;
    if rest.is_empty() {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_SEARCH_URL has no host",
        ));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_SEARCH_URL must not embed credentials; the key is sent in a header",
        ));
    }

    // An IP-literal endpoint that is *not* loopback is refused. It is not an SSRF (the
    // address comes from the owner, not from a page), but hyper skips DNS entirely for a
    // literal, which means the guarded resolver — the thing that refuses internal
    // addresses — would never be consulted. Refusing here is what keeps the adapter's
    // stated invariant true: a misconfigured endpoint cannot become a way to reach an
    // internal address. A self-hosted engine on loopback stays allowed, deliberately and
    // visibly.
    if let Some(address) = literal_address(authority_host(authority)) {
        if !address.is_loopback() {
            return Err(AppError::validation(
                "OTDEL_RESEARCH_SEARCH_URL must name a host, not an IP address \
                 (only a loopback literal is allowed, for a self-hosted engine): an \
                 address bypasses the guard that refuses internal destinations",
            ));
        }
    }

    match scheme {
        "https" => Ok(value.to_owned()),
        "http" if is_loopback_authority(authority) => Ok(value.to_owned()),
        "http" => Err(AppError::validation(
            "OTDEL_RESEARCH_SEARCH_URL may only use http:// for a loopback address \
             (127.0.0.1, ::1, localhost); use https:// for anything else",
        )),
        other => Err(AppError::validation(format!(
            "OTDEL_RESEARCH_SEARCH_URL scheme `{other}` is not supported; use https://"
        ))),
    }
}

/// The host part of an authority, without its port and without IPv6 brackets.
///
/// Split from the right so that the colons of a bracketed IPv6 literal are not mistaken
/// for a port separator.
pub fn authority_host(authority: &str) -> &str {
    let host = match authority.rsplit_once(':') {
        // `[::1]:8080` — a port only follows the bracketed form or a name.
        Some((host, port))
            if !host.is_empty()
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && (host.ends_with(']') || !host.contains(':')) =>
        {
            host
        }
        _ => authority,
    };
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Read the `openrouter:web_search` settings.
///
/// The model, the endpoint and the key all default to the ones the owner already
/// configured for the product roles, because it is the same account and the same service.
/// Everything that costs money — the engine, the result counts, the tariff — has its own
/// variable with a stated default, so a spend can be predicted before it happens.
fn load_openrouter(
    source: &dyn ConfigSource,
    api_key_inherited: bool,
) -> Result<OpenRouterSearchSettings, AppError> {
    let defaults = OpenRouterSearchSettings::default();

    let inherited_base = string_or(
        source,
        "OTDEL_LLM_BASE_URL",
        crate::llm_config::OPENROUTER_BASE_URL,
    );
    let base_url = normalise_openrouter_base(&string_or(
        source,
        "OTDEL_RESEARCH_OPENROUTER_BASE_URL",
        &inherited_base,
    ))?;

    let model = string_or(
        source,
        "OTDEL_RESEARCH_OPENROUTER_MODEL",
        &string_or(source, "OTDEL_LLM_MODEL", ""),
    );
    if !model.is_empty()
        && (model
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
            || model.len() > 200)
    {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_OPENROUTER_MODEL must be a model identifier without spaces or \
             control characters, e.g. `openai/gpt-4o-mini`",
        ));
    }

    let engine = match source.get("OTDEL_RESEARCH_OPENROUTER_ENGINE") {
        Some(value) if !value.trim().is_empty() => SearchEngine::parse(&value)?,
        _ => defaults.engine,
    };

    let max_results = bounded_u32(
        source,
        "OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS",
        u64::from(defaults.max_results),
        OPENROUTER_MAX_RESULTS_RANGE,
    )?;
    let max_total_results_per_plan = bounded_u32(
        source,
        "OTDEL_RESEARCH_OPENROUTER_MAX_TOTAL_RESULTS",
        u64::from(defaults.max_total_results_per_plan),
        OPENROUTER_MAX_TOTAL_RESULTS_RANGE,
    )?;
    if max_total_results_per_plan < max_results {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_OPENROUTER_MAX_TOTAL_RESULTS must not be smaller than \
             OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS: one search would already exceed the \
             whole plan's allowance",
        ));
    }

    // The tariff defaults follow the engine the owner actually selected, so switching
    // from Exa to Parallel does not leave the forecast quoting Exa's price.
    let (base_default, included_default, extra_default) = {
        let resolved = OpenRouterSearchSettings {
            engine,
            model: model.clone(),
            ..defaults.clone()
        };
        resolved.effective_engine().default_tariff()
    };

    Ok(OpenRouterSearchSettings {
        base_url,
        model,
        engine,
        max_results,
        max_total_results_per_plan,
        base_micros: bounded_u64(
            source,
            "OTDEL_RESEARCH_OPENROUTER_SEARCH_BASE_MICROS",
            base_default,
            COST_RANGE,
        )?,
        included_results: bounded_u32(
            source,
            "OTDEL_RESEARCH_OPENROUTER_SEARCH_INCLUDED_RESULTS",
            u64::from(included_default.min(1_000)),
            (0, 1_000),
        )?,
        extra_result_micros: bounded_u64(
            source,
            "OTDEL_RESEARCH_OPENROUTER_SEARCH_EXTRA_RESULT_MICROS",
            extra_default,
            COST_RANGE,
        )?,
        token_allowance_micros: bounded_u64(
            source,
            "OTDEL_RESEARCH_OPENROUTER_TOKEN_ALLOWANCE_MICROS",
            defaults.token_allowance_micros,
            COST_RANGE,
        )?,
        api_key_inherited,
    })
}

/// The OpenRouter base URL, held to the same rule as every other endpoint.
fn normalise_openrouter_base(raw: &str) -> Result<String, AppError> {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.chars().any(char::is_control)
        || value.contains(char::is_whitespace)
        || value.contains('?')
        || value.contains('#')
    {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_OPENROUTER_BASE_URL must be a plain endpoint without whitespace, \
             a query string or a fragment",
        ));
    }
    let (scheme, rest) = value.split_once("://").ok_or_else(|| {
        AppError::validation("OTDEL_RESEARCH_OPENROUTER_BASE_URL must start with https://")
    })?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(AppError::validation(
            "OTDEL_RESEARCH_OPENROUTER_BASE_URL must name a host and must not embed \
             credentials; the key is sent in a header",
        ));
    }
    // Same reason as the search endpoint: hyper skips DNS for a literal, so the guarded
    // resolver would never see it.
    if let Some(address) = literal_address(authority_host(authority)) {
        if !address.is_loopback() {
            return Err(AppError::validation(
                "OTDEL_RESEARCH_OPENROUTER_BASE_URL must name a host, not an IP address \
                 (only a loopback literal is allowed): an address bypasses the guard that \
                 refuses internal destinations",
            ));
        }
    }
    match scheme {
        "https" => Ok(value.to_owned()),
        "http" if is_loopback_authority(authority) => Ok(value.to_owned()),
        other => Err(AppError::validation(format!(
            "OTDEL_RESEARCH_OPENROUTER_BASE_URL scheme `{other}` is not supported; use https://"
        ))),
    }
}

/// Read one API key from the configuration.
///
/// An empty or placeholder value counts as "not supplied" rather than as a key that will
/// fail on the first call: the owner is told what to set, and nothing is ever sent with
/// it. The value is wrapped immediately, so no caller ever holds a bare `String`.
fn read_key(source: &dyn ConfigSource, name: &str) -> Result<Option<ApiKey>, AppError> {
    match source.get(name) {
        Some(value) if !value.trim().is_empty() && !secret::looks_like_placeholder(&value) => {
            let value = value.trim().to_owned();
            if value.chars().any(char::is_control) {
                return Err(AppError::validation(format!(
                    "{name} must not contain control characters"
                )));
            }
            Ok(Some(ApiKey::new(value)))
        }
        _ => Ok(None),
    }
}

/// The address this host *is*, when it is written as one rather than named.
fn literal_address(host: &str) -> Option<std::net::IpAddr> {
    host.parse::<std::net::IpAddr>().ok()
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = authority_host(authority);
    host == "localhost" || literal_address(host).is_some_and(|address| address.is_loopback())
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

    fn ready_pairs() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.cntd.ru, .gost.ru"),
        ]
    }

    fn openrouter_pairs() -> Vec<(&'static str, &'static str)> {
        vec![
            ("OTDEL_RESEARCH_PROVIDER", "openrouter"),
            ("OTDEL_LLM_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_LLM_MODEL", "openai/gpt-4o-mini"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.cntd.ru"),
        ]
    }

    fn openrouter(extra: &[(&str, &str)]) -> ResearchSettings {
        let mut pairs = openrouter_pairs();
        pairs.extend(extra.iter().copied().map(|(key, value)| {
            (
                Box::leak(key.to_owned().into_boxed_str()) as &'static str,
                Box::leak(value.to_owned().into_boxed_str()) as &'static str,
            )
        }));
        ResearchSettings::load(&env(&pairs)).unwrap()
    }

    #[test]
    fn the_shipped_example_configuration_actually_loads() {
        // `.env.example` is the file an owner copies. Two of its variables are now refused
        // in combination with the shipped provider (a flat search price, a second result
        // count), so an example that had them set would fail at the first start — and the
        // only way to find that out would be to try it. This test tries it.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example");
        let text = std::fs::read_to_string(path).expect("read .env.example");

        let source: BTreeMap<String, String> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| {
                (
                    key.trim().to_owned(),
                    value.trim().trim_matches('"').to_owned(),
                )
            })
            .collect();

        assert!(
            source.contains_key("OTDEL_RESEARCH_PROVIDER"),
            "the example must name a provider"
        );
        let settings = ResearchSettings::load(&source)
            .expect("the shipped .env.example must be a configuration the loader accepts");

        // And it must ship as *not ready*: the example carries no key and no allowlist, so
        // a copied-and-run installation makes no external call until the owner decides to.
        assert!(
            !settings.availability().is_ready(),
            "the example must not describe a researcher that would start calling out"
        );
    }

    #[test]
    fn the_openrouter_adapter_needs_no_endpoint_of_its_own() {
        // It calls the same `/chat/completions` the product roles already use, so asking
        // for a separate search URL would be asking for a second copy of one answer.
        let settings = openrouter(&[]);
        assert_eq!(settings.provider, SearchProviderKind::OpenRouterWebSearch);
        assert_eq!(settings.availability(), SearchAvailability::Ready);
        assert_eq!(settings.search_host().as_deref(), Some("openrouter.ai"));
        assert!(settings.search_url.is_empty());
    }

    #[test]
    fn the_model_key_is_inherited_only_by_the_openrouter_adapter_and_never_printed() {
        let inherited = openrouter(&[]);
        assert!(inherited.api_key.is_some());
        assert!(
            inherited.openrouter.api_key_inherited,
            "the owner must be able to see which key is being spent"
        );
        let rendered = format!("{inherited:?}");
        assert!(!rendered.contains("sk-or-v1-"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");

        // Its own key wins, and then nothing is inherited.
        let own = openrouter(&[("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef")]);
        assert!(!own.openrouter.api_key_inherited);

        // The generic adapter never inherits: a search endpoint the owner configured is a
        // different service from the model, and a model key sent to it would be a leak.
        let generic = ResearchSettings::load(&env(&[
            ("OTDEL_RESEARCH_SEARCH_URL", "https://search.example.com/v1"),
            ("OTDEL_LLM_API_KEY", "sk-or-v1-0123456789abcdef"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.cntd.ru"),
        ]))
        .unwrap();
        assert!(generic.api_key.is_none());
        assert_eq!(
            generic.availability(),
            SearchAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_RESEARCH_API_KEY"],
            }
        );
    }

    #[test]
    fn an_openrouter_researcher_without_a_key_or_a_model_says_which_variable_is_missing() {
        let settings = ResearchSettings::load(&env(&[
            ("OTDEL_RESEARCH_PROVIDER", "openrouter"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.cntd.ru"),
        ]))
        .unwrap();
        assert_eq!(
            settings.availability(),
            SearchAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_LLM_API_KEY", "OTDEL_LLM_MODEL"],
            }
        );
    }

    #[test]
    fn auto_resolves_to_exa_for_a_model_that_cannot_search_itself() {
        // The case the owner actually runs: `openai/gpt-4o-mini` has no built-in search,
        // so `auto` is Exa, and both the label and the tariff have to say Exa rather than
        // leaving the price to be discovered on an invoice.
        let mini = openrouter(&[]);
        assert_eq!(mini.openrouter.engine, SearchEngine::Auto);
        assert_eq!(mini.openrouter.effective_engine(), SearchEngine::Exa);
        assert!(mini.openrouter.is_exa_fallback());
        assert_eq!(mini.openrouter.base_micros, 7_000);

        for model in [
            "perplexity/sonar",
            "openai/gpt-4o-search-preview",
            "x/y:online",
        ] {
            let native = openrouter(&[("OTDEL_LLM_MODEL", model)]);
            assert_eq!(
                native.openrouter.effective_engine(),
                SearchEngine::Native,
                "{model} carries its own search"
            );
            assert!(!native.openrouter.is_exa_fallback());
        }
    }

    #[test]
    fn the_engine_is_an_allowlist_not_a_free_string() {
        for engine in ["exa", "parallel", "native", "auto", "EXA"] {
            assert!(
                ResearchSettings::load(&env(&{
                    let mut pairs = openrouter_pairs();
                    pairs.push(("OTDEL_RESEARCH_OPENROUTER_ENGINE", engine));
                    pairs
                }))
                .is_ok(),
                "{engine} is supported"
            );
        }
        // Engines this system has never priced and never tested must not be reachable by
        // typing their name into an environment variable.
        for engine in ["perplexity", "firecrawl", "google", ""] {
            let result = ResearchSettings::load(&env(&{
                let mut pairs = openrouter_pairs();
                pairs.push(("OTDEL_RESEARCH_OPENROUTER_ENGINE", engine));
                pairs
            }));
            if engine.is_empty() {
                // Empty means "not set", which is `auto`.
                assert!(result.is_ok());
            } else {
                assert!(result.is_err(), "{engine} must be refused");
            }
        }
    }

    #[test]
    fn the_exa_tariff_is_per_request_with_ten_results_included() {
        let settings = openrouter(&[("OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS", "12")]);
        let openrouter = &settings.openrouter;

        // $0.007 covers up to ten; each further result is $0.001.
        assert_eq!(openrouter.search_cost_micros(1), 7_000);
        assert_eq!(openrouter.search_cost_micros(10), 7_000);
        assert_eq!(openrouter.search_cost_micros(12), 9_000);
        // The forecast adds the model tokens the call spends reading its own results.
        assert_eq!(openrouter.forecast_micros(), 9_000 + 3_000);
        assert_eq!(
            settings.costs.search_micros,
            openrouter.forecast_micros(),
            "the ledger reserves the forecast, not a flat number from elsewhere"
        );
    }

    #[test]
    fn an_engine_without_a_per_result_price_is_not_charged_one() {
        let parallel = openrouter(&[("OTDEL_RESEARCH_OPENROUTER_ENGINE", "parallel")]);
        assert_eq!(parallel.openrouter.base_micros, 5_000);
        assert_eq!(parallel.openrouter.search_cost_micros(25), 5_000);

        // Native search is billed by the model provider in its own tokens; there is no
        // separate per-request tariff to declare, and inventing one would be fiction.
        let native = openrouter(&[("OTDEL_RESEARCH_OPENROUTER_ENGINE", "native")]);
        assert_eq!(native.openrouter.base_micros, 0);
        assert_eq!(native.openrouter.forecast_micros(), 3_000);
    }

    #[test]
    fn two_variables_may_not_answer_the_same_question() {
        // A flat per-search price alongside a computed one would leave the forecast
        // depending on which line the reader happened to believe.
        let mut pairs = openrouter_pairs();
        pairs.push(("OTDEL_RESEARCH_COST_PER_SEARCH_MICROS", "5000"));
        let error = ResearchSettings::load(&env(&pairs)).unwrap_err();
        assert!(error.to_string().contains("OTDEL_RESEARCH_OPENROUTER"));

        let mut pairs = openrouter_pairs();
        pairs.push(("OTDEL_RESEARCH_MAX_RESULTS_PER_QUERY", "8"));
        assert!(ResearchSettings::load(&env(&pairs)).is_err());

        // Agreeing is fine — there is nothing to be confused about.
        let mut pairs = openrouter_pairs();
        pairs.push(("OTDEL_RESEARCH_MAX_RESULTS_PER_QUERY", "5"));
        assert!(ResearchSettings::load(&env(&pairs)).is_ok());
    }

    #[test]
    fn the_result_count_defaults_to_five_and_is_bounded_per_plan() {
        let settings = openrouter(&[]);
        assert_eq!(settings.openrouter.max_results, 5);
        assert_eq!(settings.limits.max_results_per_query, 5);
        assert_eq!(settings.openrouter.max_total_results_per_plan, 20);

        // A plan allowance smaller than a single search is not a bound, it is a deadlock.
        let mut pairs = openrouter_pairs();
        pairs.push(("OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS", "10"));
        pairs.push(("OTDEL_RESEARCH_OPENROUTER_MAX_TOTAL_RESULTS", "5"));
        assert!(ResearchSettings::load(&env(&pairs)).is_err());

        // OpenRouter clamps the tool at 25; asking for more is a typo, not a wish.
        let mut pairs = openrouter_pairs();
        pairs.push(("OTDEL_RESEARCH_OPENROUTER_MAX_RESULTS", "40"));
        assert!(ResearchSettings::load(&env(&pairs)).is_err());
    }

    #[test]
    fn the_openrouter_endpoint_obeys_the_same_url_rules_as_every_other() {
        for base in [
            "http://openrouter.ai/api/v1",
            "https://203.0.113.10/api/v1",
            "https://user:pass@openrouter.ai/api/v1",
            "https://openrouter.ai/api/v1?key=secret",
        ] {
            let mut pairs = openrouter_pairs();
            pairs.push((
                "OTDEL_RESEARCH_OPENROUTER_BASE_URL",
                Box::leak(base.to_owned().into_boxed_str()),
            ));
            assert!(
                ResearchSettings::load(&env(&pairs)).is_err(),
                "{base} must be refused"
            );
        }

        // A self-hosted gateway on loopback stays possible, deliberately and visibly.
        let local = openrouter(&[(
            "OTDEL_RESEARCH_OPENROUTER_BASE_URL",
            "http://127.0.0.1:8080/v1",
        )]);
        assert_eq!(
            local.openrouter.chat_completions_url(),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn without_a_provider_the_researcher_reports_all_three_missing_variables() {
        let settings = ResearchSettings::load(&env(&[])).unwrap();
        assert_eq!(settings.provider, SearchProviderKind::HttpJson);
        assert_eq!(
            settings.availability(),
            SearchAvailability::NeedsConfiguration {
                missing: vec![
                    "OTDEL_RESEARCH_SEARCH_URL",
                    "OTDEL_RESEARCH_API_KEY",
                    "OTDEL_RESEARCH_ALLOWED_HOSTS",
                ],
            }
        );
    }

    #[test]
    fn a_key_and_an_endpoint_without_an_allowlist_are_not_ready() {
        // The dangerous half-configuration: able to search, with nothing it may read.
        // Treating it as ready would mean money spent on results nobody can open.
        let settings = ResearchSettings::load(&env(&[
            ("OTDEL_RESEARCH_SEARCH_URL", "https://search.example.com/v1"),
            ("OTDEL_RESEARCH_API_KEY", "srch-0123456789abcdef"),
        ]))
        .unwrap();
        assert_eq!(
            settings.availability(),
            SearchAvailability::NeedsConfiguration {
                missing: vec!["OTDEL_RESEARCH_ALLOWED_HOSTS"],
            }
        );
    }

    #[test]
    fn a_complete_configuration_is_ready() {
        let settings = ResearchSettings::load(&env(&ready_pairs())).unwrap();
        assert_eq!(settings.availability(), SearchAvailability::Ready);
        assert_eq!(
            settings.search_host().as_deref(),
            Some("search.example.com")
        );
        assert_eq!(settings.allowed_hosts.len(), 2);
    }

    #[test]
    fn a_placeholder_key_is_not_a_key() {
        for placeholder in ["", "   ", "changeme", "your-api-key-here"] {
            let mut pairs = ready_pairs();
            pairs[1] = ("OTDEL_RESEARCH_API_KEY", placeholder);
            let settings = ResearchSettings::load(&env(&pairs)).unwrap();
            assert!(
                !settings.availability().is_ready(),
                "`{placeholder}` must not count as a configured key"
            );
        }
    }

    #[test]
    fn disabled_is_its_own_state() {
        let mut pairs = ready_pairs();
        pairs.push(("OTDEL_RESEARCH_PROVIDER", "disabled"));
        let settings = ResearchSettings::load(&env(&pairs)).unwrap();
        assert_eq!(settings.availability(), SearchAvailability::Disabled);
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        let mut pairs = ready_pairs();
        pairs[1] = ("OTDEL_RESEARCH_API_KEY", "srch-supersecretvalue");
        let settings = ResearchSettings::load(&env(&pairs)).unwrap();

        let rendered = format!("{settings:?}");
        assert!(!rendered.contains("supersecret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // The host is not a secret and the owner needs to see which service is called.
        assert!(rendered.contains("search.example.com"), "{rendered}");
    }

    #[test]
    fn insecure_and_malformed_endpoints_are_refused() {
        for url in [
            "http://search.example.com/v1",
            "ftp://example.com",
            "search.example.com/v1",
            "https://user:pass@example.com/v1",
            "https://example.com/v1?key=abc",
            "https://exa mple.com/v1",
        ] {
            let error =
                ResearchSettings::load(&env(&[("OTDEL_RESEARCH_SEARCH_URL", url)])).unwrap_err();
            assert!(
                error.message.contains("OTDEL_RESEARCH_SEARCH_URL"),
                "`{url}` must be refused, got: {}",
                error.message
            );
        }

        // A self-hosted engine on loopback is the one exception — for every spelling.
        for url in [
            "http://127.0.0.1:8888/search",
            "http://localhost:8888/search",
            "http://[::1]:8888/search",
            "https://127.0.0.1/search",
        ] {
            let settings =
                ResearchSettings::load(&env(&[("OTDEL_RESEARCH_SEARCH_URL", url)])).unwrap();
            assert_eq!(settings.search_url, url, "`{url}` is a self-hosted engine");
        }
    }

    #[test]
    fn a_search_endpoint_written_as_a_non_loopback_address_is_refused() {
        // hyper resolves nothing for an IP literal, so the guarded resolver — the thing
        // that refuses internal destinations — would never be consulted for one.
        // Refusing the configuration is what keeps that guarantee true.
        for url in [
            "https://169.254.169.254/search",
            "https://192.168.1.10/v1/search",
            "https://10.0.0.5:443/search",
            "https://[fd00::1]/search",
            "https://[2001:db8::1]/search",
        ] {
            let error =
                ResearchSettings::load(&env(&[("OTDEL_RESEARCH_SEARCH_URL", url)])).unwrap_err();
            assert!(
                error.message.contains("not an IP address"),
                "`{url}` must be refused with a reason about addresses, got: {}",
                error.message
            );
        }

        // A perfectly ordinary named endpoint is unaffected.
        assert!(ResearchSettings::load(&env(&[(
            "OTDEL_RESEARCH_SEARCH_URL",
            "https://search.example.com/v1/search",
        )]))
        .is_ok());
    }

    #[test]
    fn the_allowlist_matches_exactly_what_was_declared() {
        let list = HostAllowlist::parse("docs.cntd.ru, .gost.ru").unwrap();

        assert!(list.allows("docs.cntd.ru"));
        assert!(list.allows("DOCS.CNTD.RU"));
        // A trailing root dot is the same host.
        assert!(list.allows("docs.cntd.ru."));
        assert!(list.allows("gost.ru"));
        assert!(list.allows("www.gost.ru"));

        // A bare entry never authorises a subdomain: `evil.docs.cntd.ru` is not the
        // publisher the owner named.
        assert!(!list.allows("evil.docs.cntd.ru"));
        // Nor does a suffix rule authorise a host that merely ends with the same text.
        assert!(!list.allows("notgost.ru"));
        assert!(!list.allows("gost.ru.evil.com"));
        assert!(!list.allows("cntd.ru"));
        assert!(!list.allows(""));
        assert!(!list.allows("127.0.0.1"));
    }

    #[test]
    fn allowlist_entries_that_are_not_bare_hosts_are_refused() {
        for entry in [
            "https://example.com",
            "example.com/path",
            "example.com:8443",
            "*.example.com",
            "user@example.com",
            "localhost",
            "-bad.example.com",
            "example..com",
        ] {
            assert!(
                HostAllowlist::parse(entry).is_err(),
                "`{entry}` must be refused as an allowlist entry"
            );
        }
    }

    #[test]
    fn limits_and_costs_are_bounded_and_defaulted() {
        let settings = ResearchSettings::load(&env(&[])).unwrap();
        assert_eq!(settings.limits, ResearchLimits::default());
        assert_eq!(settings.costs, ResearchCosts::default());

        assert!(
            ResearchSettings::load(&env(&[("OTDEL_RESEARCH_MAX_QUERIES_PER_PLAN", "0")])).is_err()
        );
        assert!(ResearchSettings::load(&env(&[("OTDEL_RESEARCH_MAX_PAGE_BYTES", "10")])).is_err());
        assert!(
            ResearchSettings::load(&env(&[("OTDEL_RESEARCH_PLAN_TIME_BUDGET_SECONDS", "5")]))
                .is_err()
        );
        assert!(ResearchSettings::load(&env(&[("OTDEL_RESEARCH_CURRENCY", "dollars")])).is_err());

        let tightened = ResearchSettings::load(&env(&[
            ("OTDEL_RESEARCH_MAX_QUERIES_PER_PLAN", "1"),
            ("OTDEL_RESEARCH_MAX_SOURCES_PER_PLAN", "2"),
            ("OTDEL_RESEARCH_CURRENCY", "rub"),
        ]))
        .unwrap();
        assert_eq!(tightened.limits.max_queries_per_plan, 1);
        assert_eq!(tightened.limits.max_sources_per_plan, 2);
        assert_eq!(tightened.costs.currency, "RUB");
    }

    #[test]
    fn a_plan_may_not_be_allowed_more_than_the_whole_bureau() {
        let error = ResearchSettings::load(&env(&[
            ("OTDEL_RESEARCH_PLAN_BUDGET_MICROS", "2000000"),
            ("OTDEL_RESEARCH_BUDGET_MICROS", "1000000"),
        ]))
        .unwrap_err();
        assert!(error.message.contains("OTDEL_RESEARCH_PLAN_BUDGET_MICROS"));
    }

    #[test]
    fn a_custom_key_header_is_accepted_and_authorization_is_the_default() {
        let mut pairs = ready_pairs();
        pairs.push(("OTDEL_RESEARCH_API_KEY_HEADER", "X-Subscription-Token"));
        let custom = ResearchSettings::load(&env(&pairs)).unwrap();
        assert_eq!(
            custom.api_key_header.as_deref(),
            Some("X-Subscription-Token")
        );

        let mut pairs = ready_pairs();
        pairs.push(("OTDEL_RESEARCH_API_KEY_HEADER", "Authorization"));
        let default = ResearchSettings::load(&env(&pairs)).unwrap();
        assert_eq!(
            default.api_key_header, None,
            "the default needs no header name"
        );

        let mut pairs = ready_pairs();
        pairs.push(("OTDEL_RESEARCH_API_KEY_HEADER", "X Token: value"));
        assert!(ResearchSettings::load(&env(&pairs)).is_err());
    }
}
