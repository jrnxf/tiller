//! `recovery.md` section 6: no automatic repair removes unlanded work.
//!
//! Every combination of run state, scan result, pane state and worktree
//! listing is run through passes against the fakes (the whole space, not a
//! sample), and `run.clean` must happen exactly when the five positive facts
//! hold: run ended, scanned in this pass, `unlanded_commits = 0`, `dirty = 0`,
//! the worktree list shows it, and the pane's agent is dead or the pane is closed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::RunOutcome;
use tiller_core::state::runs::EndRun;
use tiller_reconcile::sources::WorktreeScan;

#[derive(Clone, Copy, Debug)]
enum PaneState {
    Alive,
    Dead,
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct Case {
    run_ended: bool,
    exists: bool,
    dirty: Option<bool>,
    unlanded: Option<i64>,
    listed: bool,
    pane: PaneState,
}

impl Case {
    fn cleanable(self) -> bool {
        self.run_ended
            && self.exists
            && self.dirty == Some(false)
            && self.unlanded == Some(0)
            && self.listed
            && !matches!(self.pane, PaneState::Alive)
    }
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for run_ended in [false, true] {
        for exists in [false, true] {
            for dirty in [None, Some(false), Some(true)] {
                for unlanded in [None, Some(0), Some(3)] {
                    for listed in [false, true] {
                        for pane in [PaneState::Alive, PaneState::Dead, PaneState::Gone] {
                            out.push(Case {
                                run_ended,
                                exists,
                                dirty,
                                unlanded,
                                listed,
                                pane,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

#[test]
fn no_automatic_repair_removes_unlanded_work() {
    for case in cases() {
        let mut w = World::new();
        let worker = w.worker("w2:p1");
        w.place();
        let run = w.run_of(&worker);
        let path = run.worktree_path.clone().unwrap();
        w.scanner.set(WorktreeScan {
            path: path.clone().into(),
            exists: case.exists,
            is_worktree: case.exists,
            branch: Some("fm/work".into()),
            head_sha: Some("aaaa1111".into()),
            dirty: case.dirty,
            unlanded_commits: case.unlanded,
        });
        w.scanner.list(if case.listed {
            Some(vec![path.as_str()])
        } else {
            None
        });
        let t0 = now();
        w.pass(t0);
        if case.run_ended {
            w.store_mut()
                .run_end(
                    &owner(),
                    &run.id,
                    EndRun {
                        outcome: RunOutcome::Abandoned,
                        note: Some("done with it".into()),
                    },
                )
                .unwrap();
        }
        match case.pane {
            PaneState::Alive => {}
            PaneState::Dead => w.exit_pane("w2:p1"),
            PaneState::Gone => w.remove_pane("w2:p1"),
        }
        // Two passes, as the loop would run them, plus a daemon restart.
        w.pass(t0.plus_minutes(1));
        w.restart();
        w.pass(t0.plus_minutes(2));

        let cleaned = w
            .event_kinds()
            .iter()
            .filter(|k| *k == "run.cleaned")
            .count();
        let closed = w.launcher.closed().len();
        if case.cleanable() {
            assert_eq!(cleaned, 1, "{case:?}: should have been returned once");
        } else {
            assert_eq!(cleaned, 0, "{case:?}: must never be returned");
        }
        if matches!(case.pane, PaneState::Alive) {
            assert_eq!(closed, 0, "{case:?}: a live agent is never closed");
        }
        if !case.run_ended {
            assert!(
                w.run(&run.id).ended_at.is_none(),
                "{case:?}: a pass never ends a run on inference"
            );
        }
        if case.exists && (case.unlanded != Some(0) || case.dirty != Some(false)) {
            let gone = w.count(&format!(
                "SELECT count(*) FROM worktrees WHERE path = '{path}' AND gone_at IS NOT NULL"
            ));
            assert_eq!(
                gone, 0,
                "{case:?}: a worktree with work or unknowns is never marked gone"
            );
        }
        assert_eq!(
            w.count(
                "SELECT count(*) FROM cards WHERE state = 'closed' AND display_id != 'fleet-1'"
            ),
            0,
            "{case:?}: no card closes on inference"
        );
    }
}

/// A builder that ended `reported` with its head on an open PR left its
/// commits to the PR, which the card tracks: no "still holds work" note.
/// The same worktree under a run that ended `abandoned` still gets one.
#[test]
fn work_on_an_open_pr_is_not_noted_as_held() {
    for (outcome, noted) in [(RunOutcome::Reported, 0), (RunOutcome::Abandoned, 1)] {
        let mut w = World::new();
        let worker = w.worker("w2:p1");
        w.place();
        let run = w.run_of(&worker);
        let path = run.worktree_path.clone().unwrap();
        w.scanner.set(WorktreeScan {
            path: path.clone().into(),
            exists: true,
            is_worktree: true,
            branch: Some("fm/work".into()),
            head_sha: Some("aaaa1111".into()),
            dirty: Some(false),
            unlanded_commits: Some(1),
        });
        let t0 = now();
        w.pass(t0);
        w.link_pr(&worker, 4, "aaaa1111");
        w.store_mut()
            .run_end(
                &owner(),
                &run.id,
                EndRun {
                    outcome,
                    note: None,
                },
            )
            .unwrap();
        w.pass(t0.plus_minutes(1));
        let notes = w.count("SELECT count(*) FROM outbox WHERE body LIKE '%still holds work%'");
        assert_eq!(notes, noted, "{outcome}");
    }
}
