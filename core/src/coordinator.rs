//! The Coordinator (CO). Mirrors `docs/agents.md`.
//!
//! The CO is the only stateful orchestrator for a single WI: it owns the state
//! machine, runs the agents, and persists after every step so it can resume
//! after a crash. At this skeleton stage `step` is a no-op placeholder.

use crate::config::Config;
use crate::persistence::{Store, StoreError, Transition};
use crate::state::State;

/// Errors surfaced by the Coordinator.
#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("illegal transition {from} -> {to}")]
    IllegalTransition { from: State, to: State },
}

/// Orchestrates one work item through the state machine.
pub struct Coordinator {
    config: Config,
    store: Store,
    state: State,
}

impl Coordinator {
    /// Create a Coordinator over an opened `store`, resuming the persisted state
    /// if present, otherwise starting at `Intake`.
    pub fn new(config: Config, store: Store) -> Result<Coordinator, CoordinatorError> {
        let state = store.current_state()?.unwrap_or(State::Intake);
        Ok(Coordinator {
            config,
            store,
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
    ///
    /// Rejects transitions not permitted by the state machine, and persists the
    /// accepted transition atomically before updating in-memory state.
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

    /// The next state for an autonomous step, or `None` when the WI is blocked
    /// (awaiting HI) or terminal.
    ///
    /// This is the happy-path skeleton: agent-driven branches (e.g. PLs raising
    /// questions, or RV rejecting) are not yet wired, so autonomous states move
    /// forward. Review gates from config decide whether the optional human-review
    /// states are entered.
    fn next_autonomous(&self) -> Option<State> {
        use State::*;
        match self.state {
            Intake => Some(Planning),
            Planning => Some(Converging),
            Converging => Some(if self.config.reviews.plan_review {
                PlanReview
            } else {
                Implementing
            }),
            Implementing => Some(Reviewing),
            Reviewing => Some(if self.config.reviews.work_review {
                WorkReview
            } else {
                Done
            }),
            // Blocked (HI) or terminal: no autonomous progress.
            IntakeReview | PlanReview | WorkReview | Done | Failed | Abandoned => None,
        }
    }

    /// Advance the WI by one autonomous step.
    ///
    /// Returns the (possibly unchanged) state. The state is unchanged when the
    /// WI is blocked on HI or terminal — the caller must resolve HI to proceed.
    pub fn step(&mut self) -> Result<State, CoordinatorError> {
        match self.next_autonomous() {
            Some(next) => {
                let reason = format!("auto: {} -> {next}", self.state);
                self.transition_to(next, &reason)
            }
            None => Ok(self.state),
        }
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

    fn coordinator(config: Config) -> Coordinator {
        let store = Store::open_in_memory().unwrap();
        Coordinator::new(config, store).unwrap()
    }

    #[test]
    fn new_coordinator_starts_at_intake() {
        let co = coordinator(Config::default());
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn runs_until_first_review_gate_by_default() {
        // Default config enables both review gates.
        let mut co = coordinator(Config::default());
        let state = co.run_until_blocked().unwrap();
        assert_eq!(state, State::PlanReview);

        let history = co.history().unwrap();
        let path: Vec<State> = history.iter().map(|t| t.to).collect();
        assert_eq!(
            path,
            vec![State::Planning, State::Converging, State::PlanReview]
        );
    }

    #[test]
    fn runs_to_done_when_review_gates_disabled() {
        let mut config = Config::default();
        config.reviews.plan_review = false;
        config.reviews.work_review = false;
        let mut co = coordinator(config);
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
        let mut co = coordinator(Config::default());
        let err = co.transition_to(State::Done, "nope").unwrap_err();
        assert!(matches!(err, CoordinatorError::IllegalTransition { .. }));
        // State is unchanged after a rejected transition.
        assert_eq!(co.state(), State::Intake);
    }

    #[test]
    fn resumes_persisted_state_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quorum.db");

        {
            let store = Store::open(&path).unwrap();
            let mut co = Coordinator::new(Config::default(), store).unwrap();
            assert_eq!(co.run_until_blocked().unwrap(), State::PlanReview);
        }

        // Reopen: a fresh Coordinator must resume at the persisted state.
        let store = Store::open(&path).unwrap();
        let co = Coordinator::new(Config::default(), store).unwrap();
        assert_eq!(co.state(), State::PlanReview);
        assert_eq!(co.history().unwrap().len(), 3);
    }
}
