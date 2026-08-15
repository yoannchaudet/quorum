//! Quorum Core — headless logic for driving a single work item.
//!
//! See `docs/architecture.md`. The Core is frontend-agnostic; the CLI (and a
//! future Tauri UX) are thin drivers over this crate.

pub mod agent;
pub mod cancel;
pub mod capabilities;
pub mod config;
pub mod convergence;
pub mod coordinator;
pub mod delivery;
pub mod observability;
pub mod persistence;
pub mod prompt;
pub mod repository;
pub mod state;
pub mod worktree;

pub use agent::{AgentError, AgentRole, AgentRunner, CopilotRunner, EchoRunner};
pub use cancel::CancelToken;
pub use capabilities::{
    BrowserCapability, CapabilityError, ExecutionCapabilities, LocalServerCapability,
};
pub use config::Config;
pub use coordinator::{Coordinator, Decision};
pub use delivery::{
    DeliveryBackend, DeliveryError, DryRunDelivery, GitHubDelivery, GitHubRepository, PullRequest,
};
pub use observability::{
    channel_observer, ActivityEvent, ActivityKind, ActivityObserver, ArtifactSnapshot,
    CallbackObserver, ImplementationDocument, NoopActivityObserver, PlanDocument, StatusSnapshot,
};
pub use persistence::{
    Artifact, Database, DeliveryRecord, DeliveryStatus, ImplementationRound,
    ImplementationRoundStatus, RegisteredRepository, RepositoryId, Store, Transition, WorkItemId,
    WorkItemSummary, WorktreeRecord,
};
pub use prompt::Prompt;
pub use repository::{
    resolve_worktree_start, validate_delivery_target, RepositoryError, RepositoryRoot,
    WorktreeStart,
};
pub use state::{Kind, State};
pub use worktree::{
    branch_name, create_work_item_with_worktree, ensure_worktree, ensure_worktree_with_start,
    finalize_implementation_round, git_common_dir, worktree_head, worktree_is_clean,
    worktree_record, GitImplementationWorkspace, ImplementationWorkspace, RoundGitResult,
    WorktreeError,
};
