//! Agents: the fleet roles (T4.1) and spawn, guards, landing, intake and
//! handoff (T4.2-T4.5).
//!
//! `roles` holds the five role definitions from `roles/`, embedded in the
//! binary so a launch never reads a role file from disk, and the per-run
//! rendering of their settings fragments.

pub mod control;
pub mod curator;
pub mod guards;
pub mod handoff;
pub mod intake;
pub mod land;
pub mod review;
pub mod roles;
pub mod spawn;
