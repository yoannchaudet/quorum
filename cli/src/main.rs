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
    Config, Coordinator, CopilotRunner, Database, Decision, RegisteredRepository, RepositoryRoot,
    State, Store, WorkItemId,
};

#[derive(Parser)]
#[command(name = "quorum", version, about = "Drive a single Quorum work item")]
struct Cli {
    /// Path to the config file (default: ~/.quorum/config.yaml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Resolve repository context from this folder instead of the current directory.
    #[arg(long, global = true)]
    context: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the repositories Quorum is allowed to use.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
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
        /// The work item id.
        wi: String,
    },
    /// Approve the current review gate (PlanReview/WorkReview) and continue.
    Approve {
        /// The work item id.
        wi: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reject the current review gate and send the work back a phase.
    Reject {
        /// The work item id.
        wi: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Answer planner questions at IntakeReview and continue.
    Answer {
        /// The work item id.
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
        /// The work item id.
        wi: String,
    },
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Register a Git repository with Quorum.
    Register {
        /// A path inside the repository (default: --context, then cwd).
        path: Option<PathBuf>,
    },
    /// Unregister a Git repository without deleting its work items.
    Unregister {
        /// A path inside the repository (default: --context, then cwd).
        path: Option<PathBuf>,
    },
    /// List registered repositories.
    List,
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
    let context = cli.context;

    match cli.command {
        Command::Repo { command } => {
            run_repo_command(&config, context.as_deref(), command)?;
        }
        Command::Run { work_item, dry_run } => {
            let wi_id = work_item_id(&work_item);
            validate_wi_id(&wi_id)?;
            let (mut database, repository) = open_registered_context(&config, context.as_deref())?;
            let internal_id = database
                .get_or_create_work_item(&repository.id, &wi_id)
                .context("creating work item state")?;
            let workspace = config.work_item_dir(internal_id.as_str());
            std::fs::create_dir_all(&workspace)
                .with_context(|| format!("creating {}", workspace.display()))?;

            let mut store = database
                .into_store(internal_id)
                .context("opening work item state")?;
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
        Command::Status { wi } => {
            let (store, internal_id) = open_work_item(&config, context.as_deref(), &wi)?;
            let workspace = config.work_item_dir(internal_id.as_str());
            let mut co =
                Coordinator::new(config, store, Box::new(EchoRunner), workspace, wi.clone())
                    .context("initializing coordinator")?;
            // Ensure the HI session row exists (deterministic; repairs it if a
            // crash occurred before it was recorded).
            co.ensure_session().context("recording HI session")?;
            report(&wi, &co)?;
        }
        Command::Approve { wi, dry_run } => {
            resolve_and_continue(config, context.as_deref(), &wi, Decision::Approve, dry_run)?;
        }
        Command::Reject { wi, dry_run } => {
            resolve_and_continue(config, context.as_deref(), &wi, Decision::Reject, dry_run)?;
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
            resolve_and_continue(
                config,
                context.as_deref(),
                &wi,
                Decision::Answer(answer),
                dry_run,
            )?;
        }
        Command::Abandon { wi } => {
            // Abandon is terminal; no autonomous continuation, so agents are unused.
            resolve_and_continue(config, context.as_deref(), &wi, Decision::Abandon, true)?;
        }
    }

    Ok(())
}

/// Open the WI, apply the human `decision`, then continue autonomously until the
/// next blocked or terminal state.
fn resolve_and_continue(
    config: Config,
    context: Option<&std::path::Path>,
    wi_id: &str,
    decision: Decision,
    dry_run: bool,
) -> Result<()> {
    validate_wi_id(wi_id)?;
    let (store, internal_id) = open_work_item(&config, context, wi_id)?;
    let workspace = config.work_item_dir(internal_id.as_str());
    let runner: Box<dyn AgentRunner> = if dry_run {
        Box::new(EchoRunner)
    } else {
        Box::new(CopilotRunner::new(config.sandbox.clone()))
    };
    let mut co = Coordinator::new(config, store, runner, workspace, wi_id)
        .context("initializing coordinator")?;
    co.resolve(decision)
        .context("resolving human intervention")?;
    co.run_until_blocked().context("advancing work item")?;
    report(wi_id, &co)?;
    Ok(())
}

fn open_database(config: &Config) -> Result<Database> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating {}", config.data_dir.display()))?;
    Database::open(&config.database_path()).context("opening Quorum state")
}

fn run_repo_command(
    config: &Config,
    context: Option<&std::path::Path>,
    command: RepoCommand,
) -> Result<()> {
    match command {
        RepoCommand::Register { path } => {
            let root = resolve_repository(path.as_deref().or(context))?;
            let mut database = open_database(config)?;
            let repository = database
                .register_repository(&root)
                .context("registering repository")?;
            println!(
                "registered: {} {}",
                repository.id,
                repository.root.display()
            );
        }
        RepoCommand::Unregister { path } => {
            let root = resolve_repository(path.as_deref().or(context))?;
            let mut database = open_database(config)?;
            let repository = database
                .unregister_repository(&root)
                .context("unregistering repository")?
                .with_context(|| {
                    format!("repository {} is not registered", root.as_path().display())
                })?;
            println!(
                "unregistered: {} {}",
                repository.id,
                repository.root.display()
            );
        }
        RepoCommand::List => {
            let database = open_database(config)?;
            for repository in database.repositories().context("listing repositories")? {
                println!("{}\t{}", repository.id, repository.root.display());
            }
        }
    }
    Ok(())
}

fn resolve_repository(context: Option<&std::path::Path>) -> Result<RepositoryRoot> {
    let path = match context {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().context("reading current directory")?,
    };
    RepositoryRoot::discover(&path)
        .with_context(|| format!("resolving repository from {}", path.display()))
}

fn open_registered_context(
    config: &Config,
    context: Option<&std::path::Path>,
) -> Result<(Database, RegisteredRepository)> {
    let root = resolve_repository(context)?;
    let database = open_database(config)?;
    let repository = database
        .registered_repository(&root)
        .context("looking up repository registration")?
        .with_context(|| {
            format!(
                "repository {} is not registered; run `quorum repo register {}`",
                root.as_path().display(),
                root.as_path().display()
            )
        })?;
    Ok((database, repository))
}

fn open_work_item(
    config: &Config,
    context: Option<&std::path::Path>,
    wi_id: &str,
) -> Result<(Store, WorkItemId)> {
    validate_wi_id(wi_id)?;
    let (database, repository) = open_registered_context(config, context)?;
    let internal_id = database
        .work_item_id(&repository.id, wi_id)
        .context("looking up work item")?
        .with_context(|| format!("no work item {wi_id}"))?;
    let store = database
        .into_store(internal_id.clone())
        .context("opening work item state")?;
    Ok((store, internal_id))
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
        println!("state: {state} (failed — inspect the work item event history)");
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

/// Ensure a WI id is safe to embed in commands and deterministic session names.
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
