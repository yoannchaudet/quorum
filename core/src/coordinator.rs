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
    fn run_planners(&mut self) -> Result<(), CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let previous_plan = self.store.plan()?.unwrap_or_default();
        let prompt = Prompt::planner();
        let iteration = match self.store.max_candidate_iteration()? {
            Some(prev) => prev + 1,
            None => 0,
        };
        let slots: Vec<String> = self.config.planners.keys().cloned().collect();
        for slot in slots {
            let rendered = prompt.render(&[
                ("work_item", &work_item),
                ("answers", ""),
                ("previous_plan", &previous_plan),
            ])?;
            let req = AgentRequest {
                role: format!("PL:{slot}"),
                prompt: rendered,
                cwd: self.workspace.clone(),
                filesystem: Filesystem::ReadOnly,
            };
            let output = self.runner.run(&req)?;
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
        };
        let output = self.runner.run(&req)?;
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

    /// Advance the WI by one autonomous step, performing any agent work the
    /// current state requires before transitioning.
    ///
    /// Returns the (possibly unchanged) state. The state is unchanged when the
    /// WI is blocked on HI or terminal — the caller must resolve HI to proceed.
    ///
    /// Agent-driven branches (PLs raising questions, RV rejecting) are not yet
    /// wired, so autonomous states move forward on the happy path. Review gates
    /// from config decide whether the optional human-review states are entered.
    pub fn step(&mut self) -> Result<State, CoordinatorError> {
        use State::*;
        let next = match self.state {
            Intake => {
                if self.store.work_item()?.is_none() {
                    return Err(CoordinatorError::NoWorkItem);
                }
                Planning
            }
            Planning => {
                self.run_planners()?;
                Converging
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
            Implementing => Reviewing,
            Reviewing => {
                if self.config.reviews.work_review {
                    WorkReview
                } else {
                    Done
                }
            }
            // Blocked (HI) or terminal: no autonomous progress.
            IntakeReview | PlanReview | WorkReview | Done | Failed | Abandoned => {
                return Ok(self.state)
            }
        };
        let reason = format!("auto: {} -> {next}", self.state);
        self.transition_to(next, &reason)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn coordinator_with_wi(config: Config) -> Coordinator {
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# WI\ndo the thing").unwrap();
        Coordinator::new(config, store, Box::new(EchoRunner), PathBuf::from(".")).unwrap()
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
        let co = coordinator_with_wi(Config::default());
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn intake_without_work_item_errors() {
        let store = Store::open_in_memory().unwrap();
        let mut co = Coordinator::new(
            Config::default(),
            store,
            Box::new(EchoRunner),
            PathBuf::from("."),
        )
        .unwrap();
        assert!(matches!(co.step(), Err(CoordinatorError::NoWorkItem)));
    }

    #[test]
    fn runs_until_first_review_gate_by_default() {
        let mut co = coordinator_with_wi(Config::default());
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
        let mut co = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        let candidates = co.store.candidates(0).unwrap();
        // Default roster has three planner slots.
        assert_eq!(candidates.len(), 3);
        assert!(candidates.iter().all(|(_, text)| text.contains("stub")));
    }

    #[test]
    fn single_pass_convergence_persists_a_plan() {
        // EchoRunner reports CONVERGED, so one pass suffices.
        let mut co = coordinator_with_wi(Config::default());
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

    #[test]
    fn runs_to_done_when_review_gates_disabled() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let mut co = coordinator_with_wi(config);
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
    fn illegal_transition_is_rejected() {
        let mut co = coordinator_with_wi(Config::default());
        let err = co.transition_to(State::Done, "nope").unwrap_err();
        assert!(matches!(err, CoordinatorError::IllegalTransition { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn approve_plan_review_proceeds_to_implementing() {
        let mut co = coordinator_with_wi(Config::default());
        assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        assert_eq!(co.resolve(Decision::Approve).unwrap(), State::Implementing);
    }

    #[test]
    fn reject_plan_review_returns_to_planning() {
        let mut co = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Reject).unwrap(), State::Planning);
    }

    #[test]
    fn drives_intake_to_done_via_approvals() {
        let mut co = coordinator_with_wi(Config::default());
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
        let mut co = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap();
        assert_eq!(co.resolve(Decision::Abandon).unwrap(), State::Abandoned);
    }

    #[test]
    fn resolve_rejected_when_not_blocked() {
        let mut co = coordinator_with_wi(Config::default());
        // At Intake (autonomous), no HI decision applies.
        let err = co.resolve(Decision::Approve).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn answer_only_applies_at_intake_review() {
        let mut co = coordinator_with_wi(Config::default());
        co.run_until_blocked().unwrap(); // PlanReview
        let err = co.resolve(Decision::Answer("nope".into())).unwrap_err();
        assert!(matches!(err, CoordinatorError::InvalidResolution { .. }));
    }

    #[test]
    fn rejected_resolution_records_no_events() {
        let mut co = coordinator_with_wi(Config::default());
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
