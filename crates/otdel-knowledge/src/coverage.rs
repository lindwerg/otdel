//! The page account, and the requirement check that stands before automatic publication.
//!
//! Two pure rules live here, both of them answers to the same audited failure: a run over
//! a 44-page catalogue that reported success while eight pages had never been offered to
//! anything and the draft held no terms, no questions and no gaps.
//!
//! * [`CoveragePlan`] turns *every* page of a material — not only the readable ones —
//!   into a line with a disposition, and [`CoveragePlan::settle`] turns the run's actual
//!   batches into the final account. A page cannot leave the account by being ignored:
//!   the plan is built from the page inventory, so anything the run does not process is
//!   still there, carrying the reason it was not.
//! * [`evaluate_requirements`] decides whether a draft may be published without a person
//!   reading it first. Its defining property is that **an empty array satisfies nothing**:
//!   "no glossary terms" passes only beside a declaration saying so, and the declaration
//!   is a stored row with words in it, not the absence of rows.
//!
//! Neither rule knows about a database, an HTTP client or a particular partner, which is
//! why both are tested here against fixtures rather than against BASIS.

use otdel_core::extraction::{MaterialPage, PageStatus};
use otdel_core::passport::{
    CoverageState, DeclarationTopic, PageDisposition, RequirementsState, RunCoverage,
};
use uuid::Uuid;

/// Upper bound on the coverage notes kept on a run. The reasons repeat; a run record is
/// not a log file. Matches the column's own limit in `0009_product_passports.sql`.
const MAX_COVERAGE_NOTES: usize = 100;

/// One page's line in the plan, before the run has happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPage {
    pub page_id: Uuid,
    pub page_number: i32,
    /// What is already known about this page from the reading phase. `Processed` is never
    /// the initial value — it is only reachable through [`CoveragePlan::settle`].
    pub disposition: PageDisposition,
    pub offered: bool,
    pub chars_sent: i32,
    pub batch_index: Option<i32>,
    pub reason: Option<String>,
}

impl PlannedPage {
    fn unreadable(page: &MaterialPage, disposition: PageDisposition) -> Self {
        Self {
            page_id: page.id,
            page_number: page.page_number,
            disposition,
            offered: false,
            chars_sent: 0,
            batch_index: None,
            reason: Some(disposition.describe().to_owned()),
        }
    }
}

/// Every page of one material, each with what is to become of it.
///
/// The plan is the denominator. `pages_total` comes from here, not from the catalogue of
/// quotable pages, which is precisely the substitution that let eight pages disappear:
/// the old run counted the pages it *could* send and called that the material.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoveragePlan {
    pages: Vec<PlannedPage>,
}

impl CoveragePlan {
    /// Build the plan from the material's whole page inventory.
    ///
    /// `offerable` names the pages that carry quotable text — the ones the catalogue was
    /// built from. Everything else is classified from its stored status, so "awaiting
    /// recognition" and "failed to read" stay different answers instead of collapsing
    /// into absence.
    pub fn build(pages: &[MaterialPage], offerable: &[Uuid]) -> Self {
        let mut planned: Vec<PlannedPage> = pages
            .iter()
            .map(|page| {
                if offerable.contains(&page.id) {
                    // Offerable but not yet sent: until `settle` says otherwise, the
                    // honest description is that this pass did not include it.
                    return PlannedPage {
                        page_id: page.id,
                        page_number: page.page_number,
                        disposition: PageDisposition::ExcludedByRequest,
                        offered: false,
                        chars_sent: 0,
                        batch_index: None,
                        reason: Some(PageDisposition::ExcludedByRequest.describe().to_owned()),
                    };
                }

                let disposition = match page.status {
                    PageStatus::NeedsOcr => PageDisposition::UnreadableNeedsOcr,
                    PageStatus::Failed => PageDisposition::UnreadableFailed,
                    PageStatus::Empty => PageDisposition::UnreadableEmpty,
                    // Inventoried and not reached yet. Distinct from a failure on
                    // purpose: "the reader has not got there" and "the reader could not
                    // read it" call for different actions, and collapsing them is how a
                    // queue stops making progress without anybody noticing.
                    PageStatus::Pending => PageDisposition::NotReadYet,
                    // Read, present, and still not offerable: the stored text is blank.
                    PageStatus::Extracted | PageStatus::Partial => {
                        PageDisposition::NotOfferedNoText
                    }
                };
                PlannedPage::unreadable(page, disposition)
            })
            .collect();

        planned.sort_by_key(|page| page.page_number);
        Self { pages: planned }
    }

