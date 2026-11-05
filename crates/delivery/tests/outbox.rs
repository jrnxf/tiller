//! End-to-end delivery: a file-backed `state.db`, the real delivery pass and
//! herdr poller, and a fake herdr server whose panes draw Claude Code's
//! screens (`protocol.md` 3.3, `recovery.md` R06 and R09).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tiller_core::domain::{
    Attention, AttentionKind, Card, CardKind, Effort, OutboxKind, OutboxMessage, OutboxState,
    RunOutcome, RunRole,
};
use tiller_core::state::cards::NewCard;
use tiller_core::state::outbox::NewMessage;
use tiller_core::state::runs::{Bind, EndRun, NewOrchestrator, NewRun, Relaunch, SpawnChecks};
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_delivery::outbox::{
    Deliverer, Pacing, RINGS_MINUTES, Report, SUBMIT_PASSES, SUBMIT_PRESSES,
};
use tiller_herdr::client::Herdr;
use tiller_herdr::fake::{FakeHerdr, PAUSE_ENV, Screen};
use tiller_herdr::poller::{HerdrSource, Registry};
use tiller_herdr::testing::{Fleet, at, owner};

const CHILD_ENV: &str = "TILLER_DELIVERY_CHILD";

/// A fleet with one worker in pane `w2:p1`, laid out in a fake herdr and
/// polled once so every pane carries its row tokens.
struct World {
    fleet: Fleet,
    herdr: FakeHerdr,
    worker: String,
    deliverer: Deliverer<FakeHerdr>,
}

impl World {
    fn new() -> Self {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let world = Self {
            deliverer: Deliverer::new(fleet.open(), herdr.clone(), &fleet.host).pacing(Pacing {
                settle: Duration::ZERO,
                recheck: Duration::ZERO,
            }),
            fleet,
            herdr,
            worker,
        };
        world.poll(at(0));
        world
    }

    /// One herdr poll: the pane cache, activity observations, tokens and
    /// layout.
    fn poll(&self, now: Timestamp) {
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(
                self.herdr.clone(),
                self.fleet.open(),
                &self.fleet.host,
            ))
            .unwrap();
        poller.poll_once(now);
        assert_eq!(poller.health().consecutive_failures, 0);
    }

    fn send(&mut self, key: Option<&str>, kind: OutboxKind, now: Timestamp) -> OutboxMessage {
        self.fleet
            .store
            .outbox_enqueue(
                &Ctx::at(Actor::Daemon, now),
                key,
                NewMessage {
                    target_session_id: self.worker.clone(),
                    kind,
                    body: "rebase onto main before you push".into(),
                    card_id: Some(self.card().id),
                    reply_to: None,
                },
            )
            .unwrap()
            .value
    }

    fn card(&self) -> Card {
        get(self.fleet.store.conn(), "card", &self.run().card_id).unwrap()
    }

    fn run(&self) -> tiller_core::domain::Run {
        get(self.fleet.store.conn(), "run", &self.fleet.workers[0].1).unwrap()
    }

    fn pass(&mut self, now: Timestamp) -> Report {
        let report = self.deliverer.pass(now).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report
    }

    fn message(&self, id: &str) -> OutboxMessage {
        get(self.fleet.store.conn(), "outbox message", id).unwrap()
    }

    fn submitted(&self) -> Vec<String> {
        self.herdr.fake_pane("w2:p1").submitted
    }

    fn typed(&self) -> usize {
        self.calls("send_text")
    }

    fn calls(&self, prefix: &str) -> usize {
        self.herdr
            .calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

fn line(message: &OutboxMessage, card: &Card) -> String {
    format!(
        "tiller inbox {} # {} {}",
        message.id, message.kind, card.display_id
    )
}

#[test]
fn idle_pane_gets_one_wake_line_naming_the_message() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let report = w.pass(at(2));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(w.submitted(), [line(&message, &w.card())]);
    let delivered = w.message(&message.id);
    assert_eq!(delivered.state, OutboxState::Delivered);
    assert_eq!(delivered.attempts, 1);
    assert_eq!(delivered.target_pane_id.as_deref(), Some("w2:p1"));
    assert_eq!(delivered.delivered_at, Some(at(2)));
    assert_eq!(delivered.next_attempt_at, Some(at(2).plus_minutes(2)));
    assert!(
        !w.herdr
            .calls()
            .iter()
            .any(|c| c.contains("rebase onto main")),
        "the body is never typed"
    );
}

