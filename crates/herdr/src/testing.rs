//! Test helpers (feature `testing`), from `tiller-panes`, and the fake
//! herdr's layout of a [`Fleet`].
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub use tiller_panes::testing::*;

use crate::fake::{FakeHerdr, Screen};

/// Lays the fleet out as a fresh herdr server would: the orchestrator's
/// workspace, an unrelated one, then each worker's at the bottom. Every
/// Claude pane is idle and carries no tokens yet.
impl Place for FakeHerdr {
    fn place(&self, fleet: &Fleet) {
        self.add_workspace("w1", "alpha");
        self.add_pane("w1", &fleet.orchestrator_pane, Screen::Idle);
        self.add_workspace("w3", "notes");
        self.add_pane("w3", "w3:p1", Screen::Shell);
        for (_, _, pane) in &fleet.workers {
            let workspace = pane.split(':').next().unwrap();
            self.add_workspace(workspace, "claude");
            self.add_pane(workspace, pane, Screen::Idle);
        }
    }
}
