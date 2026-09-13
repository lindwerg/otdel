//! A small, deliberately conservative `robots.txt` reader.
//!
//! Reading somebody's site because a search engine mentioned it is a thing one does with
//! permission. The rules here follow the usual convention (RFC 9309): the group whose
//! `User-agent` names us wins, otherwise the `*` group; inside a group the *longest*
//! matching rule decides, and `Allow` wins a tie. `*` and a trailing `$` are supported,
//! because a site that writes `Disallow: /*.pdf$` means it.
//!
//! Two decisions are deliberately not the permissive ones:
//!
//! * a `robots.txt` that could **not be read at all** (a refused connection, a timeout, a
//!   server error) does not yield rules, and the caller refuses the page. The usual advice
//!   is the opposite; but this fetcher exists to be defensible, and "we could not check
//!   whether we were allowed, so we read it anyway" is not. That refusal is reported as
//!   [`crate::FetchRefusal::RobotsUnavailable`] — *retryable*, and worded as "could not
//!   read", never as "the site forbade it";
//! * only a `404`/`410` — the site saying "there is no such file" — means *allow
//!   everything*, which is what the absence of a `robots.txt` has always meant.

/// Longest `robots.txt` read. Beyond this the file is not a policy any more, and the
/// remainder is ignored rather than allowed to grow the parse unboundedly.
pub const MAX_ROBOTS_BYTES: usize = 512 * 1024;
/// Upper bound on rules kept from one file.
const MAX_RULES: usize = 2_000;

/// One `Allow:`/`Disallow:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    allow: bool,
    pattern: String,
}

/// The rules that apply to this crawler on one host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    rules: Vec<Rule>,
}

impl Robots {
    /// Everything is permitted — what "no robots.txt" has always meant.
    pub fn permissive() -> Self {
        Self { rules: Vec::new() }
    }

    /// Parse the rules that apply to `user_agent` (matched case-insensitively as a
    /// prefix of the declared token, which is how crawlers are named in practice).
    pub fn parse(text: &str, user_agent: &str) -> Self {
        let wanted = user_agent.to_ascii_lowercase();

        let mut specific: Vec<Rule> = Vec::new();
        let mut wildcard: Vec<Rule> = Vec::new();
        // Which groups the current run of `User-agent:` lines opened. Consecutive
        // user-agent lines share one group of rules, which is what the format says.
        let mut in_specific = false;
        let mut in_wildcard = false;
        let mut previous_was_agent = false;

        for line in text.lines().take(50_000) {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim().to_ascii_lowercase();
            let value = value.trim();

            match field.as_str() {
                "user-agent" => {
                    if !previous_was_agent {
                        // A new group starts: forget which groups the previous one was.
                        in_specific = false;
                        in_wildcard = false;
                    }
                    let declared = value.to_ascii_lowercase();
                    if declared == "*" {
                        in_wildcard = true;
                    } else if wanted.starts_with(&declared) && !declared.is_empty() {
                        in_specific = true;
                    }
                    previous_was_agent = true;
                }
                "allow" | "disallow" => {
                    previous_was_agent = false;
                    let allow = field == "allow";
                    // `Disallow:` with an empty value means "nothing is disallowed" and
                    // is not a rule about the root path.
                    if value.is_empty() {
                        continue;
                    }
                    let rule = Rule {
                        allow,
                        pattern: value.chars().take(1_000).collect(),
                    };
                    if in_specific && specific.len() < MAX_RULES {
                        specific.push(rule.clone());
                    }
                    if in_wildcard && wildcard.len() < MAX_RULES {
                        wildcard.push(rule);
                    }
                }
                _ => previous_was_agent = false,
            }
        }

        // A group naming us replaces the wildcard group entirely — it does not add to it.
        Self {
            rules: if specific.is_empty() {
                wildcard
            } else {
                specific
            },
        }
    }

