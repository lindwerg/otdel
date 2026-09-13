//! The document fetcher: the one place in OTDEL that opens a socket towards an address
//! somebody else chose.
//!
//! Every guard it has, and the attack each one closes:
//!
//! | Guard | What it stops |
//! |---|---|
//! | [`NormalisedUrl`] at the boundary | `file://`, `data:`, a port, an IP literal, a control character |
//! | the declared host allowlist | reading a publisher the owner never approved |
//! | [`GuardedResolver`] | an allowed name resolving to `10.0.0.5` or `169.254.169.254`, including the rebinding race |
//! | `redirect::Policy::none()` | a 302 from an allowed host to an internal one |
//! | `https_only` | a downgrade to plain HTTP |
//! | `robots.txt`, checked first and failing closed | reading a site that asked not to be read |
//! | a content-type allowlist | parsing an archive or an executable as if it were prose |
//! | a byte counter around the streamed body | a response that never ends |
//! | a character bound on the extracted text | one page filling the database |
//!
//! What comes back is text, a SHA-256 of the exact bytes, and the moment it was read.
//! Those three are the snapshot a finding's quotation is checked against, and the reason
//! a citation here can be as specific as a citation into a partner's own PDF.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use otdel_core::research_config::{HostAllowlist, ResearchLimits, ResearchSettings};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::guard::{GuardedResolver, HostPolicy};
use crate::html;
use crate::provider::{
    AdapterDescription, BodyReadError, DocumentFetcher, FetchRefusal, FetchedDocument,
};
use crate::robots::{Robots, MAX_ROBOTS_BYTES};
use crate::url::NormalisedUrl;

/// Content types this phase reads. A PDF found on the web is a real source, but reading
/// one needs the 1B pipeline and a storage decision; recording it as `skipped_type` is
/// the honest placeholder, not a silent failure.
const READABLE_TYPES: [&str; 4] = [
    "text/html",
    "text/plain",
    "application/xhtml+xml",
    "application/xml",
];

/// Hosts whose `robots.txt` is remembered for the life of one fetcher.
const MAX_CACHED_ROBOTS: usize = 200;

/// HTTPS fetcher bounded by an explicit allowlist.
pub struct HttpsFetcher {
    client: reqwest::Client,
    allowed_hosts: HostAllowlist,
    limits: ResearchLimits,
    description: AdapterDescription,
    robots: Mutex<HashMap<String, Robots>>,
    last_request: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for HttpsFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsFetcher")
            .field("allowed_hosts", &self.allowed_hosts.entries())
            .field("max_page_bytes", &self.limits.max_page_bytes)
            .finish_non_exhaustive()
    }
}

