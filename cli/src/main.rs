//! Quorum CLI — a thin driver over `quorum-core` for a single work item (WI).
//!
//! See `docs/architecture.md`. This binary parses arguments, calls the Core,
//! and renders state. It holds no business logic.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quorum_core::{
    agent::{AgentRunner, EchoRunner},
    Config, Coordinator, CopilotRunner, Decision, State, Store,
};

#[derive(Parser)]
#[command(name = "quorum", version, about = "Drive a single Quorum work item")]
struct Cli {
    /// Path to the config file (default: ~/.quorum/config.yaml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start (or resume) processing a work item from a markdown file.
    Run {
        /// Path to the work item markdown file.
        work_item: PathBuf,
        /// Use stub agents instead of invoking copilot (offline; no model calls).
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the current state of a work item.
    Status {
        /// Path to the work item's state database (quorum.db).
        db: PathBuf,
    },
    /// Approve the current review gate (PlanReview/WorkReview) and continue.
    Approve {
        /// The work item id (the state directory name).
        wi: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reject the current review gate and send the work back a phase.
    Reject {
        /// The work item id (the state directory name).
        wi: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Answer planner questions at IntakeReview and continue.
    Answer {
        /// The work item id (the state directory name).
        wi: String,
        /// The answer text (or use --file).
        text: Option<String>,
        /// Read the answer from a file instead of an argument.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Abandon the work item (from any blocked state).
    Abandon {
        /// The work item id (the state directory name).
        wi: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)
        .with_context(|| format!("loading config from {}", config_path.display()))?;

    match cli.command {
        Command::Run { work_item, dry_run } => {
            let wi_id = work_item_id(&work_item);
            let db_path = config.state_dir.join(&wi_id).join("quorum.db");
            let workspace = db_path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            std::fs::create_dir_all(&workspace)
                .with_context(|| format!("creating {}", workspace.display()))?;

            let mut store = Store::open(&db_path).context("opening state database")?;
            // Intake: load the WI markdown once, if not already stored.
            if store.work_item().context("reading work item")?.is_none() {
                let text = std::fs::read_to_string(&work_item)
                    .with_context(|| format!("reading work item {}", work_item.display()))?;
                store.set_work_item(&text).context("storing work item")?;
            }

            let runner: Box<dyn AgentRunner> = if dry_run {
                Box::new(EchoRunner)
            } else {
                Box::new(CopilotRunner::new(config.sandbox.clone()))
            };
            let mut co = Coordinator::new(config, store, runner, workspace)
                .context("initializing coordinator")?;
            co.run_until_blocked().context("advancing work item")?;
            report(&wi_id, co.state());
        }
        Command::Status { db } => {
            let wi_id = db
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("work-item")
                .to_string();
            let workspace = db
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let store = Store::open(&db).context("opening state database")?;
            let co = Coordinator::new(config, store, Box::new(EchoRunner), workspace)
                .context("initializing coordinator")?;
            report(&wi_id, co.state());
        }
        Command::Approve { wi, dry_run } => {
            resolve_and_continue(config, &wi, Decision::Approve, dry_run)?;
        }
        Command::Reject { wi, dry_run } => {
            resolve_and_continue(config, &wi, Decision::Reject, dry_run)?;
        }
        Command::Answer {
            wi,
            text,
            file,
            dry_run,
        } => {
            let answer = match (text, file) {
                (_, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("reading answer from {}", path.display()))?,
                (Some(t), None) => t,
                (None, None) => anyhow::bail!("provide an answer as an argument or via --file"),
            };
            resolve_and_continue(config, &wi, Decision::Answer(answer), dry_run)?;
        }
        Command::Abandon { wi } => {
            // Abandon is terminal; no autonomous continuation, so agents are unused.
            resolve_and_continue(config, &wi, Decision::Abandon, true)?;
        }
    }

    Ok(())
}

/// Open the WI for `wi_id` under the configured state dir, apply the human
/// `decision`, then continue autonomously until the next blocked/terminal state.
fn resolve_and_continue(
    config: Config,
    wi_id: &str,
    decision: Decision,
    dry_run: bool,
) -> Result<()> {
    let db_path = config.state_dir.join(wi_id).join("quorum.db");
    if !db_path.exists() {
        anyhow::bail!("no work item {wi_id} at {}", db_path.display());
    }
    let workspace = db_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let runner: Box<dyn AgentRunner> = if dry_run {
        Box::new(EchoRunner)
    } else {
        Box::new(CopilotRunner::new(config.sandbox.clone()))
    };
    let store = Store::open(&db_path).context("opening state database")?;
    let mut co =
        Coordinator::new(config, store, runner, workspace).context("initializing coordinator")?;
    co.resolve(decision)
        .context("resolving human intervention")?;
    co.run_until_blocked().context("advancing work item")?;
    report(wi_id, co.state());
    Ok(())
}

/// Render the current state and, when blocked, the HI resume command.
fn report(wi_id: &str, state: State) {
    if state.is_blocked() {
        let session = format!("quorum/{wi_id}/{state}");
        println!("state: {state} (stuck — awaiting human intervention)");
        println!("resume: copilot --resume {session}");
    } else if state.is_terminal() {
        println!("state: {state} (done)");
    } else {
        println!("state: {state} (progressing)");
    }
}

/// Derive a stable WI id from the work item file name (stem).
fn work_item_id(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("work-item")
        .to_string()
}
