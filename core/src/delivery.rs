//! Coordinator-owned GitHub pull-request delivery.

use crate::persistence::WorktreeRecord;
use crate::worktree::{worktree_head, worktree_is_clean, WorktreeError};
use std::path::Path;
use std::process::{Command, Output};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
}

/// The GitHub repository selected by a persisted Git remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepository {
    pub hostname: String,
    pub name: String,
}

impl GitHubRepository {
    /// Parse HTTPS and SSH GitHub-style remote URLs, including GitHub Enterprise.
    pub fn parse(remote_url: &str) -> Result<Self, DeliveryError> {
        let value = remote_url.trim().trim_end_matches('/');
        let (hostname, path) = if let Some(rest) = value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .or_else(|| value.strip_prefix("ssh://"))
        {
            let rest = rest.rsplit_once('@').map(|(_, rest)| rest).unwrap_or(rest);
            rest.split_once('/')
                .ok_or_else(|| DeliveryError::UnsupportedRemote(value.to_string()))?
        } else if let Some((user_host, path)) = value.split_once(':') {
            let hostname = user_host
                .rsplit_once('@')
                .map(|(_, host)| host)
                .unwrap_or(user_host);
            (hostname, path)
        } else {
            return Err(DeliveryError::UnsupportedRemote(value.to_string()));
        };
        let name = path.trim_matches('/').trim_end_matches(".git");
        if hostname.is_empty()
            || name.is_empty()
            || name.split('/').count() != 2
            || name.split('/').any(str::is_empty)
        {
            return Err(DeliveryError::UnsupportedRemote(value.to_string()));
        }
        Ok(GitHubRepository {
            hostname: hostname.to_string(),
            name: name.to_string(),
        })
    }

    fn repo_arg(&self) -> String {
        if self.hostname == "github.com" {
            self.name.clone()
        } else {
            format!("{}/{}", self.hostname, self.name)
        }
    }
}

/// The external effects needed to hand an accepted work item to GitHub.
pub trait DeliveryBackend: Send + Sync {
    fn push(
        &self,
        workspace: &Path,
        worktree: &WorktreeRecord,
        final_head: &str,
    ) -> Result<(), DeliveryError>;
    fn create_or_adopt_pull_request(
        &self,
        workspace: &Path,
        worktree: &WorktreeRecord,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, DeliveryError>;
}

/// Production backend. Credentials are read only by this Coordinator process.
pub struct GitHubDelivery;

impl DeliveryBackend for GitHubDelivery {
    fn push(
        &self,
        workspace: &Path,
        worktree: &WorktreeRecord,
        final_head: &str,
    ) -> Result<(), DeliveryError> {
        if !worktree_is_clean(workspace)? {
            return Err(DeliveryError::DirtyWorktree);
        }
        let actual = worktree_head(workspace)?;
        if actual != final_head {
            return Err(DeliveryError::HeadChanged {
                expected: final_head.to_string(),
                actual,
            });
        }
        let remote = worktree
            .delivery_remote
            .as_deref()
            .ok_or(DeliveryError::MissingDeliverySettings)?;
        // Validate the selected remote before mutating it. This also ensures
        // `--remote` is honored instead of silently falling back to origin.
        GitHubRepository::parse(&git_stdout(
            workspace,
            &["remote", "get-url", "--push", remote],
        )?)?;
        let refspec = format!("{final_head}:refs/heads/{}", worktree.branch);
        run(workspace, "git", &["push", remote, &refspec])?;
        Ok(())
    }