    pub fn pages(&self) -> &[PlannedPage] {
        &self.pages
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Pages that a later pass could still turn into knowledge.
    pub fn resumable(&self) -> Vec<Uuid> {
        self.pages
            .iter()
            .filter(|page| page.disposition.is_resumable())
            .map(|page| page.page_id)
            .collect()
    }

    /// Record what the run actually did.
    ///
    /// `processed` lists `(page_id, batch_index, chars_sent)` for every page whose request
    /// came back; `deferred` lists the pages a budget stopped. A page in neither list keeps
    /// the disposition the plan gave it — which is why nothing can vanish here either.
    pub fn settle(&mut self, processed: &[ProcessedPage], deferred: &[Uuid]) {
        for page in &mut self.pages {
            if let Some(done) = processed.iter().find(|done| done.page_id == page.page_id) {
                page.disposition = PageDisposition::Processed;
                page.offered = true;
                page.chars_sent = done.chars_sent;
                page.batch_index = Some(done.batch_index);
                page.reason = None;
                continue;
            }
            if deferred.contains(&page.page_id) {
                page.disposition = PageDisposition::DeferredBudget;
                page.offered = false;
                page.chars_sent = 0;
                page.batch_index = None;
                page.reason = Some(PageDisposition::DeferredBudget.describe().to_owned());
            }
        }
    }

    /// The counters, the verdict and the sentences that explain it.
    ///
    /// `requirements` is folded in by the caller after the draft is known; this half only
    /// judges the pages.
    pub fn summarise(&self) -> RunCoverage {
        let total = self.pages.len();
        let processed = self.count(|page| page.disposition == PageDisposition::Processed);
        let deferred = self.count(|page| page.disposition == PageDisposition::DeferredBudget);
        let unreadable = self.count(|page| page.disposition.is_unreadable());
        let offered = self.count(|page| page.offered);
        let excluded = self.count(|page| page.disposition == PageDisposition::ExcludedByRequest);

        let mut notes: Vec<String> = Vec::new();
        let push = |note: String, notes: &mut Vec<String>| {
            if notes.len() < MAX_COVERAGE_NOTES && !notes.contains(&note) {
                notes.push(note);
            }
        };

        // Per-reason roll-ups with the page numbers named. "Восемь страниц пропущено" with
        // no numbers is the report the audit could not act on.
        for disposition in [
            PageDisposition::DeferredBudget,
            PageDisposition::UnreadableNeedsOcr,
            PageDisposition::UnreadableFailed,
            PageDisposition::UnreadableEmpty,
            PageDisposition::NotReadYet,
            PageDisposition::NotOfferedNoText,
            PageDisposition::ExcludedByRequest,
        ] {
            let numbers: Vec<String> = self
                .pages
                .iter()
                .filter(|page| page.disposition == disposition)
                .map(|page| page.page_number.to_string())
                .collect();
            if numbers.is_empty() {
                continue;
            }
            push(
                format!(
                    "{}: {} стр. ({})",
                    disposition.describe(),
                    numbers.len(),
                    summarise_numbers(&numbers)
                ),
                &mut notes,
            );
        }

        let state = if total == 0 {
            // A material with no pages at all cannot be called covered, and the reason is
            // worth stating rather than reporting 0 of 0 as success.
            push(
                "в материале нет ни одной страницы: разбирать нечего".to_owned(),
                &mut notes,
            );
            CoverageState::Incomplete
        } else if processed == total {
            CoverageState::Complete
        } else if deferred > 0 || excluded > 0 {
            // Something is still waiting on a budget or on a later pass. `partial_accounted`
            // is unavailable by construction: it would claim the material is as covered as
            // it can be, which is untrue while a page is queued.
            CoverageState::Incomplete
        } else {
            // Everything that is not processed is unreadable, and every unreadable page
            // carries its reason: the dispositions partition the plan, so this branch is
            // reached only when `processed + unreadable == total`.
            debug_assert_eq!(processed + unreadable, total);
            CoverageState::PartialAccounted
        };

        RunCoverage {
            pages_total: to_i32(total),
            pages_offered: to_i32(offered),
            pages_processed: to_i32(processed),
            pages_deferred: to_i32(deferred),
            pages_unreadable: to_i32(unreadable),
            state,
            notes,
            requirements: RequirementsState::Unknown,
            requirements_missing: Vec::new(),
            prompt_tokens: None,
            completion_tokens: None,
            cost_micro_usd: None,
        }
    }

    fn count(&self, predicate: impl Fn(&PlannedPage) -> bool) -> usize {
        self.pages.iter().filter(|page| predicate(page)).count()
    }
}

/// One page a request really carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessedPage {
    pub page_id: Uuid,
    /// 1-based index of the request within the run.
    pub batch_index: i32,
    pub chars_sent: i32,
}