    /// May this path (with its query) be requested?
    pub fn allows(&self, path_and_query: &str) -> bool {
        let path = if path_and_query.starts_with('/') {
            path_and_query
        } else {
            "/"
        };

        let mut decision = true;
        let mut best = 0usize;
        for rule in &self.rules {
            if !matches_pattern(&rule.pattern, path) {
                continue;
            }
            let length = rule.pattern.chars().count();
            // Longest match wins; `Allow` wins a tie, which is the documented
            // tie-break and the one that keeps an explicit permission meaningful.
            if length > best || (length == best && rule.allow) {
                best = length;
                decision = rule.allow;
            }
        }
        decision
    }
}

/// Prefix match with `*` (any run of characters) and a trailing `$` (end of path).
///
/// Backtracking, not leftmost-greedy. Taking the first occurrence of each literal
/// segment is the obvious implementation and it is wrong in the direction that matters:
/// `Disallow: /*/x$` against `/a/x/x` would match `/x` at the first opportunity, fail to
/// reach the end, and report the path as **allowed** — fetching a page the site
/// disallowed. Every placement of each segment is therefore tried.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let pattern = if anchored {
        &pattern[..pattern.len() - 1]
    } else {
        pattern
    };

    let segments: Vec<&str> = pattern.split('*').collect();
    // A robots pattern is a *prefix*: its first literal is anchored at the start of the
    // path. Everything after the first `*` floats.
    let Some((first, rest)) = segments.split_first() else {
        return true;
    };
    if !path.starts_with(first) {
        return false;
    }
    floats(rest, path, first.len(), anchored)
}

/// Can the remaining (floating) segments be laid over `path[cursor..]`?
fn floats(segments: &[&str], path: &str, cursor: usize, anchored: bool) -> bool {
    let Some((segment, rest)) = segments.split_first() else {
        // The pattern ended on a literal. Without `$` anything may follow it; with `$`
        // the literal had to finish the path.
        return !anchored || cursor == path.len();
    };

    if rest.is_empty() {
        return if anchored {
            // The last literal must finish the path — `ends_with` is exactly that, and it
            // is also what makes `…*$` (an empty final segment) match any remainder.
            path.len() >= cursor + segment.len() && path[cursor..].ends_with(segment)
        } else {
            find_at_or_after(path, cursor, segment).is_some()
        };
    }

    // Every placement of this segment is tried, because an earlier one that matches can
    // still leave the rest of the pattern unsatisfiable — which is the case that made
    // leftmost-greedy matching report a disallowed page as allowed.
    let mut from = cursor;
    while let Some(offset) = find_at_or_after(path, from, segment) {
        if floats(rest, path, offset + segment.len(), anchored) {
            return true;
        }
        from = next_boundary(path, offset + 1);
        if from > path.len() {
            break;
        }
    }
    false
}

/// Byte offset of the next occurrence of `needle` at or after `from`.
fn find_at_or_after(path: &str, from: usize, needle: &str) -> Option<usize> {
    if from > path.len() || !path.is_char_boundary(from) {
        return None;
    }
    path[from..].find(needle).map(|offset| from + offset)
}

