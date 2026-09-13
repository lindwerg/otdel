//! Turning a string a search engine returned into a URL this system is willing to open.
//!
//! A [`NormalisedUrl`] cannot be constructed from a bare string: it is produced by
//! [`NormalisedUrl::parse`], which refuses everything that is not a plain
//! `https://host/path[?query]`. That is the point — the fetcher's signature then says
//! that no arbitrary string can reach it, and "did somebody validate this URL?" has a
//! type-level answer rather than a review-level one.
//!
//! What is refused, and why each one matters for a component that opens sockets on
//! behalf of untrusted content:
//!
//! | Refused | Why |
//! |---|---|
//! | any scheme but `https` | `file:`, `gopher:`, `data:` and plain `http` are not sources |
//! | `user:pass@host` | credentials in a URL end up in logs, and mislead host parsing |
//! | a port other than 443 | `https://10.0.0.1:6379` is how an SSRF reaches a database |
//! | an IP-literal host | it can never match a declared publisher, and is the shape of an internal target |
//! | a fragment | never sent to the server, and two URLs differing only there are one document |
//! | non-ASCII, spaces, control characters | an already-encoded URL is unambiguous; guessing an encoding is not |
//!
//! The fragment is dropped rather than rejected: `…/gost#section-3` is a perfectly
//! normal search result, and the document it names is the same one.

use sha2::{Digest, Sha256};

/// Longest URL accepted. Far beyond any real document, short enough to bound a row.
pub const MAX_URL_CHARS: usize = 2_000;
/// Longest host accepted (the DNS limit).
const MAX_HOST_CHARS: usize = 253;

/// Why a candidate URL was not accepted. Each value is shown to the owner in the source
/// journal, so the reason a result was never opened is always on the screen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlRejection {
    #[error("ссылка пуста")]
    Empty,
    #[error("ссылка длиннее {MAX_URL_CHARS} символов")]
    TooLong,
    #[error("ссылка содержит пробелы, управляющие символы или не-ASCII: ожидается уже закодированный URL")]
    NotAscii,
    #[error("схема `{0}` не поддерживается: читаются только https-страницы")]
    UnsupportedScheme(String),
    #[error("в ссылке нет имени хоста")]
    NoHost,
    #[error("ссылка содержит учётные данные перед адресом хоста")]
    EmbeddedCredentials,
    #[error("нестандартный порт `{0}`: читается только https на порту 443")]
    UnsupportedPort(String),
    #[error("хост задан IP-адресом, а не именем публикатора")]
    IpLiteralHost,
    #[error("имя хоста некорректно")]
    InvalidHost,
}

/// An absolute `https` URL that passed every structural check.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NormalisedUrl {
    url: String,
    host: String,
    path: String,
}

impl NormalisedUrl {
    /// Parse and normalise, or say exactly why not.
    pub fn parse(raw: &str) -> Result<Self, UrlRejection> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(UrlRejection::Empty);
        }
        if value.chars().count() > MAX_URL_CHARS {
            return Err(UrlRejection::TooLong);
        }
        // A URL is an ASCII string by construction; anything else arrives from a page or
        // an API that did not encode it, and guessing the encoding would mean opening a
        // different address than the one displayed.
        if !value.is_ascii() || value.chars().any(|c| c.is_control() || c == ' ') {
            return Err(UrlRejection::NotAscii);
        }

        let (scheme, rest) = value
            .split_once("://")
            .ok_or_else(|| UrlRejection::UnsupportedScheme(scheme_of(value)))?;
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(UrlRejection::UnsupportedScheme(scheme.to_ascii_lowercase()));
        }

        // The fragment never leaves the browser, and two results differing only in their
        // anchor are the same document — dropping it is also what makes deduplication
        // work.
        let rest = rest.split('#').next().unwrap_or_default();
        if rest.is_empty() {
            return Err(UrlRejection::NoHost);
        }

        let (authority, tail) = match rest.find(['/', '?']) {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(UrlRejection::NoHost);
        }
        if authority.contains('@') {
            return Err(UrlRejection::EmbeddedCredentials);
        }
        if authority.starts_with('[') {
            // An IPv6 literal. Never a publisher's name.
            return Err(UrlRejection::IpLiteralHost);
        }

        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        // Only the default https port. `https://10.0.0.1:6379/` is the classic shape of
        // a request-forgery target, and no legitimate public document needs another port.
        if let Some(port) = port {
            if port != "443" {
                return Err(UrlRejection::UnsupportedPort(port.to_owned()));
            }
        }

        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() || host.len() > MAX_HOST_CHARS {
            return Err(UrlRejection::InvalidHost);
        }
        if looks_like_ipv4(&host) {
            return Err(UrlRejection::IpLiteralHost);
        }
        if !host.contains('.')
            || host.starts_with('-')
            || host.starts_with('.')
            || host.contains("..")
            || !host
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        {
            return Err(UrlRejection::InvalidHost);
        }

        let (path, query) = match tail.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (tail, None),
        };
        let path = if path.is_empty() { "/" } else { path };

        let mut url = format!("https://{host}{path}");
        // An empty query (`?`) carries no meaning and only creates a second spelling of
        // one document.
        if let Some(query) = query.filter(|query| !query.is_empty()) {
            url.push('?');
            url.push_str(query);
        }
        if url.chars().count() > MAX_URL_CHARS {
            return Err(UrlRejection::TooLong);
        }

        Ok(Self {
            url,
            host,
            path: path.to_owned(),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.url
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// Path without the query, which is what `robots.txt` rules are matched against.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Path and query — what is actually requested, and what a `Disallow` rule with a
    /// query pattern applies to.
    ///
    /// Computed by slicing rather than stored, because the normalised form is always
    /// exactly `https://{host}{path}[?query]` and a second stored copy could drift.
    pub fn path_and_query(&self) -> &str {
        const SCHEME: &str = "https://";
        &self.url[SCHEME.len() + self.host.len()..]
    }

    /// The host's `robots.txt`, as a URL that passed the same checks.
    pub fn robots_url(&self) -> Self {
        Self {
            url: format!("https://{}/robots.txt", self.host),
            host: self.host.clone(),
            path: "/robots.txt".to_owned(),
        }
    }

    /// SHA-256 of the normalised form, used to deduplicate sources inside one plan.
    ///
    /// The hash is of the *normalised* string, so `HTTPS://Example.COM/a#x` and
    /// `https://example.com/a` are one source rather than two.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(self.url.as_bytes()))
    }
}

