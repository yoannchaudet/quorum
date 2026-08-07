//! Quorum Core — headless logic for driving a single work item (WI).
//!
//! See `docs/architecture.md`. The Core is frontend-agnostic; the CLI (and a
//! future Tauri UX) are thin drivers over this crate.

pub mod config;
pub mod coordinator;
pub mod persistence;
pub mod state;

pub use config::Config;
pub use coordinator::Coordinator;
pub use persistence::{Store, Transition};
pub use state::{Kind, State};