    fn create_or_adopt_pull_request(
        &self,
        workspace: &Path,
        worktree: &WorktreeRecord,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, DeliveryError> {
        let target = worktree
            .target_branch
            .as_deref()
            .ok_or(DeliveryError::MissingDeliverySettings)?;
        let remote = worktree
            .delivery_remote
            .as_deref()
            .ok_or(DeliveryError::MissingDeliverySettings)?;
        let repository = GitHubRepository::parse(&git_stdout(
            workspace,
            &["remote", "get-url", "--push", remote],
        )?)?;
        let repo_arg = repository.repo_arg();
        let listed = run(
            workspace,
            "gh",
            &[
                "pr",
                "list",
                "--repo",
                &repo_arg,
                "--head",
                &worktree.branch,
                "--base",
                target,
                "--state",
                "open",
                "--json",
                "number,url",
            ],
        )?;
        let existing: Vec<GhListPullRequest> =
            serde_json::from_slice(&listed.stdout).map_err(DeliveryError::GhJson)?;
        if let Some(pr) = existing.into_iter().next() {
            let number = pr.number.to_string();
            run(
                workspace,
                "gh",
                &[
                    "pr", "edit", &number, "--repo", &repo_arg, "--title", title, "--body", body,
                ],
            )?;
            return Ok(PullRequest {
                number: pr.number,
                url: pr.url,
            });
        }

        let endpoint = format!("repos/{}/pulls", repository.name);
        let mut args = vec![
            "api".to_string(),
            "--method".to_string(),
            "POST".to_string(),
            endpoint,
            "-f".to_string(),
            format!("title={title}"),
            "-f".to_string(),
            format!("head={}", worktree.branch),
            "-f".to_string(),
            format!("base={target}"),
            "-f".to_string(),
            format!("body={body}"),
        ];
        if repository.hostname != "github.com" {
            args.push("--hostname".to_string());
            args.push(repository.hostname);
        }
        let references = args.iter().map(String::as_str).collect::<Vec<_>>();
        let created = run(workspace, "gh", &references)?;
        let pr: GhCreatedPullRequest =
            serde_json::from_slice(&created.stdout).map_err(DeliveryError::GhJson)?;
        Ok(PullRequest {
            number: pr.number,
            url: pr.html_url,
        })
    }
}

/// Offline backend used only by `--dry-run`. It still creates a persisted handoff,
/// so dry runs exercise the same state invariant without credentials or network I/O.
pub struct DryRunDelivery;

impl DeliveryBackend for DryRunDelivery {
    fn push(
        &self,
        _workspace: &Path,
        _worktree: &WorktreeRecord,
        _final_head: &str,
    ) -> Result<(), DeliveryError> {
        Ok(())
    }

