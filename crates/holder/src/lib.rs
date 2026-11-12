//! The terminal holder (`terminal-holder.md` 2, 3): `tiller holder` keeps
//! each agent's PTY, a screen model of it and a socket that serves both.

pub mod detach;
pub mod holder;
pub mod input;
pub mod keys;
pub mod lock;
pub mod model;
pub mod outbox;
pub mod peer;
pub mod pty;
pub mod repaint;
pub mod scanner;
pub mod screen;
pub mod server;

pub use server::{HolderError, Outcome, run};
