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
    Config, Coordinator, CopilotRunner, State, Store,
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
    }

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
