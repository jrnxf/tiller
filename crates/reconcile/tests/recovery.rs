//! End-to-end rows of the recovery table that resume, end, adopt or ask:
//! R01, R02, R03, R04, R07, R10, R15 (lost work), R16 and R20, against a
//! file-backed `state.db`, a fake holder and fake sources.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{
    Activity, AnsweredVia, ApprovalState, AttentionKind, CardOutcome, CardState, Delivery, Effort,
    OutboxKind, OutboxState, PrState, RunOutcome, RunRole,
};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::get;
use tiller_core::state::outbox::NewMessage;
use tiller_core::state::projects::{HostEdit, NewProject};
use tiller_core::state::runs::{NewRun, Relaunch, SpawnChecks};
use tiller_panes::Panes as _;
use tiller_reconcile::recovery::approvals::{LANDED_BY_ME, LANDED_NOT_ME, SUPERSEDED};
use tiller_reconcile::sources::HostReadings;

/// R02: a dead orchestrator with work in flight is resumed with the same
/// Claude session id; its pending messages wait and need no retarget; the
/// hook's bind completes the resume with a wake. A message to a worker that
/// died and was relaunched moves to the successor.
#[test]
fn orchestrator_crash_resumes_and_retargets_outbox() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let orchestrator = w.fleet.orchestrator_session.clone();
    let t0 = now();
    w.pass(t0);

    // A pending note to the orchestrator, then its pane vanishes.
    let note = w
        .store_mut()
        .outbox_enqueue(
            &owner(),
            None,
            NewMessage {
                target_session_id: orchestrator.clone(),
                kind: OutboxKind::Note,
                body: "a weekly note".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap()
        .value;
    w.remove_pane("w1:p1");
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("launched"), "{report:?}");
    let launches = w.launcher.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].session_id, orchestrator);
    assert_eq!(launches[0].resume.as_deref(), Some("claude-orchestrator"));
    assert_eq!(launches[0].cwd, w.config.project_paths["alpha"]);
    let session = w.session(&orchestrator);
    assert!(
        session.ended_at.is_none(),
        "the row stays open through the resume"
    );
    assert_eq!(
        session.pane_id.as_deref(),
        Some(launches[0].pane_id.as_str())
    );
    assert_eq!(
        w.messages_to(&orchestrator).len(),
        1,
        "the note waits for the same session"
    );

    // Nothing is launched twice while the bind window is open.
    w.pass(t0.plus_minutes(2));
    assert_eq!(w.launcher.launches().len(), 1);

    // SessionStart fires with the same id: the resume completes.
    w.observe(&orchestrator, Activity::Idle, t0.plus_minutes(2));
    let report = w.pass(t0.plus_minutes(3));
    assert!(report.did("resumed after a crash"), "{report:?}");
    let wake: Vec<_> = w
        .messages_to(&orchestrator)
        .into_iter()
        .filter(|m| m.kind == OutboxKind::Wake)
        .collect();
    assert_eq!(wake.len(), 1);
    assert!(
        wake[0].body.contains("1 runs active") && wake[0].body.contains("1 messages waiting"),
        "{}",
        wake[0].body
    );
    assert_eq!(w.messages_to(&orchestrator)[0].id, note.id);
    assert_eq!(w.run_of(&worker).supervisor_session_id, orchestrator);

    // A worker's message outlives its session: the relaunched session takes it.
    let steer = w
        .store_mut()
        .outbox_enqueue(
            &owner(),
            None,
            NewMessage {
                target_session_id: worker.clone(),
                kind: OutboxKind::Steer,
                body: "look at the tests".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap()
        .value;
    w.remove_pane("w2:p1");
    w.pass(t0.plus_minutes(4));
    assert!(w.session(&worker).ended_at.is_some());
    let run = w.run_of(&worker);
    let relaunched = w
        .store_mut()
        .run_relaunch(
            &session_ctx(&orchestrator),
            &run.id,
            Relaunch {
                note: "picking it back up".into(),
                new_attempt: false,
                stop_proof: None,
                model: None,
                checks: None,
            },
        )
        .unwrap()
        .value;
    w.pass(t0.plus_minutes(5));
    let moved: tiller_core::domain::OutboxMessage =
        get(w.store().conn(), "outbox message", &steer.id).unwrap();
    assert_eq!(moved.target_session_id, relaunched.session.id);
    assert_eq!(moved.state, OutboxState::Pending);
}

/// R02, section 2: `--resume` keeps the session row; the attempt and the
/// bind are events on that one row.
#[test]
fn resume_with_same_id_keeps_session_row() {
    let mut w = World::new();
    w.worker("w2:p1");
    w.place();
    let orchestrator = w.fleet.orchestrator_session.clone();
    let before = w.count("SELECT count(*) FROM sessions");
    let t0 = now();
    w.pass(t0);
    w.remove_pane("w1:p1");
    w.pass(t0.plus_minutes(1));
    w.observe(&orchestrator, Activity::Idle, t0.plus_minutes(2));
    w.pass(t0.plus_minutes(3));
    assert_eq!(
        w.count("SELECT count(*) FROM sessions"),
        before,
        "no new row"
    );
    let session = w.session(&orchestrator);
    assert!(session.ended_at.is_none());
    assert_eq!(
        session.claude_session_id.as_deref(),
        Some("claude-orchestrator")
    );
    let kinds: Vec<String> = {
        let mut stmt = w
            .store()
            .conn()
            .prepare(
                "SELECT kind FROM events WHERE entity = 'session' AND entity_id = ?1 ORDER BY seq",
            )
            .unwrap();
        stmt.query_map([&orchestrator], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(
        kinds.contains(&"session.resume_attempted".to_owned()),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"session.bound".to_owned()), "{kinds:?}");
    assert!(!kinds.contains(&"session.ended".to_owned()), "{kinds:?}");
}

/// R03: a worker whose pane is gone has its session ended with the reason,
/// while the run, its worktree and its reservation stay; the orchestrator
/// gets the evidence; the developer only after half an hour of silence.
#[test]
fn worker_crash_keeps_run_worktree_and_reservation() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let t0 = now();
    w.pass(t0);
    w.remove_pane("w2:p1");
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("R03"), "{report:?}");

    let session = w.session(&worker);
    assert_eq!(session.end_reason.as_deref(), Some("crash: pane gone"));
    let run_after = w.run(&run.id);
    assert!(run_after.ended_at.is_none(), "only run.end ends a run");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM reservations WHERE run_id = '{}' AND released_at IS NULL",
            run.id
        )),
        1,
        "the lane stays held"
    );
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM worktrees WHERE run_id = '{}' AND gone_at IS NULL",
            run.id
        )),
        1
    );
    let notes = w.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].body.contains("died") && notes[0].body.contains("crash: pane gone"),
        "{}",
        notes[0].body
    );
    assert!(
        w.open_items(&run.card_id).is_empty(),
        "the first death is the orchestrator's"
    );

    // The same pass again changes nothing.
    let again = w.pass(t0.plus_minutes(2));
    assert!(!again.did("R03"), "{again:?}");
    assert_eq!(w.notes().len(), 1);

    // Thirty minutes without a relaunch, done or cancel: the developer.
    w.pass(t0.plus_minutes(32));
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("no action"),
        "{}",
        items[0].question
    );
}

