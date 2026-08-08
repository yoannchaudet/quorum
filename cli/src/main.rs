//! Quorum CLI — a thin driver over `quorum-core` for a single work item.
//!
//! See `docs/architecture.md`. This binary parses arguments, calls the Core,
//! and renders state. It holds no business logic.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quorum_core::{
    agent::{AgentRunner, EchoRunner},
    ensure_worktree, git_common_dir, worktree_record, ActivityEvent, ActivityObserver, Config,
    Coordinator, CopilotRunner, Database, Decision, GitImplementationWorkspace, Kind,
    RegisteredRepository, RepositoryRoot, State, StatusSnapshot, Store, WorkItemId,
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

    /// Suppress live autonomous progress on stderr.
    #[arg(long, global = true)]
    quiet: bool,

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
        /// The user-facing work-item slug.
        work_item: String,
        /// Include full plans, summaries, findings, errors, and activity history.
        #[arg(long)]
        verbose: bool,
        /// Print a versioned machine-readable status document.
        #[arg(long)]
        json: bool,
    },
    /// Approve the current review gate (PlanReview/WorkReview) and continue.
    Approve {
        /// The user-facing work-item slug.
        work_item: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reject the current review gate and send the work back a phase.
    Reject {
        /// The user-facing work-item slug.
        work_item: String,
        /// Continue with stub agents instead of invoking copilot.
        #[arg(long)]
        dry_run: bool,
    },
    /// Answer planner questions at IntakeReview and continue.
    Answer {
        /// The user-facing work-item slug.
        work_item: String,
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
        /// The user-facing work-item slug.
        work_item: String,
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
    let quiet = cli.quiet;

    match cli.command {
        Command::Repo { command } => {
            run_repo_command(&config, context.as_deref(), command)?;
        }
        Command::Run { work_item, dry_run } => {
            let work_item_slug = work_item_slug(&work_item);
            validate_work_item_slug(&work_item_slug)?;
            let (mut database, repository) = open_registered_context(&config, context.as_deref())?;
            let internal_id = database
                .get_or_create_work_item(&repository.id, &work_item_slug)
                .context("creating work item state")?;
            let workspace = config
                .work_item_dir(internal_id.as_str())
                .join("implementation");
            let worktree = ensure_worktree(
                &mut database,
                &repository,
                &internal_id,
                &work_item_slug,
                &workspace,
            )
            .context("preparing work item checkout")?;

            let mut store = database
                .into_store(internal_id)
                .context("opening work item state")?;
            // Intake: load the work-item markdown once, if not already stored.
            if store.work_item().context("reading work item")?.is_none() {
                let text = std::fs::read_to_string(&work_item)
                    .with_context(|| format!("reading work item {}", work_item.display()))?;
                store.set_work_item(&text).context("storing work item")?;
            }

            let runner: Box<dyn AgentRunner> = if dry_run {
                Box::new(EchoRunner)
            } else {
                Box::new(CopilotRunner::new(
                    config.sandbox.clone(),
                    std::time::Duration::from_secs(config.limits.step_timeout_secs),
                    config
                        .work_item_dir(store.work_item_id().as_str())
                        .join("runtime"),
                ))
            };
            let common_git_dir =
                git_common_dir(&worktree.path).context("resolving worktree Git directory")?;
            let mut co = Coordinator::new(
                config,
                store,
                runner,
                Box::new(GitImplementationWorkspace),
                worktree.path,
                work_item_slug.clone(),
            )
            .context("initializing coordinator")?
            .with_implementation_allowed_dirs(vec![common_git_dir])
            .with_observer(progress_observer(quiet));
            co.run_until_blocked().context("advancing work item")?;
            report(&work_item_slug, &co)?;
        }
        Command::Status {
            work_item,
            verbose,
            json,
        } => {
            let (store, _, _) = open_work_item(&config, context.as_deref(), &work_item, false)?;
            let snapshot = StatusSnapshot::load(&store).context("assembling work item status")?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&snapshot).context("serializing status")?
                );
            } else {
                report_status(&snapshot, verbose);
            }
        }
        Command::Approve { work_item, dry_run } => {
            resolve_and_continue(
                config,
                context.as_deref(),
                &work_item,
                Decision::Approve,
                dry_run,
                quiet,
            )?;
        }
        Command::Reject { work_item, dry_run } => {
            resolve_and_continue(
                config,
                context.as_deref(),
                &work_item,
                Decision::Reject,
                dry_run,
                quiet,
            )?;
        }
        Command::Answer {
            work_item,
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
                &work_item,
                Decision::Answer(answer),
                dry_run,
                quiet,
            )?;
        }
        Command::Abandon { work_item } => {
            // Abandon is terminal; no autonomous continuation, so agents are unused.
            resolve_and_continue(
                config,
                context.as_deref(),
                &work_item,
                Decision::Abandon,
                true,
                quiet,
            )?;
        }
    }

    Ok(())
}

