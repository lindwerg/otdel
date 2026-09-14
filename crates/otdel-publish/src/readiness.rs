//! The readiness matrix, and the rules that decide it.
//!
//! `block-01-spec.md` §7 asks for readiness to be determined **separately** for
//! describing a product, proposing an audience, answering about characteristics and
//! answering about commercial terms. Separately is the whole point: a catalogue that
//! states loads and no prices is ready for one and blocked for another, and a single
//! "готово" would either hide real knowledge or imply terms nobody wrote down. §13.7
//! puts the same rule from the other side — partial readiness must not look like a full
//! commercial clearance.
//!
//! **This is availability of knowledge, never permission.** Nothing here authorises a
//! message, a promise of technical compatibility or an obligation; the interface is
//! required to say so beside it, and the phase documentation repeats it.
//!
//! Every rule below is a count over verdicts **and** a check of the fields that topic's
//! question actually needs. The second half is what F03 found missing: any one supported
//! partner fact reported "Описание продукции: знания есть", and a load with no unit
//! reported "Ответы о характеристиках: готово". A count of verdicts says the knowledge is
//! real; it does not say it answers the question being promised. There is still no model,
//! no score and no threshold anybody has to trust.

use otdel_core::knowledge::FactKind;
use otdel_core::publication::{
    ClaimScope, ClaimStatus, ReadinessEntry, ReadinessState, ReadinessTopic,
};

use otdel_knowledge::measure::{self, mentions};

use crate::chunk::normalise;
use crate::claim::CheckedClaim;

/// A gap as the readiness rules see it: just the words, so the classifier can be tested
/// without a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapText {
    pub topic: String,
    pub missing: String,
    pub blocks: Option<String>,
}

impl GapText {
    fn haystack(&self) -> String {
        normalise(&format!(
            "{} {} {}",
            self.topic,
            self.missing,
            self.blocks.as_deref().unwrap_or_default()
        ))
    }
}

/// Word stems that place a gap against a readiness topic.
///
/// This is a **lexical** rule over a small fixed vocabulary, not an understanding of the
/// sentence, and the documentation says so where a reader can see it. It earns its place
/// because "какие ответы блокирует этот пробел" (`block-01-spec.md` §11) has to be
/// answerable, and because being wrong here only ever adds or omits a caveat — it never
/// turns an unsupported claim into a supported one. A gap that matches nothing still
/// appears in the version; it simply limits no topic by name.
const COMMERCIAL_STEMS: &[&str] = &[
    "цена",
    "цены",
    "цену",
    "ценов",
    "стоимост",
    "прайс",
    "срок",
    "поставк",
    "доставк",
    "оплат",
    "скидк",
    "наличи",
    "минимальная партия",
    "price",
    "cost",
    "lead time",
    "delivery",
];
const CHARACTERISTIC_STEMS: &[&str] = &[
    "размер",
    "длин",
    "ширин",
    "высот",
    "толщин",
    "нагрузк",
    "масс",
    "вес",
    "покрыти",
    "класс",
    // «материал» is deliberately absent. In this product's own vocabulary a *материал*
    // is an uploaded document, so a perfectly ordinary gap — «цена не указана в
    // материале» — was being classified as a gap about the composition of the goods.
    // A stem that collides with the system's own nouns costs more than it earns.
    "сплав",
    "характеристик",
    "параметр",
    "допуск",
    "температур",
    "прочност",
    "сертификат",
    "испытан",
    "dimension",
    "thickness",
    "load",
    "weight",
];
const APPLICATION_STEMS: &[&str] = &[
    "примен",
    "област",
    "монтаж",
    "назначен",
    "использов",
    "совместим",
    "объект",
    "application",
];
const DESCRIPTION_STEMS: &[&str] = &[
    "описан",
    "ассортимент",
    "номенклатур",
    "каталог",
    "линейк",
    "модельн",
    "состав продукции",
];

