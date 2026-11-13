//! The terminal holder as this host's observation source
//! (`terminal-holder.md` 6.1, `recovery.md` R18 and R22): the connect and
//! its backoff, the `panes` cache, the `holder:*` observations and their
//! hold, and the upgrade trigger, against a fake holder link.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::state::get;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx};
use tiller_panes::Panes as _;
use tiller_panes::message::{Event, PaneId};
use tiller_panes::testing::at;
use tiller_poll::{Poller, Registry};
use tiller_reconcile::holder::{HOLDER_DOWN, HolderSource, Memory, UPGRADE_CHECK};

/// A Claude screen from the shared fixtures, row by row.
fn fixture(name: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../panes/tests/fixtures/claude")
        .join(format!("{name}.ansi"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn show(link: &FakeLink, pane: &PaneId, name: &str) {
    let rows = fixture(name);
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    link.panes().set_screen(pane, &rows);
}

fn poller(w: &World, link: &FakeLink, stable: Option<PathBuf>) -> Poller<HolderSource> {
    let source = HolderSource::new(
        std::sync::Arc::new(link.clone()),
        w.fleet.open(),
        &w.fleet.host,
        stable,
    );
    Registry::new().register(source).unwrap()
}

/// Waits until the source's watch has passed on `n` events in all.
fn received(poller: &Poller<HolderSource>, n: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while poller.source().received() < n {
        assert!(
            Instant::now() < deadline,
            "the watch passed on {} of {n} events",
            poller.source().received()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Emits `event` and waits until the source's watch has it.
fn emit(poller: &Poller<HolderSource>, link: &FakeLink, event: &Event) {
    let before = poller.source().received();
    link.panes().emit(event);
    received(poller, before + 1);
}

fn quiet(poller: &Poller<HolderSource>, link: &FakeLink, pane: &PaneId) {
    emit(
        poller,
        link,
        &Event::Quiet {
            pane: pane.clone(),
            at: "2026-01-01T00:00:00Z".parse().unwrap(),
        },
    );
}

fn holder_events(w: &World, session: &str) -> Vec<String> {
    w.store()
        .conn()
        .prepare(
            "SELECT event FROM observations WHERE session_id = ?1 AND source = 'holder'
             ORDER BY observed_at, rowid",
        )
        .unwrap()
        .query_map([session], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn activity(w: &World, session: &str) -> Activity {
    get::<Session>(w.store().conn(), "session", session)
        .unwrap()
        .activity
}

fn hook(w: &mut World, session: &str, event: &str, verdict: Activity, seconds: i64) {
    w.store_mut()
        .observation_record(
            &Ctx::new(Actor::Hook),
            NewObservation {
                session_id: Some(session.to_owned()),
                run_id: None,
                source: ObservationSource::Hook,
                event: event.into(),
                verdict: Some(verdict),
                observed_at: at(seconds),
                payload: json!({}),
            },
        )
        .unwrap();
}

fn stale_reason(w: &World) -> Option<String> {
    w.store()
        .conn()
        .query_row(
            "SELECT stale_reason FROM hosts WHERE id = ?1",
            [&w.fleet.host],
            |r| r.get(0),
        )
        .unwrap()
}

fn event_payloads(w: &World, kind: &str) -> Vec<Value> {
    w.store()
        .conn()
        .prepare("SELECT payload FROM events WHERE kind = ?1 ORDER BY seq")
        .unwrap()
        .query_map([kind], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
        .collect()
}

/// R18: a holder that stops answering is unknown, never gone. The cache
/// keeps every pane, no `pane_gone` is recorded, three failed connects mark
/// the host `holder_down`, the ladder leaves the session alone, and the
/// next good `list` clears the mark.
#[test]
fn unreachable_holder_marks_panes_unknown_not_gone() {
    let mut w = World::new();
    w.place();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    assert!(poller.source().connected());
    assert_eq!(
        w.count("SELECT count(*) FROM panes WHERE gone_at IS NULL AND holder_instance IS NOT NULL"),
        3,
        "the orchestrator's, the exited one and the worker's"
    );

    // Each poll is past the 30 s list, so the first one fails whether or
    // not the watch thread has seen its connection close yet.
    link.set_down(true);
    for n in 1..=3 {
        poller.poll_once(at(n * 40));
        assert_eq!(
            poller.health().consecutive_failures,
            u32::try_from(n).unwrap()
        );
    }
    assert!(!poller.source().connected());
    assert_eq!(stale_reason(&w).as_deref(), Some(HOLDER_DOWN));
    assert_eq!(
        w.count("SELECT count(*) FROM panes WHERE gone_at IS NULL AND holder_instance IS NOT NULL"),
        3,
        "the cache keeps every pane"
    );
    assert_eq!(holder_events(&w, &session), ["holder:working"]);

    // The ladder has no list, so the session is unknown, not dead.
    let report = w.pass(now());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(w.session(&session).ended_at.is_none());
    assert_eq!(activity(&w, &session), Activity::Working);
    assert!(w.launcher.launches().is_empty());

    link.set_down(false);
    poller.poll_once(at(200));
    assert_eq!(poller.health().consecutive_failures, 0);
    assert_eq!(stale_reason(&w), None, "the next list clears holder_down");
}

/// `terminal-holder.md` 6.1: the connect backs off 5 s, then 15 s, and
/// stays at 15 s: the holder runs on this host.
#[test]
fn backoff_holds_at_fifteen_seconds() {
    let mut w = World::new();
    let link = w.holder.clone();
    link.set_down(true);
    let mut poller = poller(&w, &link, None);
    let waits: Vec<u64> = (0..6)
        .map(|n| poller.poll_once(at(n * 20)).as_secs())
        .collect();
    assert_eq!(waits, [5, 15, 15, 15, 15, 15]);
    link.set_down(false);
    assert_eq!(poller.poll_once(at(200)), Duration::from_secs(2));
}

/// R22: a holder that runs another binary than the stable profile path is
/// asked to upgrade at the first connect. A refusal is a Health warning
/// that stays; the daemon asks again only after a `config.changed`, and an
/// upgrade that is taken records `holder.upgraded` with both versions and
/// clears the warning.
#[test]
fn a_refused_upgrade_is_a_health_warning() {
    let mut w = World::new();
    let link = w.holder.clone();
    let stable = w.fleet.dir.path().join("stable-tiller");
    std::fs::write(&stable, "").unwrap();
    let stable = std::fs::canonicalize(stable).unwrap();
    link.refuse_upgrades(Some("pane p1 is mid-subscribe"));
    let mut poller = poller(&w, &link, Some(stable.clone()));
    poller.poll_once(at(0));
    assert_eq!(link.upgrades(), std::slice::from_ref(&stable));
    assert_eq!(
        w.store().host_degraded(UPGRADE_CHECK).unwrap().as_deref(),
        Some("holder upgrade refused: pane p1 is mid-subscribe")
    );
    for n in [2, 4, 40] {
        poller.poll_once(at(n));
    }
    assert_eq!(link.upgrades().len(), 1, "asked once per start");
    assert!(
        w.store().host_degraded(UPGRADE_CHECK).unwrap().is_some(),
        "the warning stays"
    );

    link.refuse_upgrades(None);
    w.store_mut()
        .config_changed(&owner(), "holder.socket", json!(null))
        .unwrap();
    let before = poller.source().received();
    poller.poll_once(at(42));
    assert_eq!(link.upgrades().len(), 2, "asked again after config.changed");
    // The holder answers with `upgraded` on the watch, then restarts.
    received(&poller, before + 1);
    poller.poll_once(at(44));
    assert!(poller.source().connected());
    let upgraded = event_payloads(&w, "holder.upgraded");
    assert_eq!(upgraded.len(), 1, "{upgraded:?}");
    assert_eq!(upgraded[0]["from"], "0.1.0");
    assert_eq!(upgraded[0]["to"], "0.2.0");
    assert_eq!(upgraded[0]["instance"], link.instance());
    assert_eq!(w.store().host_degraded(UPGRADE_CHECK).unwrap(), None);
    poller.poll_once(at(46));
    assert_eq!(link.upgrades().len(), 2, "it runs the stable binary now");
}

/// An upgrade that keeps the version string, as a rebuild to a new store
/// path does, still clears a refusal once the holder runs the stable
/// binary: the re-exec keeps the instance and records no `holder.upgraded`.
#[test]
fn a_taken_upgrade_that_keeps_the_version_clears_a_refusal() {
    let mut w = World::new();
    let link = w.holder.clone();
    let stable = w.fleet.dir.path().join("stable-tiller");
    std::fs::write(&stable, "").unwrap();
    let stable = std::fs::canonicalize(stable).unwrap();
    link.set_next_version("0.1.0");
    link.refuse_upgrades(Some("preflight: the binary did not run"));
    let mut poller = poller(&w, &link, Some(stable.clone()));
    poller.poll_once(at(0));
    assert!(w.store().host_degraded(UPGRADE_CHECK).unwrap().is_some());

    link.refuse_upgrades(None);
    w.store_mut()
        .config_changed(&owner(), "holder.socket", json!(null))
        .unwrap();
    let instance = link.instance();
    let before = poller.source().received();
    poller.poll_once(at(42));
    assert_eq!(link.upgrades().len(), 2, "asked again after config.changed");
    received(&poller, before + 1);
    poller.poll_once(at(44));
    poller.poll_once(at(46));
    assert!(poller.source().connected());
    assert_eq!(link.instance(), instance, "a re-exec keeps the instance");
    assert!(event_payloads(&w, "holder.upgraded").is_empty());
    assert_eq!(w.store().host_degraded(UPGRADE_CHECK).unwrap(), None);
    assert_eq!(link.upgrades().len(), 2, "it runs the stable binary now");
}

/// A refusal does not outlive its cause: once the holder restarts from the
/// stable binary, the next check clears the warning without asking.
#[test]
fn a_refused_upgrade_clears_once_the_holder_runs_the_stable_binary() {
    let mut w = World::new();
    let link = w.holder.clone();
    let stable = w.fleet.dir.path().join("stable-tiller");
    std::fs::write(&stable, "").unwrap();
    let stable = std::fs::canonicalize(stable).unwrap();
    link.refuse_upgrades(Some("pane p1 is mid-subscribe"));
    let mut first = poller(&w, &link, Some(stable.clone()));
    first.poll_once(at(0));
    assert!(w.store().host_degraded(UPGRADE_CHECK).unwrap().is_some());

    link.restart();
    link.set_binary(Some(&stable));
    let mut second = poller(&w, &link, Some(stable.clone()));
    second.poll_once(at(10));
    assert!(second.source().connected());
    assert_eq!(
        link.upgrades(),
        std::slice::from_ref(&stable),
        "not asked again"
    );
    assert_eq!(w.store().host_degraded(UPGRADE_CHECK).unwrap(), None);
}

/// A new holder instance on the stable binary clears a refusal within the
/// same source run, and the reconnect sends no second upgrade request.
#[test]
fn a_refused_upgrade_clears_when_a_new_instance_runs_the_stable_binary() {
    let mut w = World::new();
    let link = w.holder.clone();
    let stable = w.fleet.dir.path().join("stable-tiller");
    std::fs::write(&stable, "").unwrap();
    let stable = std::fs::canonicalize(stable).unwrap();
    link.refuse_upgrades(Some("pane p1 is mid-subscribe"));
    let mut poller = poller(&w, &link, Some(stable.clone()));
    poller.poll_once(at(0));
    assert!(w.store().host_degraded(UPGRADE_CHECK).unwrap().is_some());

    // The old watch closes, and the next poll connects to the new instance.
    let before = poller.source().received();
    link.restart();
    received(&poller, before + 1);
    poller.poll_once(at(40));
    poller.poll_once(at(80));
    assert!(poller.source().connected());
    assert!(
        w.store().host_degraded(UPGRADE_CHECK).unwrap().is_some(),
        "a new instance on another binary keeps the warning"
    );

    let before = poller.source().received();
    link.restart();
    link.set_binary(Some(&stable));
    received(&poller, before + 1);
    poller.poll_once(at(120));
    poller.poll_once(at(160));
    assert!(poller.source().connected());
    assert_eq!(w.store().host_degraded(UPGRADE_CHECK).unwrap(), None);
    assert_eq!(
        link.upgrades(),
        std::slice::from_ref(&stable),
        "a reconnect sends no upgrade"
    );
}

/// A holder that already runs the stable binary, or one whose binary
/// cannot be read, is never asked.
#[test]
fn a_holder_on_the_stable_binary_is_not_asked_to_upgrade() {
    let mut w = World::new();
    let link = w.holder.clone();
    let stable = w.fleet.dir.path().join("stable-tiller");
    std::fs::write(&stable, "").unwrap();
    let stable = std::fs::canonicalize(stable).unwrap();
    link.set_binary(Some(&stable));
    let mut poller = poller(&w, &link, Some(stable));
    poller.poll_once(at(0));
    link.set_binary(None);
    w.store_mut()
        .config_changed(&owner(), "holder.socket", json!(null))
        .unwrap();
    poller.poll_once(at(2));
    assert!(link.upgrades().is_empty());
    assert_eq!(w.store().host_degraded(UPGRADE_CHECK).unwrap(), None);
}

/// The `panes` cache holds the holder's list: each pane with its
/// instance, the session its key names, its pid, label and exit.
#[test]
fn a_list_fills_the_panes_cache_and_runs_every_thirty_seconds() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    let (instance, bound, pid, label, state): (String, String, i64, String, String) = w
        .store()
        .conn()
        .query_row(
            "SELECT holder_instance, session_id, pid, label, state FROM panes WHERE pane_id = ?1",
            [pane.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(instance, link.instance());
    assert_eq!(bound, session);
    assert_eq!(
        pid,
        i64::from(link.panes().get(&pane).unwrap().unwrap().pid)
    );
    assert_eq!(label, "alpha resuming");
    assert_eq!(state, "running");

    let lists = || link.panes().calls().iter().filter(|c| *c == "list").count();
    assert_eq!(lists(), 1);
    for n in [2, 10, 28] {
        poller.poll_once(at(n));
    }
    assert_eq!(lists(), 1, "events, not polls, carry the changes");
    poller.poll_once(at(31));
    assert_eq!(lists(), 2, "the 30 s check");
}

/// Step 3 of the ladder: an exit is positive evidence. It is recorded
/// once, as `holder:exited` with its code, and ends the session's
/// activity.
#[test]
fn an_exit_is_recorded_once_with_its_code() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    let before = poller.source().received();
    assert!(link.panes().exit(&pane, Some(3), None));
    received(&poller, before + 1);
    poller.poll_once(at(2));
    poller.poll_once(at(4));
    assert_eq!(
        holder_events(&w, &session),
        ["holder:working", "holder:exited"]
    );
    assert_eq!(activity(&w, &session), Activity::Ended);
    let payload: String = w
        .store()
        .conn()
        .query_row(
            "SELECT payload FROM observations WHERE event = 'holder:exited'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["exit_code"], 3);
    assert_eq!(payload["pane_id"], pane.as_str());
    let state: String = w
        .store()
        .conn()
        .query_row(
            "SELECT state FROM panes WHERE pane_id = ?1",
            [pane.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "exited");
}

/// The holder's `notify` and `model_reset` are recorded as they come, with
/// no verdict: neither says what the agent is doing.
#[test]
fn notify_and_model_reset_are_recorded_with_no_verdict() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    emit(
        &poller,
        &link,
        &Event::Notify {
            pane: pane.clone(),
            title: "Claude".into(),
            body: "needs your attention".into(),
        },
    );
    emit(&poller, &link, &Event::ModelReset { pane: pane.clone() });
    poller.poll_once(at(2));
    let rows: Vec<(String, Option<String>, String)> = w
        .store()
        .conn()
        .prepare(
            "SELECT event, verdict, payload FROM observations
              WHERE session_id = ?1 AND source = 'holder' AND event != 'holder:working'
              ORDER BY rowid",
        )
        .unwrap()
        .query_map([&session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let events: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|(e, v, _)| (e.as_str(), v.as_deref()))
        .collect();
    assert_eq!(
        events,
        [("holder:notify", None), ("holder:model_reset", None)]
    );
    let notify: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(notify["body"], "needs your attention");
    assert_eq!(activity(&w, &session), Activity::Working);
}

/// Each `quiet` reads the visible screen: Busy is working, Empty is idle,
/// a dialog is blocked, and a draft or an unknown screen says nothing.
#[test]
fn a_quiet_screen_is_classified() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    let mut n = 0;
    for screen in ["permission-dialog", "draft", "empty", "empty", "working"] {
        n += 2;
        show(&link, &pane, screen);
        quiet(&poller, &link, &pane);
        poller.poll_once(at(n));
    }
    assert_eq!(
        holder_events(&w, &session),
        [
            "holder:working",
            "holder:blocked",
            "holder:idle",
            "holder:working"
        ],
        "a draft says nothing, and an unchanged idle is not sent again"
    );
    assert_eq!(activity(&w, &session), Activity::Working);
}

/// A new holder instance (R21) is not a gone pane until the restore has
/// recorded it: the old instance's panes are not in its list, but no
/// `pane_gone` is reported for them.
#[test]
fn a_new_instance_reports_no_pane_gone_before_its_restore() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    let old = link.instance();
    let before = poller.source().received();
    link.restart();
    received(&poller, before + 1);
    poller.poll_once(at(2));
    poller.poll_once(at(4));
    assert!(poller.source().connected());
    assert_ne!(link.instance(), old);
    assert_eq!(holder_events(&w, &session), ["holder:working"]);
    assert_eq!(
        tiller_core::state::projects::meta(w.store().conn(), "holder_instance")
            .unwrap()
            .as_deref(),
        Some(old.as_str()),
        "R21 writes the new instance last"
    );
}

/// `protocol.md` 4.5: a `holder:idle` is held while a hook `working` is
/// under 30 s old, and lands once it is older. The screen keeps reading
/// idle, so the source reads it again when the hold ends and sends it
/// again: a lost Stop hook never leaves the session busy forever.
#[test]
fn a_held_holder_idle_lands_once_the_hold_ends() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (worker, pane) = w.holder_worker();
    show(&link, &pane, "working");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    // The turn's hooks say working; its Stop hook is lost.
    hook(&mut w, &worker, "PreToolUse", Activity::Working, 1);
    show(&link, &pane, "empty");
    quiet(&poller, &link, &pane);
    for n in [5, 10, 20, 30] {
        poller.poll_once(at(n));
        assert_eq!(activity(&w, &worker), Activity::Working, "held at {n} s");
    }
    poller.poll_once(at(32));
    assert_eq!(
        activity(&w, &worker),
        Activity::Idle,
        "the hold ended at 31 s"
    );
    for n in [40, 120, 600] {
        poller.poll_once(at(n));
    }
    assert_eq!(activity(&w, &worker), Activity::Idle);
    assert_eq!(
        holder_events(&w, &worker),
        ["holder:working", "holder:idle", "holder:idle"],
        "the held reading is sent again once, when the hold ends"
    );
}

/// A hook `working` that keeps coming holds the idle again; once the hooks
/// go quiet for 30 s the idle lands. A reading that changes in the
/// meantime drops the resend.
#[test]
fn a_held_holder_idle_waits_for_each_newer_hook_working() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (worker, pane) = w.holder_worker();
    let mut poller = poller(&w, &link, None);
    hook(&mut w, &worker, "PreToolUse", Activity::Working, 0);
    show(&link, &pane, "empty");
    poller.poll_once(at(2));
    hook(&mut w, &worker, "PreToolUse", Activity::Working, 20);
    poller.poll_once(at(31));
    assert_eq!(
        activity(&w, &worker),
        Activity::Working,
        "held by the hook at 20 s"
    );
    poller.poll_once(at(51));
    assert_eq!(activity(&w, &worker), Activity::Idle);

    // A new turn: held again, then the screen reads working before the
    // hold ends, and nothing is resent.
    hook(&mut w, &worker, "PreToolUse", Activity::Working, 60);
    show(&link, &pane, "working");
    quiet(&poller, &link, &pane);
    poller.poll_once(at(61));
    hook(&mut w, &worker, "PreToolUse", Activity::Working, 62);
    show(&link, &pane, "empty");
    quiet(&poller, &link, &pane);
    poller.poll_once(at(63));
    assert_eq!(
        activity(&w, &worker),
        Activity::Working,
        "held by the hook at 62 s"
    );
    show(&link, &pane, "working");
    quiet(&poller, &link, &pane);
    poller.poll_once(at(64));
    for n in [100, 200] {
        poller.poll_once(at(n));
    }
    assert_eq!(activity(&w, &worker), Activity::Working);
    assert_eq!(
        holder_events(&w, &worker),
        [
            "holder:idle",
            "holder:idle",
            "holder:idle",
            "holder:working",
            "holder:idle",
            "holder:working"
        ]
    );
}

/// The resend is owed only while the hook `working` that held the idle is
/// still the session's activity. A permission prompt or an error the hooks
/// report during the hold is newer than the screen's idle: the resend at
/// the end of the hold must not replace it.
#[test]
fn a_held_holder_idle_is_not_resent_over_a_newer_hook_verdict() {
    for (event, newer) in [
        ("PermissionRequest", Activity::WaitingPermission),
        ("StopFailure", Activity::Errored),
    ] {
        let mut w = World::new();
        let link = w.holder.clone();
        let (worker, pane) = w.holder_worker();
        let mut poller = poller(&w, &link, None);
        hook(&mut w, &worker, "PreToolUse", Activity::Working, 1);
        show(&link, &pane, "empty");
        poller.poll_once(at(5));
        assert_eq!(activity(&w, &worker), Activity::Working, "held");
        hook(&mut w, &worker, event, newer, 10);
        for n in [32, 40, 120] {
            poller.poll_once(at(n));
            assert_eq!(activity(&w, &worker), newer, "{event} kept at {n} s");
        }
        assert_eq!(holder_events(&w, &worker), ["holder:idle"], "{event}");
        assert!(poller.source().memory().held.is_empty(), "{event}");
    }
}

/// A pane that another session binds between two polls starts afresh: the
/// new session's first reading is recorded though the screen did not
/// change, and its pane's exit is reported.
#[test]
fn a_pane_bound_to_a_new_holder_session_between_polls_starts_afresh() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (first, pane) = w.holder_worker();
    show(&link, &pane, "empty");
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    assert_eq!(holder_events(&w, &first), ["holder:idle"]);

    w.store_mut()
        .session_end(&Ctx::new(Actor::Daemon), &first, "test")
        .unwrap();
    let second = w.worker("w9:p1");
    w.fleet.workers.retain(|(s, _, _)| *s != second);
    w.fleet.bind(&second, pane.as_str());
    poller.poll_once(at(2));
    assert_eq!(
        holder_events(&w, &second),
        ["holder:idle"],
        "the new session's first reading"
    );
    let before = poller.source().received();
    link.close(&pane, Duration::ZERO).unwrap();
    received(&poller, before + 1);
    poller.poll_once(at(4));
    assert_eq!(holder_events(&w, &second), ["holder:idle", "holder:exited"]);
}

/// The per-pane memory covers only the panes bound now: a session that
/// ended leaves nothing behind.
#[test]
fn holder_pane_memory_is_pruned_to_the_bound_panes() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (gone, gone_pane) = w.holder_worker();
    let (held, held_pane) = w.holder_worker();
    show(&link, &gone_pane, "empty");
    show(&link, &held_pane, "empty");
    let mut poller = poller(&w, &link, None);
    hook(&mut w, &held, "PreToolUse", Activity::Working, 0);
    poller.poll_once(at(1));
    let before = poller.source().received();
    link.close(&gone_pane, Duration::ZERO).unwrap();
    link.forget(&gone_pane).unwrap();
    received(&poller, before + 2);
    poller.poll_once(at(2));
    let gone_key = (gone_pane.as_str().to_owned(), gone.clone());
    let held_key = (held_pane.as_str().to_owned(), held.clone());
    let memory = poller.source().memory();
    assert_eq!(memory.gone, [gone_key]);
    assert_eq!(memory.statuses, std::slice::from_ref(&held_key));
    assert_eq!(memory.held, [held_key]);
    assert_eq!(activity(&w, &gone), Activity::Ended);

    for session in [&gone, &held] {
        w.store_mut()
            .session_end(&Ctx::new(Actor::Daemon), session, "test")
            .unwrap();
    }
    poller.poll_once(at(3));
    assert_eq!(poller.source().memory(), Memory::default());
}

/// A pane the holder forgets, of the instance on record, is reported gone
/// once.
#[test]
fn a_forgotten_pane_of_the_recorded_instance_is_gone_once() {
    let mut w = World::new();
    let link = w.holder.clone();
    let (session, pane) = w.holder_worker();
    let mut poller = poller(&w, &link, None);
    poller.poll_once(at(0));
    let before = poller.source().received();
    link.close(&pane, Duration::ZERO).unwrap();
    link.forget(&pane).unwrap();
    received(&poller, before + 2);
    poller.poll_once(at(2));
    poller.poll_once(at(4));
    assert_eq!(holder_events(&w, &session), ["pane_gone"]);
    assert_eq!(activity(&w, &session), Activity::Ended);
    assert_eq!(
        w.count("SELECT count(*) FROM panes WHERE gone_at IS NOT NULL"),
        1,
        "the cache marks it gone"
    );
}
