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

/// The scale of the pass a declaration is asked to speak for.
///
/// A declaration is one sentence. The owner is expected to be able to check it by reading
/// the material — and that is a real, bounded task for a short document and an unbounded
/// one for a catalogue. Past these sizes a sentence stops being a check anybody will redo
/// and becomes a rubber stamp, so the honest record is not "cleared" but "nobody has
/// confirmed this".
///
/// The two limits are deliberately small. They are not a guess at where a model becomes
/// unreliable; they are the point past which *a person* will not re-read the document to
/// disagree, which is the only thing that made a declaration trustworthy in the first
/// place.
const MAX_PAGES_ONE_SENTENCE_MAY_SPEAK_FOR: usize = 4;
const MAX_PRODUCTS_ONE_SENTENCE_MAY_SPEAK_FOR: usize = 3;

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
    /// Facts the run classified as commercial. A *stated* price, lead time or minimum
    /// order — the thing whose absence would make it an unknown instead.
    pub commercial_facts: usize,
    /// Topics the run explicitly declared empty, with words on the record.
    pub declared: Vec<DeclarationTopic>,
    /// Pages this pass actually processed.
    pub pages_processed: usize,
    /// Readings this run could not settle: an ambiguous cell, a unit written nowhere, a
    /// page nobody could read. Each one is something still open in this material.
    pub open_uncertainties: usize,
}

/// The run context a [`DraftSnapshot`] cannot get from the candidates alone.
///
/// Separate from the draft because neither number is a candidate: the page count belongs
/// to the coverage plan and the uncertainties come from R03's table cells. Passing them in
/// explicitly keeps [`crate::candidate::CandidateDraft::snapshot`] from having to reach
/// for state it does not own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunContext {
    pub pages_processed: usize,
    pub open_uncertainties: usize,
}

/// Why a declaration did not clear its topic — or that it did.
///
/// Three outcomes rather than a boolean, because the two refusals call for different
/// actions. A contradicted declaration means the run disagreed with itself and the draft
/// is wrong; a disproportionate one means nobody has checked yet and a person must.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclarationVerdict {
    /// Small enough to be checkable, and nothing in the run contradicts it.
    Accepted,
    /// The same run produced something that makes the statement false.
    Contradicted(String),
    /// Too much material for one sentence to answer for.
    Disproportionate(String),
}

impl DraftSnapshot {
    fn declares(&self, topic: DeclarationTopic) -> bool {
        self.declared.contains(&topic)
    }

    /// Whether "there is none" on this topic may stand as this run's answer.
    ///
    /// The declaration was already checked once, at validation time, against the response
    /// that carried it: a response claiming "no terms" beside eleven terms is refused
    /// there. This is the second check, and it is the one the audited live run needed —
    /// it asks whether the *whole run* leaves the statement standing.
    ///
    /// The contradiction rules below are not heuristics about wording. Each is the topic's
    /// own meaning, read back:
    ///
    /// * an unresolved unit, an ambiguous load cell or a page nobody could read **is** a
    ///   technical unknown, so "no technical unknowns remain" cannot be said while the run
    ///   holds one;
    /// * the same readings are, by construction, things somebody has to settle, so
    ///   "nothing to ask" cannot be said either;
    /// * "nothing commercial is missing" requires that something commercial is *present*.
    ///   A run that recorded no commercial fact has not established that prices are
    ///   stated — it has established that they are not.
    fn declaration_verdict(&self, topic: DeclarationTopic) -> DeclarationVerdict {
        if let Some(reason) = self.contradiction(topic) {
            return DeclarationVerdict::Contradicted(reason);
        }
        if self.pages_processed > MAX_PAGES_ONE_SENTENCE_MAY_SPEAK_FOR
            || self.products_total > MAX_PRODUCTS_ONE_SENTENCE_MAY_SPEAK_FOR
        {
            return DeclarationVerdict::Disproportionate(format!(
                "заявление «в материале этого нет» не принято: разобрано страниц {}, \
                 изделий {} — отсутствие в таком объёме одним предложением не \
                 подтверждается, нужен человек",
                self.pages_processed, self.products_total
            ));
        }
        DeclarationVerdict::Accepted
    }

