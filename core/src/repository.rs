//! Git repository discovery for Quorum context resolution.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Immutable source and delivery intent selected before a worktree is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeStart {
    /// The user supplied revision, or `HEAD` when the default was used.
    pub requested_base: String,
    /// The immutable commit resolved from `requested_base`.
    pub base_commit: String,
    /// The remote to which the Quorum branch will be pushed.
    pub delivery_remote: String,
    /// The pull request's target branch.
    pub target_branch: String,
}

/// A canonical root of a non-bare Git working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryRoot(PathBuf);

impl RepositoryRoot {
    /// Resolve `path` to the root of its containing Git working tree.
    pub fn discover(path: &Path) -> Result<RepositoryRoot, RepositoryError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|source| RepositoryError::Spawn {
                path: path.to_path_buf(),
                source,
            })?;
        if !output.status.success() {
            return Err(RepositoryError::NotRepository {
                path: path.to_path_buf(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let root = String::from_utf8(output.stdout).map_err(|_| RepositoryError::NonUtf8 {
            path: path.to_path_buf(),
        })?;
        let root = PathBuf::from(root.trim());
        let canonical = std::fs::canonicalize(&root)
            .map_err(|source| RepositoryError::Canonicalize { path: root, source })?;
        Ok(RepositoryRoot(canonical))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    pub(crate) fn from_canonical(path: impl Into<PathBuf>) -> RepositoryRoot {
        RepositoryRoot(path.into())
    }
}

/// Resolve source and delivery intent without changing repository state.
///
/// A target is inferred only from an attached current branch (for the default
/// base) or from an unambiguous named local/selected-remote branch.
pub fn resolve_worktree_start(
    repository: &Path,
    base: Option<&str>,
    remote: Option<&str>,
    target: Option<&str>,
) -> Result<WorktreeStart, RepositoryError> {
    let requested_base = base.unwrap_or("HEAD").to_string();
    let delivery_remote = remote.unwrap_or("origin").to_string();
    let base_commit = git_stdout(
        repository,
        &[
            "rev-parse",
            "--verify",
            &format!("{requested_base}^{{commit}}"),
        ],
    )
    .map_err(|error| {
        if base.is_none() {
            RepositoryError::NoCommittedHead(repository.to_path_buf())
        } else {
            RepositoryError::InvalidBase {
                revision: requested_base.clone(),
                message: error,
            }
        }
    })?;
    git_stdout(repository, &["remote", "get-url", &delivery_remote]).map_err(|error| {
        RepositoryError::InvalidDelivery {
            message: format!("remote {delivery_remote:?} is unavailable: {error}"),
        }
    })?;

    let inferred = match base {
        None => symbolic_branch(repository)?,
        Some(revision) => branch_for_revision(repository, revision, &delivery_remote)?,
    };
    let target_branch =
        target
            .map(str::to_owned)
            .or(inferred)
            .ok_or_else(|| RepositoryError::TargetRequired {
                revision: requested_base.clone(),
            })?;
    validate_delivery_target(repository, &delivery_remote, &target_branch)?;
    Ok(WorktreeStart {
        requested_base,
        base_commit,
        delivery_remote,
        target_branch,
    })
}

/// Check that an already-persisted delivery destination still resolves.
pub fn validate_delivery_target(
    repository: &Path,
    remote: &str,
    target: &str,
) -> Result<(), RepositoryError> {
    let push_url =
        git_stdout(repository, &["remote", "get-url", "--push", remote]).map_err(|error| {
            RepositoryError::InvalidDelivery {
                message: format!("remote {remote:?} is unavailable: {error}"),
            }
        })?;
    let remote_ref = format!("refs/heads/{target}");
    let output = git_output(
        repository,
        &[
            "ls-remote",
            "--exit-code",
            "--heads",
            &push_url,
            &remote_ref,
        ],
    )?;
    if output.status.success() {
        return Ok(());
    }
    if output.status.code() != Some(2) {
        return Err(RepositoryError::Git {
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Err(RepositoryError::InvalidDelivery {
        message: format!("target branch {target:?} does not exist at remote {remote:?}"),
    })
}

fn symbolic_branch(repository: &Path) -> Result<Option<String>, RepositoryError> {
    let output = git_output(repository, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8(output.stdout)
                .map_err(|_| RepositoryError::NonUtf8GitOutput)?
                .trim()
                .to_string(),
        ));
    }
    if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(RepositoryError::Git {
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

fn branch_for_revision(
    repository: &Path,
    revision: &str,
    remote: &str,
) -> Result<Option<String>, RepositoryError> {
    let local = format!("refs/heads/{revision}");
    if git_succeeds(repository, &["show-ref", "--verify", "--quiet", &local])? {
        return Ok(Some(revision.to_string()));
    }
    let selected_remote = format!("refs/remotes/{remote}/{revision}");
    if git_succeeds(
        repository,
        &["show-ref", "--verify", "--quiet", &selected_remote],
    )? {
        return Ok(Some(revision.to_string()));
    }
    let prefix = format!("{remote}/");
    if let Some(branch) = revision.strip_prefix(&prefix) {
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        if git_succeeds(
            repository,
            &["show-ref", "--verify", "--quiet", &remote_ref],
        )? {
            return Ok(Some(branch.to_string()));
        }
    }
    Ok(None)
}

fn git_stdout(repository: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(repository, args).map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| "Git returned non-UTF-8 output".to_string())
}

fn git_succeeds(repository: &Path, args: &[&str]) -> Result<bool, RepositoryError> {
    let output = git_output(repository, args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(RepositoryError::Git {
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
    }
}

fn git_output(repository: &Path, args: &[&str]) -> Result<std::process::Output, RepositoryError> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|source| RepositoryError::Spawn {
            path: repository.to_path_buf(),
            source,
        })
}

/// Errors while resolving a Git repository context.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("failed to run git for {path}: {source}")]
    Spawn {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not inside a Git working tree: {message}")]
    NotRepository { path: PathBuf, message: String },
    #[error("Git returned a non-UTF-8 repository path for {path}")]
    NonUtf8 { path: PathBuf },
    #[error("failed to canonicalize repository root {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("repository {0} has no committed HEAD")]
    NoCommittedHead(PathBuf),
    #[error("base revision {revision:?} cannot be resolved to a commit: {message}")]
    InvalidBase { revision: String, message: String },
    #[error("a delivery target is required for base revision {revision:?}; pass --target")]
    TargetRequired { revision: String },
    #[error("invalid delivery destination: {message}")]
    InvalidDelivery { message: String },
    #[error("Git returned non-UTF-8 output")]
    NonUtf8GitOutput,
    #[error("Git failed: {message}")]
    Git { message: String },
}
