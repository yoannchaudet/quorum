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
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Root for Quorum's global database and filesystem state.
    pub data_dir: PathBuf,
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
    /// Administrative ceiling for Plan-authorized outbound internet access.
    pub allow_outbound: bool,
    /// Administrative ceiling for Plan-authorized local-network access.
    pub allow_local_network: bool,
    /// Browser automation available to the Implementer.
    pub browser: Browser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Browser {
    /// Enable the pinned Playwright MCP sidecar.
    pub enabled: bool,
    /// Open a visible isolated browser when the host has a graphical display.
    pub headed: bool,
    /// Exact npm package spec used to start Playwright MCP.
    pub package: String,
}

/// Errors surfaced while loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write config file {path}: {source}")]
    Write {
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
            data_dir: default_data_dir(),
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
            allow_outbound: true,
            allow_local_network: true,
            browser: Browser::default(),
        }
    }
}

impl Default for Browser {
    fn default() -> Self {
        Browser {
            enabled: true,
            headed: true,
            package: "@playwright/mcp@0.0.79".to_string(),
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
            step_timeout_secs: 1800,
        }
    }
}

fn default_data_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".quorum")
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
    /// The global SQLite database path.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("quorum.db")
    }

    /// Root for filesystem state that does not belong in SQLite.
    pub fn state_dir(&self) -> PathBuf {
        self.data_dir.join("state")
    }

    /// Filesystem state for one work item, keyed by its stable internal id.
    pub fn work_item_dir(&self, work_item_id: &str) -> PathBuf {
        self.state_dir().join(work_item_id)
    }

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

    /// Persist this config to `path` as YAML, creating parent directories as
    /// needed. Validates first so an invalid config is never written, and writes
    /// atomically (temp file + rename) so a failed write cannot truncate or
    /// corrupt an existing valid config. A future UX uses this to save user edits
    /// to `~/.quorum/config.yaml`.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let yaml = serde_yaml::to_string(self)?;
        // Write to a sibling temp file in the same directory (so the rename is
        // atomic on the same filesystem), then swap it into place. The name is
        // unique per write so two concurrent savers cannot clobber each other's
        // temp file and rename a half-written config into place.
        let dir = parent
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let temp = dir.join(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("config.yaml"),
            uuid::Uuid::new_v4()
        ));
        let write_err = |source: std::io::Error, at: &Path| ConfigError::Write {
            path: at.to_path_buf(),
            source,
        };
        std::fs::write(&temp, yaml).map_err(|source| write_err(source, &temp))?;
        std::fs::rename(&temp, path).map_err(|source| {
            let _ = std::fs::remove_file(&temp);
            write_err(source, path)
        })
    }

    /// The default config file path (`~/.quorum/config.yaml`).
    pub fn default_path() -> PathBuf {
        home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".quorum")
            .join("config.yaml")
    }

    /// Load from [`Config::default_path`]. Frontends use this instead of deriving the
    /// path themselves, so the Core stays the single owner of where config lives.
    pub fn load_default() -> Result<Config, ConfigError> {
        Config::load(&Config::default_path())
    }

    /// Persist to [`Config::default_path`]. Validates before writing, exactly like
    /// [`Config::save`].
    pub fn save_default(&self) -> Result<(), ConfigError> {
        self.save(&Config::default_path())
    }

    /// Validate cross-field invariants. Only checks constraints that must hold
    /// regardless of whether model ids have been filled in yet.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.models.reviewer.is_empty() && self.models.reviewer == self.models.implementer {
            return Err(ConfigError::Invalid(
                "models.reviewer must differ from models.implementer".to_string(),
            ));
        }
        if self.limits.adversarial_max_iters < 1 {
            return Err(ConfigError::Invalid(
                "limits.adversarial_max_iters must be at least 1".to_string(),
            ));
        }
        if self.limits.convergence_max_iters < 1 {
            return Err(ConfigError::Invalid(
                "limits.convergence_max_iters must be at least 1".to_string(),
            ));
        }
        if self.limits.step_timeout_secs < 1 {
            return Err(ConfigError::Invalid(
                "limits.step_timeout_secs must be at least 1".to_string(),
            ));
        }
        if self.sandbox.browser.enabled && self.sandbox.browser.package.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "sandbox.browser.package must be set when browser automation is enabled"
                    .to_string(),
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
        assert_eq!(c.limits.step_timeout_secs, 1800);
        assert!(c.planners.contains_key("planner-a"));
        assert!(c.planners.contains_key("planner-b"));
        assert!(c.planners.contains_key("planner-c"));
        assert!(c.sandbox.enabled);
        assert!(c.sandbox.experimental);
        assert_eq!(c.sandbox.deny_tools, vec!["shell(rm)".to_string()]);
        assert!(c.sandbox.allow_outbound);
        assert!(c.sandbox.allow_local_network);
        assert!(c.sandbox.browser.enabled);
        assert!(c.sandbox.browser.headed);
        assert_eq!(c.sandbox.browser.package, "@playwright/mcp@0.0.79");
        assert_eq!(c.database_path(), c.data_dir.join("quorum.db"));
        assert_eq!(c.state_dir(), c.data_dir.join("state"));
    }

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load(Path::new("/does/not/exist.yaml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn save_then_load_roundtrips_and_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.yaml");
        let mut original = Config::default();
        original.limits.step_retries = 7;
        original.save(&path).unwrap();
        assert!(path.exists());
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded, original);
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
    fn removed_state_dir_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "state_dir: /tmp/quorum\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn reviewer_equal_to_implementer_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "models:\n  implementer: x\n  reviewer: x\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn zero_adversarial_max_iters_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "limits:\n  adversarial_max_iters: 0\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn concurrent_saves_leave_a_valid_config_and_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        // Two writers racing on the same file must not rename a half-written temp
        // into place, nor leave temp files behind.
        std::thread::scope(|scope| {
            for retries in [7_u32, 9] {
                let path = path.clone();
                scope.spawn(move || {
                    for _ in 0..25 {
                        let mut config = Config::default();
                        config.limits.step_retries = retries;
                        config.save(&path).unwrap();
                    }
                });
            }
        });
        let reloaded = Config::load(&path).unwrap();
        assert!([7, 9].contains(&reloaded.limits.step_retries));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left temp files behind: {leftovers:?}");
    }

    #[test]
    fn invalid_execution_limits_are_rejected() {
        let mut config = Config::default();
        config.limits.step_timeout_secs = 0;
        assert!(config.validate().is_err());

        let mut config = Config::default();
        config.sandbox.browser.package.clear();
        assert!(config.validate().is_err());
    }
}
