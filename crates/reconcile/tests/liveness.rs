//! R12: a worker that is alive but silent, the dated pause that silences
//! the escalation, the errored session treated as silent, and the T2.6
//! carry-over: a herdr idle landing mid-turn never hastens an escalation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{Activity, AttentionKind, ObservationSource};
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx};

/// R12: at the threshold the evidence is recorded; at twice the threshold
/// the orchestrator gets a note with the pane tail and the last tool call;
/// an hour later with no action, the developer.
#[test]
fn silent_worker_escalates_to_orchestrator_then_owner() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    // The last thing anyone saw: a tool call 31 minutes ago.
    w.tool_hook(&worker, "Bash", ago(31));
    let t0 = now();
    let report = w.pass(t0);
    assert!(report.did("R12"), "{report:?}");
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM observations WHERE session_id = '{worker}' AND source = 'liveness' AND event = 'agent_alive'"
        )),
        1
    );
    assert!(w.session(&worker).ended_at.is_none(), "silent is not dead");
    assert_eq!(
        w.session(&worker).activity,
        Activity::Working,
        "the evidence moves nothing"
    );
    let notes = w.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].body.contains("silent for 31 min"),
        "{}",
        notes[0].body
    );
    assert!(
        notes[0].body.contains("Bash, started 31 min ago"),
        "{}",
        notes[0].body
    );
    assert!(notes[0].body.contains("pane tail"), "{}", notes[0].body);
    assert!(w.open_items(&run.card_id).is_empty());

    // The same pass again: nothing new.
    let again = w.pass(t0.plus_minutes(1));
    assert_eq!(w.notes().len(), 1);
    assert!(!again.did("note"), "{again:?}");

    // An hour after the note with no orchestrator action: the developer.
    w.pass(t0.plus_minutes(59));
    assert!(w.open_items(&run.card_id).is_empty());
    let report = w.pass(t0.plus_minutes(61));
    assert!(report.did("recovery item"), "{report:?}");
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("no orchestrator action"),
        "{}",
        items[0].question
    );
    assert!(
        w.launcher.launches().is_empty(),
        "the daemon never relaunches a silent worker"
    );
    assert!(w.run(&run.id).ended_at.is_none());
}

/// R12: only the daemon's own notes count as an earlier silence. A worker
/// that sends a message under a key shaped like the pass's note key does
/// not make its first silence "the second", so the developer is not asked
/// at once.
#[test]
fn a_planted_note_key_is_not_an_earlier_silence() {
    use tiller_core::domain::OutboxKind;
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.tool_hook(&worker, "Bash", ago(31));
    let orchestrator = w.fleet.orchestrator_session.clone();
    w.store_mut()
        .outbox_send(
            &Ctx::at(Actor::Session(worker.clone()), ago(5)),
            Some(&format!("reconcile:R12:note:{}:planted", run.id)),
            &orchestrator,
            OutboxKind::Note,
            "a note from the worker".into(),
        )
        .unwrap();
    let report = w.pass(now());
    assert!(report.did("note to the orchestrator"), "{report:?}");
    assert!(
        w.open_items(&run.card_id).is_empty(),
        "{:?}",
        w.open_items(&run.card_id)
    );
}

/// R12: a dated pause on the run silences the escalation until it passes.
#[test]
fn dated_pause_silences_silent_escalation() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.observe(&worker, Activity::Working, ago(45));
    w.store_mut()
        .run_pause(&owner(), &run.id, now().plus_minutes(120), None)
        .unwrap();
    let report = w.pass(now());
    assert!(!report.did("R12"), "{report:?}");
    assert!(w.notes().is_empty());
    assert!(w.open_items(&run.card_id).is_empty());
}

/// P19 (R12): a dated worker pause silences the silent-worker alarm until
/// the pause date, and the alarm returns after it (the view grants a
/// quarter hour of grace past the date). Once it fires it fires once.
#[test]
fn silent_alarm_returns_after_the_pause_date() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let t0 = now();
    w.observe(&worker, Activity::Working, t0.plus_minutes(-45));
    let until = t0.plus_minutes(120);
    w.store_mut()
        .run_pause(&owner(), &run.id, until, None)
        .unwrap();

    // Well before the date, and just past it but inside the grace: silent.
    for at in [t0, t0.plus_minutes(60), until.plus_minutes(10)] {
        let report = w.pass(at);
        assert!(!report.did("R12"), "{at}: {report:?}");
        assert!(w.notes().is_empty(), "{at}: {:?}", w.notes());
        assert!(w.open_items(&run.card_id).is_empty(), "{at}");
    }

    // Past the date and its grace: the alarm returns, once.
    let report = w.pass(until.plus_minutes(16));
    assert!(report.did("R12"), "{report:?}");
    let silent = |w: &World| {
        w.notes()
            .into_iter()
            .filter(|n| n.body.contains("silent for"))
            .count()
    };
    assert_eq!(silent(&w), 1, "{:?}", w.notes());
    assert!(
        w.launcher.launches().is_empty(),
        "the alarm never relaunches"
    );
    assert!(w.session(&worker).ended_at.is_none());

    let again = w.pass(until.plus_minutes(17));
    assert!(!again.did("R12"), "{again:?}");
    assert_eq!(silent(&w), 1, "{:?}", w.notes());
}

