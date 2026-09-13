//! Worker failures.
//!
//! The important distinction, again, is transient vs permanent: a job that cannot
//! succeed must stop with a stated reason instead of consuming its attempts and leaving
//! the operator to guess. [`WorkerError::is_permanent`] is what the queue uses to decide.

use otdel_extract::ExtractError;

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("database failure: {0}")]
    Db(#[from] otdel_db::DbError),

    #[error("storage failure: {0}")]
    Storage(#[from] otdel_storage::StorageError),

    #[error("scratch directory failure: {0}")]
    Workspace(String),

    #[error(transparent)]
    Extract(#[from] ExtractError),

    #[error("материал задания не найден")]
    MaterialMissing,

    #[error("тип файла `{0}` не обрабатывается на этапе 1B")]
    UnsupportedMediaType(String),

    /// The lease expired while the job was running and another worker may already have
    /// taken it. Stopping is the only safe move: continuing would let a stale run
    /// overwrite newer results.
    #[error("аренда задания истекла, обработка остановлена")]
    LeaseLost,

    #[error("бюро `{0}` не инициализировано")]
    BureauMissing(String),

    #[error("обработка прервана")]
    Cancelled,

    // --- phase 1C ---------------------------------------------------------------
    /// The model adapter has no key/model/endpoint. Permanent on purpose: repeating
    /// the job cannot configure it, and a queue that kept retrying would hide the one
    /// thing the owner has to do.
    #[error("{0}")]
    ProviderNotConfigured(String),

    /// The model was called and the call failed. The provider's own classification of
    /// the failure decides whether this is worth repeating.
    #[error("{diagnostic}")]
    ModelCallFailed { diagnostic: String, retryable: bool },

    /// The material has no page with usable text yet.
    #[error("в материале нет ни одной прочитанной страницы с текстом")]
    NothingToUnderstand,

    // --- phase 1D ---------------------------------------------------------------
    /// The job names a research plan that is not there. Permanent: a job without its
    /// plan cannot be run by anybody.
    #[error("план исследования для задания не найден")]
    ResearchPlanMissing,

    /// The search provider was called and the call failed. The adapter's own
    /// classification decides whether repeating is worth anything.
    #[error("{diagnostic}")]
    SearchCallFailed { diagnostic: String, retryable: bool },

    /// The pass ended for a reason that is not a failure of the machinery: the owner
    /// stopped it, the money ran out, the pass limit was reached. `permanent` says
    /// whether the queue should stop trying.
    #[error("{reason}")]
    ResearchStopped { reason: String, permanent: bool },
}

impl WorkerError {
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Extract(error) => error.is_permanent(),
            Self::MaterialMissing
            | Self::UnsupportedMediaType(_)
            | Self::ProviderNotConfigured(_)
            | Self::NothingToUnderstand
            | Self::ResearchPlanMissing => true,
            Self::ModelCallFailed { retryable, .. } | Self::SearchCallFailed { retryable, .. } => {
                !*retryable
            }
            Self::ResearchStopped { permanent, .. } => *permanent,
            Self::Db(_)
            | Self::Storage(_)
            | Self::Workspace(_)
            | Self::LeaseLost
            | Self::BureauMissing(_)
            | Self::Cancelled => false,
        }
    }

    /// A message safe to store in the job row and show to the owner: one line, bounded,
    /// no paths and no internals.
    pub fn diagnostic(&self) -> String {
        self.to_string()
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .take(500)
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_a_pdf_stops_instead_of_retrying_forever() {
        let error = WorkerError::Extract(ExtractError::NotAPdf("нет заголовка".into()));
        assert!(error.is_permanent());
    }

    #[test]
    fn a_lost_lease_is_transient_the_job_belongs_to_someone_else_now() {
        assert!(!WorkerError::LeaseLost.is_permanent());
    }

    #[test]
    fn a_missing_model_key_stops_instead_of_retrying_against_a_configuration_problem() {
        let error = WorkerError::ProviderNotConfigured("не задан OTDEL_LLM_API_KEY".to_owned());
        assert!(error.is_permanent());
        assert!(error.diagnostic().contains("OTDEL_LLM_API_KEY"));

        // A rate-limited provider, on the other hand, is worth another attempt.
        assert!(!WorkerError::ModelCallFailed {
            diagnostic: "превышен лимит запросов провайдера".to_owned(),
            retryable: true,
        }
        .is_permanent());
        assert!(WorkerError::ModelCallFailed {
            diagnostic: "ответ модели оборван лимитом длины".to_owned(),
            retryable: false,
        }
        .is_permanent());
    }

    #[test]
    fn a_research_pass_that_ran_out_of_money_does_not_keep_retrying() {
        // Raising the budget is the owner's decision; a queue that kept trying would
        // spend nothing and hide the one thing to do.
        let exhausted = WorkerError::ResearchStopped {
            reason: "бюджет бюро на исследования исчерпан".to_owned(),
            permanent: true,
        };
        assert!(exhausted.is_permanent());
        assert!(exhausted.diagnostic().contains("бюджет"));

        // A rate-limited search provider, on the other hand, is worth another attempt.
        assert!(!WorkerError::SearchCallFailed {
            diagnostic: "превышен лимит запросов поискового провайдера".to_owned(),
            retryable: true,
        }
        .is_permanent());

        // A job whose plan is gone cannot be run by anybody.
        assert!(WorkerError::ResearchPlanMissing.is_permanent());
    }

    #[test]
    fn diagnostics_are_single_line_and_bounded() {
        let error = WorkerError::Workspace("нет места\nна диске".to_owned());
        let message = error.diagnostic();
        assert!(!message.contains('\n'));
        assert!(message.len() <= 500);
    }
}
