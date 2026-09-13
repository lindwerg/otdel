//! Scripted adapters for tests. Behind the `fake` feature, off by default.
//!
//! The whole of phase 1D can be exercised with these: query building, the budget
//! ledger, the source journal, quotation checking, the queue and the interface — all
//! without a search key, without an allowlist of real publishers, and without a single
//! packet leaving the machine.
//!
//! They are also how the *interesting* cases are written down, because those are exactly
//! the ones a real provider will not produce on demand: a result whose host is not
//! allowed, a site whose `robots.txt` says no, a page that redirects, a timeout whose
//! outcome nobody knows, and a page that tries to give the model instructions.
//!
//! `build_search_provider` never returns one of these in a production binary: the
//! feature is enabled only by dev-dependencies.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use otdel_core::research_config::HostAllowlist;
use sha2::{Digest, Sha256};

use crate::html;
use crate::provider::{
    AdapterDescription, DocumentFetcher, FetchRefusal, FetchedDocument, SearchAnswer, SearchError,
    SearchHit, SearchProvider, SearchRequest,
};
use crate::url::NormalisedUrl;

/// What the fake search does on the next call.
#[derive(Debug)]
pub enum FakeSearchReply {
    Hits(Vec<SearchHit>),
    Fail(SearchError),
}

impl FakeSearchReply {
    /// The common case: a list of URLs, with no titles or snippets.
    pub fn urls(urls: &[&str]) -> Self {
        Self::Hits(
            urls.iter()
                .map(|url| SearchHit {
                    url: (*url).to_owned(),
                    title: None,
                    snippet: None,
                })
                .collect(),
        )
    }
}

/// Records the queries it was given and answers from a script.
pub struct FakeSearchProvider {
    replies: Mutex<Vec<FakeSearchReply>>,
    queries: Mutex<Vec<String>>,
}

impl std::fmt::Debug for FakeSearchProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeSearchProvider").finish_non_exhaustive()
    }
}

impl FakeSearchProvider {
    /// Replies are consumed in order; running out is itself a failure, so a test that
    /// expects two searches and gets three finds out.
    pub fn new(replies: Vec<FakeSearchReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            queries: Mutex::new(Vec::new()),
        }
    }

    pub fn answering(urls: &[&str]) -> Self {
        Self::new(vec![FakeSearchReply::urls(urls)])
    }

    pub fn failing(error: SearchError) -> Self {
        Self::new(vec![FakeSearchReply::Fail(error)])
    }

    /// Every query this provider was given, in order — so a test can assert what did and
    /// did not leave the machine.
    pub fn queries(&self) -> Vec<String> {
        self.queries.lock().expect("fake search lock").clone()
    }

    pub fn call_count(&self) -> usize {
        self.queries.lock().expect("fake search lock").len()
    }
}

#[async_trait]
impl SearchProvider for FakeSearchProvider {
    fn describe(&self) -> AdapterDescription {
        AdapterDescription {
            provider: "fake".to_owned(),
            endpoint_host: None,
            state: "ready",
            missing: Vec::new(),
            allowed_hosts: Vec::new(),
            message: "Тестовый поиск: сетевые вызовы не выполняются.".to_owned(),
        }
    }

    async fn search(&self, request: &SearchRequest) -> Result<SearchAnswer, SearchError> {
        self.queries
            .lock()
            .expect("fake search lock")
            .push(request.query.clone());

        let reply = self
            .replies
            .lock()
            .expect("fake search lock")
            .pop()
            .ok_or_else(|| {
                SearchError::InvalidResponse("тестовый поиск: ответы закончились".to_owned())
            })?;

        match reply {
            FakeSearchReply::Hits(hits) => Ok(SearchAnswer {
                hits: hits
                    .into_iter()
                    .take(request.max_results as usize)
                    .collect(),
                duration: Duration::from_millis(1),
            }),
            FakeSearchReply::Fail(error) => Err(error),
        }
    }
}

