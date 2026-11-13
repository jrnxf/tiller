//! Owner notifications (`protocol.md` 3.4, P36): a file-backed `state.db`, the
//! real notifier and a fake that records what it was asked to show.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use tiller_core::domain::{Attention, AttentionKind};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::list;

use tiller_delivery::notify::{Command, NEW_MINUTES, Note, Notifier, Notify};
use tiller_panes::testing::{Fleet, at, owner};

/// A notifier that records each note it was asked to send, and refuses
/// them all once told to.
#[derive(Clone, Default)]
struct Shown {
    notes: Arc<Mutex<Vec<String>>>,
    refuse: Arc<Mutex<bool>>,
}

impl Shown {
    fn refuse(&self) {
        *self.refuse.lock().unwrap() = true;
    }
}

impl Notify for Shown {
    fn notify(&self, note: &Note) -> Result<(), String> {
        if *self.refuse.lock().unwrap() {
            return Err("notify refused by the fake".to_owned());
        }
        self.notes
            .lock()
            .unwrap()
            .push(format!("notify {} {}", note.title, note.message));
        Ok(())
    }
}

struct World {
    fleet: Fleet,
    shown: Shown,
    notifier: Notifier<Shown>,
    card: String,
}

impl World {
    fn new() -> Self {
        let mut fleet = Fleet::new();
        fleet.worker("w2:p1");
        let shown = Shown::default();
        let card = fleet.workers[0].1.clone();
        let card: String =
            tiller_core::state::get::<tiller_core::domain::Run>(fleet.store.conn(), "run", &card)
                .unwrap()
                .card_id;
        Self {
            notifier: Notifier::new(fleet.open(), shown.clone()),
            fleet,
            shown,
            card,
        }
    }

    fn open(&mut self, kind: AttentionKind, question: &str, now: i64) -> Attention {
        let ctx = tiller_core::Ctx::at(tiller_core::Actor::Owner, at(now));
        self.fleet
            .store
            .attention_open(
                &ctx,
                Some(question),
                NewAttention {
                    card: self.card.clone(),
                    kind,
                    question: question.to_owned(),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value
    }

    fn shown(&self) -> Vec<String> {
        self.shown.notes.lock().unwrap().clone()
    }
}

#[test]
fn new_attention_item_notifies_once() {
    let mut w = World::new();
    assert!(w.notifier.pass(at(1)).unwrap().notified.is_empty());
    let decision = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    let merge = w.open(AttentionKind::Merge, "PR 7 is ready", 3);

    let report = w.notifier.pass(at(4)).unwrap();
    assert_eq!(report.notified, vec![decision.id.clone(), merge.id.clone()]);
    assert_eq!(w.shown().len(), 2);
    assert!(w.shown()[0].contains("needs a decision"), "{:?}", w.shown());
    assert!(w.shown()[1].contains("ready for your merge"));

    // A second pass, and a fresh notifier as after a daemon restart, stay quiet.
    assert!(w.notifier.pass(at(5)).unwrap().notified.is_empty());
    let mut restarted = Notifier::new(w.fleet.open(), w.shown.clone());
    assert!(restarted.pass(at(6)).unwrap().notified.is_empty());
    assert_eq!(w.shown().len(), 2);

    // Each notification is recorded in an event on its item.
    let events: i64 = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'attention.notified'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
}

#[test]
fn routine_turns_and_settled_items_never_notify() {
    let mut w = World::new();
    // Nothing opened: nothing to say.
    assert!(w.notifier.pass(at(2)).unwrap().notified.is_empty());

    // An item answered before the pass needs nobody.
    let answered = w.open(AttentionKind::Decision, "Already answered?", 3);
    w.fleet
        .store
        .attention_withdraw(&owner(), &answered.id, "no longer applies")
        .unwrap();
    assert!(w.notifier.pass(at(4)).unwrap().notified.is_empty());

    // An item opened long ago is backlog, not news.
    let old = w.open(AttentionKind::Decision, "Old question?", 5);
    let later = 5 + NEW_MINUTES * 60 + 1;
    assert!(w.notifier.pass(at(later)).unwrap().notified.is_empty());
    let open: Vec<Attention> = list(
        w.fleet.store.conn(),
        "answered_at IS NULL AND withdrawn_at IS NULL",
        &[],
    )
    .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, old.id);
    assert!(w.shown().is_empty());
}

fn kinds(w: &World, item: &str) -> Vec<String> {
    let mut stmt = w
        .fleet
        .store
        .conn()
        .prepare("SELECT kind FROM events WHERE entity_id = ?1 ORDER BY seq")
        .unwrap();
    stmt.query_map([item], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_refused_send_is_recorded_once_and_never_retried() {
    let mut w = World::new();
    w.shown.refuse();
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);

    let report = w.notifier.pass(at(4)).unwrap();
    assert!(report.notified.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, item.id);
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_failed"
        ]
    );
    let error: String = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT json_extract(payload, '$.error') FROM events WHERE kind = 'attention.notify_failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(error.contains("refused by the fake"), "{error}");