/// What a run produced, reduced to the counts the requirement check needs.
///
/// Deliberately counts rather than the rows themselves: the rule must be the same whether
/// it is asked about a draft in memory or about rows already stored, and a shape that can
/// only be built from one of the two would end up implemented twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftSnapshot {
    pub products_total: usize,
    /// Products carrying a summary — the sentence a passport opens with.
    pub products_with_summary: usize,
    pub applications_total: usize,
    pub terms_total: usize,
    pub questions_total: usize,
    /// Gaps the run classified as commercial (price, lead time, minimum order…).
    pub commercial_gaps: usize,
    /// Gaps the run classified as technical.
    pub technical_gaps: usize,
    /// Topics the run explicitly declared empty, with words on the record.
    pub declared: Vec<DeclarationTopic>,
}

impl DraftSnapshot {
    fn declares(&self, topic: DeclarationTopic) -> bool {
        self.declared.contains(&topic)
    }
}

/// One requirement, and whether this draft satisfies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    /// Stable identifier stored in `requirements_missing` and shown in the interface.
    pub name: &'static str,
    /// One sentence explaining what is missing, in Russian.
    pub explanation: &'static str,
}

/// The requirements a run must satisfy before anything may be published from it without
/// a person reading it first.
///
/// Each one can be satisfied in exactly two ways: the rows exist, or the run said out
/// loud that there are none. That second half is the whole design. An empty `glossary`
/// array is not an answer to "does this catalogue introduce terms" — it is the absence of
/// one, and the audited run passed every check it had precisely by producing nothing.
pub const REQUIREMENTS: [Requirement; 6] = [
    Requirement {
        name: "products",
        explanation: "в разборе нет ни одного изделия или услуги",
    },
    Requirement {
        name: "product_summary",
        explanation: "ни у одного изделия нет краткого описания: паспорту не с чего начаться",
    },
    Requirement {
        name: "applications",
        explanation: "нет ни одной задачи применения и не сказано, что их в материале нет",
    },
    Requirement {
        name: "glossary",
        explanation: "нет терминов и не сказано, что материал не вводит терминов",
    },
    Requirement {
        name: "questions",
        explanation: "нет ни одного подготовленного вопроса и не сказано, что спрашивать нечего",
    },
    Requirement {
        name: "commercial_unknowns",
        explanation: "коммерческие неизвестные (цена, срок, партия) не зафиксированы как \
                      пробелы и не объявлены отсутствующими",
    },
];

/// The technical half of the commercial/technical pair, kept separate so a catalogue that
/// states loads but no prices fails exactly one of them.
pub const TECHNICAL_REQUIREMENT: Requirement = Requirement {
    name: "technical_unknowns",
    explanation: "технические неизвестные не зафиксированы как пробелы и не объявлены \
                  отсутствующими",
};

