//! Running an external tool safely.
//!
//! Rules that hold for every invocation in this crate:
//!
//! * the executable is named by configuration, never by a document
//!   ([`otdel_core::extraction_config`] validates it);
//! * every argument is either a fixed flag, an integer the server computed, or a path
//!   the server created under its own temporary directory. No file name, no text and no
//!   embedded instruction from inside a PDF ever reaches a command line;
//! * no shell is involved — the program is executed directly, so quoting and metacharacters
//!   have no meaning;
//! * the call is bounded by a timeout and the child is killed when the future is dropped,
//!   so a hung tool cannot pin a worker slot forever.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::error::{ToolError, ToolResult};

/// Captured result of a finished tool run.
#[derive(Debug, Clone)]
pub(crate) struct ToolOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl ToolOutput {
    /// stdout as text. External tools do not promise valid UTF-8, and a malformed byte
    /// is not a reason to lose the page — it is replaced, and the replacement characters
    /// are what the garbled-text check later counts.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Run `program` with `args`, bounded by `timeout`.
pub(crate) async fn run(
    tool: &str,
    program: &str,
    args: &[&OsStr],
    timeout: Duration,
) -> ToolResult<ToolOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Without this a timed-out child keeps running after the future is dropped.
        .kill_on_drop(true);

    let child = command.output();

    let output = match tokio::time::timeout(timeout, child).await {
        Err(_) => {
            return Err(ToolError::Timeout {
                tool: tool.to_owned(),
                seconds: timeout.as_secs(),
            })
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::Unavailable {
                tool: tool.to_owned(),
                reason: format!("исполняемый файл `{program}` не найден"),
            })
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(ToolError::Unavailable {
                tool: tool.to_owned(),
                reason: format!("нет прав на запуск `{program}`"),
            })
        }
        Ok(Err(error)) => {
            return Err(ToolError::Failed {
                tool: tool.to_owned(),
                reason: format!("не удалось запустить `{program}`: {}", error.kind()),
            })
        }
        Ok(Ok(output)) => output,
    };

    Ok(ToolOutput {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: first_line(&String::from_utf8_lossy(&output.stderr)),
    })
}

/// A path that is safe to hand to a command-line tool as a positional argument.
///
/// Absolute by construction in this crate; the check exists so a future caller cannot
/// pass something that a tool would read as an option.
pub(crate) fn check_argument_path(path: &Path) -> ToolResult<&OsStr> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || text.starts_with('-') {
        return Err(ToolError::Failed {
            tool: "внутренняя проверка".to_owned(),
            reason: "путь для внешнего инструмента должен быть абсолютным".to_owned(),
        });
    }
    Ok(path.as_os_str())
}

/// First non-empty line, bounded — tool diagnostics land in user-visible text.
pub(crate) fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn a_missing_executable_is_reported_as_unavailable_not_as_a_failure() {
        let error = run(
            "тест",
            "otdel-no-such-binary-9f3a",
            &[],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        match error {
            ToolError::Unavailable { reason, .. } => assert!(reason.contains("не найден")),
            other => panic!("expected Unavailable, got {other}"),
        }
    }

    #[tokio::test]
    async fn a_tool_that_never_answers_is_stopped_by_the_timeout() {
        // `sleep` is in POSIX; on a machine without it the call reports Unavailable,
        // which this assertion also accepts — either way the call returns promptly.
        let error = run(
            "тест",
            "sleep",
            &[OsStr::new("30")],
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                ToolError::Timeout { .. } | ToolError::Unavailable { .. }
            ),
            "{error}"
        );
    }

    #[test]
    fn relative_and_option_like_paths_are_refused() {
        assert!(check_argument_path(&PathBuf::from("relative/file.pdf")).is_err());
        assert!(check_argument_path(&PathBuf::from("-oremove")).is_err());
        assert!(check_argument_path(&PathBuf::from("/tmp/otdel/file.pdf")).is_ok());
    }

    #[test]
    fn tool_diagnostics_are_a_single_bounded_line() {
        assert_eq!(first_line("\n\n  boom  \nsecond"), "boom");
        assert_eq!(first_line("").len(), 0);
        assert!(first_line(&"x".repeat(500)).len() <= 200);
    }
}