/// `index`, moved forward to the next character boundary. A path is normally ASCII, but
/// `Robots::allows` is public and slicing a byte index that lands mid-character panics.
fn next_boundary(path: &str, mut index: usize) -> usize {
    while index < path.len() && !path.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "otdel-research";

    #[test]
    fn an_absent_robots_file_permits_everything() {
        let robots = Robots::permissive();
        assert!(robots.allows("/"));
        assert!(robots.allows("/deep/page?x=1"));
    }

    #[test]
    fn the_wildcard_group_applies_when_no_group_names_us() {
        let robots = Robots::parse(
            "User-agent: *\nDisallow: /private/\nAllow: /private/public.html\n",
            AGENT,
        );
        assert!(robots.allows("/docs/gost"));
        assert!(!robots.allows("/private/secret"));
        // The longer, explicit Allow wins over the shorter Disallow.
        assert!(robots.allows("/private/public.html"));
    }

    #[test]
    fn a_group_naming_us_replaces_the_wildcard_group() {
        let robots = Robots::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: otdel\nDisallow: /admin/\n",
            AGENT,
        );
        // Our own group allows everything except /admin/ — the blanket Disallow in the
        // `*` group does not also apply to us.
        assert!(robots.allows("/docs/gost"));
        assert!(!robots.allows("/admin/panel"));
    }

    #[test]
    fn consecutive_user_agent_lines_share_one_group() {
        let robots = Robots::parse(
            "User-agent: somebot\nUser-agent: otdel-research\nDisallow: /x/\n\n\
             User-agent: *\nDisallow: /\n",
            AGENT,
        );
        assert!(robots.allows("/y"));
        assert!(!robots.allows("/x/page"));
    }

    #[test]
    fn wildcards_and_end_anchors_are_honoured() {
        let robots = Robots::parse(
            "User-agent: *\nDisallow: /*.pdf$\nDisallow: /a/*/private\n",
            AGENT,
        );
        assert!(!robots.allows("/files/report.pdf"));
        // `$` means the path ends there: a query after it is a different path.
        assert!(robots.allows("/files/report.pdf?download=1"));
        assert!(robots.allows("/files/report.html"));
        assert!(!robots.allows("/a/b/private"));
        assert!(robots.allows("/a/b/public"));
    }

    #[test]
    fn an_empty_disallow_forbids_nothing() {
        let robots = Robots::parse("User-agent: *\nDisallow:\n", AGENT);
        assert!(robots.allows("/"));
        assert!(robots.allows("/anything"));
    }

    #[test]
    fn comments_and_unknown_directives_are_ignored() {
        let robots = Robots::parse(
            "# a comment\nSitemap: https://example.com/sitemap.xml\n\
             User-agent: *   # us\nCrawl-delay: 10\nDisallow: /no/\n",
            AGENT,
        );
        assert!(robots.allows("/yes"));
        assert!(!robots.allows("/no/page"));
    }

    #[test]
    fn a_wildcard_rule_matches_wherever_it_can_not_only_at_the_first_opportunity() {
        // Leftmost-greedy matching finds `/x` at offset 1 in `/a/x/x`, runs out of path
        // before the `$`, and reports the rule as not matching — i.e. **allows** a page
        // the site disallowed. Wrong direction for a component whose posture is
        // "fail closed".
        let robots = Robots::parse("User-agent: *\nDisallow: /*/x$\n", AGENT);
        assert!(!robots.allows("/a/x/x"), "the rule does match this path");
        assert!(!robots.allows("/a/x"));
        assert!(robots.allows("/a/x/y"));

        // Several floating segments, each needing more than its first placement.
        let multi = Robots::parse("User-agent: *\nDisallow: /a*b*c$\n", AGENT);
        assert!(!multi.allows("/aXbYc"));
        assert!(!multi.allows("/abbc"));
        assert!(multi.allows("/aXbYcZ"));
        assert!(multi.allows("/aXc"));

        // A trailing `*` before `$` absorbs whatever is left.
        let trailing = Robots::parse("User-agent: *\nDisallow: /files/*$\n", AGENT);
        assert!(!trailing.allows("/files/anything/at/all"));
        assert!(trailing.allows("/other"));

        // The first literal stays anchored: a robots pattern is a prefix, not a search.
        let prefix = Robots::parse("User-agent: *\nDisallow: /admin\n", AGENT);
        assert!(!prefix.allows("/admin/panel"));
        assert!(prefix.allows("/public/admin"));
    }

    #[test]
    fn a_non_ascii_path_does_not_panic() {
        // `allows` is public and slices by byte offset; a path that is not ASCII must not
        // be able to land a cursor mid-character.
        let robots = Robots::parse("User-agent: *\nDisallow: /a*b$\n", AGENT);
        assert!(robots.allows("/аЖбЖ"));
        assert!(!robots.allows("/aЖb"));
    }

    #[test]
    fn a_blanket_disallow_stops_everything() {
        let robots = Robots::parse("User-agent: *\nDisallow: /\n", AGENT);
        assert!(!robots.allows("/"));
        assert!(!robots.allows("/docs/gost"));
    }
}
