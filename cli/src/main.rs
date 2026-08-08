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
        /// The answer text (mutually exclusive with --file).
        #[arg(conflicts_with = "file")]
        text: Option<String>,
        /// Read the answer from a file instead of an argument.
        #[arg(long, conflicts_with = "text")]
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
            let mut co = Coordinator::new(config, store, runner, workspace, wi_id.clone())
                .context("initializing coordinator")?;
            co.run_until_blocked().context("advancing work item")?;
            report(&wi_id, &co)?;
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
            let mut co = Coordinator::new(
                config,
                store,
                Box::new(EchoRunner),
                workspace,
                wi_id.clone(),
            )
            .context("initializing coordinator")?;
            // Ensure the HI session row exists (deterministic; repairs it if a
            // crash occurred before it was recorded).
            co.ensure_session().context("recording HI session")?;
            report(&wi_id, &co)?;
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
    validate_wi_id(wi_id)?;
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
    let mut co = Coordinator::new(config, store, runner, workspace, wi_id)
        .context("initializing coordinator")?;
    co.resolve(decision)
        .context("resolving human intervention")?;
    co.run_until_blocked().context("advancing work item")?;
    report(wi_id, &co)?;
    Ok(())
}

/// Render the current state and, when blocked, the HI resume command (and any
/// intake questions awaiting answers).
fn report(wi_id: &str, co: &Coordinator) -> Result<()> {
    let state = co.state();
    if let Some(session) = co.session_name() {
        println!("state: {state} (stuck — awaiting human intervention)");
        if state == State::IntakeReview {
            if let Some(questions) = co.questions().context("reading intake questions")? {
                println!("questions:\n{questions}");
            }
        }
        // copilot cannot create a named session non-interactively, so the human
        // names it once via /rename, then resumes by that name (see docs/sessions.md).
        println!("HI session: {session}");
        println!("  first time: run `copilot`, then `/rename {session}`");
        println!("  resume:     copilot --resume {session}");
        if let Some(hint) = resolve_hint(state) {
            println!("resolve with: quorum {hint} {wi_id}");
        }
    } else if state == State::Failed {
        println!("state: {state} (failed — see quorum.db for details)");
    } else if state.is_terminal() {
        println!("state: {state} (done)");
    } else {
        println!("state: {state} (progressing)");
    }
    Ok(())
}

/// The Quorum command(s) that resolve a given blocked state.
fn resolve_hint(state: State) -> Option<&'static str> {
    match state {
        State::IntakeReview => Some("answer"),
        State::PlanReview | State::WorkReview => Some("approve|reject"),
        _ => None,
    }
}

/// Derive a stable WI id from the work item file name (stem).
fn work_item_id(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("work-item")
        .to_string()
}

/// Ensure a user-supplied WI id is a single, safe path component so it cannot
/// escape the configured state directory (e.g. absolute paths or `..`).
fn validate_wi_id(wi_id: &str) -> Result<()> {
    let mut components = std::path::Path::new(wi_id).components();
    let first = components.next();
    let is_single_normal =
        matches!(first, Some(std::path::Component::Normal(_))) && components.next().is_none();
    if wi_id.is_empty() || wi_id == "." || wi_id == ".." || !is_single_normal {
        anyhow::bail!("invalid work item id {wi_id:?}: must be a single path component");
    }
    Ok(())
}