/// The CP5 trial's stuck steer: a Claude relaunched in the same pane reads
/// the Enter after the line as a newline, so the line sat typed but never
/// submitted.
#[test]
fn enter_read_as_a_newline_is_pressed_again_until_the_line_submits() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.herdr.enter_as_newline("w2:p1", 2);
    let report = w.pass(at(2));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(w.typed(), 1);
    assert_eq!(w.calls("send_enter"), 3);
    let submitted = w.submitted();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].trim_end(), line(&message, &w.card()));
    assert_eq!(w.herdr.fake_pane("w2:p1").composer, "");
    assert_eq!(w.message(&message.id).state, OutboxState::Delivered);
}

#[test]
fn a_line_enter_never_submits_stays_pending_and_is_finished_not_retyped() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.herdr.enter_as_newline("w2:p1", SUBMIT_PRESSES + 1);
    let report = w.pass(at(2));
    assert!(report.rung.is_empty());
    assert_eq!(
        report.deferred,
        [(
            message.id.clone(),
            "wake line typed but not submitted".to_owned()
        )]
    );
    let held = w.message(&message.id);
    assert_eq!(held.state, OutboxState::Pending);
    assert_eq!(held.attempts, 0);
    assert_eq!(held.next_attempt_at, Some(at(32)));
    assert!(w.submitted().is_empty());

    let report = w.pass(at(32));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(
        w.typed(),
        1,
        "a held wake line is finished, never typed again"
    );
    assert_eq!(w.submitted().len(), 1);
    assert_eq!(w.message(&message.id).state, OutboxState::Delivered);
}

#[test]
fn a_line_that_never_submits_is_left_like_held_text_after_three_passes() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.herdr.enter_as_newline("w2:p1", 100);
    let mut now = 2;
    for _ in 0..SUBMIT_PASSES {
        let report = w.pass(at(now));
        assert_eq!(report.deferred[0].1, "wake line typed but not submitted");
        now += 30;
    }
    let pressed = w.calls("send_enter");
    assert_eq!(pressed, (SUBMIT_PASSES * SUBMIT_PRESSES) as usize);
    let report = w.pass(at(now));
    assert_eq!(report.deferred[0].1, "composer holds text");
    assert_eq!(w.calls("send_enter"), pressed, "no more Enters");
    assert_eq!(w.typed(), 1);
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
}

#[test]
fn a_wrapped_wake_line_is_finished_but_a_draft_starting_with_one_is_not() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    // A crash after typing left the line, wrapped across two rows.
    let wrapped = line(&message, &w.card()).replacen(" # ", "\n# ", 1);
    w.herdr.type_draft("w2:p1", &wrapped);
    assert_eq!(w.pass(at(2)).rung, std::slice::from_ref(&message.id));
    assert_eq!(w.typed(), 0);
    assert_eq!(w.submitted().len(), 1);

    let second = w.send(None, OutboxKind::Steer, at(3));
    let draft = format!("{} and also rebase first", line(&second, &w.card()));
    w.herdr.type_draft("w2:p1", &draft);
    let report = w.pass(at(4));
    assert_eq!(report.deferred[0].1, "composer holds text");
    assert_eq!(w.herdr.fake_pane("w2:p1").composer, draft);
    assert_eq!(
        w.calls("send_enter"),
        1,
        "the owner's draft is never submitted"
    );
}

