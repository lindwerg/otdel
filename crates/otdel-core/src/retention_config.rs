//! Phase 1F — how long operational history is kept.
//!
//! Three decisions are made here and each one is a refusal:
//!
//! * **The default is to keep everything.** A pilot that starts deleting its own history
//!   because a default said ninety days is a pilot whose first incident has no evidence.
//!   Retention is something the owner turns on, and until then the API says so.
//!
//! * **The floor is one day, and it is not negotiable.** `0007_updates.sql` raises an
//!   exception below it, so a configuration that asked for zero would produce a server
//!   that starts and then fails on every sweep. It is refused here instead, at startup,
//!   with the reason.
//!
//! * **The queue may not outlive the log.** A job row is operational state; the event it
//!   produced is the record of what it did. Pruning events sooner than jobs would leave
//!   finished jobs whose explanation is gone, so the pair is validated together rather
//!   than each on its own — the same shape of mistake as
//!   `OTDEL_EMBEDDING_MAX_RESPONSE_BYTES` and `OTDEL_EMBEDDING_BATCH_SIZE` being legal
//!   alone and unusable together (`docs/publication-1e.md`, defect 15).

use std::time::Duration;

use crate::config::{duration_secs_or, parse_u64_or, ConfigSource};
use crate::error::AppError;
use crate::updates::RetentionState;

/// The database refuses anything shorter (`otdel.apply_retention`).
pub const MIN_KEEP_DAYS: u64 = 1;

/// Ten years. Longer than this is "keep everything" with extra steps, and a horizon that
/// overflows an interval is not a horizon.
pub const MAX_KEEP_DAYS: u64 = 3650;

/// How often the maintenance pass may sweep, at the fastest. A sweep is a delete over two
/// indexed tables; running it every few seconds would be pure load.
pub const MIN_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionSettings {
    /// How many days of event log to keep. `None` — keep everything.
    pub event_days: Option<u32>,
    /// How many days of finished jobs to keep. `None` — keep everything.
    pub job_days: Option<u32>,
    /// The most recent finished jobs of each kind that survive regardless of age, so a
    /// partner processed months ago still shows a processing history.
    pub keep_per_kind: u32,
    pub sweep_interval: Duration,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            event_days: None,
            job_days: None,
            keep_per_kind: 20,
            sweep_interval: Duration::from_secs(3600),
        }
    }
}

impl RetentionSettings {
    pub fn load(source: &dyn ConfigSource) -> Result<Self, AppError> {
        let event_days = optional_days(source, "OTDEL_RETENTION_EVENT_DAYS")?;
        let job_days = optional_days(source, "OTDEL_RETENTION_JOB_DAYS")?;

        // The log has to outlive the queue it describes. Both unset is fine (nothing is
        // pruned); one set and the other not is fine in one direction only.
        match (event_days, job_days) {
            (Some(events), Some(jobs)) if jobs > events => {
                return Err(AppError::validation(format!(
                    "OTDEL_RETENTION_JOB_DAYS ({jobs}) must not exceed \
                     OTDEL_RETENTION_EVENT_DAYS ({events}): the event log is what remains \
                     after a job row is pruned, so pruning the log first would leave \
                     finished work with no surviving explanation"
                )));
            }
            (None, Some(jobs)) => {
                return Err(AppError::validation(format!(
                    "OTDEL_RETENTION_JOB_DAYS is set to {jobs} while \
                     OTDEL_RETENTION_EVENT_DAYS is not: the queue may not be pruned \
                     without a horizon for the log that records what those jobs did"
                )));
            }
            _ => {}
        }

        let keep_per_kind =
            u32::try_from(parse_u64_or(source, "OTDEL_RETENTION_KEEP_PER_KIND", 20)?)
                .map_err(|_| AppError::validation("OTDEL_RETENTION_KEEP_PER_KIND is too large"))?
                .min(1000);

        let sweep_interval =
            duration_secs_or(source, "OTDEL_RETENTION_SWEEP_INTERVAL_SECONDS", 3600)?;
        if sweep_interval < MIN_SWEEP_INTERVAL {
            return Err(AppError::validation(format!(
                "OTDEL_RETENTION_SWEEP_INTERVAL_SECONDS must be at least {}",
                MIN_SWEEP_INTERVAL.as_secs()
            )));
        }

        Ok(Self {
            event_days,
            job_days,
            keep_per_kind,
            sweep_interval,
        })
    }

    /// Whether anything is pruned at all.
    pub const fn state(&self) -> RetentionState {
        if self.event_days.is_some() {
            RetentionState::Enabled
        } else {
            RetentionState::KeepEverything
        }
    }

    /// The horizons to hand the database, or `None` when nothing is to be pruned.
    ///
    /// A job horizon without an event horizon cannot occur — the loader refuses that
    /// pair — so the only two shapes here are "prune both" and "prune the log only".
    /// `job_days: None` is the second one, and the caller turns it into a job horizon
    /// equal to the event horizon *and* skips the queue entirely; the distinction lives
    /// in the option rather than in a sentinel number.
    pub fn horizons(&self) -> Option<RetentionHorizons> {
        Some(RetentionHorizons {
            event_days: self.event_days?,
            job_days: self.job_days,
            keep_per_kind: self.keep_per_kind,
        })
    }

