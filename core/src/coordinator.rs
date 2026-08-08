//! The Coordinator (CO). Mirrors `docs/agents.md`.
//!
//! The CO is the only stateful orchestrator for a single WI: it owns the state
//! machine, runs the agents, and persists after every step so it can resume
//! after a crash.

use std::path::PathBuf;

use crate::agent::{AgentError, AgentRequest, AgentRunner, Filesystem};
use crate::config::Config;
use crate::convergence;
use crate::persistence::{Store, StoreError, Transition};
use crate::prompt::{Prompt, PromptError};
use crate::state::State;

/// Errors surfaced by the Coordinator.
#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Prompt(#[from] PromptError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("illegal transition {from} -> {to}")]
    IllegalTransition { from: State, to: State },
    #[error("no work item has been loaded")]
    NoWorkItem,
    #[error("no plan is available to implement")]
    NoPlan,
    #[error("no implementation is available to review")]
    NoImplementation,
    #[error("failed to create workspace {path}: {source}")]
    Workspace {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot {decision} in state {state} (not an applicable human-intervention state)")]
    InvalidResolution { state: State, decision: Decision },
}

/// A human's decision to resolve a blocked (HI) state (see `docs/agents.md`).
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
    /// Too many rejected rounds; give up.
    Exhausted,
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
    /// Working directory used as the sandbox cwd for agent invocations.
    workspace: PathBuf,
    state: State,
}

impl Coordinator {
    /// Create a Coordinator over an opened `store`, resuming the persisted state
    /// if present, otherwise starting at `Intake`. `workspace` is the sandbox
    /// cwd for agent invocations.
    pub fn new(
        config: Config,
        store: Store,
        runner: Box<dyn AgentRunner>,
        workspace: PathBuf,
    ) -> Result<Coordinator, CoordinatorError> {
        let state = store.current_state()?.unwrap_or(State::Intake);
        Ok(Coordinator {
            config,
            store,
            runner,
            workspace,
            state,
        })
    }

    /// The current state of the WI.
    pub fn state(&self) -> State {
        self.state
    }

    /// The active configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The recorded transition history for this WI.
    pub fn history(&self) -> Result<Vec<Transition>, CoordinatorError> {
        Ok(self.store.history()?)
    }

    /// The intake questions surfaced to the human, if the WI is awaiting answers.
    pub fn questions(&self) -> Result<Option<String>, CoordinatorError> {
        Ok(self.store.questions()?)
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
        self.state = next;
        Ok(self.state)
    }

    /// Resolve the current blocked (HI) state with a human `decision`, performing
    /// the corresponding validated, persisted transition (see `docs/agents.md`).
    ///
    /// The decision (and any answer) is logged in the **same** transaction as the
    /// transition, so the audit log can never record a decision that did not
    /// actually advance the state.
    ///
    /// Errors if the decision does not apply to the current state (e.g. approving
    /// when the WI is not at a review gate).
    pub fn resolve(&mut self, decision: Decision) -> Result<State, CoordinatorError> {
        use Decision::*;
        use State::*;
        let (next, answer) = match (self.state, &decision) {
            (PlanReview, Approve) => (Implementing, None),
            (PlanReview, Reject) => (Planning, None),
            (WorkReview, Approve) => (Done, None),
            (WorkReview, Reject) => (Implementing, None),
            (IntakeReview, Answer(text)) => (Planning, Some(text.as_str())),
            // Abandoning is allowed from any blocked (HI) state.
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
            extra.push(("hi_answer", text));
        }
        extra.push(("hi_decision", &decision_data));

        let reason = format!("hi: {decision} {} -> {next}", self.state);
        self.store
            .record_transition_with_events(Some(self.state), next, &reason, &extra)?;
        self.state = next;
        Ok(self.state)
    }

