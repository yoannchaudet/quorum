//! Git worktree lifecycle for WI implementation checkouts.

use crate::persistence::{Database, RegisteredRepository, StoreError, WorkItemId, WorktreeRecord};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Ensure a WI has a ready linked worktree rooted at `preferred_path`.
///
/// The first call pins the repository's committed `HEAD`, persists the setup
/// intent, and then creates the branch/worktree. Later calls reconcile Git with
/// that persisted intent without changing the base.
pub fn ensure_worktree(
    database: &mut Database,
    repository: &RegisteredRepository,
    work_item_id: &WorkItemId,
    slug: &str,
    preferred_path: &Path,
) -> Result<WorktreeRecord, WorktreeError> {
    let preferred_path = normalize_target(preferred_path)?;
    let mut record = match database.worktree(work_item_id)? {
        Some(record) => {
            if record.path != preferred_path {
                return Err(WorktreeError::PathChanged {
                    recorded: record.path,
                    requested: preferred_path,
                });
            }
            record
        }
        None => {
            if branch_exists(&repository.root, &branch_name(slug, work_item_id))? {
                return Err(WorktreeError::BranchCollision(branch_name(
                    slug,
                    work_item_id,
                )));
            }
            if preferred_path.exists() {
                return Err(WorktreeError::PathOccupied(preferred_path));
            }
            let base_commit = committed_head(&repository.root)?;
            let branch = branch_name(slug, work_item_id);
            database.reserve_worktree(work_item_id, &base_commit, &branch, &preferred_path)?
        }
    };

    reconcile(database, repository, &record)?;
    record.ready = true;
    Ok(record)
}

/// Read persisted worktree metadata without touching Git.
pub fn worktree_record(
    database: &Database,
    work_item_id: &WorkItemId,
) -> Result<Option<WorktreeRecord>, WorktreeError> {
    Ok(database.worktree(work_item_id)?)
}

/// Deterministic, Git-safe branch name for a WI.
pub fn branch_name(slug: &str, work_item_id: &WorkItemId) -> String {
    let mut normalized = String::new();
    let mut previous_dash = false;
    for ch in slug.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        if safe {
            normalized.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash && !normalized.is_empty() {
            normalized.push('-');
            previous_dash = true;
        }
        if normalized.len() >= 48 {
            break;
        }
    }
    let normalized = normalized.trim_matches('-');
    let normalized = if normalized.is_empty() {
        "work-item"
    } else {
        normalized
    };
    let short_id = work_item_id.as_str().chars().take(8).collect::<String>();
    format!("quorum/{normalized}-{short_id}")
}

fn reconcile(
    database: &mut Database,
    repository: &RegisteredRepository,
    record: &WorktreeRecord,
) -> Result<(), WorktreeError> {
    let entries = list_worktrees(&repository.root)?;
    if let Some(entry) = entries
        .iter()
        .find(|entry| paths_equal(&entry.path, &record.path))
    {
        if !record.path.is_dir() {
            return Err(WorktreeError::MissingCheckout(record.path.clone()));
        }
        validate_entry(entry, record)?;
        database.mark_worktree_ready(&record.work_item_id)?;
        return Ok(());
    }

    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.branch.as_deref() == Some(record.branch.as_str()))
    {
        return Err(WorktreeError::BranchAttached {
            branch: record.branch.clone(),
            path: entry.path.clone(),
        });
    }
    if record.path.exists() {
        return Err(WorktreeError::PathOccupied(record.path.clone()));
    }

    let branch_ref = format!("refs/heads/{}", record.branch);
    if branch_exists(&repository.root, &record.branch)? {
        if !record.ready {
            let branch_commit = rev_parse_commit(&repository.root, &branch_ref)?;
            if branch_commit != record.base_commit {
                return Err(WorktreeError::BranchBaseMismatch {
                    branch: record.branch.clone(),
                    expected: record.base_commit.clone(),
                    actual: branch_commit,
                });
            }
        }
        run_git(
            &repository.root,
            &["worktree", "add", path_text(&record.path)?, &record.branch],
        )?;
    } else if record.ready {
        return Err(WorktreeError::MissingBranch(record.branch.clone()));
    } else {
        run_git(
            &repository.root,
            &[
                "worktree",
                "add",
                "-b",
                &record.branch,
                path_text(&record.path)?,
                &record.base_commit,
            ],
        )?;
    }

    let entry = list_worktrees(&repository.root)?
        .into_iter()
        .find(|entry| paths_equal(&entry.path, &record.path))
        .ok_or_else(|| WorktreeError::MissingCheckout(record.path.clone()))?;
    validate_entry(&entry, record)?;
    database.mark_worktree_ready(&record.work_item_id)?;
    Ok(())
}

fn validate_entry(entry: &WorktreeEntry, record: &WorktreeRecord) -> Result<(), WorktreeError> {
    if entry.branch.as_deref() != Some(record.branch.as_str()) {
        return Err(WorktreeError::PathUsesDifferentBranch {
            path: record.path.clone(),
            expected: record.branch.clone(),
            actual: entry.branch.clone(),
        });
    }
    if !record.ready && entry.head != record.base_commit {
        return Err(WorktreeError::BranchBaseMismatch {
            branch: record.branch.clone(),
            expected: record.base_commit.clone(),
            actual: entry.head.clone(),
        });
    }
    Ok(())
}