/// Which readiness topics one gap limits.
pub fn gap_blocks(gap: &GapText) -> Vec<ReadinessTopic> {
    let haystack = gap.haystack();
    let mut topics = Vec::new();
    let table = [
        (ReadinessTopic::CommercialAnswers, COMMERCIAL_STEMS),
        (ReadinessTopic::CharacteristicAnswers, CHARACTERISTIC_STEMS),
        (ReadinessTopic::AudienceHypotheses, APPLICATION_STEMS),
        (ReadinessTopic::ProductDescription, DESCRIPTION_STEMS),
    ];
    for (topic, stems) in table {
        if stems.iter().any(|stem| mentions(&haystack, stem)) {
            topics.push(topic);
        }
    }
    topics
}

/// Decide all four topics.
pub fn assess(claims: &[CheckedClaim], gaps: &[GapText]) -> Vec<ReadinessEntry> {
    let blocking: Vec<(ReadinessTopic, &GapText)> = gaps
        .iter()
        .flat_map(|gap| gap_blocks(gap).into_iter().map(move |topic| (topic, gap)))
        .collect();

    ReadinessTopic::ALL
        .into_iter()
        .map(|topic| assess_topic(topic, claims, &blocking))
        .collect()
}

fn assess_topic(
    topic: ReadinessTopic,
    claims: &[CheckedClaim],
    blocking: &[(ReadinessTopic, &GapText)],
) -> ReadinessEntry {
    let relevant: Vec<&CheckedClaim> = claims.iter().filter(|c| relevant_to(topic, c)).collect();

    let supported_claims: Vec<&CheckedClaim> = relevant
        .iter()
        .copied()
        .filter(|c| c.status == ClaimStatus::SourceSupported)
        .collect();
    let supported = supported_claims.len();
    let conflicted = relevant
        .iter()
        .filter(|c| c.status == ClaimStatus::Conflicted)
        .count();
    let stale = relevant
        .iter()
        .filter(|c| c.status == ClaimStatus::Stale)
        .count();
    let unknown = relevant
        .iter()
        .filter(|c| c.status == ClaimStatus::Unknown)
        .count();
    let hypothesis = relevant
        .iter()
        .filter(|c| c.status == ClaimStatus::Hypothesis)
        .count();

    let gaps_here: Vec<&GapText> = blocking
        .iter()
        .filter(|(gap_topic, _)| *gap_topic == topic)
        .map(|(_, gap)| *gap)
        .collect();

    if supported == 0 {
        let reason = if relevant.is_empty() {
            format!(
                "{}: подтверждённых источником утверждений нет",
                subject(topic)
            )
        } else {
            format!(
                "{}: ни одно из {} утверждений не подтверждено источником (гипотез — {}, \
                 расхождений — {}, устаревших — {}, недоступных — {})",
                subject(topic),
                relevant.len(),
                hypothesis,
                conflicted,
                stale,
                unknown
            )
        };
        return ReadinessEntry {
            topic,
            state: ReadinessState::Blocked,
            reason: with_gaps(reason, &gaps_here),
        };
    }

    let mut caveats: Vec<String> = Vec::new();
    // The rule F03 was missing: a supported claim is not automatically an answer to *this*
    // topic's question. Counting verdicts says the knowledge is real; only the topic's own
    // required fields say it is enough.
    if let Some(shortfall) = shortfall_for(topic, &supported_claims) {
        caveats.push(shortfall);
    }
    if conflicted > 0 {
        caveats.push(format!(
            "по {conflicted} утверждению(ям) источники расходятся — численный ответ по ним не \
             даётся"
        ));
    }
    if stale > 0 {
        caveats.push(format!(
            "{stale} утверждение(й) опирается на изменившийся источник"
        ));
    }
    if unknown > 0 {
        caveats.push(format!("{unknown} источник(ов) недоступно для проверки"));
    }
    for gap in &gaps_here {
        caveats.push(format!("пробел: {}", clip_reason(&gap.missing)));
    }

    if caveats.is_empty() {
        ReadinessEntry {
            topic,
            state: ReadinessState::Ready,
            reason: format!(
                "{}: {supported} утверждение(й) подтверждено источником",
                subject(topic)
            ),
        }
    } else {
        ReadinessEntry {
            topic,
            state: ReadinessState::Limited,
            reason: format!(
                "{}: {supported} утверждение(й) подтверждено источником, но {}",
                subject(topic),
                caveats.join("; ")
            ),
        }
    }
}

