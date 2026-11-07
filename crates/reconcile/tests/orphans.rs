//! R15: panes, reservations and worktrees against runs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{AttentionKind, Finish, RunOutcome};
use tiller_core::state::runs::EndRun;
use tiller_herdr::fake::Screen;

/// R15: a pane whose run has ended but whose agent is alive is never
/// closed; the orchestrator is asked, and the developer half an hour later.
#[test]
fn alive_pane_without_run_is_never_closed() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let t0 = now();
    w.pass(t0);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("not needed".into()),
            },
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("R15 live pane"), "{report:?}");
    assert!(
        w.launcher.closed().is_empty(),
        "a live agent is never closed"
    );
    assert!(w.herdr.fake_pane("w2:p1").screen == Screen::Idle);
    let notes = w.notes();
    let stop = format!("stop its agent with tiller agent stop {worker}");
    assert!(notes.iter().any(|n| n.body.contains(&stop)), "{notes:?}");
    assert!(w.open_items(&run.card_id).is_empty());
    w.pass(t0.plus_minutes(32));
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(w.launcher.closed().is_empty());
}

/// R15: a reservation whose run ended is released as `reconciled`.
#[test]
fn leaked_reservation_is_released() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    // Leak one: end the run behind the reservation's back.
    w.store()
        .conn()
        .execute(
            "UPDATE runs SET ended_at = ?2, outcome = 'abandoned' WHERE id = ?1",
            rusqlite::params![run.id, now()],
        )
        .unwrap();
    assert_eq!(
        w.count("SELECT count(*) FROM reservations WHERE released_at IS NULL"),
        1
    );
    let report = w.pass(now());
    assert!(report.did("R15 reservation"), "{report:?}");
    assert_eq!(
        w.count("SELECT count(*) FROM reservations WHERE released_at IS NULL"),
        0
    );
    assert_eq!(
        w.count("SELECT count(*) FROM reservations WHERE release_reason = 'reconciled'"),
        1
    );
    let again = w.pass(now().plus_minutes(1));
    assert!(!again.did("R15 reservation"), "{again:?}");
}

/// R15: an ended run's pane with a dead agent is closed, and its worktree
/// returned only on the five positive facts: run ended, scanned in this
/// pass, nothing unlanded, clean, listed by treehouse, agent dead.
#[test]
fn dead_pane_of_ended_run_is_closed_and_clean_worktree_returned() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner.list(Some(vec![path.as_str()]));
    let t0 = now();
    w.pass(t0);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("not needed".into()),
            },
        )
        .unwrap();
    // The agent exited; a shell prompt sits in the pane.
    w.herdr.set_screen("w2:p1", Screen::Shell);
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("R15 pane w2:p1 closed"), "{report:?}");
    assert_eq!(w.launcher.closed(), vec!["w2:p1".to_owned()]);
    assert!(report.did("returned"), "{report:?}");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM worktrees WHERE path = '{path}' AND gone_at IS NOT NULL"
        )),
        1
    );
    assert!(w.event_kinds().contains(&"run.cleaned".to_owned()));
    assert_eq!(
        w.scanner.released(),
        vec![std::path::PathBuf::from(&path)],
        "handed back to the pool"
    );
    assert_eq!(
        w.scanner.release_holders(),
        vec![format!("tiller:{}", run.id)],
        "only while the run still leases it"
    );
    let again = w.pass(t0.plus_minutes(2));
    assert!(!again.did("returned"), "{again:?}");

    // Without the worktree list showing it, nothing is returned.
    let mut v = World::new();
    let worker = v.worker("w2:p1");
    v.place();
    let run = v.run_of(&worker);
    v.pass(t0);
    v.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("x".into()),
            },
        )
        .unwrap();
    v.herdr.set_screen("w2:p1", Screen::Shell);
    let report = v.pass(t0.plus_minutes(1));
    assert!(!report.did("returned"), "{report:?}");
    assert!(!v.event_kinds().contains(&"run.cleaned".to_owned()));
}

