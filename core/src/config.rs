//! Configuration. Mirrors `docs/config.md`.
//!
//! Loaded from a YAML file (default `~/.quorum/config.yaml`). Every key has a
//! default; the file is optional. Precedence (CLI > file > default) is applied
//! by callers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where per-WI state and assets live (see `docs/persistence.md`).
    pub state_dir: PathBuf,
    /// Planner roster override: slot -> model id (see `docs/agents.md`).
    pub planners: BTreeMap<String, String>,
    /// Model targets for the other roles.
    pub models: Models,
    /// Human-review gates.
    pub reviews: Reviews,
    /// Loop bounds and resilience knobs.
    pub limits: Limits,
    /// Execution isolation (see `docs/isolation.md`).
    pub sandbox: Sandbox,
}

/// Model ids for the non-planner roles.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Models {
    pub implementer: String,
    /// Must differ from `implementer` for the adversarial loop to be meaningful.
    pub reviewer: String,
    /// Used for merge/convergence prompts.
    pub coordinator: String,
}

/// Optional human-review gates (see `docs/state-machine.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reviews {
    pub plan_review: bool,
    pub work_review: bool,
}

/// Loop bounds and resilience limits (see `docs/persistence.md`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub convergence_max_iters: u32,
    pub convergence_diff_threshold: f64,
    pub adversarial_max_iters: u32,
    pub step_retries: u32,
    pub step_timeout_secs: u64,
}

/// Execution isolation applied to every agent invocation (see `docs/isolation.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sandbox {
    /// Run agents inside Copilot's local sandbox.
    pub enabled: bool,
    /// The local sandbox currently requires `--experimental`.
    pub experimental: bool,
    /// Destructive tools denied even inside the sandbox (defense in depth).
    pub deny_tools: Vec<String>,
}

/// Errors surfaced while loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

impl Default for Config {
    fn default() -> Self {
        Config {
            state_dir: default_state_dir(),
            planners: default_planners(),
            models: Models::default(),
            reviews: Reviews::default(),
            limits: Limits::default(),
            sandbox: Sandbox::default(),
        }
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox {
            enabled: true,
            experimental: true,
            deny_tools: vec!["shell(rm)".to_string()],
        }
    }
}

impl Default for Reviews {
    fn default() -> Self {
        Reviews {
            plan_review: true,
            work_review: true,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            convergence_max_iters: 5,
            convergence_diff_threshold: 0.1,
            adversarial_max_iters: 5,
            step_retries: 3,
            step_timeout_secs: 600,
        }
    }
}

fn default_state_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".quorum")
        .join("state")
}

/// The fixed default planner roster (see `docs/agents.md`). Model ids are left
/// empty here and filled in by the user's config file.
fn default_planners() -> BTreeMap<String, String> {
    ["planner-a", "planner-b", "planner-c"]
        .into_iter()
        .map(|slot| (slot.to_string(), String::new()))
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl Config {
    /// Load config from `path`, merging over defaults. A missing file yields
    /// defaults; a present but partial file overrides only the keys it sets.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let cfg = match std::fs::read_to_string(path) {
            Ok(text) => serde_yaml::from_str::<Config>(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// The default config file path (`~/.quorum/config.yaml`).
    pub fn default_path() -> PathBuf {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".quorum")
            .join("config.yaml")
    }

    /// Validate cross-field invariants. Only checks constraints that must hold
    /// regardless of whether model ids have been filled in yet.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.models.reviewer.is_empty() && self.models.reviewer == self.models.implementer {
            return Err(ConfigError::Invalid(
                "models.reviewer must differ from models.implementer".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert!(c.reviews.plan_review);
        assert!(c.reviews.work_review);
        assert_eq!(c.limits.convergence_max_iters, 5);
        assert_eq!(c.limits.adversarial_max_iters, 5);
        assert_eq!(c.limits.step_retries, 3);
        assert_eq!(c.limits.step_timeout_secs, 600);
        assert!(c.planners.contains_key("planner-a"));
        assert!(c.planners.contains_key("planner-b"));
        assert!(c.planners.contains_key("planner-c"));
        assert!(c.sandbox.enabled);
        assert!(c.sandbox.experimental);
        assert_eq!(c.sandbox.deny_tools, vec!["shell(rm)".to_string()]);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load(Path::new("/does/not/exist.yaml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn partial_file_overrides_only_set_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "reviews:\n  plan_review: false\n").unwrap();
        let c = Config::load(&path).unwrap();
        assert!(!c.reviews.plan_review);
        // Untouched keys keep their defaults.
        assert!(c.reviews.work_review);
        assert_eq!(c.limits.step_retries, 3);
    }

    #[test]
    fn reviewer_equal_to_implementer_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "models:\n  implementer: x\n  reviewer: x\n").unwrap();
        assert!(Config::load(&path).is_err());
    }
}
