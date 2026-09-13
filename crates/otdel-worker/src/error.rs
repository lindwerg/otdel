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
}

impl WorkerError {
    pub fn is_permanent(&self) -> bool {
        match self {
            Self::Extract(error) => error.is_permanent(),
            Self::MaterialMissing | Self::UnsupportedMediaType(_) => true,
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
    fn diagnostics_are_single_line_and_bounded() {
        let error = WorkerError::Workspace("нет места\nна диске".to_owned());
        let message = error.diagnostic();
        assert!(!message.contains('\n'));
        assert!(message.len() <= 500);
    }
}
