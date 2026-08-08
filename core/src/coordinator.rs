//! The Coordinator. Mirrors `docs/agents.md`.
//!
//! The Coordinator is the only stateful orchestrator for a single work item: it owns the state
//! machine, runs the agents, and persists after every step so it can resume
//! after a crash.

use std::path::PathBuf;
use std::time::Instant;

use crate::agent::{AgentError, AgentRequest, AgentRole, AgentRunner, Filesystem};
use crate::capabilities::{CapabilityError, ExecutionCapabilities};
use crate::config::Config;
use crate::convergence;
use crate::observability::{ActivityEvent, ActivityKind, ActivityObserver, NoopActivityObserver};
use crate::persistence::{ImplementationRoundStatus, Store, StoreError, Transition};
use crate::prompt::{Prompt, PromptError};
use crate::state::State;
use crate::worktree::{ImplementationWorkspace, WorktreeError};

/// Errors surfaced by the Coordinator.
#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Prompt(#[from] PromptError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    #[error("illegal transition {from} -> {to}")]
    IllegalTransition { from: State, to: State },
    #[error("no work item has been loaded")]
    NoWorkItem,
    #[error("no plan is available to implement")]
    NoPlan,
    #[error("no implementation is available to review")]
    NoImplementation,
    #[error("cannot {decision} in state {state} (not an applicable human-intervention state)")]
    InvalidResolution { state: State, decision: Decision },
}

/// A human's decision to resolve a blocked state (see `docs/agents.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Accept the current gate (PlanReview or WorkReview).
    Approve,
    /// Send the work back for another pass (PlanReview or WorkReview).
    Reject,
    /// Provide answers to planner questions (IntakeReview).
    Answer(String),
    /// Cancel the work item entirely (any blocked state).
    Abandon,
}

/// The outcome of one adversarial review round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewOutcome {
    /// The Reviewer accepted the work.
    Accepted,
    /// The Reviewer rejected; loop back to the Implementer.
    Rejected,
    /// Progress stopped or the iteration bound was reached.
    Escalated(&'static str),
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Decision::Approve => "approve",
            Decision::Reject => "reject",
            Decision::Answer(_) => "answer",
            Decision::Abandon => "abandon",
        };
        f.write_str(s)
    }
}

/// Orchestrates one work item through the state machine.
pub struct Coordinator {
    config: Config,
    store: Store,
    runner: Box<dyn AgentRunner>,
    observer: Box<dyn ActivityObserver>,
    implementation_workspace: Box<dyn ImplementationWorkspace>,
    /// Working directory used as the sandbox cwd for agent invocations.
    workspace: PathBuf,
    implementation_allowed_dirs: Vec<PathBuf>,
    /// The work-item slug used to derive session names (see `docs/sessions.md`).
    work_item_slug: String,
    state: State,
}

impl Coordinator {
    /// Create a Coordinator over an opened `store`, resuming the persisted state
    /// if present, otherwise starting at `Intake`. `workspace` is the sandbox
    /// cwd for agent invocations; `work_item_slug` identifies the work item (used for
    /// session names).
    pub fn new(
        config: Config,
        store: Store,
        runner: Box<dyn AgentRunner>,
        implementation_workspace: Box<dyn ImplementationWorkspace>,
        workspace: PathBuf,
        work_item_slug: impl Into<String>,
    ) -> Result<Coordinator, CoordinatorError> {
        let state = store.current_state()?.unwrap_or(State::Intake);
        Ok(Coordinator {
            config,
            store,
            runner,
            observer: Box::new(NoopActivityObserver),
            implementation_workspace,
            workspace,
            implementation_allowed_dirs: Vec::new(),
            work_item_slug: work_item_slug.into(),
            state,
        })
    }

    pub fn with_implementation_allowed_dirs(mut self, directories: Vec<PathBuf>) -> Coordinator {
        self.implementation_allowed_dirs = directories;
        self
    }

    pub fn with_observer(mut self, observer: Box<dyn ActivityObserver>) -> Coordinator {
        self.observer = observer;
        self
    }

    /// The current state of the work item.
    pub fn state(&self) -> State {
        self.state
    }

    /// The active configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The recorded transition history for this work item.
    pub fn history(&self) -> Result<Vec<Transition>, CoordinatorError> {
        Ok(self.store.history()?)
    }

    /// The intake questions surfaced to the human when the work item awaits answers.
    pub fn questions(&self) -> Result<Option<String>, CoordinatorError> {
        Ok(self.store.questions()?)
    }

    /// The human-intervention session name for the current blocked state.
    /// Deterministic from the work-item slug and state
    /// (`quorum/<work-item-slug>/<state>`), so it survives crashes.
    pub fn session_name(&self) -> Option<String> {
        if self.state.is_blocked() {
            Some(format!("quorum/{}/{}", self.work_item_slug, self.state))
        } else {
            None
        }
    }

    /// If the work item is blocked, ensure its named session is recorded
    /// (idempotent) and return the session name. No-op for non-blocked states.
    pub fn ensure_session(&mut self) -> Result<Option<String>, CoordinatorError> {
        match self.session_name() {
            Some(name) => {
                self.store.record_session(self.state, &name)?;
                Ok(Some(name))
            }
            None => Ok(None),
        }
    }

    /// Perform a single validated, persisted transition to `next`.
    pub fn transition_to(&mut self, next: State, reason: &str) -> Result<State, CoordinatorError> {
        if !self.state.can_transition_to(next) {
            return Err(CoordinatorError::IllegalTransition {
                from: self.state,
                to: next,
            });
        }
        self.store
            .record_transition(Some(self.state), next, reason)?;
        let previous = self.state;
        self.state = next;
        self.record_activity(
            ActivityEvent::new(
                ActivityKind::Transition,
                format!("{previous} -> {next}: {reason}"),
            )
            .phase(next),
        );
        Ok(self.state)
    }

