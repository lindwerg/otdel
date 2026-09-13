//! Turning one approved question into the handful of strings that will be sent outside.
//!
//! Two properties, and both of them are refusals.
//!
//! **The partner is never named to a search engine.** A 1C question is written inside the
//! partner's own context and may well mention them by name ("какая нагрузка у профилей
//! BASIS?"). Sending that to an external service tells a third party which company this
//! bureau is working on, and `docs/block-01-spec.md` §6.5/§8 are explicit that closed
//! partner context does not leak into a public library. So a query naming the partner is
//! **refused with a stated reason** rather than quietly stripped: rewriting it would send
//! a question whose meaning nobody checked, and the owner can rephrase it in industry
//! terms in a second.
//!
//! **Queries are built deterministically, not by a model.** What was searched for is a
//! fact the owner can read in the journal and reproduce, and there is no step between
//! "the question" and "the request" where a model could decide to look for something
//! else. The transformation is a small, testable one: the question as written, and the
//! same question with the interrogative scaffolding removed.

/// Longest query sent. Search engines ignore more than this anyway, and a long query is
/// a worse one.
pub const MAX_QUERY_CHARS: usize = 300;
/// A query shorter than this cannot be about anything in particular.
pub const MIN_QUERY_CHARS: usize = 8;

/// Words that turn a statement into a question and carry no meaning for a search engine.
const INTERROGATIVES: [&str; 22] = [
    "какая",
    "какой",
    "какое",
    "какие",
    "каков",
    "какова",
    "каковы",
    "что",
    "сколько",
    "когда",
    "где",
    "чем",
    "как",
    "нужно",
    "ли",
    "является",
    "существует",
    "есть",
    "имеется",
    "укажите",
    "уточните",
    "подскажите",
];

/// Legal forms and decorations that are not part of a company's distinctive name.
const LEGAL_FORMS: [&str; 14] = [
    "ооо", "оао", "зао", "пао", "ао", "ип", "нао", "ltd", "llc", "inc", "gmbh", "jsc", "plc",
    "corp",
];

/// Why a query was not sent. Recorded on the plan and shown as-is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryRefusal {
    #[error("вопрос пуст")]
    Empty,
    #[error("вопрос слишком короткий для внешнего поиска")]
    TooShort,
    #[error(
        "вопрос называет партнёра («{0}»): внешний поиск по нему не выполняется. \
         Переформулируйте вопрос отраслевыми терминами — без названия компании"
    )]
    NamesPartner(String),
}

/// Build the queries of one plan.
///
/// Returns at most `max` distinct strings, in the order they will be sent. An error
/// means **nothing** is sent for this plan and nothing is spent.
pub fn plan_queries(
    question: &str,
    topic: Option<&str>,
    partner_name: &str,
    max: usize,
) -> Result<Vec<String>, QueryRefusal> {
    let question = sanitise(question);
    if question.is_empty() {
        return Err(QueryRefusal::Empty);
    }
    if question.chars().count() < MIN_QUERY_CHARS {
        return Err(QueryRefusal::TooShort);
    }

    let identifying = identifying_tokens(partner_name);
    if let Some(found) = contains_identifying_token(&question, &identifying) {
        return Err(QueryRefusal::NamesPartner(found));
    }

    let mut queries: Vec<String> = vec![question.clone()];

    // The same question without its interrogative scaffolding: closer to how a standard
    // or a datasheet phrases the thing being asked about.
    let keywords = keyword_form(&question);
    if keywords.chars().count() >= MIN_QUERY_CHARS && keywords != question {
        queries.push(keywords.clone());
    }

    // The gap's topic, when it adds a word the question does not already contain.
    if let Some(topic) = topic.map(sanitise).filter(|topic| !topic.is_empty()) {
        let base = if keywords.is_empty() {
            question.clone()
        } else {
            keywords
        };
        if !contains_phrase(&base, &topic) {
            queries.push(clip(&format!("{topic} {base}")));
        }
    }

    let mut chosen: Vec<String> = Vec::new();
    for query in queries {
        // Every query is checked again: the topic could carry the name the question did
        // not.
        if contains_identifying_token(&query, &identifying).is_some() {
            continue;
        }
        if query.chars().count() < MIN_QUERY_CHARS || chosen.contains(&query) {
            continue;
        }
        chosen.push(query);
        if chosen.len() >= max.max(1) {
            break;
        }
    }

    if chosen.is_empty() {
        return Err(QueryRefusal::TooShort);
    }
    Ok(chosen)
}