/// Two panes, the first one never taking its Enter: the pass types both
/// lines, pauses once, and records the healthy pane without waiting out the
/// stuck one's presses, so one slow pane cannot hold up another.
#[test]
fn a_pane_that_never_submits_does_not_delay_another() {
    let mut fleet = Fleet::new();
    let stuck_session = fleet.worker("w2:p1");
    let healthy_session = fleet.worker("w4:p1");
    let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
    fleet.place(&herdr);
    let registry = Registry::new();
    let mut poller = registry
        .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
        .unwrap();
    poller.poll_once(at(0));

    let mut ids = Vec::new();
    for (session, run) in [
        (&stuck_session, &fleet.workers[0].1),
        (&healthy_session, &fleet.workers[1].1),
    ] {
        let card_id = get::<tiller_core::domain::Run>(fleet.store.conn(), "run", run)
            .unwrap()
            .card_id;
        let message = fleet
            .store
            .outbox_enqueue(
                &Ctx::at(Actor::Daemon, at(1)),
                None,
                NewMessage {
                    target_session_id: session.clone(),
                    kind: OutboxKind::Steer,
                    body: "rebase onto main before you push".into(),
                    card_id: Some(card_id),
                    reply_to: None,
                },
            )
            .unwrap()
            .value;
        ids.push(message.id);
    }
    herdr.enter_as_newline("w2:p1", 100);

    let mut deliverer = Deliverer::new(fleet.open(), herdr.clone(), &fleet.host).pacing(Pacing {
        settle: Duration::ZERO,
        recheck: Duration::ZERO,
    });
    let report = deliverer.pass(at(2)).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rung, [ids[1].clone()], "{report:?}");

    let calls = herdr.calls();
    let position = |prefix: &str, last: bool| {
        let mut found = calls
            .iter()
            .enumerate()
            .filter(|(_, c)| c.starts_with(prefix));
        if last {
            found.next_back()
        } else {
            found.next()
        }
        .map(|(i, _)| i)
    };
    assert!(
        position("send_text w4:p1", false) < position("send_enter w2:p1", false),
        "every line is typed before the first pane is pressed: {calls:?}"
    );
    let healthy_done = position("send_enter w4:p1", true).unwrap();
    let stuck_last = position("send_enter w2:p1", true).unwrap();
    assert!(
        healthy_done < stuck_last,
        "the healthy pane is finished while the stuck one is still pressed: {calls:?}"
    );
    let state = |id: &str| get::<OutboxMessage>(fleet.store.conn(), "outbox message", id).unwrap();
    assert_eq!(state(&ids[1]).state, OutboxState::Delivered);
    assert_eq!(state(&ids[0]).state, OutboxState::Pending);
}

#[test]
fn one_line_per_batch_names_the_oldest() {
    let mut w = World::new();
    let first = w.send(None, OutboxKind::Steer, at(1));
    let second = w.send(None, OutboxKind::Note, at(2));
    let third = w.send(None, OutboxKind::Wake, at(3));
    let report = w.pass(at(4));
    assert_eq!(report.rung.len(), 3);
    assert_eq!(w.submitted(), [line(&first, &w.card())]);
    for id in [&first.id, &second.id, &third.id] {
        assert_eq!(w.message(id).state, OutboxState::Delivered);
    }
}

#[test]
fn busy_pane_and_dialog_defer_not_fail() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));

    w.herdr.set_screen("w2:p1", Screen::Working);
    w.poll(at(2));
    let report = w.pass(at(3));
    assert_eq!(report.deferred, [(message.id.clone(), "busy".to_owned())]);
    let deferred = w.message(&message.id);
    assert_eq!(deferred.state, OutboxState::Pending);
    assert_eq!(deferred.next_attempt_at, Some(at(33)));
    assert!(
        w.pass(at(20)).deferred.is_empty(),
        "not due again before 30 s"
    );

    w.herdr.set_screen("w2:p1", Screen::Permission);
    w.poll(at(34));
    let report = w.pass(at(35));
    assert_eq!(report.deferred[0].1, "waiting on a prompt");
    assert_eq!(w.message(&message.id).next_attempt_at, Some(at(95)));
    assert_eq!(
        w.typed(),
        0,
        "nothing is typed into a busy pane or a dialog"
    );
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);

    w.herdr.set_screen("w2:p1", Screen::Idle);
    w.poll(at(100));
    let report = w.pass(at(101));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(w.typed(), 1);
    assert_eq!(w.message(&message.id).state, OutboxState::Delivered);
}