/// One scripted page.
#[derive(Debug, Clone)]
pub struct FakePage {
    /// The body as the server would send it. HTML is extracted exactly as a real page
    /// would be, so a test's quotation is checked against real extraction output.
    pub body: String,
    pub content_type: String,
    pub license: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
}

impl FakePage {
    /// A page of plain text — the shortest way to write a fixture.
    pub fn text(body: &str) -> Self {
        Self {
            body: body.to_owned(),
            content_type: "text/plain".to_owned(),
            license: None,
            published_at: None,
        }
    }

    pub fn html(body: &str) -> Self {
        Self {
            body: body.to_owned(),
            content_type: "text/html".to_owned(),
            license: None,
            published_at: None,
        }
    }

    pub fn with_license(mut self, license: &str) -> Self {
        self.license = Some(license.to_owned());
        self
    }
}

/// What the fake fetcher does for one URL.
#[derive(Debug, Clone)]
pub enum FakeFetchReply {
    Page(FakePage),
    Refuse(FetchRefusal),
}

/// Answers from a table of URLs, and honours the same allowlist and robots rules the
/// real fetcher does.
///
/// Honouring them here rather than short-circuiting is the point: a test that asserts
/// "a host outside the allowlist is never read" is then asserting about the orchestration
/// it will really run, not about a stub that happens to say no.
pub struct FakeFetcher {
    pages: HashMap<String, FakeFetchReply>,
    allowed_hosts: Option<HostAllowlist>,
    robots_denied: Vec<String>,
    max_chars: usize,
    fetched: Mutex<Vec<String>>,
}

impl std::fmt::Debug for FakeFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeFetcher")
            .field("pages", &self.pages.len())
            .finish_non_exhaustive()
    }
}

impl Default for FakeFetcher {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl FakeFetcher {
    pub fn new(pages: Vec<(&str, FakeFetchReply)>) -> Self {
        Self {
            pages: pages
                .into_iter()
                .map(|(url, reply)| (url.to_owned(), reply))
                .collect(),
            allowed_hosts: None,
            robots_denied: Vec::new(),
            max_chars: 40_000,
            fetched: Mutex::new(Vec::new()),
        }
    }

    /// One page, the common case.
    pub fn serving(url: &str, page: FakePage) -> Self {
        Self::new(vec![(url, FakeFetchReply::Page(page))])
    }

    /// Apply the same host allowlist the real fetcher would.
    pub fn with_allowlist(mut self, allowlist: HostAllowlist) -> Self {
        self.allowed_hosts = Some(allowlist);
        self
    }

    /// Pretend this host's `robots.txt` forbids everything.
    pub fn denying_robots(mut self, host: &str) -> Self {
        self.robots_denied.push(host.to_ascii_lowercase());
        self
    }

    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = max_chars;
        self
    }

    /// Every URL this fetcher really downloaded, in order.
    pub fn fetched(&self) -> Vec<String> {
        self.fetched.lock().expect("fake fetcher lock").clone()
    }
}

#[async_trait]
impl DocumentFetcher for FakeFetcher {
    fn describe(&self) -> AdapterDescription {
        AdapterDescription {
            provider: "fake".to_owned(),
            endpoint_host: None,
            state: "ready",
            missing: Vec::new(),
            allowed_hosts: self
                .allowed_hosts
                .as_ref()
                .map(|list| list.entries().to_vec())
                .unwrap_or_default(),
            message: "Тестовая загрузка источников: сеть не используется.".to_owned(),
        }
    }

