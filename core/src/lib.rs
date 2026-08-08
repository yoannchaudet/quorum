//! Quorum Core — headless logic for driving a single work item (WI).
//!
//! See `docs/architecture.md`. The Core is frontend-agnostic; the CLI (and a
//! future Tauri UX) are thin drivers over this crate.

pub mod agent;
pub mod config;
pub mod convergence;
pub mod coordinator;
pub mod persistence;
pub mod prompt;
pub mod repository;
pub mod state;

pub use agent::{AgentRunner, CopilotRunner, EchoRunner};
pub use config::Config;
pub use coordinator::{Coordinator, Decision};
pub use persistence::{
    Database, RegisteredRepository, RepositoryId, Store, Transition, WorkItemId,
};
pub use prompt::Prompt;
pub use repository::{RepositoryError, RepositoryRoot};
pub use state::{Kind, State};