#[test]
fn owner_draft_and_unreadable_screens_are_never_typed_into() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));

    w.herdr.type_draft("w2:p1", "half typed thought");
    let report = w.pass(at(2));
    assert_eq!(report.deferred[0].1, "composer holds text");
    assert_eq!(w.herdr.fake_pane("w2:p1").composer, "half typed thought");

    // Claude exited: a bare shell prompt is never an empty composer.
    w.herdr.set_screen("w2:p1", Screen::Shell);
    let report = w.pass(at(40));
    assert_eq!(report.deferred[0].1, "composer unreadable");
    assert_eq!(w.typed(), 0);
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
}

#[test]
fn composer_held_for_ten_minutes_is_reported_like_a_missed_doorbell() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.herdr
        .type_draft("w2:p1", "draft the owner walked away from");
    w.pass(at(2));
    for n in 1..=20 {
        w.pass(at(2 + 31 * n));
    }
    let notes: Vec<OutboxMessage> = list(
        w.fleet.store.conn(),
        "target_session_id = ?1 AND kind = 'note'",
        &[&w.fleet.orchestrator_session],
    )
    .unwrap();
    assert_eq!(notes.len(), 1, "one note, however many passes");
    assert!(notes[0].body.contains(tiller_core::ids::short(&message.id)));
    assert_eq!(
        w.herdr.fake_pane("w2:p1").composer,
        "draft the owner walked away from"
    );
}

#[test]
fn pane_naming_another_session_is_never_typed_into() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let mut tokens = w.herdr.fake_pane("w2:p1").tokens;
    tokens.insert("session".into(), w.fleet.orchestrator_session.clone());
    w.herdr.set_tokens("w2:p1", tokens);
    let report = w.pass(at(2));
    assert!(report.deferred[0].1.contains("hosts session"));

    w.herdr.set_tokens("w2:p1", Default::default());
    let report = w.pass(at(40));
    assert!(report.deferred[0].1.contains("does not name its session"));
    assert_eq!(w.typed(), 0);
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);

    // The next herdr poll re-reports the tokens; the line goes out.
    w.poll(at(70));
    assert_eq!(w.pass(at(71)).rung, std::slice::from_ref(&message.id));
}

#[test]
fn idle_pane_with_unread_message_is_rung_again() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.pass(at(2));
    // The worker took the line, worked, and went idle without running it.
    w.herdr.set_screen("w2:p1", Screen::Working);
    w.poll(at(10));
    w.herdr.set_screen("w2:p1", Screen::Idle);
    w.poll(at(20));

    assert!(w.pass(at(60)).rung.is_empty(), "not before two minutes");
    // Due at 122 s, but idle only since 20 s: the ring waits for two idle
    // minutes.
    let report = w.pass(at(122));
    assert_eq!(report.deferred[0].1, "not idle");
    let report = w.pass(at(152));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    let rung = w.message(&message.id);
    assert_eq!(rung.state, OutboxState::Delivered);
    assert_eq!(rung.attempts, 2);
    assert_eq!(
        rung.delivered_at,
        Some(at(2)),
        "delivered_at keeps the first delivery"
    );
    assert_eq!(w.submitted().len(), 2);
    assert!(
        w.submitted()
            .iter()
            .all(|l| *l == line(&message, &w.card()))
    );

    // Mid-turn when the next ring is due: it waits for idle.
    w.herdr.set_screen("w2:p1", Screen::Working);
    w.poll(at(300));
    let report = w.pass(at(302));
    assert!(report.rung.is_empty());
    assert_eq!(report.deferred[0].1, "not idle");

    // Acked: no more rings.
    let ctx = Ctx::at(Actor::Session(w.worker.clone()), at(310));
    w.fleet.store.outbox_ack(&ctx, &message.id).unwrap();
    w.herdr.set_screen("w2:p1", Screen::Idle);
    w.poll(at(311));
    assert!(w.pass(at(4000)).rung.is_empty());
    assert_eq!(w.submitted().len(), 2);
}