    fn contradiction(&self, topic: DeclarationTopic) -> Option<String> {
        match topic {
            DeclarationTopic::TechnicalUnknowns if self.open_uncertainties > 0 => Some(format!(
                "заявление «технических неизвестных нет» противоречит этому же разбору: \
                 нерешённых чтений {} (единица не написана, ячейка неоднозначна, \
                 страница не прочитана) — каждое из них и есть техническая неизвестность",
                self.open_uncertainties
            )),
            DeclarationTopic::Questions if self.open_uncertainties > 0 => Some(format!(
                "заявление «спрашивать нечего» противоречит этому же разбору: \
                 нерешённых чтений {} — каждое требует, чтобы кто-то его разъяснил",
                self.open_uncertainties
            )),
            DeclarationTopic::CommercialUnknowns if self.commercial_facts == 0 => Some(
                "заявление «коммерческих неизвестных нет» не принято: в разборе нет ни \
                 одного коммерческого факта. Сказать, что цена и сроки не отсутствуют, \
                 можно только если они в материале названы"
                    .to_owned(),
            ),
            _ => None,
        }
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
/// with nothing.
///
/// Each topic is satisfied by rows, or by a declaration **that survives the run**. The
/// second half changed after a live pass over a real 44-page technical catalogue came back
/// `requirements = met` with 0 terms, 0 applications, 123 unsettled readings — and five
/// declarations saying there was nothing to find. The original rule accepted any
/// declaration, so the audited failure had simply moved: instead of an empty array passing
/// silently, a self-serving sentence passed it. A declaration is now refused when the same
/// run contradicts it or when it is asked to answer for more material than one sentence
/// can, and the refusal is reported under the topic's own name with the reason attached.
pub fn evaluate_requirements(snapshot: &DraftSnapshot) -> RequirementsOutcome {
    let mut missing: Vec<String> = Vec::new();
    let fail = |requirement: &Requirement, missing: &mut Vec<String>| {
        missing.push(format!("{}: {}", requirement.name, requirement.explanation));
    };

    // A topic with no rows: satisfied only by a declaration this run leaves standing.
    // When the declaration is refused, the topic is reported under its own name with the
    // refusal as the explanation — never with the generic "nothing was said", which would
    // hide that something *was* said and was not good enough.
    let absent = |requirement: &Requirement, topic: DeclarationTopic, missing: &mut Vec<String>| {
        if !snapshot.declares(topic) {
            fail(requirement, missing);
            return;
        }
        match snapshot.declaration_verdict(topic) {
            DeclarationVerdict::Accepted => {}
            DeclarationVerdict::Contradicted(reason)
            | DeclarationVerdict::Disproportionate(reason) => {
                missing.push(format!("{}: {}", requirement.name, reason));
            }
        }
    };

    if snapshot.products_total == 0 {
        fail(&REQUIREMENTS[0], &mut missing);
    } else if snapshot.products_with_summary == 0 {
        fail(&REQUIREMENTS[1], &mut missing);
    }

    if snapshot.applications_total == 0 {
        absent(
            &REQUIREMENTS[2],
            DeclarationTopic::Applications,
            &mut missing,
        );
    }
    if snapshot.terms_total == 0 {
        absent(&REQUIREMENTS[3], DeclarationTopic::Glossary, &mut missing);
    }
    if snapshot.questions_total == 0 {
        absent(&REQUIREMENTS[4], DeclarationTopic::Questions, &mut missing);
    }
    if snapshot.commercial_gaps == 0 {
        absent(
            &REQUIREMENTS[5],
            DeclarationTopic::CommercialUnknowns,
            &mut missing,
        );
    }
    if snapshot.technical_gaps == 0 {
        absent(
            &TECHNICAL_REQUIREMENT,
            DeclarationTopic::TechnicalUnknowns,
            &mut missing,
        );
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
            commercial_facts: 0,
            declared: Vec::new(),
            pages_processed: 3,
            open_uncertainties: 0,
        }
    }

    /// A short material that already satisfies everything with rows.
    ///
    /// Short on purpose: a declaration over this much material is proportionate, so a test
    /// that switches one topic off and declares it is testing the declaration rule and
    /// nothing else.
    fn small() -> DraftSnapshot {
        DraftSnapshot {
            products_total: 1,
            products_with_summary: 1,
            applications_total: 1,
            terms_total: 1,
            questions_total: 1,
            commercial_gaps: 1,
            technical_gaps: 1,
            commercial_facts: 0,
            declared: Vec::new(),
            pages_processed: 2,
            open_uncertainties: 0,
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
            terms_total: 0,
            ..small()
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
            questions_total: 0,
            commercial_gaps: 0,
            declared: vec![DeclarationTopic::Questions],
            ..small()
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert_eq!(outcome.missing.len(), 1);
        assert!(outcome.missing[0].starts_with("commercial_unknowns:"));
    }

    /// The live regression, at the rule that let it through.
    ///
    /// A real pass over a technical catalogue reported `requirements = met` with 0 terms,
    /// 0 applications and 123 unsettled readings, on the strength of five sentences saying
    /// there was nothing to find. Generic here — the numbers are the shape of the run, not
    /// a particular partner's.
    fn declared_everything_over_a_large_material() -> DraftSnapshot {
        DraftSnapshot {
            products_total: 31,
            products_with_summary: 31,
            applications_total: 0,
            terms_total: 0,
            questions_total: 0,
            commercial_gaps: 0,
            technical_gaps: 0,
            commercial_facts: 0,
            declared: DeclarationTopic::ALL.to_vec(),
            pages_processed: 44,
            open_uncertainties: 123,
        }
    }

    #[test]
    fn declaring_every_topic_empty_over_a_large_material_clears_nothing() {
        let outcome = evaluate_requirements(&declared_everything_over_a_large_material());

        assert_eq!(
            outcome.state,
            RequirementsState::Unmet,
            "five sentences must not clear a forty-four page catalogue: {:?}",
            outcome.missing
        );
        // Every topic that has no rows is named, declaration or not.
        assert_eq!(outcome.missing.len(), 5, "{:?}", outcome.missing);
        for topic in [
            "applications:",
            "glossary:",
            "questions:",
            "commercial_unknowns:",
            "technical_unknowns:",
        ] {
            assert!(
                outcome.missing.iter().any(|line| line.starts_with(topic)),
                "{topic} is not named in {:?}",
                outcome.missing
            );
        }
    }

    /// The refusals say *which* kind of refusal they are, because the two need different
    /// actions: a contradiction means the draft is wrong, a disproportion means nobody has
    /// checked yet.
    #[test]
    fn a_refused_declaration_explains_itself_rather_than_reading_as_silence() {
        let outcome = evaluate_requirements(&declared_everything_over_a_large_material());
        let line = |topic: &str| {
            outcome
                .missing
                .iter()
                .find(|line| line.starts_with(topic))
                .unwrap_or_else(|| panic!("{topic} missing from {:?}", outcome.missing))
                .clone()
        };

        // Contradicted: the run is holding the very things it says are not there.
        assert!(line("technical_unknowns:").contains("противоречит"));
        assert!(line("technical_unknowns:").contains("123"));
        assert!(line("questions:").contains("противоречит"));
        // "Nothing commercial is missing" needs something commercial to be present.
        assert!(line("commercial_unknowns:").contains("коммерческого факта"));
        // Disproportionate: nobody will re-read 44 pages to disagree with one sentence.
        assert!(line("glossary:").contains("нужен человек"));
        assert!(line("applications:").contains("44"));
        // None of them reads as "nothing was said": something was said and was refused.
        assert!(!line("glossary:").contains("не сказано"));
    }

    #[test]
    fn an_unsettled_reading_is_itself_a_technical_unknown_and_a_question() {
        // Small enough to be proportionate, so only the contradiction can fail it.
        let snapshot = DraftSnapshot {
            questions_total: 0,
            technical_gaps: 0,
            open_uncertainties: 1,
            declared: vec![
                DeclarationTopic::Questions,
                DeclarationTopic::TechnicalUnknowns,
            ],
            ..small()
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert_eq!(outcome.missing.len(), 2, "{:?}", outcome.missing);

        // …and with nothing unsettled, the same two declarations stand.
        let settled = DraftSnapshot {
            open_uncertainties: 0,
            ..snapshot
        };
        assert_eq!(
            evaluate_requirements(&settled).state,
            RequirementsState::Met
        );
    }

    #[test]
    fn nothing_commercial_is_missing_only_if_something_commercial_is_stated() {
        let snapshot = DraftSnapshot {
            commercial_gaps: 0,
            commercial_facts: 0,
            declared: vec![DeclarationTopic::CommercialUnknowns],
            ..small()
        };
        let outcome = evaluate_requirements(&snapshot);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert!(outcome.missing[0].starts_with("commercial_unknowns:"));

        // The same declaration beside a stated price is an observation, not a dodge.
        let priced = DraftSnapshot {
            commercial_facts: 1,
            ..snapshot
        };
        assert_eq!(evaluate_requirements(&priced).state, RequirementsState::Met);
    }

    /// A small material keeps the escape hatch: the rule limits it, it does not remove it.
    #[test]
    fn a_short_material_may_still_say_there_is_none() {
        let snapshot = DraftSnapshot {
            terms_total: 0,
            applications_total: 0,
            declared: vec![DeclarationTopic::Glossary, DeclarationTopic::Applications],
            ..small()
        };
        assert_eq!(
            evaluate_requirements(&snapshot).state,
            RequirementsState::Met
        );

        // One page more than a person will re-check, and the same sentences defer.
        let larger = DraftSnapshot {
            pages_processed: MAX_PAGES_ONE_SENTENCE_MAY_SPEAK_FOR + 1,
            ..snapshot
        };
        let outcome = evaluate_requirements(&larger);
        assert_eq!(outcome.state, RequirementsState::Unmet);
        assert_eq!(outcome.missing.len(), 2, "{:?}", outcome.missing);
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