/// R15: a run tiller landed through a squash merge holds no unlanded work,
/// though the merge gave its commits new SHAs and the PR row still reads
/// open: step 5 runs before step 6 refreshes the PR, so the succeeded
/// approval's head is what says the worktree's head landed.
#[test]
fn run_landed_by_tiller_merge_is_not_unlanded_work() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    let head = "aaaa1111";
    // The squash commit on main is new, so the worktree's commit is not
    // reachable from the default branch.
    w.scanner.worktree(&path, "fm/work", head, false, Some(1));
    let pr = w.link_pr(&worker, 1, head);
    let approval = w.approve(&worker, &pr, head, true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, head)
        .unwrap();
    w.store_mut()
        .approval_finish(
            &daemon(),
            &approval,
            Finish::Succeeded,
            serde_json::json!({"sha": "e1e1e1e1"}),
        )
        .unwrap();
    assert_eq!(w.run(&run.id).outcome, Some(RunOutcome::Landed));
    w.forge.pr(REPO, 1, merged_pr(head, "e1e1e1e1", now()));
    let report = w.pass(now());
    assert!(!report.did("unlanded work"), "{report:?}");
    let notes = w.notes();
    assert!(
        !notes.iter().any(|n| n.body.contains("still holds work")),
        "{notes:?}"
    );
}

/// R15: a live pane that names only its ended run is stopped by its
/// session, whose row may have ended: `tiller agent stop <run>` would find
/// no live session.
#[test]
fn alive_pane_naming_only_its_run_names_its_session() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let t0 = now();
    w.pass(t0);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    w.herdr.set_tokens(
        "w2:p1",
        std::collections::BTreeMap::from([("run".to_owned(), run.id.clone())]),
    );
    w.pass(t0.plus_minutes(1));
    let stop = format!("stop its agent with tiller agent stop {worker}");
    let notes = w.notes();
    assert!(notes.iter().any(|n| n.body.contains(&stop)), "{notes:?}");
}

/// R15: a run can end with work its PR has not landed yet. The agent exits
/// first, so the pane loses its session before it closes; once the work
/// lands, the closed pane is still found by its `run` token and the clean
/// worktree is returned.
#[test]
fn worktree_clean_after_the_pane_closed_is_returned() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    let head = "aaaa1111";
    w.scanner.list(Some(vec![path.as_str()]));
    w.scanner.worktree(&path, "fm/work", head, false, Some(1));
    let t0 = now();
    w.pass(t0);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    // The agent exits: its session ends while the pane still shows a shell.
    w.store_mut()
        .session_end(&daemon(), &worker, "prompt_input_exit")
        .unwrap();
    w.herdr.set_screen("w2:p1", Screen::Shell);
    w.pass(t0.plus_minutes(1));
    assert_eq!(
        w.count("SELECT count(*) FROM panes WHERE pane_id = 'w2:p1' AND session_id IS NULL"),
        1,
        "the pane row no longer names the ended session"
    );
    w.herdr.remove_pane("w2:p1");
    let report = w.pass(t0.plus_minutes(2));
    assert!(!report.did("returned"), "unlanded work stays: {report:?}");

    // The work lands.
    w.scanner.worktree(&path, "fm/work", head, false, Some(0));
    let report = w.pass(t0.plus_minutes(3));
    assert!(report.did("returned"), "{report:?}");
    assert!(w.event_kinds().contains(&"run.cleaned".to_owned()));
}

/// R15: a new attempt works on in its predecessor's worktree, so the ended
/// predecessor never returns it, not even once the new attempt ends: the
/// newest run on the path, with its own pane, is the one that does.
#[test]
fn worktree_of_an_ended_run_with_a_successor_is_kept() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner.list(Some(vec![path.as_str()]));
    let t0 = now();
    w.pass(t0);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("next attempt".into()),
            },
        )
        .unwrap();
    // The next attempt starts in the same worktree.
    let next = tiller_core::ids::new_ulid();
    w.store()
        .conn()
        .execute(
            "INSERT INTO runs (id, card_id, role, attempt, predecessor_run_id, model, effort, host_id,
                               worktree_path, branch, base_branch, base_sha, supervisor_session_id,
                               started_at)
             SELECT ?1, card_id, role, attempt + 1, id, model, effort, host_id, worktree_path, branch,
                    base_branch, base_sha, supervisor_session_id, ?2
             FROM runs WHERE id = ?3",
            rusqlite::params![next, t0.plus_minutes(1), run.id],
        )
        .unwrap();
    w.herdr.set_screen("w2:p1", Screen::Shell);
    let report = w.pass(t0.plus_minutes(2));
    assert!(
        !report.did("returned"),
        "the next attempt works in it: {report:?}"
    );
    assert!(w.scanner.released().is_empty());

    // The next attempt ends too, its pane not yet seen closed.
    w.store_mut()
        .run_end(
            &owner(),
            &next,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("done".into()),
            },
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(3));
    assert!(!report.did("returned"), "{report:?}");
    assert!(w.scanner.released().is_empty());
}
