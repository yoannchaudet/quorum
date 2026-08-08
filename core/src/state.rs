//! State machine for a single work item (WI).
//!
//! Mirrors `docs/state-machine.md` exactly. That doc is the source of truth;
//! keep this module and the diagram in lockstep.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Whether a state makes autonomous progress, is blocked on a human, or is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The Coordinator (CO) makes progress unattended.
    Autonomous,
    /// Blocked awaiting Human Intervention (HI).
    Blocked,
    /// End state; no further transitions.
    Terminal,
}

/// The states a WI moves through. See `docs/state-machine.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Intake,
    IntakeReview,
    Planning,
    Converging,
    PlanReview,
    Implementing,
    Reviewing,
    WorkReview,
    Done,
    Failed,
    Abandoned,
}

impl State {
    /// The kind of this state (autonomous / blocked on HI / terminal).
    pub fn kind(self) -> Kind {
        use State::*;
        match self {
            IntakeReview | PlanReview | WorkReview => Kind::Blocked,
            Done | Failed | Abandoned => Kind::Terminal,
            Intake | Planning | Converging | Implementing | Reviewing => Kind::Autonomous,
        }
    }

    /// True when the WI is stuck awaiting a human.
    pub fn is_blocked(self) -> bool {
        self.kind() == Kind::Blocked
    }

    /// True when the WI has reached an end state.
    pub fn is_terminal(self) -> bool {
        self.kind() == Kind::Terminal
    }

    /// States reachable from `self` in one transition, per the state diagram.
    pub fn allowed_next(self) -> &'static [State] {
        use State::*;
        match self {
            Intake => &[Planning, Failed],
            Planning => &[IntakeReview, Converging, Failed],
            IntakeReview => &[Planning, Abandoned],
            Converging => &[Planning, PlanReview, Implementing, Failed],
            PlanReview => &[Planning, Implementing, Abandoned],
            Implementing => &[Reviewing, Failed],
            Reviewing => &[Implementing, WorkReview, Done, Failed],
            WorkReview => &[Implementing, Done, Abandoned],
            Done | Failed | Abandoned => &[],
        }
    }

    /// Whether a transition from `self` to `next` is permitted.
    pub fn can_transition_to(self, next: State) -> bool {
        self.allowed_next().contains(&next)
    }

    /// The canonical string used to persist this state.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Intake => "Intake",
            State::IntakeReview => "IntakeReview",
            State::Planning => "Planning",
            State::Converging => "Converging",
            State::PlanReview => "PlanReview",
            State::Implementing => "Implementing",
            State::Reviewing => "Reviewing",
            State::WorkReview => "WorkReview",
            State::Done => "Done",
            State::Failed => "Failed",
            State::Abandoned => "Abandoned",
        }
    }

    /// Parse a state from its canonical string, as stored in the DB.
    pub fn from_db_str(s: &str) -> Option<State> {
        let v = match s {
            "Intake" => State::Intake,
            "IntakeReview" => State::IntakeReview,
            "Planning" => State::Planning,
            "Converging" => State::Converging,
            "PlanReview" => State::PlanReview,
            "Implementing" => State::Implementing,
            "Reviewing" => State::Reviewing,
            "WorkReview" => State::WorkReview,
            "Done" => State::Done,
            "Failed" => State::Failed,
            "Abandoned" => State::Abandoned,
            _ => return None,
        };
        Some(v)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_match_spec() {
        assert_eq!(State::Intake.kind(), Kind::Autonomous);
        assert_eq!(State::Planning.kind(), Kind::Autonomous);
        assert_eq!(State::Converging.kind(), Kind::Autonomous);
        assert_eq!(State::Implementing.kind(), Kind::Autonomous);
        assert_eq!(State::Reviewing.kind(), Kind::Autonomous);

        assert!(State::IntakeReview.is_blocked());
        assert!(State::PlanReview.is_blocked());
        assert!(State::WorkReview.is_blocked());

        assert!(State::Done.is_terminal());
        assert!(State::Failed.is_terminal());
        assert!(State::Abandoned.is_terminal());
    }

    #[test]
    fn terminal_states_have_no_successors() {
        for s in [State::Done, State::Failed, State::Abandoned] {
            assert!(s.allowed_next().is_empty());
        }
    }

    #[test]
    fn representative_transitions_are_allowed() {
        assert!(State::Intake.can_transition_to(State::Planning));
        assert!(State::Planning.can_transition_to(State::IntakeReview));
        assert!(State::Converging.can_transition_to(State::Planning));
        assert!(State::Reviewing.can_transition_to(State::Implementing));
        assert!(State::WorkReview.can_transition_to(State::Done));
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        assert!(!State::Intake.can_transition_to(State::Done));
        assert!(!State::Done.can_transition_to(State::Planning));
        assert!(!State::Planning.can_transition_to(State::Reviewing));
    }

    #[test]
    fn string_roundtrip_is_stable() {
        for s in [
            State::Intake,
            State::IntakeReview,
            State::Planning,
            State::Converging,
            State::PlanReview,
            State::Implementing,
            State::Reviewing,
            State::WorkReview,
            State::Done,
            State::Failed,
            State::Abandoned,
        ] {
            assert_eq!(State::from_db_str(s.as_str()), Some(s));
            assert_eq!(s.to_string(), s.as_str());
        }
        assert_eq!(State::from_db_str("bogus"), None);
    }
}
