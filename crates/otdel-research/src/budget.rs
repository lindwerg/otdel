//! The arithmetic of research money, kept away from the database so it can be reasoned
//! about on its own.
//!
//! Amounts are integers — millionths of one currency unit — and never floating point.
//! A budget compared with `f64` is a budget that eventually lets one more call through
//! than it should, and "почти исчерпан" is not a state anybody asked for.
//!
//! Nothing here spends anything. The actual reservation is a single `UPDATE … WHERE
//! limit - spent - reserved >= amount` in [`otdel_db::research`], which is what makes two
//! concurrent plans unable to pass the same ceiling. This module holds the two questions
//! that must be answered *before* that statement is reached — "what does this call cost"
//! and "is there enough" — and the formatting the owner reads.

use otdel_core::research_config::ResearchCosts;

/// One millionth of a currency unit.
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// What each kind of external call is declared to cost.
///
/// *Declared* is the operative word: these are the owner's configured tariff, not a
/// provider's invoice. Everything that shows an amount says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostModel {
    pub search_micros: i64,
    pub fetch_micros: i64,
    /// Per model request made while interpreting a plan's sources. A model call is a paid
    /// call like any other; leaving it out would make the plan's "израсходовано" a number
    /// that omits the most expensive part of a pass.
    pub model_call_micros: i64,
}

impl CostModel {
    pub fn from_settings(costs: &ResearchCosts) -> Self {
        Self {
            search_micros: i64::try_from(costs.search_micros).unwrap_or(i64::MAX),
            fetch_micros: i64::try_from(costs.fetch_micros).unwrap_or(i64::MAX),
            model_call_micros: i64::try_from(costs.model_call_micros).unwrap_or(i64::MAX),
        }
    }

    /// The cheapest call that could still be made. When even this does not fit, the plan
    /// is out of money and stops — rather than trying each step and failing at each one.
    pub fn cheapest_call(self) -> i64 {
        self.search_micros
            .min(self.fetch_micros)
            .min(self.model_call_micros)
    }
}

/// How much a plan may still use, from both ceilings at once.
///
/// Both have to allow a call: the bureau's total and this plan's own share. A plan with
/// room left inside a bureau that has none does not get to spend, and neither does the
/// other way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    pub bureau_available: i64,
    pub plan_available: i64,
}

impl Allowance {
    pub fn new(bureau_available: i64, plan_available: i64) -> Self {
        Self {
            bureau_available: bureau_available.max(0),
            plan_available: plan_available.max(0),
        }
    }

    /// The binding ceiling — what a caller may actually use.
    pub fn usable(self) -> i64 {
        self.bureau_available.min(self.plan_available)
    }

    pub fn affords(self, amount: i64) -> bool {
        amount <= self.usable()
    }

    /// Why a call cannot be made, when it cannot. `None` means it can.
    ///
    /// The reason names *which* ceiling stopped it, because raising the wrong one is a
    /// frustrating way to spend an afternoon.
    pub fn refusal(self, amount: i64, costs: &ResearchCosts) -> Option<String> {
        if self.affords(amount) {
            return None;
        }
        let needed = format_micros(amount, &costs.currency);
        Some(if self.bureau_available < amount {
            format!(
                "бюджет бюро на исследования исчерпан: доступно {}, требуется {needed}",
                format_micros(self.bureau_available, &costs.currency)
            )
        } else {
            format!(
                "бюджет этого исследования исчерпан: доступно {}, требуется {needed}",
                format_micros(self.plan_available, &costs.currency)
            )
        })
    }
}