#[test]
fn worker_message_unread_after_fifth_ring_notes_the_orchestrator() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.pass(at(2));
    let mut now = 2;
    for _ in 0..5 {
        now = (w.message(&message.id).next_attempt_at.unwrap().as_millis() - at(0).as_millis())
            / 1000;
        w.pass(at(now));
    }
    assert_eq!(w.message(&message.id).attempts, 6);
    let notes = |w: &World| -> Vec<OutboxMessage> {
        list(
            w.fleet.store.conn(),
            "target_session_id = ?1 AND kind = 'note'",
            &[&w.fleet.orchestrator_session],
        )
        .unwrap()
    };
    assert!(notes(&w).is_empty());
    w.pass(at(now + 3600));
    assert_eq!(notes(&w).len(), 1);
    w.pass(at(now + 7200));
    assert_eq!(notes(&w).len(), 1, "reported once");
}

#[test]
fn message_to_dead_session_is_retargeted_to_successor() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));

    // The pane is gone: deliveries wait, they never fail.
    w.herdr.remove_pane("w2:p1");
    w.poll(at(2));
    let report = w.pass(at(3));
    assert_eq!(report.deferred[0].1, "pane is gone");

    // The session ends; the run is still active, so the message waits.
    let hook = Ctx::at(Actor::Hook, at(4));
    w.fleet
        .store
        .session_end(&hook, &w.worker, "pane_gone")
        .unwrap();
    let report = w.pass(at(40));
    assert!(report.failed.is_empty() && report.retargeted.is_empty());
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);

    // The orchestrator relaunches the run in a new pane.
    let run = w.fleet.workers[0].1.clone();
    let orchestrator = Ctx::at(Actor::Session(w.fleet.orchestrator_session.clone()), at(50));
    let successor = w
        .fleet
        .store
        .run_relaunch(
            &orchestrator,
            &run,
            Relaunch {
                note: "pane closed".into(),
                new_attempt: false,
                stop_proof: None,
                model: None,
                checks: None,
            },
        )
        .unwrap()
        .value
        .session
        .id;
    w.herdr.add_workspace("w4", "claude");
    w.herdr.add_pane("w4", "w4:p1", Screen::Idle);
    w.fleet
        .store
        .run_bind_session(
            &Ctx::at(Actor::Hook, at(55)),
            Bind {
                session_id: successor.clone(),
                claude_session_id: "claude-successor".into(),
                pane_id: Some("w4:p1".into()),
                transcript_path: None,
                source: "startup".into(),
            },
        )
        .unwrap();
    w.poll(at(56));

    // A message waiting for a successor is looked at again 30 s on.
    assert!(w.pass(at(60)).retargeted.is_empty());
    let report = w.pass(at(70));
    assert_eq!(report.retargeted, std::slice::from_ref(&message.id));
    let moved = w.message(&message.id);
    assert_eq!(moved.target_session_id, successor);
    assert_eq!(moved.state, OutboxState::Pending);
    let report = w.pass(at(71));
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(
        w.herdr.fake_pane("w4:p1").submitted,
        [line(&message, &w.card())]
    );
    // The successor's doorbell starts at its own first ring.
    assert_eq!(
        w.message(&message.id).next_attempt_at,
        Some(at(71).plus_minutes(RINGS_MINUTES[0]))
    );
}

#[test]
fn failed_answer_opens_recovery_item() {
    let mut w = World::new();
    let answer = w.send(None, OutboxKind::Answer, at(1));
    let hook = Ctx::at(Actor::Hook, at(2));
    w.fleet
        .store
        .session_end(&hook, &w.worker, "exited")
        .unwrap();
    let run = w.fleet.workers[0].1.clone();
    w.fleet
        .store
        .run_end(
            &owner(),
            &run,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: Some("superseded".into()),
            },
        )
        .unwrap();
    let report = w.pass(at(3));
    assert_eq!(report.failed, std::slice::from_ref(&answer.id));
    let failed = w.message(&answer.id);
    assert_eq!(failed.state, OutboxState::Failed);
    assert!(failed.failure_reason.unwrap().contains("run has ended"));
    let items: Vec<Attention> = list(
        w.fleet.store.conn(),
        "card_id = ?1 AND kind = 'recovery'",
        &[&w.card().id],
    )
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Recovery);
}

