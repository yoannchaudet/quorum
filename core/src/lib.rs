//! Quorum Core — headless logic for driving a single work item.
//!
//! See `docs/architecture.md`. The Core is frontend-agnostic; the CLI (and a
//! future Tauri UX) are thin drivers over this crate.

pub mod agent;
pub mod config;
pub mod convergence;
pub mod coordinator;
pub mod observability;
pub mod persistence;
pub mod prompt;
pub mod repository;
pub mod state;
pub mod worktree;

pub use agent::{AgentRole, AgentRunner, CopilotRunner, EchoRunner};
pub use config::Config;
pub use coordinator::{Coordinator, Decision};
pub use observability::{
    ActivityEvent, ActivityKind, ActivityObserver, NoopActivityObserver, StatusSnapshot,
};
pub use persistence::{
    Database, ImplementationRound, ImplementationRoundStatus, RegisteredRepository, RepositoryId,
    Store, Transition, WorkItemId, WorktreeRecord,
};
pub use prompt::Prompt;
pub use repository::{RepositoryError, RepositoryRoot};
pub use state::{Kind, State};
pub use worktree::{
    branch_name, ensure_worktree, finalize_implementation_round, git_common_dir, worktree_head,
    worktree_is_clean, worktree_record, GitImplementationWorkspace, ImplementationWorkspace,
    RoundGitResult, WorktreeError,
};
