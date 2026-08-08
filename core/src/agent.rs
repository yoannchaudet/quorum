//! Agent invocation. Mirrors `docs/isolation.md`.
//!
//! Agents run as non-interactive `copilot` calls inside a local
//! sandbox. The [`AgentRunner`] trait abstracts the invocation so tests and the
//! `--dry-run` path can substitute a fake without spawning any process.

use crate::config::Sandbox;
use crate::{BrowserCapability, ExecutionCapabilities, LocalServerCapability};
use serde_json::json;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const CLEANUP_GRACE: Duration = Duration::from_millis(250);

/// Filesystem posture for a sandboxed agent (see `docs/isolation.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    /// Planners and the Reviewer: analysis only.
    ReadOnly,
    /// Implementer: may write within its workspace.
    ReadWrite,
}

/// Canonical role for one agent invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRole {
    IntakePlanner { slot: String },
    Planner { slot: String },
    CoordinatorMerge,
    Implementer,
    Reviewer,
}

impl AgentRole {
    pub fn intake_planner(slot: impl Into<String>) -> AgentRole {
        AgentRole::IntakePlanner { slot: slot.into() }
    }

    pub fn planner(slot: impl Into<String>) -> AgentRole {
        AgentRole::Planner { slot: slot.into() }
    }
}

impl fmt::Display for AgentRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentRole::IntakePlanner { slot } => write!(formatter, "Intake Planner:{slot}"),
            AgentRole::Planner { slot } => write!(formatter, "Planner:{slot}"),
            AgentRole::CoordinatorMerge => formatter.write_str("Coordinator:merge"),
            AgentRole::Implementer => formatter.write_str("Implementer"),
            AgentRole::Reviewer => formatter.write_str("Reviewer"),
        }
    }
}

/// A single agent invocation request.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    /// Typed role used for behavior, logging, and diagnostics.
    pub role: AgentRole,
    /// The fully rendered prompt.
    pub prompt: String,
    /// Working directory the agent runs in (its sandbox cwd).
    pub cwd: PathBuf,
    /// Filesystem posture for this role.
    pub filesystem: Filesystem,
    /// The model id to run, if configured. Empty means use the CLI default.
    pub model: String,
    /// Planning, implementation, or review iteration when applicable.
    pub iteration: Option<u32>,
    /// Additional sandbox paths required by this invocation.
    pub additional_dirs: Vec<PathBuf>,
    /// Human-approved execution grant. Present only for the Implementer.
    pub execution: Option<ExecutionCapabilities>,
}

/// Errors from running an agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("failed to spawn agent for {role}: {source}")]
    Spawn {
        role: String,
        source: std::io::Error,
    },
    #[error("agent for {role} exited with status {code}: {stderr}")]
    NonZeroExit {
        role: String,
        code: String,
        stderr: String,
    },
    #[error("agent for {role} timed out after {seconds}s: {stderr}")]
    Timeout {
        role: String,
        seconds: u64,
        stderr: String,
    },
    #[error("failed to prepare runtime for {role}: {source}")]
    Runtime {
        role: String,
        source: std::io::Error,
    },
    #[error("failed to serialize browser configuration for {role}: {source}")]
    BrowserConfig {
        role: String,
        source: serde_json::Error,
    },
    #[error("refusing to run {role} without the local sandbox")]
    SandboxDisabled { role: String },
}

/// Runs an agent and returns its captured stdout.
pub trait AgentRunner: Send + Sync {
    fn run(&self, req: &AgentRequest) -> Result<String, AgentError>;
}

/// Real runner: invokes the `copilot` CLI, sandboxed and non-interactive.
///
/// Command shape (see `docs/isolation.md`):
/// `copilot --sandbox --experimental --no-ask-user <tool grants> -p <prompt> --deny-tool <...>`.
///
/// Because the run is non-interactive (`--no-ask-user`), copilot cannot prompt
/// for tool approval and would otherwise deny every action. We therefore grant
/// tools up front, scoped by the request's filesystem posture: read/write agents
/// (the Implementer) get `--allow-all-tools` (the sandbox is the boundary, and the
/// deny-list still blocks destructive ops); read-only Planners and Reviewers get only
/// `--allow-tool read` so they can inspect but not modify.
pub struct CopilotRunner {
    sandbox: Sandbox,
    program: String,
    timeout: Duration,
    runtime_dir: PathBuf,
}