#[test]
fn answer_unread_an_hour_after_delivery_goes_to_the_owner() {
    let mut w = World::new();
    let answer = w.send(None, OutboxKind::Answer, at(1));
    w.pass(at(2));
    for minutes in [3, 6, 16, 31, 59] {
        w.pass(at(2).plus_minutes(minutes));
    }
    let recovery = |w: &World| -> usize {
        list::<Attention>(
            w.fleet.store.conn(),
            "card_id = ?1 AND kind = 'recovery'",
            &[&w.card().id],
        )
        .unwrap()
        .len()
    };
    assert_eq!(recovery(&w), 0);
    w.pass(at(2).plus_minutes(61));
    w.pass(at(2).plus_minutes(122));
    assert_eq!(recovery(&w), 1);
    assert_eq!(
        w.message(&answer.id).state,
        OutboxState::Delivered,
        "never failed silently"
    );
}

#[test]
fn duplicate_send_is_acked_once() {
    let mut w = World::new();
    let first = w.send(Some("steer:alpha-1:1"), OutboxKind::Steer, at(1));
    let again = w.send(Some("steer:alpha-1:1"), OutboxKind::Steer, at(2));
    assert_eq!(first.id, again.id, "a retried enqueue is the same message");

    // The daemon dies after herdr took the line and before it recorded it.
    let mut child = child_pass(&w, "send_enter.after:1", at(3));
    wait_and_kill(&mut child, &w.fleet.dir.path().join("paused"));
    assert_eq!(w.message(&first.id).state, OutboxState::Pending);
    assert_eq!(w.submitted().len(), 1);

    // The restarted daemon types the line a second time, as R06 allows.
    w.pass(at(10));
    assert_eq!(w.submitted().len(), 2);
    let delivered = w.message(&first.id);
    assert_eq!(delivered.state, OutboxState::Delivered);
    assert_eq!(delivered.attempts, 1);

    // The worker runs both lines; the message is acked once.
    let ctx = Ctx::at(Actor::Session(w.worker.clone()), at(11));
    let acked = w.fleet.store.outbox_ack(&ctx, &first.id).unwrap();
    assert!(!acked.replayed);
    let ctx = Ctx::at(Actor::Session(w.worker.clone()), at(12));
    let replay = w.fleet.store.outbox_ack(&ctx, &first.id).unwrap();
    assert!(replay.events.is_empty(), "the second ack writes nothing");
    let acks: i64 = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE entity_id = ?1 AND kind = 'outbox.acked'",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(acks, 1);
    let rows: i64 = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE kind = 'steer'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn crash_between_store_and_send_delivers_once() {
    // Killed before the first keystroke: the row stays pending and the
    // next pass sends it.
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let mut child = child_pass(&w, "send_text:1", at(2));
    wait_and_kill(&mut child, &w.fleet.dir.path().join("paused"));
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
    assert_eq!(w.typed(), 0);
    assert_eq!(w.herdr.fake_pane("w2:p1").composer, "");
    assert_eq!(w.pass(at(3)).rung, std::slice::from_ref(&message.id));
    assert_eq!(w.submitted(), [line(&message, &w.card())]);
    assert_eq!(w.typed(), 1);

    // Killed between typing the line and pressing Enter: the next pass
    // finds its own line in the composer and only presses Enter.
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let mut child = child_pass(&w, "send_text.after:1", at(2));
    wait_and_kill(&mut child, &w.fleet.dir.path().join("paused"));
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
    assert_eq!(
        w.herdr.fake_pane("w2:p1").composer,
        line(&message, &w.card())
    );
    assert_eq!(w.pass(at(3)).rung, std::slice::from_ref(&message.id));
    assert_eq!(w.typed(), 1, "the line is typed once");
    assert_eq!(w.submitted(), [line(&message, &w.card())]);
    let delivered = w.message(&message.id);
    assert_eq!(
        (delivered.state, delivered.attempts),
        (OutboxState::Delivered, 1)
    );
}

/// Runs one delivery pass in a child process that hangs at `pause`.
fn child_pass(w: &World, pause: &str, now: Timestamp) -> Child {
    let marker = w.fleet.dir.path().join("paused");
    let _ = std::fs::remove_file(&marker);
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_delivery_pass",
            "--nocapture",
            "--include-ignored",
        ])
        .env(
            CHILD_ENV,
            format!(
                "{}|{}|{}|{}",
                w.fleet.db.display(),
                w.herdr.path().display(),
                w.fleet.host,
                now.as_millis()
            ),
        )
        .env(PAUSE_ENV, format!("{pause}:{}", marker.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_and_kill(child: &mut Child, marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the child pass exited before the pause point: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "the child never reached the pause point"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

/// The child half of the crash tests; a no-op unless a parent set it up.
#[test]
#[ignore = "run by the crash tests in a child process"]
fn child_delivery_pass() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let parts: Vec<&str> = spec.split('|').collect();
    let store = Store::open(parts[0]).unwrap();
    let herdr = FakeHerdr::open(parts[1]);
    let now = Timestamp::from_millis(parts[3].parse().unwrap()).unwrap();
    Deliverer::new(store, herdr, parts[2]).pass(now).unwrap();
}

#[test]
fn herdr_down_defers_and_never_fails() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Answer, at(1));
    w.herdr.set_unreachable(true);
    for n in 0..10 {
        let report = w.pass(at(2 + 31 * n));
        assert!(report.failed.is_empty());
    }
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
    w.herdr.set_unreachable(false);
    assert_eq!(w.pass(at(400)).rung, std::slice::from_ref(&message.id));
    let _ = w.herdr.snapshot().unwrap();
}