/// R12, C07: a session `errored` for thirty minutes with its agent alive is
/// silent: the orchestrator is told, nothing is retried, resumed or ended.
#[test]
fn errored_thirty_minutes_is_silent_and_never_retried() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.store_mut()
        .observation_record(
            &Ctx::new(Actor::Hook),
            NewObservation {
                session_id: Some(worker.clone()),
                run_id: Some(run.id.clone()),
                source: ObservationSource::Hook,
                event: "StopFailure:server_error".into(),
                verdict: Some(Activity::Errored),
                observed_at: ago(31),
                payload: serde_json::json!({"error": "server_error"}),
            },
        )
        .unwrap();
    let report = w.pass(now());
    assert!(report.did("R12"), "{report:?}");
    assert_eq!(w.notes().len(), 1);
    assert!(w.launcher.launches().is_empty(), "never retried");
    assert_eq!(
        w.count("SELECT count(*) FROM events WHERE kind = 'session.resume_attempted'"),
        0
    );
    assert!(w.session(&worker).ended_at.is_none());
    assert!(w.run(&run.id).ended_at.is_none());
    assert_eq!(w.session(&worker).activity, Activity::Errored);
}

/// The T2.6 carry-over. Tool hooks coalesce for 60 s and a herdr idle
/// replaces a hook working after 30 s, so herdr misreading a 40-minute tool
/// call as idle lands on the session. The engine reads only observation
/// age: the escalation clock runs from the newest observation of any
/// source, so the misread cannot start or hasten an escalation, and a fresh
/// tool hook ends the matter the moment it lands.
#[test]
fn herdr_idle_mid_turn_never_hastens_escalation() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);

    // A long tool call starts; 31 s in, herdr reads the pane as idle.
    let start = ago(14);
    w.tool_hook(&worker, "Bash", start);
    let flip = Timestamp::from_millis(start.as_millis() + 31_000).unwrap();
    let landed = w
        .store_mut()
        .observation_record(
            &Ctx::at(Actor::Daemon, flip),
            NewObservation {
                session_id: Some(worker.clone()),
                run_id: Some(run.id.clone()),
                source: ObservationSource::Herdr,
                event: "herdr:idle".into(),
                verdict: Some(Activity::Idle),
                observed_at: flip,
                payload: serde_json::json!({}),
            },
        )
        .unwrap();
    assert!(
        landed.value.verdict_changed,
        "the hold has expired: the misread lands"
    );
    assert_eq!(w.session(&worker).activity, Activity::Idle);

    // Fourteen minutes in: no escalation of any kind, idle or not.
    let report = w.pass(now());
    assert!(!report.did("R12"), "{report:?}");
    assert!(w.notes().is_empty());

    // The next tool hook is not held back by the coalescing (the activity
    // is idle), so it lands at once and the session reads working again.
    w.tool_hook(&worker, "Bash", ago(1));
    assert_eq!(w.session(&worker).activity, Activity::Working);
    let report = w.pass(now());
    assert!(!report.did("R12"), "{report:?}");

    // The escalation comes when, and only when, the newest observation is
    // past the thresholds: the same moment it would come without the flip.
    let mut v = World::new();
    let quiet = v.worker("w2:p1");
    v.place();
    let start = ago(44);
    v.tool_hook(&quiet, "Bash", start);
    let flip = Timestamp::from_millis(start.as_millis() + 31_000).unwrap();
    let quiet_run = v.session(&quiet).run_id.clone();
    v.store_mut()
        .observation_record(
            &Ctx::at(Actor::Daemon, flip),
            NewObservation {
                session_id: Some(quiet.clone()),
                run_id: quiet_run,
                source: ObservationSource::Herdr,
                event: "herdr:idle".into(),
                verdict: Some(Activity::Idle),
                observed_at: flip,
                payload: serde_json::json!({}),
            },
        )
        .unwrap();
    let report = v.pass(now());
    assert!(report.did("R12"), "{report:?}");
    let notes = v.notes();
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].body.contains("silent for 43 min"),
        "counted from the flip, the newest observation: {}",
        notes[0].body
    );
    assert!(
        v.open_items(&v.run_of(&quiet).card_id).is_empty(),
        "the developer waits an hour after the note"
    );
}