/// The verdict, and the names of everything that is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementsOutcome {
    pub state: RequirementsState,
    /// `name: explanation` lines, stored on the run and shown as-is.
    pub missing: Vec<String>,
}

/// Decide whether a draft carries what a passport needs.
///
/// Returns [`RequirementsState::Unmet`] with the reasons, or [`RequirementsState::Met`]
/// with nothing. There is no third outcome and no threshold: every clause below is a
/// yes/no question about rows that either exist or were explicitly declared absent.
pub fn evaluate_requirements(snapshot: &DraftSnapshot) -> RequirementsOutcome {
    let mut missing: Vec<String> = Vec::new();
    let fail = |requirement: &Requirement, missing: &mut Vec<String>| {
        missing.push(format!("{}: {}", requirement.name, requirement.explanation));
    };

    if snapshot.products_total == 0 {
        fail(&REQUIREMENTS[0], &mut missing);
    } else if snapshot.products_with_summary == 0 {
        fail(&REQUIREMENTS[1], &mut missing);
    }

    if snapshot.applications_total == 0 && !snapshot.declares(DeclarationTopic::Applications) {
        fail(&REQUIREMENTS[2], &mut missing);
    }
    if snapshot.terms_total == 0 && !snapshot.declares(DeclarationTopic::Glossary) {
        fail(&REQUIREMENTS[3], &mut missing);
    }
    if snapshot.questions_total == 0 && !snapshot.declares(DeclarationTopic::Questions) {
        fail(&REQUIREMENTS[4], &mut missing);
    }
    if snapshot.commercial_gaps == 0 && !snapshot.declares(DeclarationTopic::CommercialUnknowns) {
        fail(&REQUIREMENTS[5], &mut missing);
    }
    if snapshot.technical_gaps == 0 && !snapshot.declares(DeclarationTopic::TechnicalUnknowns) {
        fail(&TECHNICAL_REQUIREMENT, &mut missing);
    }

    if missing.is_empty() {
        RequirementsOutcome {
            state: RequirementsState::Met,
            missing,
        }
    } else {
        RequirementsOutcome {
            state: RequirementsState::Unmet,
            missing,
        }
    }
}