/// One line, control-free, whitespace collapsed, bounded.
fn sanitise(text: &str) -> String {
    let collapsed = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    clip(collapsed.trim())
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_QUERY_CHARS).collect()
}

/// The question with interrogative words and punctuation removed.
fn keyword_form(question: &str) -> String {
    question
        .split_whitespace()
        .map(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .filter(|word| !word.is_empty() && !INTERROGATIVES.contains(&word.to_lowercase().as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tokens that identify this partner and must not be sent outside.
///
/// The full name always counts. A name that reduces to a single distinctive word once
/// the legal form is removed — «ООО "БАЗИС"» → `базис` — counts as that word too, which
/// is the case that matters: the brand is what a search engine would reveal. A
/// multi-word name is only matched in full, so a partner called «Северная Сталь» does
/// not make the word "сталь" unsearchable.
fn identifying_tokens(partner_name: &str) -> Vec<String> {
    let normalised = normalise(partner_name);
    if normalised.is_empty() {
        return Vec::new();
    }

    let mut tokens = vec![normalised.clone()];
    let distinctive: Vec<&str> = normalised
        .split_whitespace()
        .filter(|word| !LEGAL_FORMS.contains(word))
        .collect();
    if distinctive.len() == 1 && distinctive[0].chars().count() >= 3 {
        let single = distinctive[0].to_owned();
        if !tokens.contains(&single) {
            tokens.push(single);
        }
    }
    tokens
}

/// Does `text` name this partner?
///
/// Returns the identifying token that was found, or `None`. Public because the partner's
/// name must stay out of *everything* that leaves this machine, not only out of a search
/// query: the gap topic that phase 1C wrote from the partner's own document goes into the
/// model prompt, and it is checked with exactly this function
/// (`otdel_worker::research`).
pub fn names_partner(text: &str, partner_name: &str) -> Option<String> {
    contains_identifying_token(text, &identifying_tokens(partner_name))
}

/// Does the query contain one of the partner's identifying tokens as whole words?
fn contains_identifying_token(query: &str, identifying: &[String]) -> Option<String> {
    let haystack = normalise(query);
    identifying
        .iter()
        .find(|token| contains_phrase(&haystack, token))
        .cloned()
}

/// Whole-word phrase containment on normalised text.
///
/// Word-bounded rather than a bare substring: a partner called «Ост» must not make every
/// question about a «ГОСТ» unsearchable.
fn contains_phrase(haystack: &str, needle: &str) -> bool {
    let needle = normalise(needle);
    if needle.is_empty() {
        return false;
    }
    let haystack = normalise(haystack);
    let words: Vec<&str> = haystack.split(' ').collect();
    let wanted: Vec<&str> = needle.split(' ').collect();
    if wanted.is_empty() || wanted.len() > words.len() {
        return false;
    }
    words
        .windows(wanted.len())
        .any(|window| window == wanted.as_slice())
}

/// Lower-case, punctuation-free, single-spaced — so `«БАЗИС»,` and `базис` are the same
/// word.
fn normalise(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_becomes_the_question_and_its_keyword_form() {
        let queries = plan_queries(
            "Какая минимальная толщина цинкового покрытия по ГОСТ 9.307?",
            None,
            "BASIS",
            3,
        )
        .unwrap();

        assert_eq!(
            queries[0],
            "Какая минимальная толщина цинкового покрытия по ГОСТ 9.307?"
        );
        // The interrogative and the question mark are gone; the standard's number keeps
        // its decimal point, because `9.307` and `9 307` are not the same standard.
        assert_eq!(
            queries[1],
            "минимальная толщина цинкового покрытия по ГОСТ 9.307"
        );
        assert_eq!(queries.len(), 2);
    }

    #[test]
    fn the_topic_is_added_only_when_it_says_something_new() {
        let with_topic = plan_queries(
            "Какая минимальная толщина покрытия требуется?",
            Some("цинкование"),
            "BASIS",
            5,
        )
        .unwrap();
        assert!(
            with_topic
                .iter()
                .any(|query| query.starts_with("цинкование ")),
            "{with_topic:?}"
        );

        let redundant = plan_queries(
            "Какая минимальная толщина цинкования требуется?",
            Some("цинкования"),
            "BASIS",
            5,
        )
        .unwrap();
        assert!(
            !redundant
                .iter()
                .any(|query| query.starts_with("цинкования ")),
            "a topic already in the question adds nothing: {redundant:?}"
        );
    }

    #[test]
    fn a_question_naming_the_partner_is_refused_and_nothing_is_sent() {
        let error = plan_queries(
            "Какая нагрузка у монтажного профиля BASIS BP21?",
            None,
            "BASIS",
            3,
        )
        .unwrap_err();

        assert_eq!(error, QueryRefusal::NamesPartner("basis".to_owned()));
        // The message tells the owner what to do instead of just saying "no".
        assert!(error.to_string().contains("Переформулируйте"));
    }

    #[test]
    fn a_legal_form_does_not_hide_the_brand() {
        for question in [
            "Какие требования к профилям БАЗИС?",
            "требования базис к монтажу",
            "Что известно про «БАЗИС»?",
        ] {
            assert!(
                matches!(
                    plan_queries(question, None, "ООО \"БАЗИС\"", 3),
                    Err(QueryRefusal::NamesPartner(_))
                ),
                "`{question}` names the partner behind its legal form"
            );
        }
    }

    #[test]
    fn a_multi_word_partner_does_not_make_industry_words_unsearchable() {
        // «сталь» is half of this partner's name and an ordinary industry word. Only the
        // whole name is identifying.
        let queries = plan_queries(
            "Какая минимальная толщина покрытия для оцинкованной стали?",
            None,
            "Северная Сталь",
            3,
        )
        .unwrap();
        assert!(!queries.is_empty());

        assert!(matches!(
            plan_queries(
                "Что производит Северная Сталь по ГОСТ?",
                None,
                "Северная Сталь",
                3
            ),
            Err(QueryRefusal::NamesPartner(_))
        ));
    }

    #[test]
    fn a_partner_name_inside_a_longer_word_is_not_a_mention() {
        // A partner called «Ост» must not make every question about a ГОСТ unsearchable.
        let queries = plan_queries(
            "Какая толщина покрытия по ГОСТ 9.307 для крепежа?",
            None,
            "Ост",
            3,
        )
        .unwrap();
        assert!(!queries.is_empty(), "ГОСТ does not contain the word Ост");
    }

    #[test]
    fn the_topic_is_checked_for_the_partner_name_too() {
        // The question is clean; the gap's topic is not. The clean query still goes.
        let queries = plan_queries(
            "Какая минимальная толщина цинкового покрытия по стандарту?",
            Some("покрытие BASIS"),
            "BASIS",
            5,
        )
        .unwrap();
        assert!(
            queries
                .iter()
                .all(|query| !query.to_lowercase().contains("basis")),
            "{queries:?}"
        );
    }

    #[test]
    fn queries_are_bounded_deduplicated_and_control_free() {
        let queries = plan_queries(
            &format!("Какая {}?", "толщина ".repeat(200)),
            None,
            "BASIS",
            3,
        )
        .unwrap();
        for query in &queries {
            assert!(query.chars().count() <= MAX_QUERY_CHARS);
            assert!(!query.chars().any(char::is_control));
        }

        // A question with no interrogative scaffolding produces exactly one query.
        let single = plan_queries("толщина цинкового покрытия ГОСТ", None, "BASIS", 3).unwrap();
        assert_eq!(single.len(), 1);

        // The caller's ceiling wins.
        let capped = plan_queries(
            "Какая минимальная толщина покрытия требуется?",
            Some("цинкование"),
            "BASIS",
            1,
        )
        .unwrap();
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn an_empty_or_trivial_question_is_refused() {
        assert_eq!(plan_queries("", None, "BASIS", 3), Err(QueryRefusal::Empty));
        assert_eq!(
            plan_queries("   \n\t ", None, "BASIS", 3),
            Err(QueryRefusal::Empty)
        );
        assert_eq!(
            plan_queries("Что?", None, "BASIS", 3),
            Err(QueryRefusal::TooShort)
        );
    }

    #[test]
    fn a_question_that_is_only_interrogatives_still_sends_the_question_itself() {
        // The keyword form collapses to nothing; the question as written is still a
        // legitimate thing to search for.
        let queries = plan_queries("Сколько это есть ли как?", None, "BASIS", 3).unwrap();
        assert_eq!(queries, vec!["Сколько это есть ли как?".to_owned()]);
    }
}