    fn create_or_adopt_pull_request(
        &self,
        _workspace: &Path,
        worktree: &WorktreeRecord,
        _title: &str,
        _body: &str,
    ) -> Result<PullRequest, DeliveryError> {
        Ok(PullRequest {
            number: 1,
            url: format!("https://quorum.invalid/pull/{}", worktree.branch),
        })
    }
}

#[derive(Debug, serde::Deserialize)]
struct GhListPullRequest {
    number: u64,
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct GhCreatedPullRequest {
    number: u64,
    html_url: String,
}

fn git_stdout(workspace: &Path, args: &[&str]) -> Result<String, DeliveryError> {
    let output = run(workspace, "git", args)?;
    String::from_utf8(output.stdout)
        .map_err(|_| DeliveryError::NonUtf8GitOutput)
        .map(|value| value.trim().to_string())
}

fn run(workspace: &Path, program: &str, args: &[&str]) -> Result<Output, DeliveryError> {
    let output = Command::new(program)
        .current_dir(workspace)
        .args(args)
        .output()
        .map_err(|source| DeliveryError::Spawn {
            program: program.to_string(),
            source,
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(DeliveryError::Command {
            program: program.to_string(),
            status: output
                .status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

impl DeliveryError {
    pub fn is_retryable(&self) -> bool {
        match self {
            DeliveryError::Spawn { .. } => true,
            DeliveryError::Command {
                program, stderr, ..
            } if program == "git" || program == "gh" => Self::transient_delivery_failure(stderr),
            _ => false,
        }
    }

    fn transient_delivery_failure(stderr: &str) -> bool {
        let message = stderr.to_ascii_lowercase();
        if [
            "authentication",
            "bad credentials",
            "permission denied",
            "could not read username",
            "non-fast-forward",
            "fetch first",
            "remote rejected",
            "invalid",
            "not found",
        ]
        .iter()
        .any(|needle| message.contains(needle))
        {
            return false;
        }
        [
            "http 5",
            "http 429",
            "rate limit",
            "connection reset",
            "connection timed out",
            "network is unreachable",
            "could not resolve host",
            "temporary failure",
            "remote end hung up",
            "tls handshake timeout",
        ]
        .iter()
        .any(|needle| message.contains(needle))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error("no delivery backend is configured")]
    BackendNotConfigured,
    #[error("delivery settings are missing; provide --remote and --target when approving this migrated work item")]
    MissingDeliverySettings,
    #[error("remote {0:?} is not a supported GitHub-style repository URL")]
    UnsupportedRemote(String),
    #[error("delivery requires a clean worktree")]
    DirtyWorktree,
    #[error("delivery expected final HEAD {expected}, found {actual}")]
    HeadChanged { expected: String, actual: String },
    #[error("delivery did not persist a pull-request number and URL")]
    IncompleteHandoff,
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{program} exited with status {status}: {stderr}")]
    Command {
        program: String,
        status: String,
        stderr: String,
    },
    #[error("GitHub CLI returned invalid PR JSON: {0}")]
    GhJson(serde_json::Error),
    #[error("Git returned non-UTF-8 output")]
    NonUtf8GitOutput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_and_ssh_github_remotes() {
        assert_eq!(
            GitHubRepository::parse("https://github.com/owner/repo.git").unwrap(),
            GitHubRepository {
                hostname: "github.com".to_string(),
                name: "owner/repo".to_string(),
            }
        );
        assert_eq!(
            GitHubRepository::parse("git@github.example:team/project.git").unwrap(),
            GitHubRepository {
                hostname: "github.example".to_string(),
                name: "team/project".to_string(),
            }
        );
        assert_eq!(
            GitHubRepository::parse("ssh://git@github.example/team/project")
                .unwrap()
                .repo_arg(),
            "github.example/team/project"
        );
    }

    #[test]
    fn rejects_non_github_style_remotes() {
        assert!(matches!(
            GitHubRepository::parse("/srv/repository.git"),
            Err(DeliveryError::UnsupportedRemote(_))
        ));
        assert!(matches!(
            GitHubRepository::parse("https://github.com/owner/repo/extra"),
            Err(DeliveryError::UnsupportedRemote(_))
        ));
    }

    #[test]
    fn parses_list_and_rest_create_responses_without_duplicate_url_fields() {
        let listed: Vec<GhListPullRequest> =
            serde_json::from_str(r#"[{"number":4,"url":"https://github.test/pull/4"}]"#).unwrap();
        assert_eq!(listed[0].url, "https://github.test/pull/4");

        let created: GhCreatedPullRequest = serde_json::from_str(
            r#"{"number":5,"url":"api-url","html_url":"https://github.test/pull/5"}"#,
        )
        .unwrap();
        assert_eq!(created.html_url, "https://github.test/pull/5");
    }

    #[test]
    fn retries_only_explicit_transient_git_and_gh_failures() {
        let transient = DeliveryError::Command {
            program: "git".to_string(),
            status: "128".to_string(),
            stderr: "fatal: Could not resolve host: github.com".to_string(),
        };
        assert!(transient.is_retryable());
        let rate_limited = DeliveryError::Command {
            program: "gh".to_string(),
            status: "1".to_string(),
            stderr: "HTTP 429: rate limit exceeded".to_string(),
        };
        assert!(rate_limited.is_retryable());
        let rejected = DeliveryError::Command {
            program: "git".to_string(),
            status: "1".to_string(),
            stderr: "! [rejected] main -> main (non-fast-forward)".to_string(),
        };
        assert!(!rejected.is_retryable());
        let auth = DeliveryError::Command {
            program: "gh".to_string(),
            status: "1".to_string(),
            stderr: "authentication required".to_string(),
        };
        assert!(!auth.is_retryable());
    }
}
