//! The daemon (T2.8): single-instance lock, pollers and reconcile, the board API over axum with SSE, and the embedded board.

pub mod api;
pub mod assets;
pub mod auth;
pub mod dto;
pub mod forge;
pub mod memory;
pub mod peer;
pub mod quota;
pub mod readonly;
pub mod server;
pub mod sse;
pub mod views;
pub mod workers;

pub use server::{Config, DaemonError, Ended, PORT, run, running};