/// Open the work item, apply the human `decision`, then continue autonomously until the
/// next blocked or terminal state.
fn resolve_and_continue(
    config: Config,
    context: Option<&std::path::Path>,
    work_item_slug: &str,
    decision: Decision,
    dry_run: bool,
    quiet: bool,
) -> Result<()> {
    validate_work_item_slug(work_item_slug)?;
    let require_worktree = !matches!(decision, Decision::Abandon);
    let (store, internal_id, workspace) =
        open_work_item(&config, context, work_item_slug, require_worktree)?;
    let runner: Box<dyn AgentRunner> = if dry_run {
        Box::new(EchoRunner)
    } else {
        Box::new(CopilotRunner::new(
            config.sandbox.clone(),
            std::time::Duration::from_secs(config.limits.step_timeout_secs),
            config.work_item_dir(internal_id.as_str()).join("runtime"),
        ))
    };
    let additional_dirs = if require_worktree {
        vec![git_common_dir(&workspace).context("resolving worktree Git directory")?]
    } else {
        vec![]
    };
    let mut co = Coordinator::new(
        config,
        store,
        runner,
        Box::new(GitImplementationWorkspace),
        workspace,
        work_item_slug,
    )
    .context("initializing coordinator")?
    .with_implementation_allowed_dirs(additional_dirs)
    .with_observer(progress_observer(quiet));
    co.resolve(decision)
        .context("resolving human intervention")?;
    co.run_until_blocked().context("advancing work item")?;
    report(work_item_slug, &co)?;
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
    work_item_slug: &str,
    require_worktree: bool,
) -> Result<(Store, WorkItemId, PathBuf)> {
    validate_work_item_slug(work_item_slug)?;
    let (mut database, repository) = open_registered_context(config, context)?;
    let internal_id = database
        .work_item_id(&repository.id, work_item_slug)
        .context("looking up work item")?
        .with_context(|| format!("no work item {work_item_slug}"))?;
    let workspace = if require_worktree {
        ensure_worktree(
            &mut database,
            &repository,
            &internal_id,
            work_item_slug,
            &config
                .work_item_dir(internal_id.as_str())
                .join("implementation"),
        )
        .context("preparing work item checkout")?
        .path
    } else {
        worktree_record(&database, &internal_id)
            .context("reading worktree state")?
            .map(|record| record.path)
            .unwrap_or_else(|| config.work_item_dir(internal_id.as_str()))
    };
    let store = database
        .into_store(internal_id.clone())
        .context("opening work item state")?;
    Ok((store, internal_id, workspace))
}

struct StderrProgress {
    enabled: bool,
}