impl CopilotRunner {
    /// A runner using the given sandbox policy and the `copilot` binary.
    pub fn new(sandbox: Sandbox, timeout: Duration, runtime_dir: PathBuf) -> CopilotRunner {
        CopilotRunner {
            sandbox,
            program: "copilot".to_string(),
            timeout,
            runtime_dir,
        }
    }

    /// Build the argument vector for a request (without the program name).
    fn args(&self, req: &AgentRequest) -> Vec<String> {
        let mut args = Vec::new();
        if self.sandbox.enabled {
            args.push("--sandbox".to_string());
            if self.sandbox.experimental {
                args.push("--experimental".to_string());
            }
            for directory in &req.additional_dirs {
                args.push("--add-dir".to_string());
                args.push(policy_path(directory));
            }
            if req.role == AgentRole::Implementer {
                args.push("--add-dir".to_string());
                args.push(policy_path(&self.runtime_dir));
                if req.execution.as_ref().is_some_and(|grant| grant.shell) {
                    args.push("--add-dir".to_string());
                    args.push(policy_path(&std::env::temp_dir()));
                }
            }
        }
        args.push("--no-ask-user".to_string());
        args.push("--no-remote".to_string());
        args.push("--no-remote-export".to_string());
        args.push("--no-auto-update".to_string());
        args.push("--disable-builtin-mcps".to_string());
        // Grant tools non-interactively, scoped to the role's posture. With the
        // sandbox as the OS boundary, read/write agents may use broad tools
        // (`--allow-all-tools`); without it, we never grant blanket tools —
        // read/write agents are limited to file tools so a disabled sandbox
        // cannot become arbitrary ambient execution.
        let execution = req.execution.as_ref();
        match (req.filesystem, self.sandbox.enabled) {
            (Filesystem::ReadWrite, true) if execution.is_some_and(|grant| grant.shell) => {
                args.push("--allow-all-tools".to_string())
            }
            (Filesystem::ReadWrite, true) => {
                args.push("--allow-tool".to_string());
                args.push("read,write".to_string());
            }
            (Filesystem::ReadWrite, false) => {
                args.push("--allow-tool".to_string());
                args.push("read,write".to_string());
            }
            (Filesystem::ReadOnly, _) => {
                args.push("--allow-tool".to_string());
                args.push("read".to_string());
            }
        }
        for tool in &self.sandbox.deny_tools {
            args.push("--deny-tool".to_string());
            args.push(tool.clone());
        }
        if execution.is_some_and(|grant| grant.internet) && self.sandbox.allow_outbound {
            args.push("--allow-all-urls".to_string());
        }
        let secret_names = secret_environment_names();
        if !secret_names.is_empty() {
            args.push(format!("--secret-env-vars={}", secret_names.join(",")));
        }
        if self.browser_enabled(req) {
            args.push(format!(
                "--additional-mcp-config=@{}",
                self.runtime_dir.join("playwright-mcp.json").display()
            ));
        }
        if !req.model.is_empty() {
            args.push("--model".to_string());
            args.push(req.model.clone());
        }
        args.push("-p".to_string());
        args.push(req.prompt.clone());
        args
    }

    fn prepare_runtime(&self, req: &AgentRequest) -> Result<(), AgentError> {
        fs::create_dir_all(&self.runtime_dir).map_err(|source| AgentError::Runtime {
            role: req.role.to_string(),
            source,
        })?;
        self.prepare_copilot_home(req)?;
        if req.role != AgentRole::Implementer {
            return Ok(());
        }
        fs::create_dir_all(self.runtime_dir.join("artifacts")).map_err(|source| {
            AgentError::Runtime {
                role: req.role.to_string(),
                source,
            }
        })?;
        if !self.browser_enabled(req) {
            return Ok(());
        }
        let mut playwright_args = vec![
            "--yes".to_string(),
            self.sandbox.browser.package.clone(),
            "--isolated".to_string(),
            "--browser=chromium".to_string(),
            "--caps=vision,devtools".to_string(),
            "--viewport-size=1440x900".to_string(),
            format!(
                "--output-dir={}",
                self.runtime_dir.join("artifacts").display()
            ),
            "--output-max-size=52428800".to_string(),
            "--save-session".to_string(),
        ];
        let headed = req
            .execution
            .as_ref()
            .is_some_and(|grant| grant.browser == BrowserCapability::Headed);
        if !headed || !self.sandbox.browser.headed || !graphical_display_available() {
            playwright_args.push("--headless".to_string());
        }
        let config = json!({
            "mcpServers": {
                "playwright": {
                    "type": "local",
                    "command": "npx",
                    "tools": ["*"],
                    "args": playwright_args
                }
            }
        });
        let text =
            serde_json::to_vec_pretty(&config).map_err(|source| AgentError::BrowserConfig {
                role: req.role.to_string(),
                source,
            })?;
        fs::write(self.runtime_dir.join("playwright-mcp.json"), text).map_err(|source| {
            AgentError::Runtime {
                role: req.role.to_string(),
                source,
            }
        })
    }