#[test]
fn held_timer_restarts_once_the_composer_stops_holding_text() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    w.herdr.type_draft("w2:p1", "a draft");
    w.pass(at(2));
    // Claude exits and comes back with the draft still in the composer:
    // the draft has been held only since then.
    w.herdr.set_screen("w2:p1", Screen::Shell);
    w.pass(at(300));
    w.herdr.set_screen("w2:p1", Screen::Idle);
    let notes = |w: &World| -> usize {
        list::<OutboxMessage>(
            w.fleet.store.conn(),
            "target_session_id = ?1 AND kind = 'note'",
            &[&w.fleet.orchestrator_session],
        )
        .unwrap()
        .len()
    };
    let mut now = 331;
    while now < 331 + 600 {
        w.pass(at(now));
        now += 31;
    }
    assert_eq!(notes(&w), 0, "held for under ten minutes");
    w.pass(at(now));
    assert_eq!(notes(&w), 1);
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
}

/// herdr as the real one behaves when the target's `UserPromptSubmit` hook
/// acks the batch before `send-keys Enter` returns.
struct AckOnSubmit {
    inner: FakeHerdr,
    store: std::cell::RefCell<Store>,
    session: String,
    message: String,
}

impl Herdr for AckOnSubmit {
    fn snapshot(&self) -> tiller_herdr::client::Result<tiller_herdr::client::Snapshot> {
        self.inner.snapshot()
    }
    fn pane(
        &self,
        id: &str,
    ) -> tiller_herdr::client::Result<Option<tiller_herdr::client::PaneInfo>> {
        self.inner.pane(id)
    }
    fn read_visible(&self, id: &str) -> tiller_herdr::client::Result<String> {
        self.inner.read_visible(id)
    }
    fn send_text(&self, id: &str, text: &str) -> tiller_herdr::client::Result<()> {
        self.inner.send_text(id, text)
    }
    fn send_enter(&self, id: &str) -> tiller_herdr::client::Result<()> {
        self.inner.send_enter(id)?;
        let ctx = Ctx::at(Actor::Session(self.session.clone()), at(2));
        self.store
            .borrow_mut()
            .outbox_ack(&ctx, &self.message)
            .unwrap();
        Ok(())
    }
    fn send_escape(&self, id: &str) -> tiller_herdr::client::Result<()> {
        self.inner.send_escape(id)
    }
    fn report_tokens(
        &self,
        id: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> tiller_herdr::client::Result<()> {
        self.inner.report_tokens(id, tokens)
    }
    fn create_workspace(
        &self,
        label: &str,
        cwd: Option<&Path>,
    ) -> tiller_herdr::client::Result<tiller_herdr::client::Created> {
        self.inner.create_workspace(label, cwd)
    }
    fn rename_workspace(&self, id: &str, label: &str) -> tiller_herdr::client::Result<()> {
        self.inner.rename_workspace(id, label)
    }
    fn move_workspace(&self, id: &str, index: usize) -> tiller_herdr::client::Result<()> {
        self.inner.move_workspace(id, index)
    }
    fn close_pane(&self, id: &str) -> tiller_herdr::client::Result<()> {
        self.inner.close_pane(id)
    }
    fn notify(&self, title: &str, body: &str) -> tiller_herdr::client::Result<()> {
        self.inner.notify(title, body)
    }
}

#[test]
fn an_ack_that_beats_the_delivery_record_is_not_an_error() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let herdr = AckOnSubmit {
        inner: w.herdr.clone(),
        store: std::cell::RefCell::new(w.fleet.open()),
        session: w.worker.clone(),
        message: message.id.clone(),
    };
    let mut deliverer = Deliverer::new(w.fleet.open(), herdr, &w.fleet.host);
    let report = deliverer.pass(at(2)).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rung, std::slice::from_ref(&message.id));
    assert_eq!(w.message(&message.id).state, OutboxState::Acked);
    assert_eq!(w.submitted().len(), 1);
}