/// An amount, as the owner reads it: `0,005 USD`.
///
/// Six decimals are kept only when they carry something; a whole number of units stays a
/// whole number rather than growing a tail of zeros.
pub fn format_micros(micros: i64, currency: &str) -> String {
    let negative = micros < 0;
    let absolute = micros.unsigned_abs();
    let units = absolute / MICROS_PER_UNIT as u64;
    let fraction = absolute % MICROS_PER_UNIT as u64;

    let mut rendered = if fraction == 0 {
        units.to_string()
    } else {
        // Trailing zeros of the fraction carry no information.
        let digits = format!("{fraction:06}");
        format!("{units},{}", digits.trim_end_matches('0'))
    };
    if negative {
        rendered.insert(0, '-');
    }
    format!("{rendered} {currency}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn costs() -> ResearchCosts {
        ResearchCosts {
            currency: "USD".to_owned(),
            search_micros: 5_000,
            fetch_micros: 0,
            model_call_micros: 0,
            plan_budget_micros: 100_000,
            bureau_budget_micros: 5_000_000,
        }
    }

    #[test]
    fn the_binding_ceiling_is_the_smaller_one() {
        let bureau_is_tighter = Allowance::new(1_000, 50_000);
        assert_eq!(bureau_is_tighter.usable(), 1_000);
        assert!(bureau_is_tighter.affords(1_000));
        assert!(!bureau_is_tighter.affords(1_001));

        let plan_is_tighter = Allowance::new(50_000, 1_000);
        assert_eq!(plan_is_tighter.usable(), 1_000);
    }

    #[test]
    fn an_exhausted_allowance_names_which_ceiling_stopped_the_call() {
        let costs = costs();

        let bureau = Allowance::new(0, 100_000).refusal(5_000, &costs).unwrap();
        assert!(bureau.contains("бюджет бюро"), "{bureau}");

        let plan = Allowance::new(5_000_000, 1_000)
            .refusal(5_000, &costs)
            .unwrap();
        assert!(plan.contains("бюджет этого исследования"), "{plan}");

        // Enough money means no refusal at all.
        assert!(Allowance::new(5_000_000, 100_000)
            .refusal(5_000, &costs)
            .is_none());
    }

    #[test]
    fn a_negative_allowance_reads_as_nothing_left_not_as_credit() {
        // An unknown outcome settled after the limit was lowered can overshoot. What must
        // never happen is that the overshoot becomes spendable.
        let overspent = Allowance::new(-10_000, 50_000);
        assert_eq!(overspent.usable(), 0);
        assert!(!overspent.affords(1));
        assert!(overspent.affords(0));
    }

    #[test]
    fn a_free_call_is_still_bounded_by_the_other_limits() {
        // With a zero tariff the budget never stops a fetch — the page and time limits
        // are what bound it, and this test pins that the arithmetic agrees.
        let model = CostModel::from_settings(&costs());
        assert_eq!(model.fetch_micros, 0);
        assert_eq!(model.cheapest_call(), 0);
        assert!(Allowance::new(0, 0).affords(model.fetch_micros));
    }

    #[test]
    fn amounts_are_rendered_the_way_a_person_reads_them() {
        assert_eq!(format_micros(5_000, "USD"), "0,005 USD");
        assert_eq!(format_micros(1_000_000, "USD"), "1 USD");
        assert_eq!(format_micros(1_500_000, "RUB"), "1,5 RUB");
        assert_eq!(format_micros(0, "USD"), "0 USD");
        assert_eq!(format_micros(1, "USD"), "0,000001 USD");
        assert_eq!(format_micros(-5_000, "USD"), "-0,005 USD");
        assert_eq!(format_micros(12_345_678, "EUR"), "12,345678 EUR");
    }

    #[test]
    fn the_cost_model_survives_an_absurd_configuration() {
        let huge = ResearchCosts {
            search_micros: u64::MAX,
            ..costs()
        };
        // Saturating rather than wrapping: a cost that does not fit an i64 becomes the
        // largest one, so no real budget can afford it and nothing is called.
        let model = CostModel::from_settings(&huge);
        assert_eq!(model.search_micros, i64::MAX);
        assert!(!Allowance::new(1_000_000, 1_000_000).affords(model.search_micros));
    }
}
