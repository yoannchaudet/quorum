//! The Coordinator (CO). Mirrors `docs/agents.md`.
//!
//! The CO is the only stateful orchestrator for a single WI: it owns the state
//! machine, runs the agents, and persists after every step so it can resume
//! after a crash.

use std::path::PathBuf;

use crate::agent::{AgentError, AgentRequest, AgentRunner, Filesystem};
use crate::config::Config;
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

    /// Run the planner roster for the current planning iteration, in isolation,
    /// and persist each candidate plan.
    fn run_planners(&mut self) -> Result<(), CoordinatorError> {
        let work_item = self
            .store
            .work_item()?
            .ok_or(CoordinatorError::NoWorkItem)?;
        let prompt = Prompt::planner();
        // Convergence loop is not yet implemented, so there is a single pass.
        let iteration = 0;
        let slots: Vec<String> = self.config.planners.keys().cloned().collect();
        for slot in slots {
            let rendered = prompt.render(&[("work_item", &work_item), ("answers", "")])?;
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
                if self.config.reviews.plan_review {
                    PlanReview
                } else {
                    Implementing
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
    use crate::agent::EchoRunner;

    fn coordinator_with_wi(config: Config) -> Coordinator {
        let mut store = Store::open_in_memory().unwrap();
        store.set_work_item("# WI\ndo the thing").unwrap();
        Coordinator::new(config, store, Box::new(EchoRunner), PathBuf::from(".")).unwrap()
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
        assert!(candidates
            .iter()
            .all(|(_, text)| text.contains("Dry-run stub")));
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