    fn prepare_copilot_home(&self, req: &AgentRequest) -> Result<(), AgentError> {
        let home = self.copilot_home();
        match fs::remove_dir_all(&home) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AgentError::Runtime {
                    role: req.role.to_string(),
                    source,
                })
            }
        }
        fs::create_dir_all(&home).map_err(|source| AgentError::Runtime {
            role: req.role.to_string(),
            source,
        })?;
        self.link_auth_state(&home, req)?;

        let temporary = std::env::temp_dir();
        let mut readonly_paths = req
            .additional_dirs
            .iter()
            .map(|path| policy_path(path))
            .collect::<Vec<_>>();
        let mut readwrite_paths = vec![policy_path(&temporary)];
        if req.filesystem == Filesystem::ReadOnly {
            readonly_paths.push(policy_path(&req.cwd));
        } else {
            readwrite_paths.push(policy_path(&req.cwd));
            readwrite_paths.push(policy_path(&self.runtime_dir));
        }
        let denied_paths = sensitive_paths(&home);
        let settings = json!({
            "experimental": self.sandbox.experimental,
            "sandbox": {
                "enabled": true,
                "addCurrentWorkingDirectory": false,
                "allowDevToolAccess": req.execution.as_ref().is_some_and(|grant| grant.shell),
                "allowBypass": false,
                "auth": {
                    "git": false,
                    "gh": false
                },
                "sandboxMcpServers": !self.browser_enabled(req),
                "sandboxLspServers": true,
                "userPolicy": {
                    "filesystem": {
                        "readwritePaths": readwrite_paths,
                        "readonlyPaths": readonly_paths,
                        "deniedPaths": denied_paths,
                        "clearPolicyOnExit": true
                    },
                    "network": {
                        "allowOutbound": req.execution.as_ref().is_some_and(|grant| grant.internet)
                            && self.sandbox.allow_outbound,
                        "allowLocalNetwork": req.execution.as_ref().is_some_and(|grant| {
                            grant.local_server == LocalServerCapability::Loopback
                        }) && self.sandbox.allow_local_network
                    },
                    "seatbelt": {
                        "keychainAccess": false
                    }
                }
            }
        });
        let text =
            serde_json::to_vec_pretty(&settings).map_err(|source| AgentError::BrowserConfig {
                role: req.role.to_string(),
                source,
            })?;
        fs::write(home.join("settings.json"), text).map_err(|source| AgentError::Runtime {
            role: req.role.to_string(),
            source,
        })
    }

    fn link_auth_state(
        &self,
        home: &std::path::Path,
        req: &AgentRequest,
    ) -> Result<(), AgentError> {
        let source_home = std::env::var_os("COPILOT_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".copilot")));
        let Some(source) = source_home.map(|path| path.join("config.json")) else {
            return Ok(());
        };
        if !source.exists() {
            return Ok(());
        }
        let target = home.join("config.json");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&source, &target).map_err(|source| AgentError::Runtime {
                role: req.role.to_string(),
                source,
            })?;
        }
        #[cfg(not(unix))]
        {
            fs::copy(&source, &target).map_err(|source| AgentError::Runtime {
                role: req.role.to_string(),
                source,
            })?;
        }
        Ok(())
    }

    fn copilot_home(&self) -> PathBuf {
        let name = self
            .runtime_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime");
        self.runtime_dir
            .with_file_name(format!(".{name}-copilot-home"))
    }

    fn cleanup_copilot_home(&self) {
        let _ = fs::remove_dir_all(self.copilot_home());
    }

    fn browser_enabled(&self, req: &AgentRequest) -> bool {
        self.sandbox.browser.enabled
            && self.sandbox.allow_outbound
            && req.execution.as_ref().is_some_and(|grant| {
                grant.browser != BrowserCapability::None && grant.artifacts && grant.internet
            })
    }
}