    /// Run the planner roster for the current planning iteration, in isolation,
    /// and persist each candidate plan. Each pass uses a fresh iteration so the
    /// convergence loop can compare successive rounds.
    /// Ask each planner, in isolation, whether the WI needs clarification before
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
                role: format!("PL-intake:{slot}"),
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadOnly,
                model,
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
                role: format!("PL:{slot}"),
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadOnly,
                model,
            };
            let output = self.invoke(&req)?;
            self.store.save_candidate(&slot, iteration, &output)?;
        }
        Ok(())
    }

    /// Merge the latest candidates into a Plan and decide convergence.
    ///
    /// Runs the merge (CO) prompt over the current iteration's candidates and the
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
            role: "CO:merge".to_string(),
            prompt: rendered,
            cwd: self.workspace.clone(),
            filesystem: Filesystem::ReadOnly,
            model: self.config.models.coordinator.clone(),
        };
        let output = self.invoke(&req)?;
        let merged = convergence::parse_merge(&output);

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
        Ok(converged)
    }

    /// Run the Implementer (IM) against the accepted Plan, in its writable
    /// workspace, and persist the summary it returns.
    ///
    /// The IM runs read/write, confined to the `implementation/` subdirectory of
    /// the WI workspace (its sandbox cwd, see `docs/isolation.md`). On re-entry
    /// from the adversarial loop, the latest review feedback is fed back in.
    fn run_implementer(&mut self) -> Result<(), CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let plan = self.store.plan()?.ok_or(CoordinatorError::NoPlan)?;
        let feedback = self
            .store
            .latest_review()?
            .filter(|(_, accepted)| !accepted)
            .map(|(text, _)| text)
            .unwrap_or_default();

        // The IM's writable sandbox cwd is the workspace's implementation/ dir.
        let impl_dir = self.workspace.join("implementation");
        std::fs::create_dir_all(&impl_dir).map_err(|source| CoordinatorError::Workspace {
            path: impl_dir.display().to_string(),
            source,
        })?;

        let rendered = Prompt::implementer().render(&[
            ("work_item", &work_item),
            ("plan", &plan),
            ("feedback", &feedback),
        ])?;
        let req = AgentRequest {
            role: "IM".to_string(),
            prompt: rendered,
            cwd: impl_dir,
            filesystem: Filesystem::ReadWrite,
            model: self.config.models.implementer.clone(),
        };
        let output = self.invoke(&req)?;

        // Derive the adversarial iteration from committed review progress, so a
        // retry before the next review is recorded reuses the same iteration
        // (idempotent) rather than creating a phantom unreviewed iteration.
        let iteration = self.store.review_count()?;
        self.store.save_implementation(iteration, &output)?;
        Ok(())
    }

    /// Run the Reviewer (RV) adversarially over the latest implementation and
    /// record the verdict. Returns the outcome so the caller can drive the loop.
    ///
    /// The RV runs read-only and is a different model from the IM (see
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

        let rendered = Prompt::reviewer().render(&[
            ("work_item", &work_item),
            ("plan", &plan),
            ("implementation", &implementation),
        ])?;
        let req = AgentRequest {
            role: "RV".to_string(),
            prompt: rendered,
            cwd: self.workspace.clone(),
            filesystem: Filesystem::ReadOnly,
            model: self.config.models.reviewer.clone(),
        };
        let output = self.invoke(&req)?;
        let review = convergence::parse_review(&output);
        self.store
            .save_review(iteration, &review.findings, review.accepted)?;

        if review.accepted {
            Ok(ReviewOutcome::Accepted)
        } else if iteration + 1 >= self.config.limits.adversarial_max_iters {
            // Too many rejected rounds: give up.
            Ok(ReviewOutcome::Exhausted)
        } else {
            Ok(ReviewOutcome::Rejected)
        }
    }

    /// Invoke an agent, retrying transient failures up to `limits.step_retries`
    /// times. Returns the last error if every attempt fails. This is the
    /// transient boundary: agent runs (process spawn/exit) are what may fail
    /// intermittently and are safe to re-run (see `docs/persistence.md`).
    fn invoke(&self, req: &AgentRequest) -> Result<String, AgentError> {
        // One initial attempt plus up to `step_retries` retries.
        let attempts = self.config.limits.step_retries.saturating_add(1);
        let mut last: Option<AgentError> = None;
        for _ in 0..attempts {
            match self.runner.run(req) {
                Ok(output) => return Ok(output),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("attempts >= 1"))
    }

    /// Advance the WI by one autonomous step, performing any agent work the
    /// current state requires before transitioning.
    ///
    /// Returns the (possibly unchanged) state. The state is unchanged when the
    /// WI is blocked on HI or terminal — the caller must resolve HI to proceed.
    ///
    /// A step whose agent work fails (after retries) or which cannot proceed is
    /// moved to `Failed` (terminal), with the cause recorded, rather than
    /// aborting the process — the CO runs unattended (see `docs/persistence.md`).
    /// Store (database) errors are not recoverable and propagate.
    pub fn step(&mut self) -> Result<State, CoordinatorError> {
        if self.state.is_blocked() || self.state.is_terminal() {
            return Ok(self.state);
        }
        match self.compute_next() {
            Ok(next) => {
                let reason = format!("auto: {} -> {next}", self.state);
                self.transition_to(next, &reason)
            }
            // Database failures are fundamental — do not mask them as Failed.
            Err(e @ CoordinatorError::Store(_)) => Err(e),
            Err(cause) => self.fail(&cause.to_string()),
        }
    }

    /// Compute the next state for the current autonomous state, performing the
    /// agent work that state requires.
    fn compute_next(&mut self) -> Result<State, CoordinatorError> {
        use State::*;
        let next = match self.state {
            Intake => {
                if self.store.work_item()?.is_none() {
                    return Err(CoordinatorError::NoWorkItem);
                }
                Planning
            }
            Planning => {
                // Intake questions gate: only on the first planning pass (before
                // any candidates exist). If any planner needs clarification, block
                // for human intervention; otherwise produce candidate plans.
                if self.store.max_candidate_iteration()?.is_none() {
                    if let Some(questions) = self.run_intake_questions()? {
                        self.store.set_questions(&questions)?;
                        IntakeReview
                    } else {
                        self.run_planners()?;
                        Converging
                    }
                } else {
                    self.run_planners()?;
                    Converging
                }
            }
            Converging => {
                if self.run_merge()? {
                    // Converged: proceed to the plan gate (or straight to build).
                    if self.config.reviews.plan_review {
                        PlanReview
                    } else {
                        Implementing
                    }
                } else {
                    // Not converged: loop back for another planning round.
                    Planning
                }
            }
            Implementing => {
                self.run_implementer()?;
                Reviewing
            }
            Reviewing => match self.run_reviewer()? {
                ReviewOutcome::Accepted => {
                    if self.config.reviews.work_review {
                        WorkReview
                    } else {
                        Done
                    }
                }
                ReviewOutcome::Rejected => Implementing,
                ReviewOutcome::Exhausted => Failed,
            },
            // Not autonomous; the caller returns early for these.
            IntakeReview | PlanReview | WorkReview | Done | Failed | Abandoned => self.state,
        };
        Ok(next)
    }

    /// Move the WI to `Failed`, recording the cause. `Failed` is terminal but the
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
        Ok(State::Failed)
    }

    /// Step repeatedly until the WI is blocked on HI or reaches a terminal state.
    pub fn run_until_blocked(&mut self) -> Result<State, CoordinatorError> {
        loop {
            let before = self.state;
            let after = self.step()?;
            if after == before {
                return Ok(after);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentError, AgentRequest, EchoRunner};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn coordinator_with_wi(config: Config) -> (Coordinator, tempfile::TempDir) {
        let workspace = tempfile::tempdir().unwrap();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# WI\ndo the thing").unwrap();
        let co =
            Coordinator::new(config, store, Box::new(EchoRunner), workspace.path().into()).unwrap();
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
            if req.role.contains("intake") {
                return Ok("NONE".to_string());
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if req.role.starts_with("CO:merge") {
                let m = self.merges.fetch_add(1, Ordering::SeqCst);
                let verdict = if m < self.iterate_times {
                    "ITERATE — differs"
                } else {
                    "CONVERGED"
                };
                Ok(format!(
                    "## Plan\nplan revision {m}\n\n## Convergence\n{verdict}"
                ))
            } else {
                // Distinct planner output each call so plans are never unchanged.
                Ok(format!("## Summary\ncandidate {n} from {}", req.role))
            }
        }
    }

    #[test]
    fn new_coordinator_starts_at_intake() {
        let (co, _tmp) = coordinator_with_wi(Config::default());
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn intake_without_work_item_moves_to_failed() {
        let store = Store::open_in_memory().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            PathBuf::from("."),
        )
        .unwrap();
        // A missing work item is unrecoverable: the WI moves to Failed with the
        // cause recorded, rather than aborting the process.
        assert_eq!(co.step().unwrap(), State::Failed);
        assert!(co.store.count_events_of_kind("error").unwrap() >= 1);
    }

    #[test]
    fn runs_until_first_review_gate_by_default() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        let state = co.run_until_blocked().unwrap();
        assert_eq!(state, State::PlanReview);

        let path: Vec<State> = co.history().unwrap().iter().map(|t| t.to).collect();
        assert_eq!(
            path,
            vec![State::Planning, State::Converging, State::PlanReview]
        );
    }

    #[test]
    fn planning_persists_a_candidate_per_planner() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        let candidates = co.store.candidates(0).unwrap();
        // Default roster has three planner slots.
        assert_eq!(candidates.len(), 3);
        assert!(candidates.iter().all(|(_, text)| text.contains("stub")));
    }

    #[test]
    fn single_pass_convergence_persists_a_plan() {
        // EchoRunner reports CONVERGED, so one pass suffices.
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
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
        store.set_work_item("# WI").unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(runner),
            PathBuf::from("."),
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
        store.set_work_item("# WI").unwrap();
        let mut co = Coordinator::new(config, store, Box::new(runner), PathBuf::from(".")).unwrap();

        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        // Bounded to 2 planning iterations (0, 1).
        assert_eq!(merges.load(Ordering::SeqCst), 2);
        assert_eq!(co.store.max_candidate_iteration().unwrap(), Some(1));
    }

    /// A runner that converges immediately at merge, and for the reviewer rejects
    /// the first `reject_times` rounds then accepts. Planner/IM outputs are
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
            if req.role.contains("intake") {
                Ok("NONE".to_string())
            } else if req.role.starts_with("CO:merge") {
                Ok("## Plan\nthe plan\n\n## Convergence\nCONVERGED".to_string())
            } else if req.role == "RV" {
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
        let workspace = tempfile::tempdir().unwrap();
        // Leak the tempdir so the workspace path stays valid for the test.
        let path = workspace.keep();
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# WI").unwrap();
        Coordinator::new(config, store, runner, path).unwrap()
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
                    role: req.role.clone(),
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
        let mut co = coordinator_with_runner(config, Box::new(runner));

        // Despite the initial transient failures, the WI completes normally.
        assert_eq!(co.run_until_blocked().unwrap(), State::Done);
    }

    /// A runner that raises intake questions while `needs_answers` is set, and
    /// otherwise delegates to the EchoRunner stub.
    struct IntakeRunner {
        needs_answers: Arc<AtomicBool>,
    }

    impl AgentRunner for IntakeRunner {
        fn run(&self, req: &AgentRequest) -> Result<String, AgentError> {
            if req.role.contains("intake") {
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
        // EchoRunner returns NONE for intake, so the WI never blocks there.
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
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

        // No more questions: the WI runs to completion, and the answer is stored
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
    fn reviewer_exhausts_to_failed() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        config.limits.adversarial_max_iters = 2;
        // Always rejects.
        let runner = ReviewRunner::new(usize::MAX);
        let reviews = runner.reviews.clone();
        let mut co = coordinator_with_runner(config, Box::new(runner));

        assert_eq!(co.run_until_blocked().unwrap(), State::Failed);
        // Bounded to 2 review rounds (0, 1), both rejected.
        assert_eq!(reviews.load(Ordering::SeqCst), 2);
        assert!(co.state().is_terminal());
    }

    #[test]
    fn runs_to_done_when_review_gates_disabled() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let (mut co, _tmp) = coordinator_with_wi(config);
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
    fn implementer_persists_summary_and_creates_workspace() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let (mut co, tmp) = coordinator_with_wi(config);
        assert_eq!(co.run_until_blocked().unwrap(), State::Done);

        // The IM summary is persisted at iteration 0.
        let latest = co.store.latest_implementation().unwrap();
        assert!(latest.is_some());
        let (iteration, summary) = latest.unwrap();
        assert_eq!(iteration, 0);
        assert!(summary.contains("IM"));

        // The IM's writable workspace was created.
        assert!(tmp.path().join("implementation").is_dir());
    }

    #[test]
    fn implementer_retry_reuses_iteration_until_reviewed() {
        // Set up a coordinator sitting at Implementing with a plan present.
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# WI").unwrap();
        store.set_plan("the plan", "").unwrap();
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
            workspace.path().into(),
        )
        .unwrap();

        // Run the IM twice with no review recorded (simulating a crash-retry
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
        store.set_work_item("# WI").unwrap();
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
            workspace.path().into(),
        )
        .unwrap();
        assert_eq!(co.state(), State::Implementing);
        // No plan is unrecoverable here: the WI moves to Failed.
        assert_eq!(co.step().unwrap(), State::Failed);
    }

    #[test]
    fn illegal_transition_is_rejected() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        let err = co.transition_to(State::Done, "nope").unwrap_err();
        assert!(matches!(err, CoordinatorError::IllegalTransition { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn approve_plan_review_proceeds_to_implementing() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Implementing);
    }

    #[test]
    fn reject_plan_review_returns_to_planning() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Reject).unwrap(), State::Planning);
    }

    #[test]
    fn drives_intake_to_done_via_approvals() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
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
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Abandon).unwrap(), State::Abandoned);
    }

    #[test]
    fn resolve_rejected_when_not_blocked() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        // At Intake (autonomous), no HI decision applies.
        let err = co.resolve(Decision::Approve).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn answer_only_applies_at_intake_review() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap(); // PlanReview
        let err = co.resolve(Decision::Answer("nope".into())).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
    }

    #[test]
    fn rejected_resolution_records_no_events() {
        let (mut co, _tmp) = coordinator_with_wi(Config::default());
        // At Intake, approve is invalid and must not write any event.
        let _ = co.resolve(Decision::Approve);
        let events = co.store.count_events().unwrap();
        // Only the autonomous transitions so far (none yet) — no HI events.
        let hi = co.store.count_events_of_kind("hi_decision").unwrap();
        assert_eq!(hi, 0);
        assert_eq!(events, 0);
    }

    #[test]
    fn resumes_persisted_state_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");

        {
            let mut store = Store::open(&path).unwrap();
            store.set_work_item("# WI").unwrap();
            let mut co = Coordinator::new(
                Config::default(),
                store,
                Box::new(EchoRunner),
                dir.path().into(),
            )
            .unwrap();
            assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        }

        // Reopen: a fresh Coordinator must resume at the persisted state.
        let store = Store::open(&path).unwrap();
        let co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            dir.path().into(),
        )
        .unwrap();
        assert_eq!(co.state(), State::PlanReview);
        assert_eq!(co.history().unwrap().len(), 3);
    }
}
