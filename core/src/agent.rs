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
/// `copilot --sandbox --experimental --no-ask-user -p <prompt> --deny-tool <...>`.
/// Per-role filesystem posture is carried on the request; the local sandbox
/// enforces it via its settings. Finer per-role enforcement is a follow-up.
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
        }
        args.push("--no-ask-user".to_string());
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
    fn echo_runner_returns_stub() {
        let out = EchoRunner.run(&req()).unwrap();
        assert!(out.contains("Dry-run stub"));
        assert!(out.contains("PL:planner-a"));
    }
}