    /// Resolve the current blocked state with a human `decision`, performing
    /// the corresponding validated, persisted transition (see `docs/agents.md`).
    ///
    /// The decision (and any answer) is logged in the **same** transaction as the
    /// transition, so the audit log can never record a decision that did not
    /// actually advance the state.
    ///
    /// Errors if the decision does not apply to the current state (e.g. approving
    /// when the work item is not at a review gate).
    pub fn resolve(&mut self, decision: Decision) -> Result<State, CoordinatorError> {
        use Decision::*;
        use State::*;
        let (next, answer) = match (self.state, &decision) {
            (PlanReview, Approve) => {
                let plan = self.store.plan()?.ok_or(CoordinatorError::NoPlan)?;
                ExecutionCapabilities::parse_plan(&plan)?;
                (Implementing, None)
            }
            (PlanReview, Reject) => (Planning, None),
            (WorkReview, Approve) => (Done, None),
            (WorkReview, Reject) => (Implementing, None),
            (IntakeReview, Answer(text)) => (Planning, Some(text.as_str())),
            // Abandoning is allowed from any blocked state.
            (s, Abandon) if s.is_blocked() => (Abandoned, None),
            _ => {
                return Err(CoordinatorError::InvalidResolution {
                    state: self.state,
                    decision,
                })
            }
        };

        if !self.state.can_transition_to(next) {
            return Err(CoordinatorError::IllegalTransition {
                from: self.state,
                to: next,
            });
        }

        let decision_data = format!("{}@{}", decision, self.state);
        let mut extra: Vec<(&str, &str)> = Vec::new();
        if let Some(text) = answer {
            extra.push(("human_answer", text));
        }
        extra.push(("human_decision", &decision_data));

        let reason = format!("human: {decision} {} -> {next}", self.state);
        self.store
            .record_transition_with_events(Some(self.state), next, &reason, &extra)?;
        let previous = self.state;
        self.state = next;
        self.record_activity(
            ActivityEvent::new(
                ActivityKind::Transition,
                format!("{previous} -> {next}: {reason}"),
            )
            .phase(next),
        );
        Ok(self.state)
    }