    async fn fetch(&self, url: &NormalisedUrl) -> Result<FetchedDocument, FetchRefusal> {
        if let Some(allowlist) = &self.allowed_hosts {
            if !allowlist.allows(url.host()) {
                return Err(FetchRefusal::HostNotAllowed(url.host().to_owned()));
            }
        }
        if self.robots_denied.iter().any(|host| host == url.host()) {
            return Err(FetchRefusal::RobotsDisallowed);
        }

        let reply = self
            .pages
            .get(url.as_str())
            .cloned()
            .unwrap_or(FakeFetchReply::Refuse(FetchRefusal::Http {
                status: 404,
                retryable: false,
            }))
            .clone();

        let page = match reply {
            FakeFetchReply::Page(page) => page,
            FakeFetchReply::Refuse(refusal) => {
                if refusal.was_sent() {
                    self.fetched
                        .lock()
                        .expect("fake fetcher lock")
                        .push(url.as_str().to_owned());
                }
                return Err(refusal);
            }
        };

        self.fetched
            .lock()
            .expect("fake fetcher lock")
            .push(url.as_str().to_owned());

        let extracted = if page.content_type.starts_with("text/plain") {
            html::extract_plain(&page.body, self.max_chars)
        } else {
            html::extract(&page.body, self.max_chars)
        };
        if extracted.text.trim().is_empty() {
            return Err(FetchRefusal::Empty);
        }

        Ok(FetchedDocument {
            url: url.clone(),
            http_status: 200,
            content_type: Some(page.content_type.clone()),
            bytes_len: page.body.len() as u64,
            content_hash: hex::encode(Sha256::digest(page.body.as_bytes())),
            text: extracted.text,
            title: extracted.title,
            license: page.license.or(extracted.license),
            license_note: extracted.license_note,
            published_at: page.published_at.or(extracted.published_at),
            retrieved_at: Utc::now(),
            duration: Duration::from_millis(1),
            truncated: extracted.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_replies_are_consumed_in_order_and_queries_are_recorded() {
        let provider = FakeSearchProvider::new(vec![
            FakeSearchReply::urls(&["https://docs.example.org/a"]),
            FakeSearchReply::Fail(SearchError::RateLimited),
        ]);

        let first = provider
            .search(&SearchRequest {
                query: "один".to_owned(),
                max_results: 5,
            })
            .await
            .unwrap();
        assert_eq!(first.hits.len(), 1);

        let second = provider
            .search(&SearchRequest {
                query: "два".to_owned(),
                max_results: 5,
            })
            .await
            .unwrap_err();
        assert!(matches!(second, SearchError::RateLimited));

        assert_eq!(
            provider.queries(),
            vec!["один".to_owned(), "два".to_owned()]
        );
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn the_fake_fetcher_honours_the_allowlist_and_robots() {
        let fetcher = FakeFetcher::new(vec![
            (
                "https://docs.example.org/gost",
                FakeFetchReply::Page(FakePage::text("минимальная толщина 55 мкм")),
            ),
            (
                "https://blog.example.net/post",
                FakeFetchReply::Page(FakePage::text("что-то ещё")),
            ),
        ])
        .with_allowlist(HostAllowlist::parse("docs.example.org, blog.example.net").unwrap())
        .denying_robots("blog.example.net");

        let allowed = NormalisedUrl::parse("https://docs.example.org/gost").unwrap();
        let document = fetcher.fetch(&allowed).await.unwrap();
        assert!(document.text.contains("55 мкм"));
        assert_eq!(document.content_hash.len(), 64);

        let robots_denied = NormalisedUrl::parse("https://blog.example.net/post").unwrap();
        assert_eq!(
            fetcher.fetch(&robots_denied).await.unwrap_err(),
            FetchRefusal::RobotsDisallowed
        );

        let outside = NormalisedUrl::parse("https://other.example.com/page").unwrap();
        assert_eq!(
            fetcher.fetch(&outside).await.unwrap_err(),
            FetchRefusal::HostNotAllowed("other.example.com".to_owned())
        );

        // Only the page that was really read is in the journal.
        assert_eq!(
            fetcher.fetched(),
            vec!["https://docs.example.org/gost".to_owned()]
        );
    }

    #[tokio::test]
    async fn an_unlisted_url_is_a_404_rather_than_an_invented_page() {
        let fetcher = FakeFetcher::default();
        let url = NormalisedUrl::parse("https://docs.example.org/missing").unwrap();
        assert_eq!(
            fetcher.fetch(&url).await.unwrap_err(),
            FetchRefusal::Http {
                status: 404,
                retryable: false
            }
        );
    }
}