impl ActivityObserver for StderrProgress {
    fn on_activity(&self, event: &ActivityEvent) {
        if !self.enabled {
            return;
        }
        let mut details = Vec::new();
        if let Some(model) = &event.model {
            details.push(format!("model={model}"));
        }
        if let Some(attempt) = event.attempt {
            details.push(format!("attempt={attempt}"));
        }
        if let Some(elapsed) = event.elapsed_ms {
            details.push(format!("elapsed={}", format_duration(elapsed)));
        }
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!(" ({})", details.join(", "))
        };
        eprintln!(
            "{} {}{}",
            format_time(event.timestamp_ms),
            event.message,
            suffix
        );
        let _ = std::io::stderr().flush();
    }

    fn on_persistence_error(
        &self,
        event: &ActivityEvent,
        error: &quorum_core::persistence::StoreError,
    ) {
        if self.enabled {
            eprintln!(
                "{} warning: could not persist activity {:?}: {error}",
                format_time(event.timestamp_ms),
                event.kind
            );
            let _ = std::io::stderr().flush();
        }
    }
}

fn progress_observer(quiet: bool) -> Box<dyn ActivityObserver> {
    Box::new(StderrProgress { enabled: !quiet })
}

fn report_status(snapshot: &StatusSnapshot, verbose: bool) {
    println!(
        "{}\n  internal id: {}\n  repository: {}",
        snapshot.identity.slug, snapshot.identity.id, snapshot.identity.repository_root
    );
    println!(
        "\nstate: {} ({})",
        snapshot.state.current,
        kind_label(snapshot.state.kind)
    );
    if let Some(activity) = snapshot.activities.last() {
        println!(
            "latest activity: {} ({} ago) — {}",
            format_time(activity.timestamp_ms),
            format_duration(now_millis().saturating_sub(activity.timestamp_ms)),
            activity.message
        );
    }

    if let Some(session) = &snapshot.session_name {
        println!("\nhuman intervention:");
        if snapshot.state.current == State::IntakeReview {
            if let Some(questions) = &snapshot.questions {
                println!("  questions: {}", display_text(questions, verbose));
            }
        }
        println!("  session: {session}");
        println!("  resume: copilot --resume {session}");
        if let Some(hint) = resolve_hint(snapshot.state.current) {
            println!("  resolve: quorum {hint} {}", snapshot.identity.slug);
        }
    }

    println!(
        "\nplanning: {} iteration(s), {} candidate(s), {} planner(s)",
        snapshot.planning.iterations,
        snapshot.planning.candidate_count,
        snapshot.planning.planners.len()
    );
    if !snapshot.planning.planners.is_empty() {
        println!("  planners: {}", snapshot.planning.planners.join(", "));
    }
    match &snapshot.planning.plan {
        Some(plan) => println!("  plan: {}", display_text(plan, verbose)),
        None => println!("  plan: not available"),
    }
    if let Some(execution) = &snapshot.planning.execution {
        println!("  approved execution:");
        for line in execution.to_string().lines() {
            println!("    {line}");
        }
    }
    if let Some(metrics) = &snapshot.planning.metrics {
        println!("  convergence: {metrics}");
    }

    println!("\nimplementation:");
    if snapshot.implementations.is_empty() {
        println!("  no rounds");
    }
    for round in &snapshot.implementations {
        let result = match (&round.result_commit, round.changed) {
            (Some(commit), Some(true)) => format!("commit {}", short_sha(commit)),
            (Some(_), Some(false)) => "empty round".to_string(),
            _ => "not finalized".to_string(),
        };
        println!(
            "  round {}: {} — {} (start {})",
            round.iteration + 1,
            round.status,
            result,
            short_sha(&round.start_commit)
        );
        if let Some(summary) = &round.summary {
            println!("    summary: {}", display_text(summary, verbose));
        }
    }

    println!("\nreviews:");
    if snapshot.reviews.is_empty() {
        println!("  no reviews");
    }
    for review in &snapshot.reviews {
        println!(
            "  round {}: {} — {}",
            review.iteration + 1,
            if review.accepted { "ACCEPT" } else { "REJECT" },
            display_text(&review.findings, verbose)
        );
    }

    println!("\nartifacts:");
    if snapshot.artifacts.is_empty() {
        println!("  no artifacts");
    }
    for artifact in &snapshot.artifacts {
        println!(
            "  round {}: {} ({})",
            artifact.iteration + 1,
            artifact.path,
            artifact.media_type
        );
    }

    println!("\nworkspace: {}", snapshot.workspace.path);
    if let Some(branch) = &snapshot.workspace.branch {
        println!("  branch: {branch}");
    }
    if let Some(base) = &snapshot.workspace.base_commit {
        println!("  base: {}", short_sha(base));
    }
    if let Some(head) = &snapshot.workspace.head {
        println!("  HEAD: {}", short_sha(head));
    }
    println!(
        "  checkout: {}",
        if snapshot.workspace.ready {
            "ready"
        } else {
            "not ready"
        }
    );
    if let Some(clean) = snapshot.workspace.clean {
        println!("  working tree: {}", if clean { "clean" } else { "dirty" });
    }

    if !snapshot.errors.is_empty() {
        println!("\nfailures:");
        for error in &snapshot.errors {
            println!(
                "  {} — {}",
                format_time(error.timestamp_ms),
                display_text(&error.message, verbose)
            );
        }
    }

    println!("\ntransitions:");
    if snapshot.transitions.is_empty() {
        println!("  none");
    }
    for transition in &snapshot.transitions {
        println!(
            "  {} {} -> {} ({})",
            format_time(transition.ts.parse().unwrap_or(0)),
            transition
                .from
                .map(|state| state.to_string())
                .unwrap_or_else(|| "-".to_string()),
            transition.to,
            transition.reason
        );
    }

    let activities = if verbose {
        snapshot.activities.as_slice()
    } else {
        let start = snapshot.activities.len().saturating_sub(10);
        &snapshot.activities[start..]
    };
    println!("\nactivity{}:", if verbose { "" } else { " (latest 10)" });
    if activities.is_empty() {
        println!("  none");
    }
    for activity in activities {
        println!(
            "  {} — {}",
            format_time(activity.timestamp_ms),
            activity.message
        );
    }
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Autonomous => "progressing",
        Kind::Blocked => "blocked",
        Kind::Terminal => "terminal",
    }
}

