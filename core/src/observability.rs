//! Typed activity events and work-item status snapshots.

use serde::{Deserialize, Serialize};

use crate::persistence::{Store, StoreError};
use crate::state::{Kind, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    PhaseStarted,
    AgentStarted,
    AgentRetrying,
    AgentCompleted,
    AgentFailed,
    Convergence,
    ImplementationRound,
    Review,
    Transition,
    HumanIntervention,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub timestamp_ms: u64,
    pub kind: ActivityKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<State>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl ActivityEvent {
    pub fn new(kind: ActivityKind, message: impl Into<String>) -> ActivityEvent {
        ActivityEvent {
            id: None,
            timestamp_ms: now_millis(),
            kind,
            message: message.into(),
            phase: None,
            role: None,
            model: None,
            iteration: None,
            attempt: None,
            elapsed_ms: None,
        }
    }

    pub fn phase(mut self, phase: State) -> ActivityEvent {
        self.phase = Some(phase);
        self
    }

    pub fn role(mut self, role: impl Into<String>) -> ActivityEvent {
        self.role = Some(role.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> ActivityEvent {
        let model = model.into();
        if !model.is_empty() {
            self.model = Some(model);
        }
        self
    }

    pub fn iteration(mut self, iteration: u32) -> ActivityEvent {
        self.iteration = Some(iteration);
        self
    }

    pub fn attempt(mut self, attempt: u32) -> ActivityEvent {
        self.attempt = Some(attempt);
        self
    }

    pub fn elapsed(mut self, elapsed_ms: u64) -> ActivityEvent {
        self.elapsed_ms = Some(elapsed_ms);
        self
    }
}

pub trait ActivityObserver: Send + Sync {
    fn on_activity(&self, event: &ActivityEvent);

    fn on_persistence_error(&self, _event: &ActivityEvent, _error: &StoreError) {}
}

pub struct NoopActivityObserver;

impl ActivityObserver for NoopActivityObserver {
    fn on_activity(&self, _event: &ActivityEvent) {}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemIdentitySnapshot {
    pub id: String,
    pub slug: String,
    pub repository_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub current: State,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningSnapshot {
    pub iterations: u32,
    pub candidate_count: u32,
    pub planners: Vec<String>,
    pub plan: Option<String>,
    pub metrics: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementationSnapshot {
    pub iteration: u32,
    pub status: String,
    pub start_commit: String,
    pub result_commit: Option<String>,
    pub tree_sha: Option<String>,
    pub changed: Option<bool>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSnapshot {
    pub iteration: u32,
    pub accepted: bool,
    pub findings: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub path: String,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub ready: bool,
    pub head: Option<String>,
    pub clean: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub version: u32,
    pub identity: WorkItemIdentitySnapshot,
    pub state: StateSnapshot,
    pub questions: Option<String>,
    pub session_name: Option<String>,
    pub transitions: Vec<crate::persistence::Transition>,
    pub planning: PlanningSnapshot,
    pub implementations: Vec<ImplementationSnapshot>,
    pub reviews: Vec<ReviewSnapshot>,
    pub errors: Vec<ActivityEvent>,
    pub activities: Vec<ActivityEvent>,
    pub workspace: WorkspaceSnapshot,
}

impl StatusSnapshot {
    pub fn load(store: &Store) -> Result<StatusSnapshot, StoreError> {
        let mut snapshot = store.status_snapshot()?;
        if snapshot.workspace.ready && !snapshot.workspace.path.is_empty() {
            let path = std::path::Path::new(&snapshot.workspace.path);
            snapshot.workspace.head = crate::worktree::worktree_head(path).ok();
            snapshot.workspace.clean = crate::worktree::worktree_is_clean(path).ok();
        }
        Ok(snapshot)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