    pub fn message(&self) -> String {
        match (self.event_days, self.job_days) {
            (None, _) => "Очистка выключена: журнал событий и завершённые задания хранятся \
                          без ограничения срока. Включается переменными \
                          OTDEL_RETENTION_EVENT_DAYS и OTDEL_RETENTION_JOB_DAYS."
                .to_owned(),
            (Some(events), None) => format!(
                "Журнал событий хранится {events} дн.; завершённые задания не удаляются. \
                 Опубликованные версии и оригиналы не удаляются никогда."
            ),
            (Some(events), Some(jobs)) => format!(
                "Журнал событий хранится {events} дн., завершённые задания — {jobs} дн. \
                 (последние {} на каждый вид остаются независимо от возраста). \
                 Опубликованные версии и оригиналы не удаляются никогда.",
                self.keep_per_kind
            ),
        }
    }
}

/// The three numbers `otdel.apply_retention` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionHorizons {
    pub event_days: u32,
    /// `None` — finished jobs are not pruned at all.
    pub job_days: Option<u32>,
    pub keep_per_kind: u32,
}

fn optional_days(source: &dyn ConfigSource, key: &str) -> Result<Option<u32>, AppError> {
    let Some(raw) = source.get(key) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let days: u64 = trimmed
        .parse()
        .map_err(|_| AppError::validation(format!("{key} must be a whole number of days")))?;
    if !(MIN_KEEP_DAYS..=MAX_KEEP_DAYS).contains(&days) {
        return Err(AppError::validation(format!(
            "{key} must be between {MIN_KEEP_DAYS} and {MAX_KEEP_DAYS} days; the database \
             refuses a shorter horizon, because a policy that removes what happened today \
             is not a retention policy"
        )));
    }
    Ok(Some(u32::try_from(days).unwrap_or(u32::MAX)))
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

    #[test]
    fn nothing_is_pruned_until_somebody_asks_for_it() {
        let settings = RetentionSettings::load(&env(&[])).unwrap();
        assert_eq!(settings.state(), RetentionState::KeepEverything);
        assert_eq!(settings.horizons(), None);
        assert!(
            settings.message().contains("выключена"),
            "the default has to say it keeps everything: {}",
            settings.message()
        );
    }

    #[test]
    fn a_horizon_shorter_than_the_database_floor_is_refused_at_startup() {
        // The database raises an exception below one day, so accepting zero here would
        // produce a server that starts and then fails on every sweep.
        for value in ["0", "-1"] {
            let error = RetentionSettings::load(&env(&[("OTDEL_RETENTION_EVENT_DAYS", value)]))
                .unwrap_err();
            assert!(
                error.message.contains("OTDEL_RETENTION_EVENT_DAYS"),
                "{}",
                error.message
            );
        }
        assert!(RetentionSettings::load(&env(&[("OTDEL_RETENTION_EVENT_DAYS", "99999")])).is_err());
    }

    #[test]
    fn the_queue_may_not_be_pruned_sooner_than_the_log_that_explains_it() {
        let error = RetentionSettings::load(&env(&[
            ("OTDEL_RETENTION_EVENT_DAYS", "7"),
            ("OTDEL_RETENTION_JOB_DAYS", "30"),
        ]))
        .unwrap_err();
        assert!(error.message.contains("OTDEL_RETENTION_JOB_DAYS"));

        // Equal is fine, and so is a shorter job horizon.
        for jobs in ["7", "3"] {
            let settings = RetentionSettings::load(&env(&[
                ("OTDEL_RETENTION_EVENT_DAYS", "7"),
                ("OTDEL_RETENTION_JOB_DAYS", jobs),
            ]))
            .unwrap();
            assert_eq!(settings.state(), RetentionState::Enabled);
        }
    }

    #[test]
    fn pruning_the_queue_without_a_log_horizon_is_refused() {
        let error =
            RetentionSettings::load(&env(&[("OTDEL_RETENTION_JOB_DAYS", "30")])).unwrap_err();
        assert!(
            error.message.contains("OTDEL_RETENTION_EVENT_DAYS"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_sweep_cannot_be_scheduled_faster_than_the_floor() {
        let error =
            RetentionSettings::load(&env(&[("OTDEL_RETENTION_SWEEP_INTERVAL_SECONDS", "30")]))
                .unwrap_err();
        assert!(error
            .message
            .contains("OTDEL_RETENTION_SWEEP_INTERVAL_SECONDS"));
    }

    #[test]
    fn an_enabled_policy_reports_both_horizons_in_words() {
        let settings = RetentionSettings::load(&env(&[
            ("OTDEL_RETENTION_EVENT_DAYS", "90"),
            ("OTDEL_RETENTION_JOB_DAYS", "30"),
            ("OTDEL_RETENTION_KEEP_PER_KIND", "5"),
        ]))
        .unwrap();
        let horizons = settings.horizons().unwrap();
        assert_eq!(horizons.event_days, 90);
        assert_eq!(horizons.job_days, Some(30));
        assert_eq!(horizons.keep_per_kind, 5);

        let message = settings.message();
        assert!(message.contains("90"), "{message}");
        assert!(message.contains("30"), "{message}");
        assert!(
            message.contains("не удаляются никогда"),
            "the policy must state what it protects: {message}"
        );
    }
}