/// R01: `daemon.started` turns an `executing` approval `uncertain`, and a
/// timer that passed while the daemon was down fires exactly once.
#[test]
fn restart_turns_executing_uncertain_and_fires_timers_once() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    let run = w.run_of(&worker);
    let parked = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: tiller_core::domain::CardKind::Ship,
                title: "parked".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    w.store_mut()
        .card_hold(
            &owner(),
            &parked.id,
            now().plus_minutes(1),
            "wait for the release",
        )
        .unwrap();
    w.forge.pr_error(REPO, 7, "offline");

    let t = now().plus_minutes(2);
    let report = w.pass(t);
    assert!(report.did("R01 approval"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Uncertain);
    assert_eq!(
        w.count("SELECT count(*) FROM holds WHERE released_at IS NOT NULL"),
        1
    );
    let items = w.open_items(&parked.id);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Decision);
    let events = w
        .event_kinds()
        .iter()
        .filter(|k| *k == "hold.released")
        .count();
    w.pass(t.plus_minutes(1));
    assert_eq!(
        w.event_kinds()
            .iter()
            .filter(|k| *k == "hold.released")
            .count(),
        events,
        "fired once"
    );
    let _ = run;
}

/// P19 (R01 timers): a dated hold is not released by a pass before its
/// date; the first pass after it releases it, re-asking the owner with a
/// `decision` item, and later passes fire nothing more.
#[test]
fn dated_hold_holds_until_its_date_then_asks_once() {
    let mut w = World::new();
    w.place();
    let parked = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: tiller_core::domain::CardKind::Ship,
                title: "parked".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let t0 = now();
    let until = t0.plus_minutes(60);
    w.store_mut()
        .card_hold(&owner(), &parked.id, until, "wait for the release")
        .unwrap();
    let released = |w: &World| {
        w.event_kinds()
            .iter()
            .filter(|k| *k == "hold.released")
            .count()
    };

    for at in [t0, t0.plus_minutes(30), until.plus_minutes(-1)] {
        let report = w.pass(at);
        assert!(!report.did("timer: hold"), "{at}: {report:?}");
        assert_eq!(released(&w), 0, "{at}");
        assert_eq!(
            w.count("SELECT count(*) FROM holds WHERE released_at IS NOT NULL"),
            0,
            "{at}"
        );
        assert!(w.open_items(&parked.id).is_empty(), "{at}");
    }

    let report = w.pass(until.plus_minutes(1));
    assert!(report.did("timer: hold"), "{report:?}");
    assert_eq!(released(&w), 1);
    let items = w.open_items(&parked.id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Decision);
    assert_eq!(items[0].question, "wait for the release");

    w.pass(until.plus_minutes(2));
    w.pass(until.plus_minutes(24 * 60));
    assert_eq!(released(&w), 1, "fired once");
    assert_eq!(w.open_items(&parked.id).len(), 1);
}