/// How many distinct properties of one product it takes before the product is
/// *described* rather than measured once.
///
/// Two is the smallest number that is not one, and one was the defect: a catalogue that
/// yielded a single load figure was reported as "описание продукции: знания есть". This is
/// a floor on completeness, not a score — nothing above it is claimed to be better.
const MIN_DESCRIPTION_PROPERTIES: usize = 2;

/// What this topic still lacks, in words, or `None` when its required fields are there.
///
/// Each branch answers one question: *what does a version need before it may say it can
/// answer this kind of question?* — which is `R01c`'s "Ready определяется по обязательным
/// полям конкретной задачи/продукта, не по одному факту".
fn shortfall_for(topic: ReadinessTopic, supported: &[&CheckedClaim]) -> Option<String> {
    match topic {
        ReadinessTopic::ProductDescription => {
            let described = supported
                .iter()
                .filter_map(|claim| {
                    claim
                        .product_name
                        .as_deref()
                        .map(|product| (normalise(product), normalise(&claim.attribute)))
                })
                .fold(Vec::<(String, Vec<String>)>::new(), |mut acc, (p, a)| {
                    match acc.iter_mut().find(|(product, _)| *product == p) {
                        Some((_, properties)) if !properties.contains(&a) => properties.push(a),
                        Some(_) => {}
                        None => acc.push((p, vec![a])),
                    }
                    acc
                });
            let best = described
                .iter()
                .map(|(_, properties)| properties.len())
                .max()
                .unwrap_or(0);
            (best < MIN_DESCRIPTION_PROPERTIES).then(|| {
                format!(
                    "ни об одном изделии не подтверждено больше одного свойства: этого мало для \
                     описания продукции (нужно не менее {MIN_DESCRIPTION_PROPERTIES})"
                )
            })
        }
        ReadinessTopic::CharacteristicAnswers | ReadinessTopic::CommercialAnswers => {
            let complete = supported.iter().any(|claim| states_a_whole_quantity(claim));
            (!complete).then(|| {
                "ни одна подтверждённая величина не полна: значение без единицы измерения или \
                 повторяющее название свойства не даёт численного ответа"
                    .to_owned()
            })
        }
        // A hypothesis about who buys this is prose by nature; it has no required unit.
        ReadinessTopic::AudienceHypotheses => None,
    }
}

/// Is this claim a whole statement of a quantity?
///
/// A number needs something saying what it measures — the unit column or the value's own
/// wording (`3,5 кН`). A value that only repeats its property's name is a heading, never a
/// quantity, whatever verdict reached it. A value with no number at all is qualitative
/// ("материал: сталь") and is complete as written.
fn states_a_whole_quantity(claim: &CheckedClaim) -> bool {
    if measure::restates_attribute(&claim.attribute, &claim.value_text) {
        return false;
    }
    if !measure::has_number(&claim.value_text) {
        return true;
    }
    claim.unit.is_some() || measure::inline_unit(&claim.value_text).is_some()
}

/// Which claims bear on which topic.
///
/// An industry conclusion never supports a statement about the partner's own offering or
/// its commercial terms — it is about the industry. It is allowed to support an audience
/// hypothesis, which is the one topic that is explicitly a hypothesis about context
/// rather than a claim about this partner's goods.
fn relevant_to(topic: ReadinessTopic, claim: &CheckedClaim) -> bool {
    let is_partner = claim.origin.scope() == ClaimScope::Partner;
    match topic {
        ReadinessTopic::ProductDescription => is_partner,
        ReadinessTopic::CharacteristicAnswers => {
            is_partner && matches!(claim.kind, FactKind::Characteristic | FactKind::Limitation)
        }
        ReadinessTopic::AudienceHypotheses => matches!(claim.kind, FactKind::Application),
        ReadinessTopic::CommercialAnswers => is_partner && claim.kind == FactKind::Commercial,
    }
}

const fn subject(topic: ReadinessTopic) -> &'static str {
    match topic {
        ReadinessTopic::ProductDescription => "Описание продукции",
        ReadinessTopic::AudienceHypotheses => "Гипотезы применения и аудитории",
        ReadinessTopic::CharacteristicAnswers => "Ответы о характеристиках",
        ReadinessTopic::CommercialAnswers => "Ответы о коммерческих условиях",
    }
}