    /// Run the planner roster for the current planning iteration, in isolation,
    /// and persist each candidate plan. Each pass uses a fresh iteration so the
    /// convergence loop can compare successive rounds.
    /// Ask each planner, in isolation, whether the work item needs clarification before
    /// planning (see `prompts/intake-questions.md`). Returns the aggregated
    /// questions when any planner raises some, or `None` when all are satisfied.
    fn run_intake_questions(&mut self) -> Result<Option<String>, CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let answers = self.store.answers()?;
        let prompt = Prompt::intake_questions();
        let slots: Vec<String> = self.config.planners.keys().cloned().collect();
        let mut blocks = Vec::new();
        for slot in slots {
            let rendered = prompt.render(&[("work_item", &work_item), ("answers", &answers)])?;
            let model = self.config.planners.get(&slot).cloned().unwrap_or_default();
            let req = AgentRequest {
                role: AgentRole::intake_planner(slot.clone()),
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadOnly,
                model,
                iteration: None,
                additional_dirs: vec![],
                execution: None,
            };
            let output = self.invoke(&req)?;
            let trimmed = output.trim();
            // `NONE` (any case) means this planner has no questions.
            if !trimmed.eq_ignore_ascii_case("NONE") && !trimmed.is_empty() {
                blocks.push(format!("### {slot}\n{trimmed}"));
            }
        }
        if blocks.is_empty() {
            Ok(None)
        } else {
            Ok(Some(blocks.join("\n\n")))
        }
    }

    fn run_planners(&mut self) -> Result<(), CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let previous_plan = self.store.plan()?.unwrap_or_default();
        let answers = self.store.answers()?;
        let prompt = Prompt::planner();
        let iteration = match self.store.max_candidate_iteration()? {
            Some(prev) => prev + 1,
            None => 0,
        };
        let slots: Vec<String> = self.config.planners.keys().cloned().collect();
        for slot in slots {
            let rendered = prompt.render(&[
                ("work_item", &work_item),
                ("answers", &answers),
                ("previous_plan", &previous_plan),
            ])?;
            let model = self.config.planners.get(&slot).cloned().unwrap_or_default();
            let req = AgentRequest {
                role: AgentRole::planner(slot.clone()),
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadOnly,
                model,
                iteration: Some(iteration),
                additional_dirs: vec![],
                execution: None,
            };
            let output = self.invoke(&req)?;
            self.store.save_candidate(&slot, iteration, &output)?;
        }
        Ok(())
    }

    /// Merge the latest candidates into a Plan and decide convergence.
    ///
    /// Runs the Coordinator merge prompt over the current iteration's candidates and the
    /// previous merged plan, persists the merged plan, and returns whether the
    /// loop has converged — either because the model reported `CONVERGED`, the
    /// merged plan is materially unchanged (diff within the configured
    /// threshold), or the max-iterations bound has been reached.
    fn run_merge(&mut self) -> Result<bool, CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let iteration = self.store.max_candidate_iteration()?.unwrap_or(0);
        let candidates = self.store.candidates(iteration)?;
        let joined = candidates
            .iter()
            .map(|(slot, text)| format!("### {slot}\n{text}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let previous_plan = self.store.plan()?.unwrap_or_default();

        let rendered = Prompt::merge().render(&[
            ("work_item", &work_item),
            ("candidates", &joined),
            ("previous_plan", &previous_plan),
        ])?;
        let req = AgentRequest {
            role: AgentRole::CoordinatorMerge,
            prompt: rendered,
            cwd: self.workspace.clone(),
            filesystem: Filesystem::ReadOnly,
            model: self.config.models.coordinator.clone(),
            iteration: Some(iteration),
            additional_dirs: vec![],
            execution: None,
        };
        let output = self.invoke(&req)?;
        let merged = convergence::parse_merge(&output);
        ExecutionCapabilities::parse_plan(&merged.plan)?;

        // Fallback signal: a plan materially unchanged from the previous one is
        // treated as converged even if the model said otherwise.
        let unchanged = !previous_plan.is_empty()
            && convergence::diff_ratio(&merged.plan, &previous_plan)
                <= self.config.limits.convergence_diff_threshold;
        let at_max = iteration + 1 >= self.config.limits.convergence_max_iters;
        let converged = merged.converged || unchanged || at_max;

        let metrics = format!(
            "iteration={iteration};model_converged={};unchanged={unchanged};at_max={at_max}",
            merged.converged
        );
        self.store.set_plan(&merged.plan, &metrics)?;
        self.record_activity(
            ActivityEvent::new(
                ActivityKind::Convergence,
                if converged {
                    format!("planning iteration {} converged", iteration + 1)
                } else {
                    format!("planning iteration {} requires another pass", iteration + 1)
                },
            )
            .phase(State::Converging)
            .iteration(iteration),
        );
        Ok(converged)
    }

    /// Run the Implementer against the accepted Plan, in its writable
    /// workspace, and persist the summary it returns.
    ///
    /// The Implementer runs read/write at the worktree root (its sandbox cwd, see
    /// `docs/isolation.md`). On re-entry from the adversarial loop, the latest
    /// review feedback is fed back in.
    fn run_implementer(&mut self) -> Result<(), CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let plan = self.store.plan()?.ok_or(CoordinatorError::NoPlan)?;
        let execution = ExecutionCapabilities::parse_plan(&plan)?;
        let feedback = self
            .store
            .latest_review()?
            .filter(|(_, accepted)| !accepted)
            .map(|(text, _)| text)
            .unwrap_or_default();

        let iteration = self.store.review_count()?;
        let mut round = match self.store.implementation_round(iteration)? {
            Some(round) => round,
            None => {
                if !self.implementation_workspace.is_clean(&self.workspace)? {
                    return Err(WorktreeError::DirtyUncommittedWork.into());
                }
                let head = self.implementation_workspace.head(&self.workspace)?;
                if iteration > 0 {
                    let expected = self
                        .store
                        .implementation_round(iteration - 1)?
                        .and_then(|previous| previous.result_commit)
                        .ok_or(CoordinatorError::NoImplementation)?;
                    if head != expected {
                        return Err(WorktreeError::UnexpectedHead {
                            expected,
                            actual: head,
                        }
                        .into());
                    }
                }
                let round = self.store.reserve_implementation_round(iteration, &head)?;
                self.record_activity(
                    ActivityEvent::new(
                        ActivityKind::ImplementationRound,
                        format!(
                            "implementation round {} reserved at {}",
                            iteration + 1,
                            short_sha(&head)
                        ),
                    )
                    .phase(State::Implementing)
                    .iteration(iteration),
                );
                round
            }
        };

        if round.status == ImplementationRoundStatus::Committed {
            self.record_activity(
                ActivityEvent::new(
                    ActivityKind::ImplementationRound,
                    format!("implementation round {} already committed", iteration + 1),
                )
                .phase(State::Implementing)
                .iteration(iteration),
            );
            return Ok(());
        }

        if round.status == ImplementationRoundStatus::Running {
            let head = self.implementation_workspace.head(&self.workspace)?;
            if head != round.start_commit {
                return Err(WorktreeError::UnexpectedHead {
                    expected: round.start_commit,
                    actual: head,
                }
                .into());
            }
            let runtime_dir = self
                .workspace
                .parent()
                .unwrap_or(&self.workspace)
                .join("runtime");
            let artifact_dir = runtime_dir.join("artifacts");
            let runtime_text = runtime_dir.display().to_string();
            let artifact_text = artifact_dir.display().to_string();
            let execution_text = execution.to_string();
            let rendered = Prompt::implementer().render(&[
                ("work_item", &work_item),
                ("plan", &plan),
                ("feedback", &feedback),
                ("runtime_dir", &runtime_text),
                ("artifact_dir", &artifact_text),
                ("execution_capabilities", &execution_text),
            ])?;
            let req = AgentRequest {
                role: AgentRole::Implementer,
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadWrite,
                model: self.config.models.implementer.clone(),
                iteration: Some(iteration),
                additional_dirs: self.implementation_allowed_dirs.clone(),
                execution: Some(execution.clone()),
            };
            let invocation = self.invoke(&req);
            let artifacts = if execution.artifacts {
                self.store.sync_artifacts(iteration, &artifact_dir)?
            } else {
                0
            };
            if artifacts > 0 {
                self.record_activity(
                    ActivityEvent::new(
                        ActivityKind::Artifact,
                        format!("{artifacts} execution artifact(s) retained"),
                    )
                    .phase(State::Implementing)
                    .iteration(iteration),
                );
            }
            let output = invocation?;
            let head_after_agent = self.implementation_workspace.head(&self.workspace)?;
            if head_after_agent != round.start_commit {
                return Err(WorktreeError::UnexpectedHead {
                    expected: round.start_commit,
                    actual: head_after_agent,
                }
                .into());
            }
            self.store
                .mark_implementation_agent_complete(iteration, &output)?;
            round.status = ImplementationRoundStatus::AgentComplete;
        } else {
            self.record_activity(
                ActivityEvent::new(
                    ActivityKind::ImplementationRound,
                    format!(
                        "implementation round {} resuming commit finalization",
                        iteration + 1
                    ),
                )
                .phase(State::Implementing)
                .iteration(iteration),
            );
        }

        let result = self.implementation_workspace.finalize(
            &self.workspace,
            self.store.work_item_id(),
            &self.work_item_slug,
            &round,
        )?;
        self.store
            .complete_implementation_round(iteration, &result.commit, &result.tree)?;
        let changed = result.commit != round.start_commit;
        self.record_activity(
            ActivityEvent::new(
                ActivityKind::ImplementationRound,
                if changed {
                    format!(
                        "implementation round {} committed {}",
                        iteration + 1,
                        short_sha(&result.commit)
                    )
                } else {
                    format!("implementation round {} produced no changes", iteration + 1)
                },
            )
            .phase(State::Implementing)
            .iteration(iteration),
        );
        Ok(())
    }

    /// Run the Reviewer adversarially over the latest implementation and
    /// record the verdict. Returns the outcome so the caller can drive the loop.
    ///
    /// The Reviewer runs read-only and is a different model from the Implementer (see
    /// `docs/agents.md`). Its review is keyed to the implementation's iteration.
    fn run_reviewer(&mut self) -> Result<ReviewOutcome, CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let plan = self.store.plan()?.ok_or(CoordinatorError::NoPlan)?;
        let (iteration, implementation) = self
            .store
            .latest_implementation()?
            .ok_or(CoordinatorError::NoImplementation)?;

        let artifacts = self
            .store
            .artifacts()?
            .into_iter()
            .map(|artifact| format!("- {}", artifact.path))
            .collect::<Vec<_>>()
            .join("\n");
        let rendered = Prompt::reviewer().render(&[
            ("work_item", &work_item),
            ("plan", &plan),
            ("implementation", &implementation),
            ("artifacts", &artifacts),
        ])?;
        let artifact_dir = self
            .workspace
            .parent()
            .unwrap_or(&self.workspace)
            .join("runtime")
            .join("artifacts");
        let req = AgentRequest {
            role: AgentRole::Reviewer,
            prompt: rendered,
            cwd: self.workspace.clone(),
            filesystem: Filesystem::ReadOnly,
            model: self.config.models.reviewer.clone(),
            iteration: Some(iteration),
            additional_dirs: if artifact_dir.exists() {
                vec![artifact_dir]
            } else {
                vec![]
            },
            execution: None,
        };
        let output = self.invoke(&req)?;
        let review = convergence::parse_review(&output);
        self.store
            .save_review(iteration, &review.findings, review.accepted)?;

        let outcome = if review.accepted {
            ReviewOutcome::Accepted
        } else if iteration > 0
            && self
                .store
                .implementation_tree(iteration)?
                .zip(self.store.implementation_tree(iteration - 1)?)
                .is_some_and(|(current, previous)| current == previous)
        {
            ReviewOutcome::Escalated("implementation unchanged")
        } else if iteration + 1 >= self.config.limits.adversarial_max_iters {
            ReviewOutcome::Escalated("max adversarial iterations")
        } else {
            ReviewOutcome::Rejected
        };
        let message = match outcome {
            ReviewOutcome::Accepted => format!("review round {} accepted", iteration + 1),
            ReviewOutcome::Rejected => format!("review round {} rejected", iteration + 1),
            ReviewOutcome::Escalated(cause) => {
                format!("review round {} escalated: {cause}", iteration + 1)
            }
        };
        self.record_activity(
            ActivityEvent::new(ActivityKind::Review, message)
                .phase(State::Reviewing)
                .iteration(iteration),
        );
        Ok(outcome)
    }

    /// Invoke an agent, retrying transient failures up to `limits.step_retries`
    /// times. Returns the last error if every attempt fails. This is the
    /// transient boundary: agent runs (process spawn/exit) are what may fail
    /// intermittently and are safe to re-run (see `docs/persistence.md`).
    fn invoke(&mut self, req: &AgentRequest) -> Result<String, CoordinatorError> {
        // One initial attempt plus up to `step_retries` retries.
        let attempts = self.config.limits.step_retries.saturating_add(1);
        let mut last: Option<AgentError> = None;
        for attempt in 1..=attempts {
            let mut started =
                ActivityEvent::new(ActivityKind::AgentStarted, format!("{} started", req.role))
                    .phase(self.state)
                    .role(req.role.to_string())
                    .model(req.model.clone())
                    .attempt(attempt);
            if let Some(iteration) = req.iteration {
                started = started.iteration(iteration);
            }
            self.record_activity(started);
            let start = Instant::now();
            match self.runner.run(req) {
                Ok(output) => {
                    let mut completed = ActivityEvent::new(
                        ActivityKind::AgentCompleted,
                        format!("{} completed", req.role),
                    )
                    .phase(self.state)
                    .role(req.role.to_string())
                    .model(req.model.clone())
                    .attempt(attempt)
                    .elapsed(start.elapsed().as_millis() as u64);
                    if let Some(iteration) = req.iteration {
                        completed = completed.iteration(iteration);
                    }
                    self.record_activity(completed);
                    return Ok(output);
                }
                Err(error) => {
                    let final_attempt = attempt == attempts || !agent_error_is_retryable(&error);
                    let mut failed = ActivityEvent::new(
                        if final_attempt {
                            ActivityKind::AgentFailed
                        } else {
                            ActivityKind::AgentRetrying
                        },
                        if final_attempt {
                            format!("{} failed", req.role)
                        } else {
                            format!("{} failed; retrying", req.role)
                        },
                    )
                    .phase(self.state)
                    .role(req.role.to_string())
                    .model(req.model.clone())
                    .attempt(attempt)
                    .elapsed(start.elapsed().as_millis() as u64);
                    if let Some(iteration) = req.iteration {
                        failed = failed.iteration(iteration);
                    }
                    self.record_activity(failed);
                    if !agent_error_is_retryable(&error) {
                        return Err(error.into());
                    }
                    last = Some(error);
                }
            }
        }
        Err(last.expect("attempts >= 1").into())
    }

    /// Advance the work item by one autonomous step, performing any agent work the
    /// current state requires before transitioning.
    ///
    /// Returns the (possibly unchanged) state. The state is unchanged when the
    /// work item is blocked on human input or terminal.
    ///
    /// A step whose agent work fails (after retries) or which cannot proceed is
    /// moved to `Failed` (terminal), with the cause recorded, rather than
    /// aborting the process — the Coordinator runs unattended.
    /// Store (database) errors are not recoverable and propagate.
    pub fn step(&mut self) -> Result<State, CoordinatorError> {
        if self.state.is_blocked() || self.state.is_terminal() {
            return Ok(self.state);
        }
        self.record_activity(
            ActivityEvent::new(
                ActivityKind::PhaseStarted,
                format!("{} phase started", self.state),
            )
            .phase(self.state),
        );
        match self.compute_next() {
            Ok((next, detail)) => {
                let reason = detail.unwrap_or_else(|| format!("auto: {} -> {next}", self.state));
                self.transition_to(next, &reason)
            }
            // Database failures are fundamental — do not mask them as Failed.
            Err(e @ CoordinatorError::Store(_)) => Err(e),
            Err(cause) => self.fail(&cause.to_string()),
        }
    }

    /// Compute the next state for the current autonomous state, performing the
    /// agent work that state requires.
    fn compute_next(&mut self) -> Result<(State, Option<String>), CoordinatorError> {
        use State::*;
        let (next, detail) = match self.state {
            Intake => {
                if self.store.work_item()?.is_none() {
                    return Err(CoordinatorError::NoWorkItem);
                }
                (Planning, None)
            }
            Planning => {
                // Intake questions gate: only on the first planning pass (before
                // any candidates exist). If any planner needs clarification, block
                // for human intervention; otherwise produce candidate plans.
                if self.store.max_candidate_iteration()?.is_none() {
                    if let Some(questions) = self.run_intake_questions()? {
                        self.store.set_questions(&questions)?;
                        (IntakeReview, None)
                    } else {
                        self.run_planners()?;
                        (Converging, None)
                    }
                } else {
                    self.run_planners()?;
                    (Converging, None)
                }
            }
            Converging => {
                if self.run_merge()? {
                    // Converged: proceed to the plan gate (or straight to build).
                    if self.config.reviews.plan_review {
                        (PlanReview, None)
                    } else {
                        (Implementing, None)
                    }
                } else {
                    // Not converged: loop back for another planning round.
                    (Planning, None)
                }
            }
            Implementing => {
                self.run_implementer()?;
                (Reviewing, None)
            }
            Reviewing => match self.run_reviewer()? {
                ReviewOutcome::Accepted => {
                    if self.config.reviews.work_review {
                        (WorkReview, None)
                    } else {
                        (Done, None)
                    }
                }
                ReviewOutcome::Rejected => (Implementing, None),
                ReviewOutcome::Escalated(cause) => (
                    WorkReview,
                    Some(format!("escalated to WorkReview: {cause}")),
                ),
            },
            // Not autonomous; the caller returns early for these.
            IntakeReview | PlanReview | WorkReview | Done | Failed | Abandoned => {
                (self.state, None)
            }
        };
        Ok((next, detail))
    }

    /// Move the work item to `Failed`, recording the cause.
    /// database is preserved for inspection (see `docs/persistence.md`).
    fn fail(&mut self, cause: &str) -> Result<State, CoordinatorError> {
        // fail() is only reached from autonomous states, all of which permit a
        // transition to Failed; guard against future regressions.
        debug_assert!(
            self.state.can_transition_to(State::Failed),
            "{} has no transition to Failed",
            self.state
        );
        self.store.record_transition_with_events(
            Some(self.state),
            State::Failed,
            &format!("step failed: {cause}"),
            &[("error", cause)],
        )?;
        self.state = State::Failed;
        self.record_activity(ActivityEvent::new(ActivityKind::Failed, cause).phase(State::Failed));
        Ok(State::Failed)
    }

    /// Step repeatedly until the work item is blocked or reaches a terminal state.
    ///
    /// When it stops at a blocked state, the named session is recorded so
    /// the human can resume it (see `docs/sessions.md`).
    pub fn run_until_blocked(&mut self) -> Result<State, CoordinatorError> {
        loop {
            let before = self.state;
            let after = self.step()?;
            if after == before {
                self.ensure_session()?;
                let kind = if after.is_blocked() {
                    ActivityKind::HumanIntervention
                } else if after == State::Failed {
                    ActivityKind::Failed
                } else {
                    ActivityKind::Completed
                };
                self.record_activity(
                    ActivityEvent::new(kind, format!("work item stopped at {after}")).phase(after),
                );
                return Ok(after);
            }
        }
    }

    fn record_activity(&mut self, event: ActivityEvent) {
        match self.store.record_activity(&event) {
            Ok(recorded) => self.observer.on_activity(&recorded),
            Err(error) => self.observer.on_persistence_error(&event, &error),
        }
    }
}