impl AgentRunner for CopilotRunner {
    fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
        if !self.sandbox.enabled {
            return Err(AgentError::SandboxDisabled {
                role: req.role.to_string(),
            });
        }
        if let Err(error) = self.prepare_runtime(req) {
            self.cleanup_copilot_home();
            return Err(error);
        }
        let mut command = Command::new(&self.program);
        command
            .args(self.args(req))
            .current_dir(&req.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.env("COPILOT_HOME", self.copilot_home());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(source) => {
                self.cleanup_copilot_home();
                return Err(AgentError::Spawn {
                    role: req.role.to_string(),
                    source,
                });
            }
        };
        let stdout_reader = child.stdout.take().map(read_bounded);
        let stderr_reader = child.stderr.take().map(read_bounded);
        let timeout = req
            .execution
            .as_ref()
            .map(|grant| Duration::from_secs(grant.timeout_seconds()))
            .map(|requested| requested.min(self.timeout))
            .unwrap_or(self.timeout);
        let status = match child.wait_timeout(timeout) {
            Ok(status) => status,
            Err(source) => {
                terminate_process_tree(&mut child);
                self.cleanup_copilot_home();
                return Err(AgentError::Spawn {
                    role: req.role.to_string(),
                    source,
                });
            }
        };
        let timed_out = status.is_none();
        if timed_out {
            terminate_process_tree(&mut child);
        }
        let status = match status {
            Some(status) => status,
            None => match child.wait() {
                Ok(status) => status,
                Err(source) => {
                    self.cleanup_copilot_home();
                    return Err(AgentError::Spawn {
                        role: req.role.to_string(),
                        source,
                    });
                }
            },
        };
        terminate_process_tree(&mut child);
        let stdout = join_output(stdout_reader);
        let stderr = join_output(stderr_reader);
        self.cleanup_copilot_home();
        if timed_out {
            return Err(AgentError::Timeout {
                role: req.role.to_string(),
                seconds: timeout.as_secs(),
                stderr,
            });
        }
        if !status.success() {
            return Err(AgentError::NonZeroExit {
                role: req.role.to_string(),
                code: status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr,
            });
        }
        Ok(stdout)
    }
}

fn read_bounded<R: Read + Send + 'static>(mut reader: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut tail = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    tail.extend_from_slice(&buffer[..read]);
                    if tail.len() > OUTPUT_LIMIT_BYTES {
                        tail.drain(..tail.len() - OUTPUT_LIMIT_BYTES);
                    }
                }
            }
        }
        String::from_utf8_lossy(&tail).trim().to_string()
    })
}