impl HttpsFetcher {
    /// Build the client, or refuse when there is nothing it would be allowed to read.
    pub fn new(settings: &ResearchSettings) -> Result<Self, FetchRefusal> {
        if settings.allowed_hosts.is_empty() {
            return Err(FetchRefusal::NotConfigured(
                "не задан список разрешённых источников (OTDEL_RESEARCH_ALLOWED_HOSTS)".to_owned(),
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(settings.limits.request_timeout)
            .connect_timeout(Duration::from_secs(10))
            // Never follow a redirect. An allowed host answering `302 Location:
            // http://169.254.169.254/` is the shortest path past every check above.
            .redirect(reqwest::redirect::Policy::none())
            // Belt to the URL parser's braces: even a redirect that somehow survived
            // could not downgrade the connection.
            .https_only(true)
            .user_agent(crate::USER_AGENT)
            // The connector's only source of addresses.
            .dns_resolver(GuardedResolver::shared(
                HostPolicy::Declared(settings.allowed_hosts.clone()),
                false,
            ))
            .build()
            .map_err(|error| {
                warn!(error = %error, "could not build the document HTTP client");
                FetchRefusal::Transport
            })?;

        Ok(Self {
            client,
            allowed_hosts: settings.allowed_hosts.clone(),
            limits: settings.limits,
            description: AdapterDescription {
                provider: settings.provider.as_str().to_owned(),
                endpoint_host: None,
                state: "ready",
                missing: Vec::new(),
                allowed_hosts: settings.allowed_hosts.entries().to_vec(),
                message: format!(
                    "Источники читаются только с объявленных хостов ({}).",
                    settings.allowed_hosts.entries().join(", ")
                ),
            },
            robots: Mutex::new(HashMap::new()),
            last_request: Mutex::new(None),
        })
    }

    async fn pace(&self) {
        if self.limits.min_request_interval.is_zero() {
            return;
        }
        let mut last = self.last_request.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < self.limits.min_request_interval {
                tokio::time::sleep(self.limits.min_request_interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    /// The host's crawling rules, read once per host and remembered.
    ///
    /// Both failure directions fail closed, and they are *different* failures:
    ///
    /// * the site answered and said no → [`FetchRefusal::RobotsDisallowed`], permanent,
    ///   and remembered for this host;
    /// * the site could not be asked → [`FetchRefusal::RobotsUnavailable`], **retryable**,
    ///   and **not** remembered. A three-second blip must not mark a publisher forbidden
    ///   for the rest of the process's life, and the message must not claim the site
    ///   refused permission when nobody managed to ask it.
    async fn robots_for(&self, url: &NormalisedUrl) -> Result<Robots, FetchRefusal> {
        if let Some(known) = self.robots.lock().await.get(url.host()).cloned() {
            return Ok(known);
        }

        let Some(robots) = self.load_robots(url).await else {
            return Err(FetchRefusal::RobotsUnavailable);
        };

        let mut cache = self.robots.lock().await;
        if cache.len() < MAX_CACHED_ROBOTS {
            cache.insert(url.host().to_owned(), robots.clone());
        }
        Ok(robots)
    }

    /// The host's rules, or `None` when they could not be established at all.
    async fn load_robots(&self, url: &NormalisedUrl) -> Option<Robots> {
        self.pace().await;
        let target = url.robots_url();

        let response = match self.client.get(target.as_str()).send().await {
            Ok(response) => response,
            Err(error) => {
                debug!(host = url.host(), error = %error, "robots.txt could not be requested");
                return None;
            }
        };

        let status = response.status().as_u16();
        // "There is no robots.txt" is the only answer that means "read what you like".
        if status == 404 || status == 410 {
            return Some(Robots::permissive());
        }
        if !(200..300).contains(&status) {
            // 5xx, 403, a redirect — the site did not give us its rules. That is not the
            // same as the site saying no, and it is worth trying again later.
            debug!(host = url.host(), status, "robots.txt was not served");
            return None;
        }

        match read_bounded(response, MAX_ROBOTS_BYTES as u64).await {
            Ok(bytes) => Some(Robots::parse(
                &String::from_utf8_lossy(&bytes),
                crate::ROBOTS_TOKEN,
            )),
            Err(_) => None,
        }
    }
}

#[async_trait]
impl DocumentFetcher for HttpsFetcher {
    fn describe(&self) -> AdapterDescription {
        self.description.clone()
    }

    async fn fetch(&self, url: &NormalisedUrl) -> Result<FetchedDocument, FetchRefusal> {
        // 1. The owner's list decides, before anything else happens.
        if !self.allowed_hosts.allows(url.host()) {
            return Err(FetchRefusal::HostNotAllowed(url.host().to_owned()));
        }

        // 2. The site's own rules decide next, and an unreadable robots.txt is a "no".
        if !self.robots_for(url).await?.allows(url.path_and_query()) {
            return Err(FetchRefusal::RobotsDisallowed);
        }

        self.pace().await;
        let started = Instant::now();
        let retrieved_at = Utc::now();

        let response = self
            .client
            .get(url.as_str())
            .header(
                reqwest::header::ACCEPT,
                "text/html, application/xhtml+xml, text/plain;q=0.9, */*;q=0.1",
            )
            .send()
            .await
            .map_err(|error| {
                debug!(url = %url, error = %error, "document request failed");
                if error.is_connect() {
                    FetchRefusal::Transport
                } else if error.is_timeout() {
                    FetchRefusal::Timeout
                } else {
                    FetchRefusal::Transport
                }
            })?;

        let status = response.status();
        if status.is_redirection() {
            return Err(FetchRefusal::Redirected {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            let code = status.as_u16();
            return Err(FetchRefusal::Http {
                status: code,
                retryable: code == 429 || (500..600).contains(&code),
            });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_ascii_lowercase());
        let media_type = content_type
            .as_deref()
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            // No content type at all: treated as HTML, which is what a web server
            // without one is almost always serving. The extraction is markup-tolerant.
            .unwrap_or_else(|| "text/html".to_owned());

        if !READABLE_TYPES.contains(&media_type.as_str()) {
            return Err(FetchRefusal::UnsupportedType(media_type));
        }

        // A declared length over the limit is refused before a byte of body is read.
        if let Some(length) = response.content_length() {
            if length > self.limits.max_page_bytes {
                return Err(FetchRefusal::TooLarge {
                    limit: self.limits.max_page_bytes,
                });
            }
        }

        let bytes = read_bounded(response, self.limits.max_page_bytes).await?;
        let content_hash = hex::encode(Sha256::digest(&bytes));
        let bytes_len = bytes.len() as u64;

        // Decoded as UTF-8 with replacement. A page in another encoding comes out partly
        // mangled — and a quotation then simply fails to match, which is the safe
        // direction: no finding is stored on text nobody can reproduce.
        let body = String::from_utf8_lossy(&bytes);
        let max_chars = self.limits.max_page_chars as usize;
        let extracted = if media_type == "text/plain" {
            html::extract_plain(&body, max_chars)
        } else {
            html::extract(&body, max_chars)
        };

        if extracted.text.trim().is_empty() {
            return Err(FetchRefusal::Empty);
        }

        let duration = started.elapsed();
        debug!(
            url = %url,
            bytes = bytes_len,
            chars = extracted.text.chars().count(),
            duration_ms = duration.as_millis() as u64,
            "document read"
        );

        Ok(FetchedDocument {
            url: url.clone(),
            http_status: status.as_u16(),
            content_type,
            bytes_len,
            content_hash,
            text: extracted.text,
            title: extracted.title,
            license: extracted.license,
            license_note: extracted.license_note,
            published_at: extracted.published_at,
            retrieved_at,
            duration,
            truncated: extracted.truncated,
        })
    }
}

/// Read a body, stopping the moment it exceeds `limit`, in this module's vocabulary.
async fn read_bounded(response: reqwest::Response, limit: u64) -> Result<Vec<u8>, FetchRefusal> {
    crate::provider::read_bounded(response, limit)
        .await
        .map_err(|error| match error {
            BodyReadError::TooLarge => FetchRefusal::TooLarge { limit },
            BodyReadError::Timeout => FetchRefusal::Timeout,
            BodyReadError::Transport => FetchRefusal::Transport,
        })
}

/// The fetcher of an installation with no allowlist: no client, no socket, one honest
/// refusal.
pub struct UnconfiguredFetcher {
    description: AdapterDescription,
    reason: String,
}

impl UnconfiguredFetcher {
    pub fn new(settings: &ResearchSettings) -> Self {
        let availability = settings.availability();
        let missing = match &availability {
            otdel_core::research_config::SearchAvailability::NeedsConfiguration { missing } => {
                missing.clone()
            }
            _ => Vec::new(),
        };
        let message = if missing.is_empty() {
            "Чтение внешних источников выключено настройкой OTDEL_RESEARCH_PROVIDER=disabled."
                .to_owned()
        } else {
            format!(
                "Чтение внешних источников не настроено: задайте {}. Ни одна страница не \
                 загружается.",
                missing.join(", ")
            )
        };

        Self {
            description: AdapterDescription {
                provider: settings.provider.as_str().to_owned(),
                endpoint_host: None,
                state: availability.as_str(),
                missing,
                allowed_hosts: settings.allowed_hosts.entries().to_vec(),
                message: message.clone(),
            },
            reason: message,
        }
    }
}

#[async_trait]
impl DocumentFetcher for UnconfiguredFetcher {
    fn describe(&self) -> AdapterDescription {
        self.description.clone()
    }

    async fn fetch(&self, _url: &NormalisedUrl) -> Result<FetchedDocument, FetchRefusal> {
        Err(FetchRefusal::NotConfigured(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(extra: &[(&str, &str)]) -> ResearchSettings {
        let mut source: BTreeMap<String, String> = [
            (
                "OTDEL_RESEARCH_SEARCH_URL",
                "https://search.example.com/v1/search",
            ),
            ("OTDEL_RESEARCH_API_KEY", "srch-testkeyvalue0123"),
            ("OTDEL_RESEARCH_ALLOWED_HOSTS", "docs.example.org"),
        ]
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
        for (key, value) in extra {
            source.insert((*key).to_owned(), (*value).to_owned());
        }
        ResearchSettings::load(&source).unwrap()
    }

    #[test]
    fn without_an_allowlist_no_client_is_built() {
        let source: BTreeMap<String, String> = BTreeMap::new();
        let error = HttpsFetcher::new(&ResearchSettings::load(&source).unwrap()).unwrap_err();
        assert!(matches!(error, FetchRefusal::NotConfigured(_)));
        assert!(error.to_string().contains("OTDEL_RESEARCH_ALLOWED_HOSTS"));
    }

    #[tokio::test]
    async fn a_host_outside_the_allowlist_is_refused_before_any_socket_is_opened() {
        let fetcher = HttpsFetcher::new(&settings(&[])).unwrap();
        // A real host that certainly exists — the point is that nothing is contacted.
        let url = NormalisedUrl::parse("https://example.com/whatever").unwrap();

        let refusal = fetcher.fetch(&url).await.unwrap_err();
        assert_eq!(
            refusal,
            FetchRefusal::HostNotAllowed("example.com".to_owned())
        );
        assert!(!refusal.was_sent(), "a refused host costs nothing");
        assert!(!refusal.is_retryable());
    }

    #[tokio::test]
    async fn the_unconfigured_fetcher_reads_nothing_and_says_what_is_missing() {
        let source: BTreeMap<String, String> = BTreeMap::new();
        let fetcher = UnconfiguredFetcher::new(&ResearchSettings::load(&source).unwrap());
        let description = fetcher.describe();
        assert_eq!(description.state, "needs_configuration");
        assert!(description.allowed_hosts.is_empty());

        let url = NormalisedUrl::parse("https://docs.example.org/gost").unwrap();
        let refusal = fetcher.fetch(&url).await.unwrap_err();
        assert!(matches!(refusal, FetchRefusal::NotConfigured(_)));
        assert!(!refusal.was_sent());
    }

    #[test]
    fn the_description_lists_the_declared_hosts_and_no_secret() {
        let fetcher = HttpsFetcher::new(&settings(&[(
            "OTDEL_RESEARCH_ALLOWED_HOSTS",
            "docs.example.org,.gost.ru",
        )]))
        .unwrap();
        let description = fetcher.describe();
        assert_eq!(
            description.allowed_hosts,
            vec!["docs.example.org".to_owned(), ".gost.ru".to_owned()]
        );
        let rendered = format!("{fetcher:?}");
        assert!(!rendered.contains("srch-"), "{rendered}");
    }

    #[test]
    fn only_readable_document_types_are_parsed() {
        for readable in ["text/html", "text/plain", "application/xhtml+xml"] {
            assert!(READABLE_TYPES.contains(&readable));
        }
        for unreadable in [
            "application/pdf",
            "application/zip",
            "image/png",
            "application/octet-stream",
        ] {
            assert!(
                !READABLE_TYPES.contains(&unreadable),
                "{unreadable} must be recorded as skipped, not parsed as prose"
            );
        }
    }
}