fn agent_error_is_retryable(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::Spawn { .. } | AgentError::NonZeroExit { .. }
    )
}

fn short_sha(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentError, AgentRequest, EchoRunner};
    use crate::observability::ActivityEvent;
    use crate::persistence::Database;
    use crate::worktree::{ImplementationWorkspace, RoundGitResult};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const TEST_CAPABILITIES: &str = "### Execution capabilities\n```yaml\nshell: true\ninternet: false\nlocal_server: none\nbrowser: none\nartifacts: false\ntimeout_minutes: 30\n```";

    fn test_plan(summary: &str) -> String {
        format!("### Summary\n{summary}\n\n{TEST_CAPABILITIES}")
    }

    struct CollectingObserver {
        events: Arc<Mutex<Vec<ActivityEvent>>>,
    }

    impl ActivityObserver for CollectingObserver {
        fn on_activity(&self, event: &ActivityEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }

    struct FakeImplementationWorkspace {
        head: Mutex<String>,
        stable_tree: bool,
    }

    impl FakeImplementationWorkspace {
        fn new(stable_tree: bool) -> FakeImplementationWorkspace {
            FakeImplementationWorkspace {
                head: Mutex::new("base-commit".to_string()),
                stable_tree,
            }
        }
    }

    impl ImplementationWorkspace for FakeImplementationWorkspace {
        fn head(&self, _worktree: &std::path::Path) -> Result<String, WorktreeError> {
            Ok(self.head.lock().unwrap().clone())
        }

        fn is_clean(&self, _worktree: &std::path::Path) -> Result<bool, WorktreeError> {
            Ok(true)
        }

        fn finalize(
            &self,
            _worktree: &std::path::Path,
            _work_item_id: &crate::persistence::WorkItemId,
            _slug: &str,
            round: &crate::persistence::ImplementationRound,
        ) -> Result<RoundGitResult, WorktreeError> {
            let commit = format!("round-{}", round.iteration);
            *self.head.lock().unwrap() = commit.clone();
            Ok(RoundGitResult {
                commit,
                tree: if self.stable_tree {
                    "stable-tree".to_string()
                } else {
                    format!("tree-{}", round.iteration)
                },
            })
        }
    }

    fn fake_workspace() -> Box<dyn ImplementationWorkspace> {
        Box::new(FakeImplementationWorkspace::new(false))
    }

    fn coordinator_with_work_item(config: Config) -> (Coordinator, tempfile::TempDir) {
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item\ndo the thing").unwrap();
        let co = Coordinator::new(
            config,
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            workspace.path().into(),
            "test-work-item",
        )
        .unwrap();
        (co, workspace)
    }

    /// A runner whose merge output reports `ITERATE` for the first `iterate_times`
    /// merge calls, then `CONVERGED`. Planner outputs vary per call so the plan is
    /// never "unchanged". Used to exercise the convergence loop.
    struct IteratingRunner {
        iterate_times: usize,
        merges: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    impl IteratingRunner {
        fn new(iterate_times: usize) -> IteratingRunner {
            IteratingRunner {
                iterate_times,
                merges: Arc::new(AtomicUsize::new(0)),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl AgentRunner for IteratingRunner {
        fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
            if matches!(req.role, AgentRole::IntakePlanner { .. }) {
                return Ok("NONE".to_string());
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if req.role == AgentRole::CoordinatorMerge {
                let m = self.merges.fetch_add(1, Ordering::SeqCst);
                let verdict = if m < self.iterate_times {
                    "ITERATE — differs"
                } else {
                    "CONVERGED"
                };
                Ok(format!(
                    "## Plan\n{}\n\n## Convergence\n{verdict}",
                    test_plan(&format!("plan revision {m}"))
                ))
            } else {
                // Distinct planner output each call so plans are never unchanged.
                Ok(format!("## Summary\ncandidate {n} from {}", req.role))
            }
        }
    }

    #[test]
    fn new_coordinator_starts_at_intake() {
        let (co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn intake_without_work_item_moves_to_failed() {
        let store = Store::open_in_memory().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            PathBuf::from("."),
            "test-work-item",
        )
        .unwrap();
        // A missing work item is unrecoverable: it moves to Failed with the
        // cause recorded, rather than aborting the process.
        assert_eq!(co.step().unwrap(), State::Failed);
        assert!(co.store.count_events_of_kind("error").unwrap() >= 1);
    }

    #[test]
    fn runs_until_first_review_gate_by_default() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        let state = co.run_until_blocked().unwrap();
        assert_eq!(state, State::PlanReview);

        let path: Vec<State> = co.history().unwrap().iter().map(|t| t.to).collect();
        assert_eq!(
            path,
            vec![State::Planning, State::Converging, State::PlanReview]
        );
    }

    #[test]
    fn blocking_records_a_named_session() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);

        // The session name is deterministic and derived from the work-item slug and state.
        let expected = "quorum/test-work-item/PlanReview".to_string();
        assert_eq!(co.session_name(), Some(expected.clone()));
        // And it was persisted (recoverable across a crash).
        assert_eq!(co.store.session(State::PlanReview).unwrap(), Some(expected));
    }

    #[test]
    fn non_blocked_state_has_no_session() {
        let (co, _tmp) = coordinator_with_work_item(Config::default());
        // At Intake (autonomous), there is no human-intervention session.
        assert_eq!(co.session_name(), None);
    }

    #[test]
    fn planning_persists_a_candidate_per_planner() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        co.run_until_blocked().unwrap();
        let candidates = co.store.candidates(0).unwrap();
        // Default roster has three planner slots.
        assert_eq!(candidates.len(), 3);
        assert!(candidates.iter().all(|(_, text)| text.contains("stub")));
    }

    #[test]
    fn single_pass_convergence_persists_a_plan() {
        // EchoRunner reports CONVERGED, so one pass suffices.
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        let plan = co.store.plan().unwrap();
        assert!(plan.is_some(), "a converged plan must be persisted");
        // Only one planning iteration.
        assert_eq!(co.store.max_candidate_iteration().unwrap(), Some(0));
    }

    #[test]
    fn iterates_then_converges() {
        let runner = IteratingRunner::new(2); // ITERATE twice, then CONVERGE
        let merges = runner.merges.clone();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        let mut config = Config::default();
        config.limits.convergence_diff_threshold = 0.0;
        let mut co = Coordinator::new(
            config,
            store,
            Box::new(runner),
            fake_workspace(),
            PathBuf::from("."),
            "test-work-item",
        )
        .unwrap();

        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        // 3 merge calls: ITERATE, ITERATE, CONVERGED.
        assert_eq!(merges.load(Ordering::SeqCst), 3);
        // 3 planning iterations: 0, 1, 2.
        assert_eq!(co.store.max_candidate_iteration().unwrap(), Some(2));

        // History shows the Planning<->Converging loop.
        let converging = co
            .history()
            .unwrap()
            .iter()
            .filter(|t| t.to == State::Converging)
            .count();
        assert_eq!(converging, 3);
    }

    #[test]
    fn convergence_stops_at_max_iters() {
        let mut config = Config::default();
        config.limits.convergence_max_iters = 2;
        // Never reports CONVERGED, so only the max-iters bound stops the loop.
        let runner = IteratingRunner::new(usize::MAX);
        let merges = runner.merges.clone();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        let mut co = Coordinator::new(
            config,
            store,
            Box::new(runner),
            fake_workspace(),
            PathBuf::from("."),
            "test-work-item",
        )
        .unwrap();

        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        // Bounded to 2 planning iterations (0, 1).
        assert_eq!(merges.load(Ordering::SeqCst), 2);
        assert_eq!(co.store.max_candidate_iteration().unwrap(), Some(1));
    }

    /// A runner that converges immediately at merge, and for the reviewer rejects
    /// the first `reject_times` rounds then accepts. Planner and Implementer outputs are
    /// generic. Used to drive the adversarial loop.
    struct ReviewRunner {
        reject_times: usize,
        reviews: Arc<AtomicUsize>,
    }

    impl ReviewRunner {
        fn new(reject_times: usize) -> ReviewRunner {
            ReviewRunner {
                reject_times,
                reviews: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl AgentRunner for ReviewRunner {
        fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
            if matches!(req.role, AgentRole::IntakePlanner { .. }) {
                Ok("NONE".to_string())
            } else if req.role == AgentRole::CoordinatorMerge {
                Ok(format!(
                    "## Plan\n{}\n\n## Convergence\nCONVERGED",
                    test_plan("the plan")
                ))
            } else if req.role == AgentRole::Reviewer {
                let n = self.reviews.fetch_add(1, Ordering::SeqCst);
                let verdict = if n < self.reject_times {
                    "REJECT"
                } else {
                    "ACCEPT"
                };
                Ok(format!("## Verdict\n{verdict}\n\n## Findings\nfinding {n}"))
            } else {
                Ok("## Summary\ncandidate".to_string())
            }
        }
    }

    fn coordinator_with_runner(config: Config, runner: Box<dyn AgentRunner>) -> Coordinator {
        coordinator_with_runner_and_workspace(config, runner, fake_workspace())
    }

    fn coordinator_with_runner_and_workspace(
        config: Config,
        runner: Box<dyn AgentRunner>,
        implementation_workspace: Box<dyn ImplementationWorkspace>,
    ) -> Coordinator {
        let workspace = tempfile::tempdir().unwrap();
        // Leak the tempdir so the workspace path stays valid for the test.
        let path = workspace.keep();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        Coordinator::new(
            config,
            store,
            runner,
            implementation_workspace,
            path,
            "test-work-item",
        )
        .unwrap()
    }

    /// A runner that errors for the first `fail_times` calls, then delegates to
    /// the EchoRunner stub. Used to exercise retry/Failed behavior.
    struct FailingRunner {
        fail_times: usize,
        calls: Arc<AtomicUsize>,
    }

    impl FailingRunner {
        fn new(fail_times: usize) -> FailingRunner {
            FailingRunner {
                fail_times,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl AgentRunner for FailingRunner {
        fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_times {
                Err(AgentError::NonZeroExit {
                    role: req.role.to_string(),
                    code: "1".to_string(),
                    stderr: "boom".to_string(),
                })
            } else {
                EchoRunner.run(req)
            }
        }
    }

    #[test]
    fn persistent_step_failure_moves_to_failed() {
        // The runner always errors; the first planner exhausts its retries.
        let config = Config::default(); // step_retries = 3 -> 4 attempts
        let runner = FailingRunner::new(usize::MAX);
        let calls = runner.calls.clone();
        let mut co = coordinator_with_runner(config, Box::new(runner));

        assert_eq!(co.run_until_blocked().unwrap(), State::Failed);
        // One initial attempt plus step_retries (3) retries = 4 attempts.
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        // The cause was recorded.
        assert!(co.store.count_events_of_kind("error").unwrap() >= 1);
    }

    #[test]
    fn transient_failures_are_retried_then_succeed() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        // Fail step_retries - 1 times, then every call succeeds.
        let runner = FailingRunner::new(2);
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut co = coordinator_with_runner(config, Box::new(runner)).with_observer(Box::new(
            CollectingObserver {
                events: events.clone(),
            },
        ));

        // Despite the initial transient failures, the work item completes normally.
        assert_eq!(co.run_until_blocked().unwrap(), State::Done);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.kind == ActivityKind::AgentRetrying)
                .count(),
            2
        );
    }

    #[test]
    fn emits_and_persists_agent_lifecycle_activity() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            workspace.path().to_path_buf(),
            "test-work-item",
        )
        .unwrap()
        .with_observer(Box::new(CollectingObserver {
            events: events.clone(),
        }));

        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);

        let observed = events.lock().unwrap().clone();
        let started = observed
            .iter()
            .position(|event| event.kind == ActivityKind::AgentStarted)
            .unwrap();
        let completed = observed
            .iter()
            .position(|event| event.kind == ActivityKind::AgentCompleted)
            .unwrap();
        assert!(started < completed);
        assert_eq!(observed, co.store.activities().unwrap());
        assert!(observed
            .iter()
            .any(|event| event.kind == ActivityKind::HumanIntervention));
    }

    /// A runner that raises intake questions while `needs_answers` is set, and
    /// otherwise delegates to the EchoRunner stub.
    struct IntakeRunner {
        needs_answers: Arc<AtomicBool>,
    }

    impl AgentRunner for IntakeRunner {
        fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
            if matches!(req.role, AgentRole::IntakePlanner { .. }) {
                if self.needs_answers.load(Ordering::SeqCst) {
                    Ok("1. Please clarify the scope?".to_string())
                } else {
                    Ok("NONE".to_string())
                }
            } else {
                EchoRunner.run(req)
            }
        }
    }

    #[test]
    fn no_questions_proceeds_past_intake() {
        // EchoRunner returns NONE for intake, so the work item never blocks there.
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert!(co.store.questions().unwrap().is_none());
    }

    #[test]
    fn questions_block_at_intake_review_then_answer_proceeds() {
        let needs = Arc::new(AtomicBool::new(true));
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let mut co = coordinator_with_runner(
            config,
            Box::new(IntakeRunner {
                needs_answers: needs.clone(),
            }),
        );

        // Planners have questions: block at IntakeReview with questions stored.
        assert_eq!(co.run_until_blocked().unwrap(), State::IntakeReview);
        let questions = co.store.questions().unwrap();
        assert!(questions.unwrap().contains("clarify the scope"));

        // Human answers; planners are now satisfied.
        needs.store(false, Ordering::SeqCst);
        assert_eq!(
            co.resolve(Decision::Answer("scope is X".into())).unwrap(),
            State::Planning
        );

        // No more questions: the work item runs to completion, and the answer is stored
        // (and fed to planners).
        assert_eq!(co.run_until_blocked().unwrap(), State::Done);
        assert!(co.store.answers().unwrap().contains("scope is X"));
        // No candidates were produced during the blocked first pass.
        assert!(co.store.max_candidate_iteration().unwrap().is_some());
    }

    #[test]
    fn reviewer_accept_records_review_and_gates_to_work_review() {
        // Default gates on: accept -> WorkReview.
        let mut co = coordinator_with_runner(Config::default(), Box::new(ReviewRunner::new(0)));
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Implementing);
        assert_eq!(co.run_until_blocked().unwrap(), State::WorkReview);
        let review = co.store.latest_review().unwrap().unwrap();
        assert!(review.1, "review should be accepted");
    }

    #[test]
    fn reviewer_reject_then_accept_loops_then_completes() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let runner = ReviewRunner::new(1); // reject once, then accept
        let reviews = runner.reviews.clone();
        let mut co = coordinator_with_runner(config, Box::new(runner));

        assert_eq!(co.run_until_blocked().unwrap(), State::Done);
        // Two review rounds (reject@0, accept@1) and two implementations.
        assert_eq!(reviews.load(Ordering::SeqCst), 2);
        assert_eq!(co.store.review_count().unwrap(), 2);
        assert_eq!(
            co.store.latest_implementation().unwrap().map(|(i, _)| i),
            Some(1)
        );
        // The adversarial loop shows Implementing entered twice.
        let implementing = co
            .history()
            .unwrap()
            .iter()
            .filter(|t| t.to == State::Implementing)
            .count();
        assert_eq!(implementing, 2);
    }

    #[test]
    fn reviewer_exhausts_to_forced_work_review() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        config.limits.adversarial_max_iters = 2;
        // Always rejects.
        let runner = ReviewRunner::new(usize::MAX);
        let reviews = runner.reviews.clone();
        let mut co = coordinator_with_runner(config, Box::new(runner));

        assert_eq!(co.run_until_blocked().unwrap(), State::WorkReview);
        // Bounded to 2 review rounds (0, 1), both rejected.
        assert_eq!(reviews.load(Ordering::SeqCst), 2);
        assert!(!co.state().is_terminal());
    }

    #[test]
    fn unchanged_consecutive_trees_force_work_review() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        config.limits.adversarial_max_iters = 5;
        let runner = ReviewRunner::new(usize::MAX);
        let reviews = runner.reviews.clone();
        let mut co = coordinator_with_runner_and_workspace(
            config,
            Box::new(runner),
            Box::new(FakeImplementationWorkspace::new(true)),
        );

        assert_eq!(co.run_until_blocked().unwrap(), State::WorkReview);
        assert_eq!(reviews.load(Ordering::SeqCst), 2);
        assert!(co
            .history()
            .unwrap()
            .last()
            .unwrap()
            .reason
            .contains("implementation unchanged"));
    }

    #[test]
    fn agent_complete_round_resumes_without_rerunning_implementer() {
        struct CountingRunner(Arc<AtomicUsize>);

        impl AgentRunner for CountingRunner {
            fn run(&self, _req: &AgentRequest) -> Result<String, AgentError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("unexpected".to_string())
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        store.set_plan(&test_plan("the plan"), "").unwrap();
        store
            .reserve_implementation_round(0, "base-commit")
            .unwrap();
        store
            .mark_implementation_agent_complete(0, "persisted summary")
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(CountingRunner(calls.clone())),
            fake_workspace(),
            workspace.path().to_path_buf(),
            "test-work-item",
        )
        .unwrap();

        co.run_implementer().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            co.store.latest_implementation().unwrap(),
            Some((0, "persisted summary".to_string()))
        );
        assert_eq!(
            co.store.implementation_round(0).unwrap().unwrap().status,
            ImplementationRoundStatus::Committed
        );
    }

    #[test]
    fn runs_to_done_when_review_gates_disabled() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let (mut co, _tmp) = coordinator_with_work_item(config);
        let state = co.run_until_blocked().unwrap();
        assert_eq!(state, State::Done);

        let path: Vec<State> = co.history().unwrap().iter().map(|t| t.to).collect();
        assert_eq!(
            path,
            vec![
                State::Planning,
                State::Converging,
                State::Implementing,
                State::Reviewing,
                State::Done,
            ]
        );
    }

    #[test]
    fn implementer_persists_summary_in_supplied_workspace() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let (mut co, tmp) = coordinator_with_work_item(config);
        assert_eq!(co.run_until_blocked().unwrap(), State::Done);

        // The Implementer summary is persisted at iteration 0.
        let latest = co.store.latest_implementation().unwrap();
        assert!(latest.is_some());
        let (iteration, summary) = latest.unwrap();
        assert_eq!(iteration, 0);
        assert!(summary.contains("Implementer"));

        assert!(tmp.path().is_dir());
    }

    #[test]
    fn implementer_retry_reuses_iteration_until_reviewed() {
        // Set up a coordinator sitting at Implementing with a plan present.
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        store.set_plan(&test_plan("the plan"), "").unwrap();
        for (f, t) in [
            (State::Intake, State::Planning),
            (State::Planning, State::Converging),
            (State::Converging, State::Implementing),
        ] {
            store.record_transition(Some(f), t, "x").unwrap();
        }
        let workspace = tempfile::tempdir().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            workspace.path().into(),
            "test-work-item",
        )
        .unwrap();

        // Run the Implementer twice with no review recorded (simulating a crash-retry
        // before the transition): the iteration must stay 0, not grow.
        co.run_implementer().unwrap();
        co.run_implementer().unwrap();
        assert_eq!(
            co.store.latest_implementation().unwrap().map(|(i, _)| i),
            Some(0)
        );
    }

    #[test]
    fn implementing_without_plan_moves_to_failed() {
        // Jump straight to Implementing with no plan persisted.
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# Work Item").unwrap();
        store
            .record_transition(Some(State::Intake), State::Planning, "x")
            .unwrap();
        store
            .record_transition(Some(State::Planning), State::Converging, "x")
            .unwrap();
        store
            .record_transition(Some(State::Converging), State::Implementing, "x")
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            workspace.path().into(),
            "test-work-item",
        )
        .unwrap();
        assert_eq!(co.state(), State::Implementing);
        // No plan is unrecoverable here: the work item moves to Failed.
        assert_eq!(co.step().unwrap(), State::Failed);
    }

    #[test]
    fn illegal_transition_is_rejected() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        let err = co.transition_to(State::Done, "nope").unwrap_err();
        assert!(matches!(err, CoordinatorError::IllegalTransition { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn deterministic_agent_failures_are_not_retried() {
        assert!(!agent_error_is_retryable(&AgentError::Timeout {
            role: "Implementer".to_string(),
            seconds: 1,
            stderr: String::new(),
        }));
        assert!(!agent_error_is_retryable(&AgentError::SandboxDisabled {
            role: "Implementer".to_string(),
        }));
        assert!(agent_error_is_retryable(&AgentError::NonZeroExit {
            role: "Implementer".to_string(),
            code: "1".to_string(),
            stderr: String::new(),
        }));
    }

    #[test]
    fn approve_plan_review_proceeds_to_implementing() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Implementing);
    }

    #[test]
    fn plan_review_rejects_a_missing_capability_grant() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        co.store.set_plan("### Summary\nNo grant", "").unwrap();
        assert!(matches!(
            co.resolve(Decision::Approve),
            Err(CoordinatorError::Capability(
                CapabilityError::MissingSection
            ))
        ));
        assert_eq!(co.state(), State::PlanReview);
    }

    #[test]
    fn reject_plan_review_returns_to_planning() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Reject).unwrap(), State::Planning);
    }

    #[test]
    fn drives_intake_to_done_via_approvals() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        // Plan gate.
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Implementing);
        // Work gate.
        assert_eq!(co.run_until_blocked().unwrap(), State::WorkReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Done);
        assert!(co.state().is_terminal());
    }

    #[test]
    fn abandon_from_blocked_state_terminates() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Abandon).unwrap(), State::Abandoned);
    }

    #[test]
    fn resolve_rejected_when_not_blocked() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        // At Intake (autonomous), no human-intervention decision applies.
        let err = co.resolve(Decision::Approve).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn answer_only_applies_at_intake_review() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        co.run_until_blocked().unwrap(); // PlanReview
        let err = co.resolve(Decision::Answer("nope".into())).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
    }

    #[test]
    fn rejected_resolution_records_no_events() {
        let (mut co, _tmp) = coordinator_with_work_item(Config::default());
        // At Intake, approve is invalid and must not write any event.
        let _ = co.resolve(Decision::Approve);
        let events = co.store.count_events().unwrap();
        // Only the autonomous transitions so far (none yet) — no human-intervention events.
        let human_decisions = co.store.count_events_of_kind("human_decision").unwrap();
        assert_eq!(human_decisions, 0);
        assert_eq!(events, 0);
    }

    #[test]
    fn resumes_persisted_state_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");
        let work_item_id;

        {
            let mut database = Database::open(&path).unwrap();
            let root = crate::repository::RepositoryRoot::from_canonical("/test/repository");
            let repository = database.register_repository(&root).unwrap();
            work_item_id = database
                .get_or_create_work_item(&repository.id, "test-work-item")
                .unwrap();
            let mut store = database.into_store(work_item_id.clone()).unwrap();
            store.set_work_item("# Work Item").unwrap();
            let mut co = Coordinator::new(
                Config::default(),
                store,
                Box::new(EchoRunner),
                fake_workspace(),
                dir.path().into(),
                "test-work-item",
            )
            .unwrap();
            assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        }

        // Reopen: a fresh Coordinator must resume at the persisted state.
        let store = Database::open(&path)
            .unwrap()
            .into_store(work_item_id)
            .unwrap();
        let co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            fake_workspace(),
            dir.path().into(),
            "test-work-item",
        )
        .unwrap();
        assert_eq!(co.state(), State::PlanReview);
        assert_eq!(co.history().unwrap().len(), 3);
    }
}