/// P31: an undated wait is an attention item, not a hold, and waits for the
/// owner's word. Passes spaced days apart never nag: no notification, no
/// note, no second item, and the item stays open and unchanged.
#[test]
fn undated_hold_never_nags_across_days() {
    let mut w = World::new();
    w.place();
    let parked = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: tiller_core::domain::CardKind::Ship,
                title: "release".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let waiting = w
        .store_mut()
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: parked.id.clone(),
                kind: AttentionKind::Decision,
                question: "Hold the release until the owner says go".into(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    assert_eq!(
        w.count("SELECT count(*) FROM holds"),
        0,
        "an undated wait is not a hold"
    );

    let t0 = now();
    w.pass(t0);
    let items_before = w.items().len();
    let notes_before = w.notes().len();
    let events_before = w.count("SELECT count(*) FROM events WHERE card_id IS NOT NULL")
        - w.count(&format!(
            "SELECT count(*) FROM events WHERE card_id = '{}' AND kind LIKE 'observation%'",
            parked.id
        ));
    assert!(w.launcher.notifications().is_empty());

    for days in [1, 2, 5, 9, 30] {
        w.pass(t0.plus_minutes(days * 24 * 60));
        assert!(
            w.launcher.notifications().is_empty(),
            "day {days}: {:?}",
            w.launcher.notifications()
        );
        assert!(w.launcher.launches().is_empty(), "day {days}");
        assert_eq!(w.items().len(), items_before, "day {days}: {:?}", w.items());
        assert_eq!(w.notes().len(), notes_before, "day {days}");
        let held = w.open_items(&parked.id);
        assert_eq!(held.len(), 1, "day {days}: {held:?}");
        assert_eq!(held[0].id, waiting.id);
        assert_eq!(held[0].question, waiting.question);
    }
    assert_eq!(w.count("SELECT count(*) FROM holds"), 0);
    let _ = events_before;
}

/// R04: after a reboot every live session is dead; the orchestrator is
/// resumed in the project's checkout, then each worker in its own worktree,
/// one launch per ten seconds; the binds bring wakes, a summary per project,
/// one notification, and the boot id last.
#[test]
fn reboot_resumes_workers_in_own_worktrees() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    let b = w.worker("w3:p1");
    w.place();
    w.config.boot_id = Some("boot-0".into());
    let t0 = now();
    w.pass(t0);
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-0")
    );

    // The machine restarts: panes gone, daemon back with a new boot id.
    for pane in ["w1:p1", "w2:p1", "w3:p1"] {
        w.remove_pane(pane);
    }
    w.restart();
    w.config.boot_id = Some("boot-1".into());
    let t1 = t0.plus_minutes(10);
    let report = w.pass(t1);
    assert!(report.did("boot id changed"), "{report:?}");
    assert_eq!(w.launcher.launches().len(), 1, "one launch per ten seconds");
    assert_eq!(
        w.launcher.launches()[0].session_id,
        w.fleet.orchestrator_session
    );
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-0"),
        "the boot id is written last"
    );
    let t2 = Timestamp::from_millis(t1.as_millis() + 11_000).unwrap();
    w.pass(t2);
    let t3 = Timestamp::from_millis(t2.as_millis() + 11_000).unwrap();
    w.pass(t3);
    let launches = w.launcher.launches();
    assert_eq!(launches.len(), 3, "{launches:?}");
    for worker in [&a, &b] {
        let run = w.run_of(worker);
        let launch = launches
            .iter()
            .find(|l| l.session_id == *worker)
            .expect("worker launched");
        assert_eq!(
            launch.cwd.to_str(),
            run.worktree_path.as_deref(),
            "its own worktree"
        );
        assert_eq!(
            launch.resume.as_deref(),
            Some(format!("claude-{worker}").as_str())
        );
    }
    for session in [&w.fleet.orchestrator_session.clone(), &a, &b] {
        assert!(w.session(session).ended_at.is_none(), "rows survive");
        assert!(w.run_of_opt(session).is_none_or(|r| r.ended_at.is_none()));
    }

    // Every SessionStart fires; the restore completes.
    let t4 = t3.plus_minutes(1);
    for session in [w.fleet.orchestrator_session.clone(), a.clone(), b.clone()] {
        w.observe(&session, Activity::Idle, t4);
    }
    let report = w.pass(t4.plus_minutes(1));
    assert!(report.did("restore complete"), "{report:?}");
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-1")
    );
    let wakes: Vec<_> = w
        .messages_to(&a)
        .into_iter()
        .filter(|m| m.kind == OutboxKind::Wake)
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(
        wakes[0].body.contains("resumed after a reboot"),
        "{}",
        wakes[0].body
    );
    let notes = w.notes();
    assert!(
        notes
            .iter()
            .any(|n| n.body.contains("host rebooted; restore of alpha")),
        "{notes:?}"
    );
    assert_eq!(w.launcher.notifications().len(), 1);
    assert!(
        w.launcher.notifications()[0]
            .1
            .contains("2 of 2 workers resumed")
    );
    assert!(w.items().is_empty(), "nothing to ask: {:?}", w.items());

    // The next pass finds everyone alive and does nothing.
    let quiet = w.pass(t4.plus_minutes(2));
    assert!(quiet.repairs.is_empty(), "{quiet:?}");
}

/// R21: the terminal holder died and came back as a new instance on the
/// same boot. Each session whose pane went with it goes through the agent
/// probe on its recorded pid: the dead ones are resumed in their own
/// worktrees, one launch per ten seconds, and a child that outlived the
/// holder is left alone. The new instance is written last, with
/// `holder.lost`, and the owner hears once.
#[test]
fn lost_holder_resumes_sessions_and_notifies_once() {
    let mut w = World::new();
    w.place();
    let link = w.holder.clone();
    let (a, _) = w.holder_worker();
    let (b, _) = w.holder_worker();
    let (c, pane_c) = w.holder_worker();
    w.holder_seen();
    let t0 = now();
    w.pass(t0);
    let old = link.instance();
    let pid_c = link.panes().get(&pane_c).unwrap().unwrap().pid;
    let pid_o = w.pane(&w.fleet.orchestrator_pane.clone()).pid;
    assert_eq!(
        w.count("SELECT count(*) FROM panes WHERE holder_instance IS NOT NULL AND pid IS NOT NULL"),
        5,
        "the cache records each pane's pid, the placed ones too"
    );

    // The holder is killed, and the orchestrator's child with it. c's
    // child ignored SIGHUP and still runs.
    link.restart();
    w.probe
        .set_pid(pid_o, tiller_reconcile::sources::Probe::Dead);
    w.probe
        .set_pid(pid_c, tiller_reconcile::sources::Probe::Alive);
    let new = link.instance();
    assert_ne!(new, old);
    let t1 = t0.plus_minutes(10);
    let report = w.pass(t1);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(w.launcher.launches().len(), 1, "one launch per ten seconds");
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "holder_instance")
            .unwrap()
            .as_deref(),
        Some(old.as_str()),
        "the instance is written last"
    );
    let t2 = Timestamp::from_millis(t1.as_millis() + 11_000).unwrap();
    w.pass(t2);
    let t3 = Timestamp::from_millis(t2.as_millis() + 11_000).unwrap();
    w.pass(t3);
    let t4 = Timestamp::from_millis(t3.as_millis() + 11_000).unwrap();
    let report = w.pass(t4);
    assert!(report.did("R21 restore complete"), "{report:?}");

    let launches = w.launcher.launches();
    assert_eq!(launches.len(), 3, "{launches:?}");
    assert_eq!(
        launches[0].session_id, w.fleet.orchestrator_session,
        "the orchestrator goes first"
    );
    for worker in [&a, &b] {
        let run = w.run_of(worker);
        let launch = launches
            .iter()
            .find(|l| l.session_id == *worker)
            .expect("worker launched");
        assert_eq!(
            launch.cwd.to_str(),
            run.worktree_path.as_deref(),
            "its own worktree"
        );
        assert_eq!(
            launch.resume.as_deref(),
            Some(format!("claude-{worker}").as_str())
        );
        assert!(
            w.session(worker).pane_id.is_some_and(|pane| link
                .panes()
                .get(&pane.into())
                .unwrap()
                .is_some()),
            "bound to a pane of the new holder"
        );
    }
    assert!(
        launches.iter().all(|l| l.session_id != c),
        "c outlived the holder"
    );
    for session in [&a, &b, &c] {
        assert!(w.session(session).ended_at.is_none(), "rows survive");
        assert!(w.run_of(session).ended_at.is_none());
    }
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "holder_instance")
            .unwrap()
            .as_deref(),
        Some(new.as_str())
    );
    let lost: Vec<serde_json::Value> = w
        .store()
        .conn()
        .prepare("SELECT payload FROM events WHERE kind = 'holder.lost'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
        .collect();
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert_eq!(lost[0]["previous"], old.as_str());
    assert_eq!(lost[0]["instance"], new.as_str());
    let host: tiller_core::domain::Host = get(w.store().conn(), "host", &w.fleet.host).unwrap();
    assert_eq!(
        w.launcher.notifications(),
        [(
            format!("the terminal holder on {} restarted", host.label),
            "4 of 4 agents resumed".to_owned()
        )]
    );
    let notes = w.notes();
    let summary: Vec<_> = notes
        .iter()
        .filter(|n| {
            n.body
                .contains("the terminal holder restarted; restore of alpha")
        })
        .collect();
    assert_eq!(summary.len(), 1, "{notes:?}");
    for session in [&a, &b, &c] {
        assert!(
            summary[0].body.contains(&w.run_of(session).id),
            "{}",
            summary[0].body
        );
    }
    assert!(w.items().is_empty(), "nothing to ask: {:?}", w.items());

    // The resumed sessions bind; each is woken once. The owner hears
    // nothing more.
    let t5 = t4.plus_minutes(1);
    let orchestrator = w.fleet.orchestrator_session.clone();
    for session in [&orchestrator, &a, &b] {
        w.observe(session, Activity::Idle, t5);
    }
    w.pass(t5.plus_minutes(1));
    let wakes: Vec<_> = w
        .messages_to(&a)
        .into_iter()
        .filter(|m| m.kind == OutboxKind::Wake)
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(
        wakes[0].body.contains("resumed after a holder restart"),
        "{}",
        wakes[0].body
    );
    w.pass(t5.plus_minutes(2));
    assert_eq!(w.launcher.notifications().len(), 1, "notified once");
    assert_eq!(w.launcher.launches().len(), 3);
}

