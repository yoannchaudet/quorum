//! Git repository discovery for Quorum context resolution.

use std::path::{Path, PathBuf};
use std::process::Command;

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
}