impl std::fmt::Display for NormalisedUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

fn scheme_of(value: &str) -> String {
    value
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .chars()
        .take(20)
        .collect()
}

/// Four dot-separated numbers — an address, not a name.
fn looks_like_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_https_url_is_accepted_and_normalised() {
        let url = NormalisedUrl::parse("HTTPS://Docs.Example.COM:443/a/b?x=1#section").unwrap();
        assert_eq!(url.as_str(), "https://docs.example.com/a/b?x=1");
        assert_eq!(url.host(), "docs.example.com");
        assert_eq!(url.path(), "/a/b");
        assert_eq!(url.path_and_query(), "/a/b?x=1");

        // A bare host gets the root path, and the root dot is not part of the name.
        assert_eq!(
            NormalisedUrl::parse("https://example.com.")
                .unwrap()
                .as_str(),
            "https://example.com/"
        );
    }

    #[test]
    fn everything_that_is_not_a_public_https_document_is_refused() {
        for (raw, expected) in [
            ("", UrlRejection::Empty),
            ("   ", UrlRejection::Empty),
            (
                "http://example.com/a",
                UrlRejection::UnsupportedScheme("http".to_owned()),
            ),
            (
                "file:///etc/passwd",
                UrlRejection::UnsupportedScheme("file".to_owned()),
            ),
            (
                "javascript:alert(1)",
                UrlRejection::UnsupportedScheme("javascript".to_owned()),
            ),
            (
                "data:text/html,<b>x</b>",
                UrlRejection::UnsupportedScheme("data".to_owned()),
            ),
            ("https://", UrlRejection::NoHost),
            (
                "https://user:pass@example.com/a",
                UrlRejection::EmbeddedCredentials,
            ),
            (
                "https://example.com:6379/a",
                UrlRejection::UnsupportedPort("6379".to_owned()),
            ),
            // The two shapes an internal target usually takes.
            ("https://127.0.0.1/a", UrlRejection::IpLiteralHost),
            (
                "https://169.254.169.254/latest/meta-data/",
                UrlRejection::IpLiteralHost,
            ),
            ("https://[::1]/a", UrlRejection::IpLiteralHost),
            ("https://localhost/a", UrlRejection::InvalidHost),
            ("https://ex ample.com/a", UrlRejection::NotAscii),
            ("https://пример.рф/a", UrlRejection::NotAscii),
            ("https://example..com/a", UrlRejection::InvalidHost),
        ] {
            assert_eq!(
                NormalisedUrl::parse(raw),
                Err(expected.clone()),
                "`{raw}` must be refused as {expected:?}"
            );
        }

        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_CHARS));
        assert_eq!(NormalisedUrl::parse(&long), Err(UrlRejection::TooLong));
    }

    #[test]
    fn a_control_character_cannot_smuggle_a_second_request_line() {
        // CR/LF in a URL is how header injection is attempted; it never survives here.
        for raw in [
            "https://example.com/a\r\nHost: evil.example",
            "https://example.com/a\u{0}b",
            "https://example.com/\u{7f}",
        ] {
            assert_eq!(NormalisedUrl::parse(raw), Err(UrlRejection::NotAscii));
        }
    }

    #[test]
    fn normalisation_makes_one_document_one_source() {
        let first = NormalisedUrl::parse("https://example.com/doc?a=1").unwrap();
        let second = NormalisedUrl::parse("HTTPS://EXAMPLE.com:443/doc?a=1#top").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.digest().len(), 64);

        // A different query really is a different document.
        let third = NormalisedUrl::parse("https://example.com/doc?a=2").unwrap();
        assert_ne!(first.digest(), third.digest());

        // …and an empty query is not a query at all.
        assert_eq!(
            NormalisedUrl::parse("https://example.com/doc?")
                .unwrap()
                .as_str(),
            "https://example.com/doc"
        );
    }

    #[test]
    fn the_robots_url_is_derived_from_the_host_not_from_the_page() {
        let url = NormalisedUrl::parse("https://docs.example.com/deep/page?x=1").unwrap();
        let robots = url.robots_url();
        assert_eq!(robots.as_str(), "https://docs.example.com/robots.txt");
        assert_eq!(robots.host(), url.host());
    }
}