/// R21, a session whose launch files are gone: its resume is refused before
/// any attempt is recorded, and it is skipped like any other session the
/// restore cannot resume. Nothing rebuilds its files.
#[test]
fn lost_holder_skips_a_session_whose_launch_files_are_gone() {
    let mut w = World::new();
    w.place();
    let link = w.use_holder();
    let (a, _) = w.holder_worker();
    w.holder_seen();
    let t0 = now();
    w.pass(t0);

    link.restart();
    w.launcher
        .refuse_next("run r1: launch file settings.json is missing");
    let report = w.pass(t0.plus_minutes(10));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.did("R21 restore complete"), "{report:?}");
    assert!(w.launcher.launches().is_empty(), "nothing was launched");
    assert!(
        !w.event_kinds()
            .iter()
            .any(|k| k == "session.resume_attempted"),
        "a refused resume records no attempt"
    );
    let items = w.items();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].card_id, w.run_of(&a).card_id);
    assert!(
        items[0].question.contains("was not resumed")
            && items[0].question.contains("settings.json is missing"),
        "{}",
        items[0].question
    );
    let notes = w.notes();
    assert!(
        notes.iter().any(|n| n
            .body
            .contains("skipped: run r1: launch file settings.json is missing")),
        "{notes:?}"
    );
    // As for any skipped session, R03 then ends the paneless session; the
    // run keeps its worktree for the owner's relaunch.
    assert!(w.session(&a).ended_at.is_some());
    assert!(w.run_of(&a).ended_at.is_none());

    // Later passes do not try again or ask twice.
    w.pass(t0.plus_minutes(11));
    assert!(w.launcher.launches().is_empty());
    assert_eq!(w.items().len(), 1);
}

/// R02, a crashed orchestrator whose launch files are gone: the resume is
/// refused, the session ends, and the owner gets one item to relaunch it
/// or hold the project.
#[test]
fn orchestrator_crash_with_lost_launch_files_asks_the_owner() {
    let mut w = World::new();
    w.worker("w2:p1");
    w.place();
    let orchestrator = w.fleet.orchestrator_session.clone();
    let t0 = now();
    w.pass(t0);

    w.launcher
        .refuse_next("session s1 recorded no launch manifest, so its launch cannot be repeated");
    w.herdr.remove_pane("w1:p1");
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(w.launcher.launches().is_empty(), "nothing was launched");
    assert!(
        !w.event_kinds()
            .iter()
            .any(|k| k == "session.resume_attempted"),
        "a refused resume records no attempt"
    );
    assert!(w.session(&orchestrator).ended_at.is_some());
    let items = w.items();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0]
            .question
            .contains("could not be resumed after a crash")
            && items[0].question.contains("recorded no launch manifest"),
        "{}",
        items[0].question
    );
    w.pass(t0.plus_minutes(2));
    assert!(w.launcher.launches().is_empty());
    assert_eq!(w.items().len(), 1, "asked once");
}