fn display_text(value: &str, verbose: bool) -> String {
    if verbose {
        return value.to_string();
    }
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let snippet = chars.by_ref().take(160).collect::<String>();
    if chars.next().is_some() {
        format!("{snippet}…")
    } else {
        snippet
    }
}

fn short_sha(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

fn format_time(timestamp_ms: u64) -> String {
    let seconds = (timestamp_ms / 1_000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

fn format_duration(elapsed_ms: u64) -> String {
    if elapsed_ms < 1_000 {
        format!("{elapsed_ms}ms")
    } else {
        format!("{:.1}s", elapsed_ms as f64 / 1_000.0)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Render the current state and, when blocked, the human-intervention resume command
/// (and any
/// intake questions awaiting answers).
fn report(work_item_slug: &str, co: &Coordinator) -> Result<()> {
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
        println!("human-intervention session: {session}");
        println!("  first time: run `copilot`, then `/rename {session}`");
        println!("  resume:     copilot --resume {session}");
        if let Some(hint) = resolve_hint(state) {
            println!("resolve with: quorum {hint} {work_item_slug}");
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

/// Derive a stable work-item slug from the input file name.
fn work_item_slug(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("work-item")
        .to_string()
}

/// Ensure a work-item slug is safe in commands and deterministic session names.
fn validate_work_item_slug(work_item_slug: &str) -> Result<()> {
    let mut components = std::path::Path::new(work_item_slug).components();
    let first = components.next();
    let is_single_normal =
        matches!(first, Some(std::path::Component::Normal(_))) && components.next().is_none();
    if work_item_slug.is_empty()
        || work_item_slug == "."
        || work_item_slug == ".."
        || !is_single_normal
    {
        anyhow::bail!("invalid work-item slug {work_item_slug:?}: must be a single path component");
    }
    Ok(())
}