#[test]
fn unread_note_waits_for_a_live_orchestrator() {
    let mut w = World::new();
    let message = w.send(None, OutboxKind::Steer, at(1));
    let hook = Ctx::at(Actor::Hook, at(1));
    let orchestrator = w.fleet.orchestrator_session.clone();
    w.fleet
        .store
        .session_end(&hook, &orchestrator, "exited")
        .unwrap();
    w.pass(at(2));
    let mut now = 2;
    for _ in 0..5 {
        now = (w.message(&message.id).next_attempt_at.unwrap().as_millis() - at(0).as_millis())
            / 1000;
        w.pass(at(now));
    }
    let report = w.pass(at(now + 3600));
    assert!(report.reported.is_empty(), "no one to tell yet");

    let next = w
        .fleet
        .store
        .session_start(
            &Ctx::at(Actor::Owner, at(now + 3601)),
            NewOrchestrator {
                project_id: "alpha".into(),
                pane_id: Some("w1:p1".into()),
                claude_session_id: Some("claude-orchestrator-2".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session
        .id;
    let report = w.pass(at(now + 7200));
    assert_eq!(report.reported, std::slice::from_ref(&message.id));
    let notes: Vec<OutboxMessage> = list(
        w.fleet.store.conn(),
        "target_session_id = ?1 AND kind = 'note'",
        &[&next],
    )
    .unwrap();
    assert_eq!(notes.len(), 1);
}

#[test]
fn a_remote_lane_is_left_to_its_own_tiller() {
    let mut w = World::new();
    let remote = w.fleet.remote_host("m-1");
    let card = w
        .fleet
        .store
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: "remote work".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let session = w
        .fleet
        .store
        .run_start(
            &owner(),
            None,
            NewRun {
                worktree_path: Some("/home/user/worktrees/remote".into()),
                card: card.display_id,
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: Some(remote),
                branch: None,
                base_branch: None,
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value
        .session
        .id;
    let message = w
        .fleet
        .store
        .outbox_enqueue(
            &Ctx::at(Actor::Daemon, at(1)),
            None,
            NewMessage {
                target_session_id: session,
                kind: OutboxKind::Steer,
                body: "for the remote lane".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap()
        .value;
    let events = |w: &World| -> i64 {
        w.fleet
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE entity_id = ?1",
                [&message.id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = events(&w);
    let calls = w.herdr.calls().len();
    for n in 0..20 {
        let report = w.pass(at(2 + 2 * n));
        assert!(report.rung.is_empty() && report.deferred.is_empty() && report.failed.is_empty());
    }
    assert_eq!(events(&w), before, "nothing written for a remote lane");
    assert_eq!(w.herdr.calls().len(), calls, "herdr never asked");
    assert_eq!(w.message(&message.id).state, OutboxState::Pending);
}