/// R02, a crashed orchestrator whose script fails for a reason that may
/// pass, such as a `config.toml` that does not parse: nothing is recorded,
/// the session stays open with no item, and a later pass resumes it.
#[test]
fn orchestrator_crash_with_a_failed_script_tries_again_later() {
    let mut w = World::new();
    w.worker("w2:p1");
    w.place();
    let orchestrator = w.fleet.orchestrator_session.clone();
    let t0 = now();
    w.pass(t0);

    w.launcher
        .fail_script_next("config.toml does not parse: expected a value");
    w.herdr.remove_pane("w1:p1");
    let report = w.pass(t0.plus_minutes(1));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("config.toml does not parse")),
        "{report:?}"
    );
    assert!(w.launcher.launches().is_empty(), "nothing was launched");
    assert!(
        !w.event_kinds()
            .iter()
            .any(|k| k == "session.resume_attempted"),
        "a failed script records no attempt"
    );
    assert!(w.session(&orchestrator).ended_at.is_none());
    assert!(w.items().is_empty(), "{:?}", w.items());

    let report = w.pass(t0.plus_minutes(2));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.did("launched"), "{report:?}");
    let launches = w.launcher.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].session_id, orchestrator);
    assert_eq!(launches[0].resume.as_deref(), Some("claude-orchestrator"));
    assert!(w.items().is_empty(), "{:?}", w.items());
}

/// R04: a daemon that dies in the middle of the restore repeats it safely:
/// a session already alive is not launched again, and the boot id is still
/// written only at the end.
#[test]
fn reboot_restore_is_idempotent_when_interrupted() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    w.place();
    w.config.boot_id = Some("boot-0".into());
    let t0 = now();
    w.pass(t0);
    for pane in ["w1:p1", "w2:p1"] {
        w.remove_pane(pane);
    }
    w.restart();
    w.config.boot_id = Some("boot-1".into());
    let t1 = t0.plus_minutes(10);
    w.pass(t1);
    assert_eq!(w.launcher.launches().len(), 1);

    // The daemon dies and comes back: the orchestrator's new pane is alive.
    w.restart();
    let t2 = Timestamp::from_millis(t1.as_millis() + 11_000).unwrap();
    w.pass(t2);
    let launches = w.launcher.launches();
    assert_eq!(launches.len(), 2, "{launches:?}");
    assert_eq!(launches[1].session_id, a);
    assert_eq!(
        launches
            .iter()
            .filter(|l| l.session_id == w.fleet.orchestrator_session)
            .count(),
        1,
        "the alive orchestrator is not launched twice"
    );
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-0")
    );
    w.observe(
        &w.fleet.orchestrator_session.clone(),
        Activity::Idle,
        t2.plus_minutes(1),
    );
    w.observe(&a, Activity::Idle, t2.plus_minutes(1));
    w.pass(t2.plus_minutes(2));
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-1")
    );
}

/// R04: a changed boot id only opens the check. Sessions whose panes and
/// agents are alive are left alone, nothing is launched, and the boot id is
/// recorded.
#[test]
fn changed_boot_id_with_live_agents_ends_nothing() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    w.place();
    w.config.boot_id = Some("boot-0".into());
    let t0 = now();
    w.pass(t0);
    w.restart();
    w.config.boot_id = Some("boot-1".into());
    // The holder still lists the running panes, so nothing needs a probe.
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("restore complete"), "{report:?}");
    assert!(w.launcher.launches().is_empty());
    assert!(w.session(&a).ended_at.is_none());
    assert!(
        w.session(&w.fleet.orchestrator_session.clone())
            .ended_at
            .is_none()
    );
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'session.ended'"),
        0
    );
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "boot_id")
            .unwrap()
            .as_deref(),
        Some("boot-1")
    );
}

/// R04, R17: a resumed session that shows a permission prompt is never
/// answered: nothing is typed into its pane, no second launch follows, and
/// the summary names it.
#[test]
fn resumed_session_at_trust_prompt_is_never_answered() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    w.place();
    w.config.boot_id = Some("boot-0".into());
    let t0 = now();
    w.pass(t0);
    // The orchestrator survives; the worker's pane died with the reboot.
    w.remove_pane("w2:p1");
    w.restart();
    w.config.boot_id = Some("boot-1".into());
    w.launcher.launched_screen(Screen::Permission);
    let t1 = t0.plus_minutes(10);
    w.pass(t1);
    assert_eq!(w.launcher.launches().len(), 1);
    // Past the bind window with no hook: the prompt holds it, no retry.
    let report = w.pass(t1.plus_minutes(3));
    assert_eq!(
        w.launcher.launches().len(),
        1,
        "no second launch at a prompt"
    );
    assert!(report.did("restore complete"), "{report:?}");
    let typed: Vec<String> = w
        .holder
        .panes()
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("input ") || c.starts_with("paste ") || c.starts_with("keys "))
        .collect();
    assert!(
        typed.is_empty(),
        "nothing is typed into any pane: {typed:?}"
    );
    let notes = w.notes();
    assert!(
        notes.iter().any(|n| n.body.contains("trust prompt")),
        "{notes:?}"
    );
    assert!(w.session(&a).ended_at.is_none());
}

/// R07: a PR merged by hand stales the approvals, withdraws the merge item
/// and asks the owner; the owner's "it was me" closes the card landed by
/// the daemon with the runs ended landed.
#[test]
fn merged_pr_without_record_closes_card() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", false);
    let run = w.run_of(&worker);
    assert_eq!(w.open_items(&run.card_id).len(), 1, "the merge item");
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    let t0 = now();
    w.pass(t0);
    assert_eq!(w.approval(&approval).state, ApprovalState::Requested);

    w.forge.pr(REPO, 7, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("R07"), "{report:?}");
    assert_eq!(w.pr(&pr).state, PrState::Merged);
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0]
            .question
            .contains("merged outside Tiller by someone-else"),
        "{}",
        items[0].question
    );
    assert!(
        items[0]
            .recommendation
            .as_deref()
            .unwrap_or("")
            .starts_with(LANDED_BY_ME),
        "{:?}",
        items[0].recommendation
    );
    assert_eq!(
        w.card(&run.card_id).state,
        CardState::Open,
        "never closed on inference"
    );
    assert!(w.run(&run.id).ended_at.is_none());
    assert!(
        w.notes()
            .iter()
            .any(|n| n.body.contains("merged outside Tiller"))
    );

    // Another open question holds the close; nothing ends ahead of it.
    let other = w
        .store_mut()
        .attention_open(
            &owner(),
            None,
            tiller_core::state::attention::NewAttention {
                card: run.card_id.clone(),
                kind: AttentionKind::Decision,
                question: "which release note?".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .value;
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &items[0].id,
            LANDED_BY_ME,
            AnsweredVia::Board,
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(2));
    assert!(
        !report.did("closed landed") && report.errors.is_empty(),
        "{report:?}"
    );
    assert!(
        w.run(&run.id).ended_at.is_none(),
        "no run ends ahead of a refused close"
    );
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &other.id,
            "the short one",
            AnsweredVia::Board,
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(3));
    assert!(report.did("closed landed"), "{report:?}");
    let card = w.card(&run.card_id);
    assert_eq!(card.state, CardState::Closed);
    assert_eq!(card.outcome, Some(CardOutcome::Landed));
    assert_eq!(card.closed_by.as_deref(), Some("daemon"));
    assert_eq!(w.run(&run.id).outcome, Some(RunOutcome::Landed));
    assert!(
        w.breaches("G10").is_empty(),
        "the owner's own merge is not a breach"
    );
}

