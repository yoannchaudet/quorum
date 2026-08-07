//! The Coordinator (CO). Mirrors `docs/agents.md`.
//!
//! The CO is the only stateful orchestrator for a single WI: it owns the state
//! machine, runs the agents, and persists after every step so it can resume
//! after a crash. At this skeleton stage `step` is a no-op placeholder.

use crate::config::Config;
use crate::persistence::{Store, StoreError};
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
    // Used by `step` once real orchestration lands; retained now to fix the surface.
    #[allow(dead_code)]
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

    /// Advance the WI by one step.
    ///
    /// Placeholder: real step logic (running PLs, merging, IM/RV) lands in a
    /// later change. For now this is a no-op so the skeleton compiles and the
    /// control surface is fixed.
    pub fn step(&mut self) -> Result<State, CoordinatorError> {
        // TODO: drive agents and persist the resulting transition atomically.
        Ok(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_coordinator_starts_at_intake() {
        let store = Store::open_in_memory().unwrap();
        let co = Coordinator::new(Config::default(), store).unwrap();
        assert_eq!(co.state(), State::Intake);
    }
}
