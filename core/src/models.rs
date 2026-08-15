//! Enumerating the models the local `copilot` CLI can actually run.
//!
//! Config stores plain model ids (see `docs/config.md`), but a frontend offering a
//! picker needs the set of *valid* ids. The CLI publishes them in `copilot help
//! config`, under the `model` key, so we shell out and parse that block rather than
//! hardcoding a list that silently rots as models come and go.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

/// How long `copilot help config` may take before we give up. Help output is local
/// and near-instant; a hang means something is wrong and a frontend must not block
/// on it forever.
const HELP_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors surfaced while enumerating available models.
#[derive(Debug, thiserror::Error)]
pub enum ModelsError {
    #[error("failed to run `{program} help config`: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("`{program} help config` did not finish within {seconds}s")]
    Timeout { program: String, seconds: u64 },
    #[error("`{program} help config` exited with status {status}")]
    Status { program: String, status: String },
    #[error("`{program} help config` listed no models")]
    Empty { program: String },
}

/// The models the local `copilot` CLI can run, in the order it lists them.
pub fn available_models() -> Result<Vec<String>, ModelsError> {
    available_models_from("copilot")
}

/// Same as [`available_models`], against an explicit program name (used by tests).
pub fn available_models_from(program: &str) -> Result<Vec<String>, ModelsError> {
    available_models_within(program, HELP_TIMEOUT)
}

fn available_models_within(
    program: &str,
    timeout: Duration,
) -> Result<Vec<String>, ModelsError> {
    let spawn_error = |source| ModelsError::Spawn {
        program: program.to_string(),
        source,
    };
    let mut child = Command::new(program)
        .args(["help", "config"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(spawn_error)?;
    // Drain stdout on a worker thread: waiting on the child first would deadlock if
    // the help text ever outgrew the pipe buffer.
    let stdout = child.stdout.take().map(read_to_string);
    let status = match child.wait_timeout(timeout).map_err(spawn_error)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ModelsError::Timeout {
                program: program.to_string(),
                seconds: timeout.as_secs(),
            });
        }
    };
    let help = stdout.and_then(|reader| reader.join().ok()).unwrap_or_default();
    if !status.success() {
        return Err(ModelsError::Status {
            program: program.to_string(),
            status: status.to_string(),
        });
    }
    let models = parse_models(&help);
    if models.is_empty() {
        return Err(ModelsError::Empty {
            program: program.to_string(),
        });
    }
    Ok(models)
}

fn read_to_string<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// Extract the quoted model ids listed under the `model` key of `copilot help config`.
///
/// The block looks like:
///
/// ```text
///   `model`: AI model to use for Copilot CLI; ...
///     - "claude-sonnet-5"
///     - "gpt-5.4"
///
///   `contextTier`: ...
/// ```
///
/// The key line may wrap over several lines, so we skip non-bullet lines until the
/// list starts, then stop at the first line that is neither a `- "…"` bullet nor
/// blank. That keeps neighbouring keys — and their own unquoted bullet lists — out.
pub fn parse_models(help: &str) -> Vec<String> {
    let mut lines = help.lines().skip_while(|line| !is_model_key(line));
    if lines.next().is_none() {
        return Vec::new();
    }
    let mut models = Vec::new();
    let mut started = false;
    for line in lines {
        let trimmed = line.trim();
        match quoted_bullet(trimmed) {
            Some(model) => {
                started = true;
                models.push(model.to_string());
            }
            // Tolerate blank lines and a wrapped key description before the list,
            // but stop as soon as the list has ended.
            None if trimmed.is_empty() || !started => {
                if started {
                    break;
                }
            }
            None => break,
        }
    }
    models
}

/// Whether `line` is the `model` key line, tolerating the backticks the CLI uses.
fn is_model_key(line: &str) -> bool {
    let trimmed = line.trim_start();
    let key = trimmed.trim_start_matches('`');
    key.starts_with("model`:") || key.starts_with("model:")
}

/// The id inside a `- "some-model"` bullet, requiring both quotes.
fn quoted_bullet(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("- ")?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty()).then_some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = r#"Configuration Settings:

  `logLevel`: log level for CLI; defaults to "default".

  `model`: AI model to use for Copilot CLI; can be changed with /model command.
    - "claude-sonnet-5"
    - "gpt-5.4"
    - "gemini-3.7-flash"

  `contextTier`: context window tier for tiered-pricing models.
    - Can also be set with --context flag
"#;

    #[test]
    fn parses_models_in_listed_order() {
        assert_eq!(
            parse_models(HELP),
            vec!["claude-sonnet-5", "gpt-5.4", "gemini-3.7-flash"]
        );
    }

    #[test]
    fn stops_before_the_next_key() {
        // `contextTier`'s own bullet is unquoted, so it must not be absorbed.
        assert!(!parse_models(HELP)
            .iter()
            .any(|model| model.contains("--context")));
    }

    #[test]
    fn tolerates_a_blank_line_before_the_list() {
        let help = "  `model`: AI model to use.\n\n    - \"gpt-5.4\"\n\n  `banner`: once\n";
        assert_eq!(parse_models(help), vec!["gpt-5.4"]);
    }

    #[test]
    fn tolerates_a_wrapped_key_description() {
        let help = concat!(
            "  `model`: AI model to use for Copilot CLI;\n",
            "    can be changed with the /model command.\n",
            "    - \"gpt-5.4\"\n",
            "\n",
            "  `banner`: once\n",
        );
        assert_eq!(parse_models(help), vec!["gpt-5.4"]);
    }

    #[test]
    fn ignores_an_unterminated_quote() {
        let help = "  `model`: AI model.\n    - \"gpt-5.4\n    - \"gpt-5.5\"\n";
        assert_eq!(parse_models(help), vec!["gpt-5.5"]);
    }

    #[test]
    fn missing_model_key_yields_nothing() {
        assert!(parse_models("Configuration Settings:\n\n  `banner`: once\n").is_empty());
    }

    #[test]
    fn help_without_bullets_yields_nothing() {
        assert!(parse_models("  `model`: AI model to use.\n\n  `banner`: once\n").is_empty());
    }

    #[test]
    fn missing_program_is_reported_not_panicked() {
        let error = available_models_from("quorum-no-such-copilot-binary").unwrap_err();
        assert!(matches!(error, ModelsError::Spawn { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_program_times_out_rather_than_blocking_forever() {
        use std::os::unix::fs::PermissionsExt;
        // A stand-in `copilot` that ignores its arguments and never returns in time.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hanging-copilot");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let error =
            available_models_within(&script.display().to_string(), Duration::from_millis(150))
                .unwrap_err();

        assert!(
            matches!(error, ModelsError::Timeout { .. }),
            "expected a timeout, got {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should give up promptly, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_failing_program_reports_its_status() {
        // `false` exits non-zero without printing a model list.
        let error = available_models_within("false", Duration::from_secs(5)).unwrap_err();
        assert!(
            matches!(error, ModelsError::Status { .. }),
            "expected a status error, got {error:?}"
        );
    }
}