/// R07: the owner's "Superseded" closes the card `superseded` by the daemon,
/// the answer as evidence, with the runs ended `abandoned`. No pass reports
/// an error on it, then or later, and nothing calls the merge a breach.
#[test]
fn superseded_answer_closes_card_superseded() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.scanner.list(Some(Vec::new()));
    w.link_pr(&worker, 9, "a1a1a1a1");
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let t0 = now();
    w.forge.pr(REPO, 9, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    w.pass(t0);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    w.store_mut()
        .attention_answer(&owner(), None, &items[0].id, SUPERSEDED, AnsweredVia::Board)
        .unwrap();

    let report = w.pass(t0.plus_minutes(1));
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(report.did("closed superseded"), "{report:?}");
    let card = w.card(&run.card_id);
    assert_eq!(card.state, CardState::Closed);
    assert_eq!(card.outcome, Some(CardOutcome::Superseded));
    assert_eq!(card.closed_by.as_deref(), Some("daemon"));
    assert_eq!(w.run(&run.id).outcome, Some(RunOutcome::Abandoned));
    let report = w.pass(t0.plus_minutes(2));
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(w.breaches("G10").is_empty(), "{:?}", w.breaches("G10"));
}

/// R07: an answer whose close core refuses ends no run, so the card is
/// never left open with half the answer carried out.
#[test]
fn a_refused_r07_close_ends_no_run() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.scanner.list(Some(Vec::new()));
    w.link_pr(&worker, 9, "a1a1a1a1");
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let t0 = now();
    w.forge.pr(REPO, 9, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    w.pass(t0);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    w.store_mut()
        .attention_answer(&owner(), None, &items[0].id, SUPERSEDED, AnsweredVia::Board)
        .unwrap();
    w.store()
        .conn()
        .execute_batch(
            "CREATE TRIGGER refuse_close BEFORE UPDATE OF state ON cards
             WHEN NEW.state = 'closed'
             BEGIN SELECT RAISE(ABORT, 'close refused'); END",
        )
        .unwrap();

    let report = w.pass(t0.plus_minutes(1));
    assert!(!report.errors.is_empty(), "{report:?}");
    assert_eq!(w.card(&run.card_id).state, CardState::Open);
    assert!(
        w.run(&run.id).ended_at.is_none(),
        "no run ends ahead of a refused close"
    );
}

/// R07, G10: the same merge answered "not me" still lands the card, and
/// fires the unauthorized-action trigger.
#[test]
fn merge_without_approval_opens_recovery() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.link_pr(&worker, 8, "a1a1a1a1");
    let run = w.run_of(&worker);
    w.forge
        .pr(REPO, 8, merged_pr("a1a1a1a1", "e1e1e1e1", now()));
    let t0 = now();
    w.pass(t0);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(w.breaches("G10").is_empty());
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &items[0].id,
            LANDED_NOT_ME,
            AnsweredVia::Board,
        )
        .unwrap();
    w.pass(t0.plus_minutes(1));
    assert_eq!(w.card(&run.card_id).outcome, Some(CardOutcome::Landed));
    assert_eq!(w.breaches("G10").len(), 1);
    w.pass(t0.plus_minutes(2));
    assert_eq!(w.breaches("G10").len(), 1, "once");
}

/// R07, G10 (`protocol.md` 6.3): a session that stored the "not me" breach
/// key first does not lose the owner's answer. The store refuses the key,
/// so the pass records the breach without it, naming the key and who held
/// it, and reports the refusal once. Only then does the card close landed;
/// later passes and a daemon restart record and report nothing more.
#[test]
fn a_planted_not_me_key_still_records_the_breach() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.link_pr(&worker, 8, "a1a1a1a1");
    let run = w.run_of(&worker);
    let t0 = now();
    w.forge.pr(REPO, 8, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    w.pass(t0);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    let key = format!("reconcile:R07:not-me:{}", items[0].id);
    plant_key(&w, &key, Some(&format!("session:{worker}")));
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &items[0].id,
            LANDED_NOT_ME,
            AnsweredVia::Board,
        )
        .unwrap();

    let report = w.pass(t0.plus_minutes(1));
    assert!(report.did("unauthorized-action trigger"), "{report:?}");
    let refusals = |report: &tiller_reconcile::Report| {
        report
            .errors
            .iter()
            .filter(|error| error.contains(&key))
            .count()
    };
    assert_eq!(refusals(&report), 1, "{report:?}");
    let breaches = w.breaches("G10");
    assert_eq!(breaches.len(), 1, "{breaches:?}");
    assert_eq!(breaches[0]["pass_key"], key.as_str());
    assert_eq!(breaches[0]["key_held_by"], format!("session:{worker}"));
    assert_eq!(w.card(&run.card_id).outcome, Some(CardOutcome::Landed));

    let report = w.pass(t0.plus_minutes(2));
    assert_eq!(refusals(&report), 0, "{report:?}");
    w.restart();
    let report = w.pass(t0.plus_minutes(3));
    assert_eq!(refusals(&report), 0, "{report:?}");
    assert_eq!(w.breaches("G10").len(), 1, "once");
}