fn committed_head(repository: &Path) -> Result<String, WorktreeError> {
    rev_parse_commit(repository, "HEAD").map_err(|error| match error {
        WorktreeError::Git { .. } => WorktreeError::NoCommittedHead(repository.to_path_buf()),
        other => other,
    })
}

fn rev_parse_commit(repository: &Path, reference: &str) -> Result<String, WorktreeError> {
    let expression = format!("{reference}^{{commit}}");
    let output = run_git(repository, &["rev-parse", "--verify", &expression])?;
    Ok(String::from_utf8(output.stdout)
        .map_err(|_| WorktreeError::NonUtf8GitOutput)?
        .trim()
        .to_string())
}

fn branch_exists(repository: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let reference = format!("refs/heads/{branch}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .output()
        .map_err(|source| WorktreeError::Spawn {
            repository: repository.to_path_buf(),
            source,
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_failure(repository, output)),
    }
}

#[derive(Debug)]
struct WorktreeEntry {
    path: PathBuf,
    head: String,
    branch: Option<String>,
}

fn list_worktrees(repository: &Path) -> Result<Vec<WorktreeEntry>, WorktreeError> {
    let output = run_git(repository, &["worktree", "list", "--porcelain"])?;
    let text = String::from_utf8(output.stdout).map_err(|_| WorktreeError::NonUtf8GitOutput)?;
    let mut entries = Vec::new();
    for block in text.split("\n\n").filter(|block| !block.trim().is_empty()) {
        let mut path = None;
        let mut head = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(value));
            } else if let Some(value) = line.strip_prefix("HEAD ") {
                head = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
                branch = Some(value.to_string());
            }
        }
        if let (Some(path), Some(head)) = (path, head) {
            entries.push(WorktreeEntry { path, head, branch });
        }
    }
    Ok(entries)
}

fn normalize_target(path: &Path) -> Result<PathBuf, WorktreeError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(WorktreeError::CurrentDirectory)?
            .join(path)
    };
    let parent = absolute
        .parent()
        .ok_or_else(|| WorktreeError::InvalidPath(absolute.clone()))?;
    std::fs::create_dir_all(parent).map_err(|source| WorktreeError::CreateParent {
        path: parent.to_path_buf(),
        source,
    })?;
    let parent =
        std::fs::canonicalize(parent).map_err(|source| WorktreeError::CanonicalizeParent {
            path: parent.to_path_buf(),
            source,
        })?;
    let name = absolute
        .file_name()
        .ok_or_else(|| WorktreeError::InvalidPath(absolute.clone()))?;
    Ok(parent.join(name))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn path_text(path: &Path) -> Result<&str, WorktreeError> {
    path.to_str()
        .ok_or_else(|| WorktreeError::NonUtf8Path(path.to_path_buf()))
}

fn run_git(repository: &Path, args: &[&str]) -> Result<Output, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .map_err(|source| WorktreeError::Spawn {
            repository: repository.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(git_failure(repository, output));
    }
    Ok(output)
}

fn git_failure(repository: &Path, output: Output) -> WorktreeError {
    WorktreeError::Git {
        repository: repository.to_path_buf(),
        status: output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_string()),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("failed to run git in {repository}: {source}")]
    Spawn {
        repository: PathBuf,
        source: std::io::Error,
    },
    #[error("git in {repository} exited with status {status}: {stderr}")]
    Git {
        repository: PathBuf,
        status: String,
        stderr: String,
    },
    #[error("repository {0} has no committed HEAD")]
    NoCommittedHead(PathBuf),
    #[error("Git returned non-UTF-8 output")]
    NonUtf8GitOutput,
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("failed to read the current directory: {0}")]
    CurrentDirectory(std::io::Error),
    #[error("invalid worktree path {0}")]
    InvalidPath(PathBuf),
    #[error("failed to create worktree parent {path}: {source}")]
    CreateParent {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to canonicalize worktree parent {path}: {source}")]
    CanonicalizeParent {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "worktree path already exists and is not the recorded checkout: {0}; inspect it and remove it manually only if it is safe"
    )]
    PathOccupied(PathBuf),
    #[error("worktree path changed from {recorded} to {requested}")]
    PathChanged {
        recorded: PathBuf,
        requested: PathBuf,
    },
    #[error("worktree checkout is missing at {0}")]
    MissingCheckout(PathBuf),
    #[error("branch {0} already exists and is not owned by this work item")]
    BranchCollision(String),
    #[error("recorded worktree branch {0} is missing")]
    MissingBranch(String),
    #[error("branch {branch} is already attached at {path}")]
    BranchAttached { branch: String, path: PathBuf },
    #[error("branch {branch} points to {actual}, expected base {expected}")]
    BranchBaseMismatch {
        branch: String,
        expected: String,
        actual: String,
    },
    #[error("worktree {path} uses branch {actual:?}, expected {expected}")]
    PathUsesDifferentBranch {
        path: PathBuf,
        expected: String,
        actual: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_are_safe_and_include_identity() {
        let id = WorkItemId::for_test("12345678-aaaa-bbbb-cccc-dddddddddddd");
        assert_eq!(
            branch_name("Feature: bad name.lock", &id),
            "quorum/feature-bad-name-lock-12345678"
        );
        assert_eq!(branch_name("🚀", &id), "quorum/work-item-12345678");
    }
}