/// `1, 2, 3, 7` → `1–3, 7`. Page lists get long; a run record is read by a person.
fn summarise_numbers(numbers: &[String]) -> String {
    let parsed: Vec<i32> = numbers.iter().filter_map(|n| n.parse().ok()).collect();
    if parsed.len() != numbers.len() {
        return numbers.join(", ");
    }

    let mut ranges: Vec<(i32, i32)> = Vec::new();
    for number in parsed {
        match ranges.last_mut() {
            Some(last) if last.1 + 1 == number => last.1 = number,
            _ => ranges.push((number, number)),
        }
    }

    ranges
        .iter()
        .map(|(from, to)| {
            if from == to {
                from.to_string()
            } else {
                format!("{from}–{to}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use otdel_core::extraction::TextSource;
    use otdel_core::extraction_context::DiagramInterpretation;

    fn page(number: i32, status: PageStatus) -> MaterialPage {
        MaterialPage {
            id: Uuid::from_u128(u128::try_from(number).unwrap()),
            material_id: Uuid::from_u128(1_000),
            page_number: number,
            status,
            text_source: TextSource::TextLayer,
            char_count: 100,
            word_count: 20,
            image_count: 0,
            width_pt: None,
            height_pt: None,
            rotation: 0,
            parser_name: None,
            parser_version: None,
            ocr_engine: None,
            ocr_version: None,
            ocr_language: None,
            duration_ms: None,
            attempts: 1,
            diagnostic: None,
            extracted_at: Some(Utc::now()),
            region_count: 0,
            table_count: 0,
            extraction_revision: None,
            drawing_count: 0,
            diagram_interpretation: DiagramInterpretation::None,
        }
    }

    fn processed(number: i32, batch: i32) -> ProcessedPage {
        ProcessedPage {
            page_id: Uuid::from_u128(u128::try_from(number).unwrap()),
            batch_index: batch,
            chars_sent: 900,
        }
    }

    /// The audited shape, generically: a material whose readable pages are all processed
    /// and whose unreadable ones are named rather than absent.
    #[test]
    fn every_page_of_the_material_is_in_the_account() {
        let pages: Vec<MaterialPage> = (1..=10)
            .map(|number| {
                let status = if number > 8 {
                    PageStatus::NeedsOcr
                } else {
                    PageStatus::Extracted
                };
                page(number, status)
            })
            .collect();
        let offerable: Vec<Uuid> = pages[..8].iter().map(|page| page.id).collect();

        let mut plan = CoveragePlan::build(&pages, &offerable);
        assert_eq!(
            plan.pages().len(),
            10,
            "the denominator is the whole material"
        );

        let done: Vec<ProcessedPage> = (1..=8).map(|number| processed(number, 1)).collect();
        plan.settle(&done, &[]);
        let coverage = plan.summarise();

        assert_eq!(coverage.pages_total, 10);
        assert_eq!(coverage.pages_processed, 8);
        assert_eq!(coverage.pages_unreadable, 2);
        assert_eq!(coverage.pages_deferred, 0);
        assert_eq!(coverage.unaccounted(), 0);
        assert_eq!(coverage.state, CoverageState::PartialAccounted);
        assert!(
            coverage.notes.iter().any(|note| note.contains("9–10")),
            "the pages nobody could read are named: {:?}",
            coverage.notes
        );
    }

    #[test]
    fn a_material_read_end_to_end_is_complete() {
        let pages: Vec<MaterialPage> = (1..=3).map(|n| page(n, PageStatus::Extracted)).collect();
        let offerable: Vec<Uuid> = pages.iter().map(|page| page.id).collect();
        let mut plan = CoveragePlan::build(&pages, &offerable);
        plan.settle(&[processed(1, 1), processed(2, 1), processed(3, 2)], &[]);

        let coverage = plan.summarise();
        assert_eq!(coverage.state, CoverageState::Complete);
        assert!(coverage.notes.is_empty());
        assert!(coverage.state.allows_automatic_publication());
    }

    #[test]
    fn a_run_stopped_by_its_budget_is_never_complete_and_says_which_pages_wait() {
        let pages: Vec<MaterialPage> = (1..=6).map(|n| page(n, PageStatus::Extracted)).collect();
        let offerable: Vec<Uuid> = pages.iter().map(|page| page.id).collect();
        let mut plan = CoveragePlan::build(&pages, &offerable);

        let deferred: Vec<Uuid> = pages[3..].iter().map(|page| page.id).collect();
        plan.settle(
            &[processed(1, 1), processed(2, 1), processed(3, 1)],
            &deferred,
        );

        let coverage = plan.summarise();
        assert_eq!(coverage.pages_processed, 3);
        assert_eq!(coverage.pages_deferred, 3);
        assert_eq!(coverage.state, CoverageState::Incomplete);
        assert!(
            !coverage.state.allows_automatic_publication(),
            "a material with pages still queued must not publish itself"
        );
        assert_eq!(
            plan.resumable().len(),
            3,
            "the remainder is queued, not dropped"
        );
        assert!(coverage.notes.iter().any(|note| note.contains("4–6")));
    }

    #[test]
    fn a_page_the_run_never_mentioned_keeps_its_reason_and_blocks_completion() {
        let pages: Vec<MaterialPage> = (1..=4).map(|n| page(n, PageStatus::Extracted)).collect();
        let offerable: Vec<Uuid> = pages.iter().map(|page| page.id).collect();
        let mut plan = CoveragePlan::build(&pages, &offerable);
        // Page 4 is in neither list — the exact shape of the eight silent pages.
        plan.settle(&[processed(1, 1), processed(2, 1), processed(3, 1)], &[]);

        let coverage = plan.summarise();
        assert_eq!(coverage.state, CoverageState::Incomplete);
        let orphan = plan
            .pages()
            .iter()
            .find(|page| page.page_number == 4)
            .unwrap();
        assert_eq!(orphan.disposition, PageDisposition::ExcludedByRequest);
        assert!(orphan.reason.is_some(), "no page leaves without a reason");
    }

    #[test]
    fn an_empty_material_is_incomplete_rather_than_trivially_covered() {
        let plan = CoveragePlan::build(&[], &[]);
        let coverage = plan.summarise();
        assert_eq!(coverage.state, CoverageState::Incomplete);
        assert!(!coverage.notes.is_empty());
    }

    // --- requirements ---------------------------------------------------------------

    fn full() -> DraftSnapshot {
        DraftSnapshot {
            products_total: 3,
            products_with_summary: 2,
            applications_total: 4,
            terms_total: 5,
            questions_total: 2,
            commercial_gaps: 1,
            technical_gaps: 1,
            declared: Vec::new(),
        }
    }

    #[test]
    fn a_complete_draft_meets_every_requirement() {
        let outcome = evaluate_requirements(&full());
        assert_eq!(outcome.state, RequirementsState::Met);
        assert!(outcome.missing.is_empty());
    }

    /// The whole point of the package, as one test: nothing at all must not pass.
    #[test]
    fn an_empty_draft_satisfies_nothing() {
        let outcome = evaluate_requirements(&DraftSnapshot::default());
        assert_eq!(outcome.state, RequirementsState::Unmet);
        // products, applications, glossary, questions, commercial, technical — six.
        // `product_summary` is not among them: there are no products to summarise yet,
        // and reporting both would be two names for one problem.
        assert_eq!(outcome.missing.len(), 6, "{:?}", outcome.missing);
        assert!(outcome
            .missing
            .iter()
            .any(|line| line.starts_with("products:")));
        assert!(outcome
            .missing
            .iter()
            .any(|line| line.starts_with("glossary:")));
    }

    /// Exactly the audited draft: products and a handful of facts, and silence elsewhere.
    #[test]
    fn products_without_anything_else_do_not_pass() {
        let snapshot = DraftSnapshot {
            products_total: 44,
            products_with_summary: 44,
            ..DraftSnapshot::default()
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert_eq!(outcome.missing.len(), 5, "{:?}", outcome.missing);
    }

    #[test]
    fn an_explicit_declaration_satisfies_a_requirement_and_an_empty_array_does_not() {
        let bare = DraftSnapshot {
            products_total: 1,
            products_with_summary: 1,
            applications_total: 1,
            terms_total: 0,
            questions_total: 1,
            commercial_gaps: 1,
            technical_gaps: 1,
            declared: Vec::new(),
        };
        assert_eq!(
            evaluate_requirements(&bare).state,
            RequirementsState::Unmet,
            "an empty glossary alone is not an answer"
        );

        let declared = DraftSnapshot {
            declared: vec![DeclarationTopic::Glossary],
            ..bare
        };
        assert_eq!(
            evaluate_requirements(&declared).state,
            RequirementsState::Met
        );
    }

    #[test]
    fn a_declaration_for_one_topic_does_not_cover_another() {
        let snapshot = DraftSnapshot {
            products_total: 1,
            products_with_summary: 1,
            applications_total: 1,
            terms_total: 1,
            questions_total: 0,
            commercial_gaps: 0,
            technical_gaps: 1,
            declared: vec![DeclarationTopic::Questions],
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert_eq!(outcome.missing.len(), 1);
        assert!(outcome.missing[0].starts_with("commercial_unknowns:"));
    }

    #[test]
    fn a_product_without_a_summary_is_named_as_such() {
        let snapshot = DraftSnapshot {
            products_with_summary: 0,
            ..full()
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.missing.len(), 1);
        assert!(outcome.missing[0].starts_with("product_summary:"));
    }

    #[test]
    fn page_numbers_are_summarised_as_ranges() {
        assert_eq!(
            summarise_numbers(&["1".into(), "2".into(), "3".into(), "7".into()]),
            "1–3, 7"
        );
        assert_eq!(summarise_numbers(&["5".into()]), "5");
    }
}