    // At most once: a second pass and a restart neither send nor record again.
    let mut restarted = Notifier::new(w.fleet.open(), w.shown.clone());
    assert!(w.notifier.pass(at(5)).unwrap().failed.is_empty());
    assert!(restarted.pass(at(6)).unwrap().failed.is_empty());
    assert_eq!(kinds(&w, &item.id).len(), 3);
}

#[test]
fn a_sent_notification_writes_its_outcome_once() {
    let mut w = World::new();
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    assert_eq!(
        w.notifier.pass(at(4)).unwrap().notified,
        std::slice::from_ref(&item.id)
    );
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_sent"
        ]
    );

    // At most once: a second pass and a restart write nothing more, and a
    // late outcome call applies nothing on an item that has its outcome.
    let mut restarted = Notifier::new(w.fleet.open(), w.shown.clone());
    assert!(w.notifier.pass(at(5)).unwrap().notified.is_empty());
    assert!(restarted.pass(at(6)).unwrap().notified.is_empty());
    let ctx = tiller_core::Ctx::at(tiller_core::Actor::Daemon, at(7));
    assert!(
        w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    assert!(
        w.fleet
            .store
            .notification_fail(&ctx, &item.id, "late")
            .unwrap()
            .replayed
    );
    assert_eq!(kinds(&w, &item.id).len(), 3);
}

#[test]
fn a_record_alone_takes_one_outcome_and_only_one() {
    let mut w = World::new();
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    let ctx = tiller_core::Ctx::at(tiller_core::Actor::Daemon, at(4));
    // No record yet: an outcome applies nothing.
    assert!(
        w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    w.fleet.store.notification_record(&ctx, &item.id).unwrap();
    assert!(
        !w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    assert!(
        w.fleet
            .store
            .notification_fail(&ctx, &item.id, "late")
            .unwrap()
            .replayed
    );
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_sent"
        ]
    );
}

/// An outcome that cannot be written after a send does not end the pass:
/// the other new items are still notified in it.
#[test]
fn a_failed_outcome_write_does_not_stop_the_pass() {
    let mut w = World::new();
    let first = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    let second = w.open(AttentionKind::Decision, "Which release note?", 3);
    w.fleet
        .store
        .conn()
        .execute_batch(&format!(
            "CREATE TRIGGER fault BEFORE INSERT ON events
             WHEN NEW.kind = 'attention.notify_sent' AND NEW.entity_id = '{}'
             BEGIN SELECT RAISE(ABORT, 'disk fault'); END;",
            first.id
        ))
        .unwrap();

    let report = w.notifier.pass(at(4)).unwrap();
    assert_eq!(report.notified, [first.id.clone(), second.id.clone()]);
    assert_eq!(w.shown().len(), 2);
    assert_eq!(
        kinds(&w, &second.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_sent"
        ]
    );
}

/// A note as the notifier builds it.
fn note() -> Note {
    Note {
        title: "alpha".into(),
        message: "Decision: ship it?".into(),
        session_id: Some("sess-1".into()),
        pane: Some("h1".into()),
        cwd: None,
        host: "laptop".into(),
    }
}

/// A `sh -c` command that runs `body`.
fn shell(body: &str) -> Command {
    Command::new(vec!["sh".into(), "-c".into(), body.into()]).unwrap()
}

#[test]
fn the_notify_command_gets_the_note_on_stdin_and_the_pane_in_its_env() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let command = shell(&format!(
        "{{ cat; echo; echo \"$TILLER_SESSION_ID $TILLER_PANE $TILLER_HOST\"; }} > {}",
        out.display()
    ));
    command.notify(&note()).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let (payload, env) = text.trim_end().split_once('\n').unwrap();
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(payload["hook_event_name"], "Notification");
    assert_eq!(payload["title"], "alpha");
    assert_eq!(payload["message"], "Decision: ship it?");
    assert_eq!(payload["session_id"], "sess-1");
    assert_eq!(env, "sess-1 h1 laptop");
}

#[test]
fn a_notify_command_that_fails_or_hangs_is_a_failed_send() {
    let failed = shell("echo no display >&2; exit 3").notify(&note());
    let error = failed.unwrap_err();
    assert!(error.contains("no display"), "{error}");
    let hung = shell("sleep 5")
        .with_limit(std::time::Duration::from_millis(200))
        .notify(&note());
    assert!(hung.unwrap_err().contains("did not exit"));
    assert!(Command::new(Vec::new()).is_none());
}

/// A process the command leaves behind holds its stderr open, and the send
/// still ends within the limit.
#[test]
fn a_notify_command_that_leaves_a_process_behind_returns_within_the_limit() {
    let limit = std::time::Duration::from_secs(2);
    let started = std::time::Instant::now();
    shell("sleep 5 & exit 0")
        .with_limit(limit)
        .notify(&note())
        .unwrap();
    assert!(started.elapsed() < limit, "{:?}", started.elapsed());

    let started = std::time::Instant::now();
    let error = shell("echo no display >&2; sleep 5 & exit 3")
        .with_limit(limit)
        .notify(&note())
        .unwrap_err();
    assert!(started.elapsed() < limit, "{:?}", started.elapsed());
    assert!(error.contains("no display"), "{error}");
}