/// R15 (`protocol.md` 6.3): a session that stored the "worktree missing"
/// note key first does not stop the question. The note is refused and
/// reported once, the pass goes on, and the developer is asked once the
/// note's wait has passed, counted from when the key was stored.
#[test]
fn a_planted_missing_note_key_still_asks_the_developer() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "aaaa1111", false, Some(0));
    w.pass(now());
    let key = format!("reconcile:R15:missing:{}", run.id);
    plant_key(&w, &key, Some(&format!("session:{worker}")));
    w.scanner.remove(&path);

    let report = w.pass(now().plus_minutes(6));
    let refusals = |report: &tiller_reconcile::Report| {
        report
            .errors
            .iter()
            .filter(|error| error.contains(&key))
            .count()
    };
    assert_eq!(refusals(&report), 1, "{report:?}");
    assert!(w.open_items(&run.card_id).is_empty());

    let report = w.pass(now().plus_minutes(31));
    assert_eq!(refusals(&report), 0, "reported once: {report:?}");
    assert!(report.did("worktree missing: recovery item"), "{report:?}");
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains("is missing"),
        "{}",
        items[0].question
    );
}

/// A planted note key starts no clock while the pass never tried the note:
/// with no live orchestrator, nobody was told, so the developer is not
/// told that the orchestrator has not acted.
#[test]
fn a_planted_note_key_with_no_orchestrator_starts_no_clock() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "aaaa1111", false, Some(0));
    w.pass(now());
    let orchestrator = w.fleet.orchestrator_session.clone();
    w.store()
        .conn()
        .execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            rusqlite::params![now(), orchestrator],
        )
        .unwrap();
    let key = format!("reconcile:R15:missing:{}", run.id);
    plant_key(&w, &key, Some(&format!("session:{worker}")));
    w.scanner.remove(&path);

    w.pass(now().plus_minutes(6));
    w.pass(now().plus_minutes(31));
    let asked: Vec<_> = w
        .open_items(&run.card_id)
        .into_iter()
        .filter(|item| item.question.contains("is missing"))
        .collect();
    assert!(asked.is_empty(), "{asked:?}");
}

/// G10 on a direct-push project: a run's branch head that reached the
/// default branch with no `land_direct` approval is a breach and a question.
#[test]
fn default_branch_move_without_land_opens_recovery() {
    let mut w = World::new();
    w.place();
    w.store_mut()
        .project_add(
            &owner(),
            NewProject {
                id: "beta".into(),
                name: "Beta".into(),
                repo: Some("example/beta".into()),
                remote: Some("https://example.com/beta.git".into()),
                default_branch: None,
                delivery: Delivery::DirectPush,
                checks: None,
                capacity: Some(2),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    let beta_orchestrator = w
        .store_mut()
        .session_start(
            &owner(),
            tiller_core::state::runs::NewOrchestrator {
                project_id: "beta".into(),
                pane_id: Some("w9:p1".into()),
                claude_session_id: Some("claude-beta".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .value
        .session
        .id;
    let card = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "beta".into(),
                kind: tiller_core::domain::CardKind::Ship,
                title: "direct".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let started = w
        .store_mut()
        .run_start(
            &session_ctx(&beta_orchestrator),
            None,
            NewRun {
                card: card.id.clone(),
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some("/home/user/worktrees/beta-1".into()),
                branch: Some("fm/beta-1".into()),
                base_branch: Some("main".into()),
                base_sha: Some("base".into()),
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value;
    // A fresh worktree still at its base landed nothing.
    w.scanner.worktree(
        "/home/user/worktrees/beta-1",
        "fm/beta-1",
        "base",
        false,
        Some(0),
    );
    w.forge.default_head("beta", "b2b2b2b2");
    w.forge.descends("base", "b2b2b2b2");
    w.pass(now());
    assert!(
        w.breaches("G10").is_empty(),
        "a worktree at its base is not a landing"
    );
    w.scanner.worktree(
        "/home/user/worktrees/beta-1",
        "fm/beta-1",
        "a1a1a1a1",
        false,
        Some(1),
    );
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let report = w.pass(now());
    assert!(report.did("G10"), "{report:?}");
    assert_eq!(w.breaches("G10").len(), 1);
    let items = w.open_items(&card.id);
    assert_eq!(items.len(), 1);
    assert!(
        items[0].question.contains("no land_direct approval"),
        "{}",
        items[0].question
    );
    assert!(
        w.run(&started.run.id).ended_at.is_none(),
        "nothing ends on inference"
    );
    w.pass(now().plus_minutes(1));
    assert_eq!(w.breaches("G10").len(), 1);
}

/// R07 and R08 in order: an execution the daemon lost track of is resolved
/// from the forge before anything calls the merge an outside one.
#[test]
fn uncertain_execution_is_never_read_as_outside_merge() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 9, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    let run = w.run_of(&worker);
    // The daemon restarts while the merge call is in flight; the forge then
    // shows the PR merged at the bound head.
    w.forge
        .pr(REPO, 9, merged_pr("a1a1a1a1", "e1e1e1e1", now()));
    let report = w.pass(now());
    assert!(report.did("R08 uncertain approval"), "{report:?}");
    assert!(!report.did("R07"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Succeeded);
    assert_eq!(w.card(&run.card_id).outcome, Some(CardOutcome::Landed));
    assert_eq!(w.run(&run.id).outcome, Some(RunOutcome::Landed));
    assert!(
        w.items().iter().all(|i| i.kind != AttentionKind::Recovery),
        "{:?}",
        w.items()
    );
}

/// R10: after a restore the world is newer than the records: a live pane
/// whose metadata names a session is that session's pane again.
#[test]
fn restore_adopts_live_panes_by_metadata() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    // The restored database says w2:p1; the holder shows the session in w5:p1.
    let meta = w.pane("w2:p1").meta;
    w.remove_pane("w2:p1");
    w.add_pane("w5:p1", &worker, meta, Screen::Working);
    let report = w.pass(now());
    assert!(report.did("R10"), "{report:?}");
    assert_eq!(w.session(&worker).pane_id.as_deref(), Some("w5:p1"));
    assert!(w.session(&worker).ended_at.is_none());
    assert!(
        !report.did("R03"),
        "adoption comes before the ladder reads the old pane as gone: {report:?}"
    );
}

/// R15, G19: an active run whose worktree vanished while its last scan
/// showed unlanded commits is lost work: a breach, a question at once, and
/// nothing else changes.
#[test]
fn worktree_gone_with_active_run_opens_recovery() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "aaaa1111", false, Some(2));
    w.pass(now());
    assert!(w.items().is_empty());
    w.scanner.remove(&path);
    let report = w.pass(now().plus_minutes(6));
    assert!(report.did("lost work") || report.did("G19"), "{report:?}");
    assert_eq!(w.breaches("G19").len(), 1);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1);
    assert!(
        items[0].question.contains("lost work"),
        "{}",
        items[0].question
    );
    assert!(w.run(&run.id).ended_at.is_none());
    assert!(w.session(&worker).ended_at.is_none());
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM worktrees WHERE path = '{path}' AND gone_at IS NOT NULL"
        )),
        1,
        "the cache records the path as gone"
    );
}