fn join_output(reader: Option<thread::JoinHandle<String>>) -> String {
    reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let process_group = -(child.id() as i32);
        if libc::kill(process_group, libc::SIGTERM) == 0 {
            thread::sleep(CLEANUP_GRACE);
        }
        let _ = libc::kill(process_group, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn secret_environment_names() -> Vec<String> {
    let mut names = std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let value = value.into_string().unwrap_or_default();
            is_secret_environment(&name, &value).then_some(name)
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn is_secret_environment(name: &str, value: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "API_KEY",
    ]
    .iter()
    .any(|part| upper.contains(part))
        || upper.starts_with("AWS_")
        || upper.starts_with("AZURE_")
        || upper.starts_with("GOOGLE_")
        || upper.starts_with("GCP_")
        || upper.starts_with("GCLOUD_")
        || upper.ends_with("_PRIVATE_KEY")
        || upper.ends_with("_KEY")
        || upper.ends_with("_DSN")
        || upper == "SSH_AUTH_SOCK"
        || ((upper.ends_with("_URL") || upper.ends_with("_URI"))
            && value.contains("://")
            && value.contains('@'))
}

fn graphical_display_available() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return std::env::var_os("CI").is_none();
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn sensitive_paths(isolated_home: &std::path::Path) -> Vec<String> {
    let mut paths = vec![policy_path(isolated_home)];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for relative in [
            ".copilot",
            ".ssh",
            ".aws",
            ".azure",
            ".config/gh",
            ".config/gcloud",
            "Library/Keychains",
        ] {
            paths.push(policy_path(&home.join(relative)));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn policy_path(path: &std::path::Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Fake runner for tests and `--dry-run`: returns a canned response without
/// spawning any process.
pub struct EchoRunner;

impl AgentRunner for EchoRunner {
    fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
        // Intake-questions asks whether clarification is needed; the stub never
        // has questions, so it returns NONE (no blocking under --dry-run).
        if matches!(req.role, AgentRole::IntakePlanner { .. }) {
            return Ok("NONE".to_string());
        }
        // A universal stub: a `## Plan`, a `CONVERGED` convergence signal, and an
        // `ACCEPT` verdict, so the whole pipeline advances in a single pass under
        // `--dry-run`.
        Ok(format!(
            "## Plan\n### Summary\nDry-run stub for {}; no model was called.\n\n### Steps\n1. TODO\n\n### Risks & assumptions\n- Dry run.\n\n### Execution capabilities\n```yaml\nshell: true\ninternet: false\nlocal_server: none\nbrowser: none\nartifacts: false\ntimeout_minutes: 30\n```\n\n## Convergence\nCONVERGED\n\n## Verdict\nACCEPT\n\n## Findings\nNONE",
            req.role
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn runner(sandbox: Sandbox) -> CopilotRunner {
        CopilotRunner::new(
            sandbox,
            Duration::from_secs(10),
            PathBuf::from("/tmp/quorum-runtime"),
        )
    }

    fn req() -> AgentRequest {
        AgentRequest {
            role: AgentRole::planner("planner-a"),
            prompt: "do the thing".to_string(),
            cwd: PathBuf::from("/tmp"),
            filesystem: Filesystem::ReadOnly,
            model: String::new(),
            iteration: None,
            additional_dirs: vec![],
            execution: None,
        }
    }

    fn full_execution() -> ExecutionCapabilities {
        ExecutionCapabilities {
            shell: true,
            internet: true,
            local_server: LocalServerCapability::Loopback,
            browser: BrowserCapability::Headed,
            artifacts: true,
            timeout_minutes: 30,
        }
    }

    #[test]
    fn command_includes_sandbox_and_noninteractive_flags() {
        let runner = runner(Sandbox::default());
        let args = runner.args(&req());
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.contains(&"--experimental".to_string()));
        assert!(args.contains(&"--no-ask-user".to_string()));
        assert!(args.contains(&"--deny-tool".to_string()));
        assert!(args.contains(&"shell(rm)".to_string()));
        // No model configured: --model is omitted.
        assert!(!args.contains(&"--model".to_string()));
        // The prompt is passed via -p.
        let p = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[p + 1], "do the thing");
    }

    #[test]
    fn command_includes_model_when_set() {
        let runner = runner(Sandbox::default());
        let mut r = req();
        r.model = "some-model".to_string();
        let args = runner.args(&r);
        let m = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[m + 1], "some-model");
    }

    #[test]
    fn read_only_role_gets_read_grant_only() {
        let runner = runner(Sandbox::default());
        let args = runner.args(&req()); // req() is ReadOnly
        assert!(!args.contains(&"--allow-all-tools".to_string()));
        let a = args.iter().position(|a| a == "--allow-tool").unwrap();
        assert_eq!(args[a + 1], "read");
    }

    #[test]
    fn read_write_role_gets_allow_all_tools() {
        let runner = runner(Sandbox::default());
        let mut r = req();
        r.filesystem = Filesystem::ReadWrite;
        r.execution = Some(full_execution());
        let args = runner.args(&r);
        assert!(args.contains(&"--allow-all-tools".to_string()));
        assert!(!args.contains(&"--allow-tool".to_string()));
        // The deny-list still applies even with allow-all.
        assert!(args.contains(&"shell(rm)".to_string()));
    }

    #[test]
    fn sandbox_includes_additional_directories() {
        let runner = runner(Sandbox::default());
        let mut request = req();
        request.additional_dirs = vec![PathBuf::from("/repo/.git")];
        let args = runner.args(&request);
        let position = args.iter().position(|arg| arg == "--add-dir").unwrap();
        assert_eq!(args[position + 1], "/repo/.git");
    }

    #[test]
    fn disabled_sandbox_omits_additional_directories() {
        let runner = runner(Sandbox {
            enabled: false,
            experimental: false,
            deny_tools: vec![],
            ..Sandbox::default()
        });
        let mut request = req();
        request.additional_dirs = vec![PathBuf::from("/repo/.git")];
        let args = runner.args(&request);
        assert!(!args.contains(&"--add-dir".to_string()));
    }

    #[test]
    fn disabled_sandbox_omits_sandbox_flags() {
        let sandbox = Sandbox {
            enabled: false,
            experimental: true,
            deny_tools: vec![],
            ..Sandbox::default()
        };
        let runner = runner(sandbox);
        let args = runner.args(&req());
        assert!(!args.contains(&"--sandbox".to_string()));
        assert!(!args.contains(&"--experimental".to_string()));
        assert!(args.contains(&"--no-ask-user".to_string()));
    }

    #[test]
    fn read_write_without_sandbox_scopes_to_file_tools() {
        // Without the OS boundary we must NOT grant blanket tools.
        let sandbox = Sandbox {
            enabled: false,
            experimental: false,
            deny_tools: vec![],
            ..Sandbox::default()
        };
        let runner = runner(sandbox);
        let mut r = req();
        r.filesystem = Filesystem::ReadWrite;
        let args = runner.args(&r);
        assert!(!args.contains(&"--allow-all-tools".to_string()));
        let a = args.iter().position(|a| a == "--allow-tool").unwrap();
        assert_eq!(args[a + 1], "read,write");
    }

    #[test]
    fn echo_runner_returns_stub() {
        let out = EchoRunner.run(&req()).unwrap();
        assert!(out.contains("Dry-run stub"));
        assert!(out.contains("Planner:planner-a"));
    }

    #[test]
    fn role_labels_use_full_names() {
        assert_eq!(
            AgentRole::intake_planner("planner-a").to_string(),
            "Intake Planner:planner-a"
        );
        assert_eq!(
            AgentRole::planner("planner-b").to_string(),
            "Planner:planner-b"
        );
        assert_eq!(AgentRole::CoordinatorMerge.to_string(), "Coordinator:merge");
        assert_eq!(AgentRole::Implementer.to_string(), "Implementer");
        assert_eq!(AgentRole::Reviewer.to_string(), "Reviewer");
    }

    #[test]
    fn implementer_browser_config_is_pinned_and_isolated() {
        let runtime = tempfile::tempdir().unwrap();
        let runner = CopilotRunner::new(
            Sandbox::default(),
            Duration::from_secs(10),
            runtime.path().to_path_buf(),
        );
        let mut request = req();
        request.role = AgentRole::Implementer;
        request.filesystem = Filesystem::ReadWrite;
        request.execution = Some(full_execution());
        runner.prepare_runtime(&request).unwrap();
        let config: serde_json::Value = serde_json::from_slice(
            &std::fs::read(runtime.path().join("playwright-mcp.json")).unwrap(),
        )
        .unwrap();
        let args = config["mcpServers"]["playwright"]["args"]
            .as_array()
            .unwrap();
        assert!(args.iter().any(|value| value == "@playwright/mcp@0.0.79"));
        assert!(args.iter().any(|value| value == "--isolated"));
        assert!(runtime.path().join("artifacts").is_dir());
        runner.cleanup_copilot_home();
    }

    #[test]
    fn implementer_browser_respects_outbound_ceiling() {
        let runtime = tempfile::tempdir().unwrap();
        let sandbox = Sandbox {
            allow_outbound: false,
            ..Sandbox::default()
        };
        let runner = CopilotRunner::new(
            sandbox,
            Duration::from_secs(10),
            runtime.path().to_path_buf(),
        );
        let mut request = req();
        request.role = AgentRole::Implementer;
        request.filesystem = Filesystem::ReadWrite;
        request.execution = Some(full_execution());

        assert!(!runner.browser_enabled(&request));
        runner.prepare_runtime(&request).unwrap();
        assert!(!runtime.path().join("playwright-mcp.json").exists());
        runner.cleanup_copilot_home();
    }

    #[test]
    fn implementer_policy_allows_runtime_temp_and_loopback() {
        let runtime = tempfile::tempdir().unwrap();
        let runner = CopilotRunner::new(
            Sandbox::default(),
            Duration::from_secs(10),
            runtime.path().to_path_buf(),
        );
        let mut request = req();
        request.role = AgentRole::Implementer;
        request.filesystem = Filesystem::ReadWrite;
        request.execution = Some(full_execution());
        request.cwd = runtime.path().join("workspace");
        request.additional_dirs = vec![runtime.path().join("git-common")];
        let args = runner.args(&request);
        let temporary = policy_path(&std::env::temp_dir());
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--add-dir" && pair[1] == temporary));
        runner.prepare_runtime(&request).unwrap();

        let settings: serde_json::Value = serde_json::from_slice(
            &std::fs::read(runner.copilot_home().join("settings.json")).unwrap(),
        )
        .unwrap();
        let sandbox = &settings["sandbox"];
        assert_eq!(sandbox["allowBypass"], false);
        assert_eq!(sandbox["auth"]["git"], false);
        assert_eq!(sandbox["auth"]["gh"], false);
        assert_eq!(sandbox["sandboxMcpServers"], false);
        assert_eq!(sandbox["userPolicy"]["network"]["allowLocalNetwork"], true);
        let readwrite = sandbox["userPolicy"]["filesystem"]["readwritePaths"]
            .as_array()
            .unwrap();
        assert!(readwrite
            .iter()
            .any(|path| path == &policy_path(&request.cwd)));
        assert!(readwrite
            .iter()
            .any(|path| path == &policy_path(runtime.path())));
        assert!(readwrite
            .iter()
            .any(|path| path == &policy_path(&std::env::temp_dir())));
        let readonly = sandbox["userPolicy"]["filesystem"]["readonlyPaths"]
            .as_array()
            .unwrap();
        assert!(readonly
            .iter()
            .any(|path| path == &policy_path(&request.additional_dirs[0])));
        assert!(!runner.copilot_home().starts_with(runtime.path()));
        let denied = sandbox["userPolicy"]["filesystem"]["deniedPaths"]
            .as_array()
            .unwrap();
        assert!(denied
            .iter()
            .any(|path| path == &policy_path(&runner.copilot_home())));
        runner.cleanup_copilot_home();
    }

    #[test]
    fn read_only_policy_does_not_grant_workspace_writes() {
        let runtime = tempfile::tempdir().unwrap();
        let runner = CopilotRunner::new(
            Sandbox::default(),
            Duration::from_secs(10),
            runtime.path().to_path_buf(),
        );
        let request = req();
        runner.prepare_runtime(&request).unwrap();
        let settings: serde_json::Value = serde_json::from_slice(
            &std::fs::read(runner.copilot_home().join("settings.json")).unwrap(),
        )
        .unwrap();
        let filesystem = &settings["sandbox"]["userPolicy"]["filesystem"];
        assert!(filesystem["readonlyPaths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == &policy_path(&request.cwd)));
        assert!(!filesystem["readwritePaths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == &policy_path(&request.cwd)));
        runner.cleanup_copilot_home();
    }

    #[test]
    fn secret_environment_name_detection_is_conservative() {
        assert!(is_secret_environment("GITHUB_TOKEN", "secret"));
        assert!(is_secret_environment("database_password", "secret"));
        assert!(is_secret_environment("AWS_ACCESS_KEY_ID", "secret"));
        assert!(is_secret_environment("SSH_AUTH_SOCK", "/tmp/agent.sock"));
        assert!(is_secret_environment(
            "DATABASE_URL",
            "postgres://user:password@localhost/db"
        ));
        assert!(!is_secret_environment("DATABASE_URL", "sqlite:///tmp/db"));
        assert!(!is_secret_environment("PATH", "/bin"));
        assert!(!is_secret_environment("CARGO_HOME", "/tmp/cargo"));
    }

    #[cfg(unix)]
    #[test]
    fn runner_times_out_and_terminates_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("survived");
        let script = temp.path().join("fake-copilot");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n(sleep 1; touch '{}') &\nsleep 10\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut runner = CopilotRunner::new(
            Sandbox {
                browser: crate::config::Browser {
                    enabled: false,
                    ..crate::config::Browser::default()
                },
                ..Sandbox::default()
            },
            Duration::from_millis(100),
            temp.path().join("runtime"),
        );
        runner.program = script.display().to_string();
        let error = runner.run(&req()).unwrap_err();
        assert!(matches!(error, AgentError::Timeout { .. }));
        std::thread::sleep(Duration::from_secs(1));
        assert!(!marker.exists());
    }
}