fn with_gaps(reason: String, gaps: &[&GapText]) -> String {
    if gaps.is_empty() {
        return clip_reason(&reason);
    }
    let listed = gaps
        .iter()
        .map(|gap| clip_reason(&gap.missing))
        .collect::<Vec<_>>()
        .join("; ");
    clip_reason(&format!("{reason}. Пробелы: {listed}"))
}

fn clip_reason(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(1_000)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "причина не записана".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use otdel_core::publication::ClaimOrigin;
    use uuid::Uuid;

    /// A complete claim: a value, and a unit that says what the value measures.
    ///
    /// The unit is not decoration in this fixture. Since R01 a number with no unit does
    /// not make a topic ready, so a fixture without one would be testing the incomplete
    /// case in every test that only means to test something else.
    fn claim(kind: FactKind, status: ClaimStatus, origin: ClaimOrigin) -> CheckedClaim {
        CheckedClaim {
            origin,
            origin_id: Uuid::from_u128(1),
            product_name: (origin == ClaimOrigin::PartnerMaterial).then(|| "BP21".to_owned()),
            kind,
            status,
            attribute: "нагрузка".to_owned(),
            value_text: "3.5".to_owned(),
            unit: Some("кН".to_owned()),
            conditions: None,
            model_context: None,
            check_note: None,
            evidence: Vec::new(),
        }
    }

    /// The same claim about another property, so a product can be *described* rather
    /// than measured once.
    fn other_property(kind: FactKind, status: ClaimStatus) -> CheckedClaim {
        let mut claim = claim(kind, status, ClaimOrigin::PartnerMaterial);
        claim.origin_id = Uuid::from_u128(2);
        claim.attribute = "длина".to_owned();
        claim.value_text = "1200".to_owned();
        claim.unit = Some("мм".to_owned());
        claim
    }

    fn reason_of(entries: &[ReadinessEntry], topic: ReadinessTopic) -> String {
        entries
            .iter()
            .find(|entry| entry.topic == topic)
            .expect("every topic is assessed")
            .reason
            .clone()
    }

    fn state_of(entries: &[ReadinessEntry], topic: ReadinessTopic) -> ReadinessState {
        entries
            .iter()
            .find(|entry| entry.topic == topic)
            .expect("every topic is assessed")
            .state
    }

    #[test]
    fn every_topic_is_always_assessed_so_none_is_silently_absent() {
        let entries = assess(&[], &[]);
        assert_eq!(entries.len(), ReadinessTopic::ALL.len());
        for topic in ReadinessTopic::ALL {
            assert_eq!(state_of(&entries, topic), ReadinessState::Blocked);
        }
    }

    #[test]
    fn a_supported_characteristic_makes_characteristics_ready_and_leaves_commerce_blocked() {
        // The BASIS case: a catalogue states loads and no prices. Publishing it as
        // wholly ready would be the failure of `block-01-spec.md` §13.7.
        let claims = vec![claim(
            FactKind::Characteristic,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        )];
        let entries = assess(&claims, &[]);
        assert_eq!(
            state_of(&entries, ReadinessTopic::CharacteristicAnswers),
            ReadinessState::Ready
        );
        assert_eq!(
            state_of(&entries, ReadinessTopic::CommercialAnswers),
            ReadinessState::Blocked
        );
    }

    #[test]
    fn one_fact_about_a_product_is_not_a_description_of_it() {
        // F03: «Описание продукции: знания есть» was reported from any single supported
        // partner fact, so a catalogue that yielded one load figure looked like a product
        // description. One property is one property.
        let claims = vec![claim(
            FactKind::Characteristic,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        )];
        let entries = assess(&claims, &[]);
        assert_eq!(
            state_of(&entries, ReadinessTopic::ProductDescription),
            ReadinessState::Limited
        );
        let reason = reason_of(&entries, ReadinessTopic::ProductDescription);
        assert!(reason.contains("одно"), "{reason}");
    }

    #[test]
    fn two_properties_of_one_product_do_describe_it() {
        let claims = vec![
            claim(
                FactKind::Characteristic,
                ClaimStatus::SourceSupported,
                ClaimOrigin::PartnerMaterial,
            ),
            other_property(FactKind::Characteristic, ClaimStatus::SourceSupported),
        ];
        assert_eq!(
            state_of(&assess(&claims, &[]), ReadinessTopic::ProductDescription),
            ReadinessState::Ready
        );
    }

    #[test]
    fn a_number_with_no_unit_never_makes_characteristic_answers_ready() {
        // The published BASIS row: `2,0/2,5`, no unit, no conditions. The topic is not
        // ready for a technical question, whatever the claim's own verdict is.
        let mut naked = claim(
            FactKind::Characteristic,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        );
        naked.value_text = "2,0/2,5".to_owned();
        naked.unit = None;

        let entries = assess(&[naked], &[]);
        assert_eq!(
            state_of(&entries, ReadinessTopic::CharacteristicAnswers),
            ReadinessState::Limited
        );
        let reason = reason_of(&entries, ReadinessTopic::CharacteristicAnswers);
        assert!(reason.contains("единиц"), "{reason}");
    }

    #[test]
    fn a_price_with_no_currency_never_makes_commercial_answers_ready() {
        let mut naked = claim(
            FactKind::Commercial,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        );
        naked.attribute = "цена".to_owned();
        naked.value_text = "1200".to_owned();
        naked.unit = None;

        assert_eq!(
            state_of(&assess(&[naked], &[]), ReadinessTopic::CommercialAnswers),
            ReadinessState::Limited
        );
    }

    #[test]
    fn a_heading_stored_as_a_value_carries_no_topic_even_if_it_arrives_supported() {
        // The checker lowers this claim, and readiness refuses it a second time: a
        // readiness matrix is read as permission to answer, so it does not depend on one
        // upstream rule having fired.
        let mut heading = claim(
            FactKind::Characteristic,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        );
        heading.attribute = "безопасная рабочая нагрузка (Н)".to_owned();
        heading.value_text = "безопасная рабочая нагрузка (Н)".to_owned();
        heading.unit = None;

        assert_ne!(
            state_of(
                &assess(&[heading], &[]),
                ReadinessTopic::CharacteristicAnswers
            ),
            ReadinessState::Ready
        );
    }

    #[test]
    fn a_contradiction_limits_the_topic_rather_than_blocking_or_hiding_it() {
        let claims = vec![
            claim(
                FactKind::Characteristic,
                ClaimStatus::SourceSupported,
                ClaimOrigin::PartnerMaterial,
            ),
            claim(
                FactKind::Characteristic,
                ClaimStatus::Conflicted,
                ClaimOrigin::PartnerMaterial,
            ),
        ];
        let entries = assess(&claims, &[]);
        assert_eq!(
            state_of(&entries, ReadinessTopic::CharacteristicAnswers),
            ReadinessState::Limited
        );
        let reason = &entries
            .iter()
            .find(|e| e.topic == ReadinessTopic::CharacteristicAnswers)
            .unwrap()
            .reason;
        assert!(reason.contains("расходятся"), "{reason}");
    }

    #[test]
    fn unsupported_claims_alone_never_make_a_topic_ready() {
        for status in [
            ClaimStatus::Hypothesis,
            ClaimStatus::Unknown,
            ClaimStatus::Conflicted,
            ClaimStatus::Stale,
        ] {
            let claims = vec![claim(
                FactKind::Characteristic,
                status,
                ClaimOrigin::PartnerMaterial,
            )];
            let entries = assess(&claims, &[]);
            assert_eq!(
                state_of(&entries, ReadinessTopic::CharacteristicAnswers),
                ReadinessState::Blocked,
                "{} must not carry a topic",
                status.as_str()
            );
        }
    }

    #[test]
    fn an_industry_conclusion_never_makes_the_partners_own_topics_ready() {
        // "Не приписывать партнёру свойства конкурента" (`block-01-plan.md`, 1D §4),
        // applied to readiness: an industry conclusion cannot vouch for this partner's
        // product description, characteristics or commercial terms.
        let claims = vec![
            claim(
                FactKind::Characteristic,
                ClaimStatus::SourceSupported,
                ClaimOrigin::IndustryResearch,
            ),
            claim(
                FactKind::Commercial,
                ClaimStatus::SourceSupported,
                ClaimOrigin::IndustryResearch,
            ),
        ];
        let entries = assess(&claims, &[]);
        assert_eq!(
            state_of(&entries, ReadinessTopic::ProductDescription),
            ReadinessState::Blocked
        );
        assert_eq!(
            state_of(&entries, ReadinessTopic::CharacteristicAnswers),
            ReadinessState::Blocked
        );
        assert_eq!(
            state_of(&entries, ReadinessTopic::CommercialAnswers),
            ReadinessState::Blocked
        );
    }

    #[test]
    fn a_price_gap_limits_commercial_answers_by_name() {
        let claims = vec![claim(
            FactKind::Commercial,
            ClaimStatus::SourceSupported,
            ClaimOrigin::PartnerMaterial,
        )];
        let gaps = vec![GapText {
            topic: "Коммерческие условия".to_owned(),
            missing: "В каталоге не указана цена и срок поставки".to_owned(),
            blocks: None,
        }];
        let entries = assess(&claims, &gaps);
        assert_eq!(
            state_of(&entries, ReadinessTopic::CommercialAnswers),
            ReadinessState::Limited
        );
        assert_eq!(
            state_of(&entries, ReadinessTopic::CharacteristicAnswers),
            ReadinessState::Blocked,
            "a price gap says nothing about characteristics"
        );
    }

    #[test]
    fn a_gap_matching_no_vocabulary_limits_nothing_by_name() {
        // It is still carried into the version and shown; it simply does not pretend to
        // know which answers it blocks.
        let gap = GapText {
            topic: "Прочее".to_owned(),
            missing: "Неизвестно нечто неопределённое".to_owned(),
            blocks: None,
        };
        assert!(gap_blocks(&gap).is_empty());
    }

    #[test]
    fn a_gap_about_a_missing_price_is_not_a_gap_about_the_material_of_the_goods() {
        // Found by the integration suite: «цена не указана в материале» was classified as
        // a characteristic gap, because *материал* is this system's own word for an
        // uploaded document as well as a property of a product.
        let gap = GapText {
            topic: "цена".to_owned(),
            missing: "цена не указана в материале".to_owned(),
            blocks: None,
        };
        assert_eq!(gap_blocks(&gap), vec![ReadinessTopic::CommercialAnswers]);
    }

    #[test]
    fn a_stem_must_start_a_word_rather_than_hide_inside_one() {
        // Found by the test above: «вес» is inside «известно», so a substring match
        // classified «неизвестно нечто» as a gap about weight.
        assert!(!mentions("неизвестно нечто", "вес"));
        assert!(mentions("вес изделия не указан", "вес"));
        assert!(mentions("нагрузка не указана", "нагрузк"));
        // A multi-word stem carries its own boundaries.
        assert!(mentions("unknown lead time for this item", "lead time"));
    }

    #[test]
    fn gap_classification_folds_case_and_spacing() {
        let gap = GapText {
            topic: "  ЦЕНА  ".to_owned(),
            missing: "не указана".to_owned(),
            blocks: None,
        };
        assert_eq!(gap_blocks(&gap), vec![ReadinessTopic::CommercialAnswers]);
    }

    #[test]
    fn a_reason_is_never_empty_even_when_the_gap_text_is_only_control_characters() {
        // A blank reason on the screen looks like a malfunction; the database also
        // refuses one (`CHECK (char_length(btrim(reason)) BETWEEN 1 AND 1000)`).
        let entries = assess(
            &[claim(
                FactKind::Commercial,
                ClaimStatus::SourceSupported,
                ClaimOrigin::PartnerMaterial,
            )],
            &[GapText {
                topic: "цена".to_owned(),
                missing: "\u{1}\u{1}".to_owned(),
                blocks: None,
            }],
        );
        for entry in entries {
            assert!(!entry.reason.trim().is_empty());
            assert!(entry.reason.chars().count() <= 1_000);
        }
    }
}