/// R01, P43: a `no-mistakes` daemon on another version than its CLI writes
/// `host.degraded`, and an hour later one `recovery` item on `fleet-1`; the
/// fix is the owner's, so nothing else happens. When the skew clears, the
/// check recovers.
#[test]
fn tool_version_skew_degrades_the_host_and_opens_recovery_after_an_hour() {
    let mut w = World::new();
    w.worker("w2:p1");
    w.place();
    let skew = "the no-mistakes daemon is v1.84.0, older than its CLI v1.106.1; \
                restart it with: no-mistakes daemon restart";
    w.readings.set(HostReadings {
        version_skew: Some(skew.into()),
        ..HostReadings::default()
    });
    let t0 = now();
    w.pass(t0);
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'host.degraded' AND json_extract(payload, '$.check') = 'tool_versions'"),
        1
    );
    w.pass(t0.plus_minutes(30));
    assert!(w.open_items(&w.health_card()).is_empty());
    w.pass(t0.plus_minutes(61));
    let items = w.open_items(&w.health_card());
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains("no-mistakes daemon restart"),
        "{}",
        items[0].question
    );
    w.pass(t0.plus_minutes(66));
    assert_eq!(w.open_items(&w.health_card()).len(), 1, "once");
    w.readings.set(HostReadings::default());
    w.pass(t0.plus_minutes(71));
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'host.recovered' AND json_extract(payload, '$.check') = 'tool_versions'"),
        1
    );
}

/// R16: a floor breached for half an hour goes to the developer on
/// `fleet-1`, once, with the consumers; nothing is killed or deleted.
#[test]
fn breached_floor_for_30_minutes_opens_recovery_item() {
    let mut w = World::new();
    w.worker("w2:p1");
    w.place();
    let host = w.fleet.host.clone();
    let rev: i64 = w.count(&format!("SELECT rev FROM hosts WHERE id = '{host}'"));
    w.store_mut()
        .host_edit(
            &owner(),
            &host,
            rev,
            HostEdit {
                disk_floor_mb: Some(Some(40_000)),
                ..HostEdit::default()
            },
        )
        .unwrap();
    w.readings.set(HostReadings {
        disk_free_mb: Some(31_000),
        ..HostReadings::default()
    });
    let t0 = now();
    w.pass(t0);
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'host.degraded' AND json_extract(payload, '$.check') = 'disk'"),
        1
    );
    assert!(w.open_items(&w.health_card()).is_empty());
    w.pass(t0.plus_minutes(31));
    let items = w.open_items(&w.health_card());
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains("31000 MB free"),
        "{}",
        items[0].question
    );
    w.pass(t0.plus_minutes(36));
    assert_eq!(w.open_items(&w.health_card()).len(), 1, "once");
    // A failed reading is unknown, never a recovery.
    w.readings.set(HostReadings::default());
    w.pass(t0.plus_minutes(37));
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'host.recovered' AND json_extract(payload, '$.check') = 'disk'"),
        0
    );
    w.readings.set(HostReadings {
        disk_free_mb: Some(50_000),
        ..HostReadings::default()
    });
    w.pass(t0.plus_minutes(41));
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'host.recovered' AND json_extract(payload, '$.check') = 'disk'"),
        1
    );
}

/// R20: while the owner marker names another system a pass writes nothing;
/// the flip back runs a full pass that rebinds live evidence.
#[test]
fn marker_flip_back_rebinds_live_evidence() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let marker = w.config.tiller_home.join("owner");
    std::fs::write(&marker, "firstmate\n").unwrap();
    let before = w.count("SELECT count(*) FROM events");
    // Meanwhile the world moved: the worker's session lives in another pane.
    let meta = w.pane("w2:p1").meta;
    w.remove_pane("w2:p1");
    w.add_pane("w5:p1", &worker, meta, Screen::Idle);
    let report = w.pass(now());
    assert!(report.read_only);
    assert!(report.repairs.is_empty());
    assert_eq!(
        w.count("SELECT count(*) FROM events"),
        before,
        "no mutation while read only"
    );
    assert_eq!(w.session(&worker).pane_id.as_deref(), Some("w2:p1"));

    std::fs::write(&marker, "tiller\n").unwrap();
    let report = w.pass(now().plus_minutes(1));
    assert!(!report.read_only);
    assert!(report.did("R20"), "{report:?}");
    assert_eq!(w.session(&worker).pane_id.as_deref(), Some("w5:p1"));
    assert!(w.session(&worker).ended_at.is_none());
}

trait RunOfOpt {
    fn run_of_opt(&self, session: &str) -> Option<tiller_core::domain::Run>;
}

impl RunOfOpt for World {
    fn run_of_opt(&self, session: &str) -> Option<tiller_core::domain::Run> {
        let session = self.session(session);
        session.run_id.map(|id| self.run(&id))
    }
}
