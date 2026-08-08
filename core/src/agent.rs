//! Agent invocation. Mirrors `docs/isolation.md`.
//!
//! Agents (PL/IM/RV) run as non-interactive `copilot` calls inside a local
//! sandbox. The [`AgentRunner`] trait abstracts the invocation so tests and the
//! `--dry-run` path can substitute a fake without spawning any process.

use crate::config::Sandbox;
use std::path::PathBuf;
use std::process::Command;

/// Filesystem posture for a sandboxed agent (see `docs/isolation.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    /// PL and RV: analysis only.
    ReadOnly,
    /// IM: may write within its workspace.
    ReadWrite,
}

/// A single agent invocation request.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    /// Role label, for logging/diagnostics (e.g. "PL:planner-a").
    pub role: String,
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
/// (IM) get `--allow-all-tools` (the sandbox is the boundary, and the deny-list
/// still blocks destructive ops); read-only agents (PL, RV) get only
/// `--allow-tool read` so they can inspect but not modify.
pub struct CopilotRunner {
    sandbox: Sandbox,
    program: String,
}

impl CopilotRunner {
    /// A runner using the given sandbox policy and the `copilot` binary.
    pub fn new(sandbox: Sandbox) -> CopilotRunner {
        CopilotRunner {
            sandbox,
            program: "copilot".to_string(),
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
                args.push(directory.display().to_string());
            }
        }
        args.push("--no-ask-user".to_string());
        // Grant tools non-interactively, scoped to the role's posture. With the
        // sandbox as the OS boundary, read/write agents may use broad tools
        // (`--allow-all-tools`); without it, we never grant blanket tools —
        // read/write agents are limited to file tools so a disabled sandbox
        // cannot become arbitrary ambient execution.
        match (req.filesystem, self.sandbox.enabled) {
            (Filesystem::ReadWrite, true) => args.push("--allow-all-tools".to_string()),
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
        if !req.model.is_empty() {
            args.push("--model".to_string());
            args.push(req.model.clone());
        }
        args.push("-p".to_string());
        args.push(req.prompt.clone());
        args
    }
}

impl AgentRunner for CopilotRunner {
    fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
        let output = Command::new(&self.program)
            .args(self.args(req))
            .current_dir(&req.cwd)
            .output()
            .map_err(|source| AgentError::Spawn {
                role: req.role.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(AgentError::NonZeroExit {
                role: req.role.clone(),
                code: output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Fake runner for tests and `--dry-run`: returns a canned response without
/// spawning any process.
pub struct EchoRunner;

impl AgentRunner for EchoRunner {
    fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
        // Intake-questions asks whether clarification is needed; the stub never
        // has questions, so it returns NONE (no blocking under --dry-run).
        if req.role.contains("intake") {
            return Ok("NONE".to_string());
        }
        // A universal stub: a `## Plan`, a `CONVERGED` convergence signal, and an
        // `ACCEPT` verdict, so the whole pipeline advances in a single pass under
        // `--dry-run`.
        Ok(format!(
            "## Plan\nDry-run stub for {}; no model was called.\n\n## Steps\n1. TODO\n\n## Risks & assumptions\n- Dry run.\n\n## Convergence\nCONVERGED\n\n## Verdict\nACCEPT\n\n## Findings\nNONE",
            req.role
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> AgentRequest {
        AgentRequest {
            role: "PL:planner-a".to_string(),
            prompt: "do the thing".to_string(),
            cwd: PathBuf::from("/tmp"),
            filesystem: Filesystem::ReadOnly,
            model: String::new(),
            iteration: None,
            additional_dirs: vec![],
        }
    }

    #[test]
    fn command_includes_sandbox_and_noninteractive_flags() {
        let runner = CopilotRunner::new(Sandbox::default());
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
        let runner = CopilotRunner::new(Sandbox::default());
        let mut r = req();
        r.model = "some-model".to_string();
        let args = runner.args(&r);
        let m = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[m + 1], "some-model");
    }

    #[test]
    fn read_only_role_gets_read_grant_only() {
        let runner = CopilotRunner::new(Sandbox::default());
        let args = runner.args(&req()); // req() is ReadOnly
        assert!(!args.contains(&"--allow-all-tools".to_string()));
        let a = args.iter().position(|a| a == "--allow-tool").unwrap();
        assert_eq!(args[a + 1], "read");
    }

    #[test]
    fn read_write_role_gets_allow_all_tools() {
        let runner = CopilotRunner::new(Sandbox::default());
        let mut r = req();
        r.filesystem = Filesystem::ReadWrite;
        let args = runner.args(&r);
        assert!(args.contains(&"--allow-all-tools".to_string()));
        assert!(!args.contains(&"--allow-tool".to_string()));
        // The deny-list still applies even with allow-all.
        assert!(args.contains(&"shell(rm)".to_string()));
    }

    #[test]
    fn sandbox_includes_additional_directories() {
        let runner = CopilotRunner::new(Sandbox::default());
        let mut request = req();
        request.additional_dirs = vec![PathBuf::from("/repo/.git")];
        let args = runner.args(&request);
        let position = args.iter().position(|arg| arg == "--add-dir").unwrap();
        assert_eq!(args[position + 1], "/repo/.git");
    }

    #[test]
    fn disabled_sandbox_omits_additional_directories() {
        let runner = CopilotRunner::new(Sandbox {
            enabled: false,
            experimental: false,
            deny_tools: vec![],
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
        };
        let runner = CopilotRunner::new(sandbox);
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
        };
        let runner = CopilotRunner::new(sandbox);
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
        assert!(out.contains("PL:planner-a"));
    }
}
