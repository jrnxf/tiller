//! The replay harness (T5.1): feeds recorded asks through intake and the
//! orchestrator in sandbox mode.
//!
//! A replay builds a sandbox (a temporary Tiller home with the bundle
//! imported, a git origin per project and the fake herdr), then for each
//! ask: runs the real `UserPromptSubmit` hook, hands the turn to an
//! [`Orchestrator`], applies its decisions through the same core and spawn
//! functions the CLI calls, ends each stubbed run, and runs the real `Stop`
//! hook. Every decision and its result is recorded, and an [`audit`] of the
//! sandbox afterwards proves nothing left it.

pub mod audit;
pub mod baseline;
pub mod harness;
pub mod orchestrator;
pub mod sandbox;

pub use audit::Audit;
pub use harness::{Record, Replay};
pub use orchestrator::{Decision, Orchestrator, Recorded, Turn};
pub use sandbox::Sandbox;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Input(String),
    #[error("sandbox: {0}")]
    Sandbox(String),
    #[error("orchestrator: {0}")]
    Orchestrator(String),
    #[error(transparent)]
    State(#[from] tiller_core::Error),
    #[error(transparent)]
    Spawn(#[from] tiller_agents::spawn::SpawnError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
