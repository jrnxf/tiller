//! The record of what a launch loaded (`memory.md` section 3): `mem.load`
//! writes `session_notes`, one row per kb note the launch rendered, and
//! copies the newest set on a resume, `/clear` or `/compact`.
//!
//! Nothing here reads the kb store. The launcher renders the notes
//! ([`crate::kb::render`]); this module records which ones a session got.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::get;
use crate::domain::{Run, Session};
use crate::error::{Error, Result};
use crate::kb::Rendered;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// One note a session loaded: its kb id and the SHA-256 of its rendered
/// section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedNote {
    pub id: String,
    pub sha256: String,
}

/// What `mem.load` recorded for one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loaded {
    pub session_id: String,
    pub trigger: String,
    pub loaded_at: Timestamp,
    /// The notes, in the order they were rendered or copied.
    pub notes: Vec<LoadedNote>,
}

/// What one [`Store::mem_load_owed`] pass did.
#[derive(Debug)]
pub struct CopyPass {
    /// The copies made, in observation order.
    pub made: Vec<Loaded>,
    /// The copies that failed, in observation order. The pass went on past
    /// each one.
    pub failed: Vec<FailedCopy>,
    /// The `since` of the next pass.
    pub mark: CopyMark,
}

/// Where the next [`Store::mem_load_owed`] pass reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyMark {
    /// The pass's time (`ctx.now`), or the `recorded_at` of the earliest
    /// observation whose copy failed and is not [`FailedCopy::given_up`],
    /// when that is earlier. Every observation committed after the pass
    /// read is stamped later than the pass's time, less
    /// [`MARK_SLACK_MINUTES`], and a failed copy is read, and tried, again
    /// on each pass until it is given up.
    pub read_from: Timestamp,
    /// The pass's time. Every copy the pass read past its window was made
    /// or given up then, so the next pass does not read it again.
    pub passed_at: Timestamp,
}

/// One copy a pass could not write.
#[derive(Debug)]
pub struct FailedCopy {
    pub session_id: String,
    pub trigger: String,
    pub observed_at: Timestamp,
    pub error: Error,
    /// This was the copy's last try: its observation was recorded, or its
    /// session ended, [`COPY_WINDOW_MINUTES`] or more before the pass, so
    /// no later pass reads it again, and `session_notes` keeps no row for
    /// it.
    pub given_up: bool,
}

/// The `session_notes.trigger` values a `SessionStart` observation copies
/// the newest set under. The launch writes `startup` itself.
const COPY_TRIGGERS: &[&str] = &["resume", "clear", "compact"];

/// How long after its session ended a `SessionStart` observation still
/// gets its copy: the daemon may have been down when it came in. A failed
/// copy is tried again for as long after its observation was recorded.
const COPY_WINDOW_MINUTES: i64 = 60;

/// How far before the last pass's mark a pass still reads. An observation
/// is stamped (`recorded_at`, its ctx time) before its transaction takes
/// the write lock, so one stamped before the mark can commit after the
/// pass read.
const MARK_SLACK_MINUTES: i64 = 5;

/// One copy [`Store::owed_copies`] found: session, trigger, `observed_at`,
/// `recorded_at` and the session's `ended_at`.
type OwedCopy = (String, String, Timestamp, Timestamp, Option<Timestamp>);

/// The floor of a pass with no mark: before every observation.
const EPOCH: Timestamp = Timestamp::UNIX_EPOCH;

impl Store {
    /// `mem.load` (the launcher, once a launch's line went in): one
    /// `startup` row per note it rendered for the session, at this moment,
    /// and the `session.memory_loaded` event with the store's state, the
    /// notes, the ones it skipped and the hash of the rendered section. A
    /// launch that loaded nothing still writes its event, so Health reads
    /// the newest state of the store.
    pub fn mem_load(
        &mut self,
        ctx: &Ctx,
        session: &str,
        rendered: &Rendered,
    ) -> Result<Applied<Loaded>> {
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let at = tx.now();
            let notes: Vec<LoadedNote> = rendered
                .loaded()
                .map(|e| LoadedNote {
                    id: e.id.clone(),
                    sha256: e.sha256.clone(),
                })
                .collect();
            let written = insert(tx, &session.id, "startup", at, &notes)?;
            let skipped: Vec<_> = rendered
                .skipped()
                .map(|e| json!({"id": e.id, "reason": e.reason}))
                .collect();
            let card = card_of(tx, &session)?;
            tx.event(
                NewEvent::new("session", &session.id, "session.memory_loaded")
                    .card_opt(card)
                    .payload(json!({
                        "trigger": "startup",
                        "kb": rendered.kb.as_str(),
                        "notes": written,
                        "skipped": skipped,
                        "hash": rendered.hash,
                    })),
            )?;
            Ok(Loaded {
                session_id: session.id,
                trigger: "startup".to_owned(),
                loaded_at: at,
                notes: written,
            })
        })
    }

    /// `mem.load` on a resume, `/clear` or `/compact` (the daemon, on the
    /// `SessionStart` observation, `protocol.md` 4.2): copies the session's
    /// newest set at `at` with the new trigger at `at`, so a copy made late
    /// copies the set the session had then, not one recorded after. It
    /// renders and hashes nothing, since the appended prompt is the one the
    /// session launched with. A session that loaded no note by `at` writes
    /// nothing and opens no transaction; a copy already recorded at `at`
    /// writes nothing either.
    pub fn mem_load_again(
        &mut self,
        ctx: &Ctx,
        session: &str,
        trigger: &str,
        at: Timestamp,
    ) -> Result<Applied<Loaded>> {
        if !COPY_TRIGGERS.contains(&trigger) {
            return Err(Error::Usage(format!(
                "a copied load is for resume, clear or compact, not {trigger}"
            )));
        }
        if set_as_of(self.conn(), session, Some(at))?.is_empty() {
            return Ok(nothing(session, trigger, at));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let notes = set_as_of(tx.conn(), &session.id, Some(at))?;
            let written = insert(tx, &session.id, trigger, at, &notes)?;
            if !written.is_empty() {
                let card = card_of(tx, &session)?;
                tx.event(
                    NewEvent::new("session", &session.id, "session.memory_loaded")
                        .card_opt(card)
                        .payload(json!({"trigger": trigger, "notes": written})),
                )?;
            }
            Ok(Loaded {
                session_id: session.id,
                trigger: trigger.to_owned(),
                loaded_at: at,
                notes: written,
            })
        })
    }

    /// The daemon's copy pass: makes each copy a `SessionStart` observation
    /// for resume, clear or compact owes. One is owed when the hook
    /// recorded it for a session that had loaded memory by its time, live
    /// or ended less than [`COPY_WINDOW_MINUTES`] ago, the hook bound the
    /// session (no `bind_refused` in its payload), and no row of its
    /// trigger is recorded at its time.
    ///
    /// `since` is the [`CopyPass::mark`] of the caller's last pass: only
    /// observations recorded after its `read_from`, less
    /// [`MARK_SLACK_MINUTES`], are read, so a pass does not grow with a long
    /// session's history. `None` reads every observation, as the first pass
    /// after a start does. A copy that fails is returned in
    /// [`CopyPass::failed`], the pass goes on to the next one, and the mark
    /// stays at or before it, so every later pass tries it again until it
    /// is made, or until [`COPY_WINDOW_MINUTES`] after its observation was
    /// recorded or its session ended. The first pass at or past that time
    /// gives it up ([`FailedCopy::given_up`]): it tries it a last time when
    /// the observation's hour passed, and not at all when the session's
    /// did, since a copy is owed only within an hour of the session's end.
    /// A copy given up holds no mark, so one that never goes in does not
    /// make every pass read a growing history, and the next pass does not
    /// read it again.
    pub fn mem_load_owed(&mut self, ctx: &Ctx, since: Option<CopyMark>) -> Result<CopyPass> {
        let owed = self.owed_copies(ctx.now, since)?;
        let window = ctx.now.plus_minutes(-COPY_WINDOW_MINUTES);
        let mut pass = CopyPass {
            made: Vec::new(),
            failed: Vec::new(),
            mark: CopyMark {
                read_from: ctx.now,
                passed_at: ctx.now,
            },
        };
        for (session, trigger, at, recorded_at, ended_at) in owed {
            if ended_at.is_some_and(|ended| ended <= window) {
                // Its session's hour passed since the last pass read it,
                // so it is not made late, and its drop is reported.
                pass.failed.push(FailedCopy {
                    session_id: session,
                    trigger,
                    observed_at: at,
                    error: Error::Internal(
                        "its session ended an hour ago, before the copy went in".to_owned(),
                    ),
                    given_up: true,
                });
                continue;
            }
            match self.mem_load_again(ctx, &session, &trigger, at) {
                Ok(copied) if copied.value.notes.is_empty() => {}
                Ok(copied) => pass.made.push(copied.value),
                Err(error) => {
                    // The next floor is the mark less the slack, so this
                    // observation is read again, until it is given up.
                    let given_up = recorded_at <= window;
                    if !given_up {
                        pass.mark.read_from = pass.mark.read_from.min(recorded_at);
                    }
                    pass.failed.push(FailedCopy {
                        session_id: session,
                        trigger,
                        observed_at: at,
                        error,
                        given_up,
                    });
                }
            }
        }
        Ok(pass)
    }

    /// The observations [`Self::mem_load_owed`] owes a copy at `now`, read
    /// from `since` less the slack: session, trigger, `observed_at`,
    /// `recorded_at` and the session's `ended_at`, in observation order.
    /// With a mark, a copy the last pass read past its window (made or
    /// given up then) is not read again; one that crossed its window since
    /// is read once more, so its drop is reported.
    fn owed_copies(&self, now: Timestamp, since: Option<CopyMark>) -> Result<Vec<OwedCopy>> {
        // With no mark every observation is read, so the planner, not the
        // `recorded_at` index, picks the scan.
        let (index, floor, cut) = match since {
            Some(mark) => {
                let cut = mark.passed_at.plus_minutes(-COPY_WINDOW_MINUTES);
                (
                    "INDEXED BY observations_recorded",
                    mark.read_from.plus_minutes(-MARK_SLACK_MINUTES).max(cut),
                    cut,
                )
            }
            None => ("", EPOCH, now.plus_minutes(-COPY_WINDOW_MINUTES)),
        };
        let events = COPY_TRIGGERS
            .iter()
            .map(|trigger| format!("'SessionStart:{trigger}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = self.conn().prepare_cached(&format!(
            "SELECT o.session_id, substr(o.event, length('SessionStart:') + 1), o.observed_at,
                        o.recorded_at, s.ended_at
                 FROM observations o {index}
                 JOIN sessions s ON s.id = o.session_id
                 WHERE o.recorded_at > ?2
                   AND (s.ended_at IS NULL OR s.ended_at > ?1)
                   AND EXISTS (SELECT 1 FROM session_notes l
                               WHERE l.session_id = s.id AND l.loaded_at <= o.observed_at)
                   AND o.source = 'hook'
                   AND o.event IN ({events})
                   AND json_extract(o.payload, '$.bind_refused') IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM session_notes l
                       WHERE l.session_id = o.session_id AND l.loaded_at = o.observed_at
                         AND l.trigger = substr(o.event, length('SessionStart:') + 1))
                 ORDER BY o.observed_at, o.id"
        ))?;
        let rows = stmt.query_map(params![cut, floor], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ids of the session's newest load set: the notes of its latest
    /// `session_notes` moment.
    pub fn mem_loaded(&self, session: &str) -> Result<Vec<String>> {
        Ok(set_as_of(self.conn(), session, None)?
            .into_iter()
            .map(|n| n.id)
            .collect())
    }
}

/// The card of the session's run, for the event's timeline.
fn card_of(tx: &Tx<'_>, session: &Session) -> Result<Option<String>> {
    Ok(match &session.run_id {
        Some(run) => Some(get::<Run>(tx.conn(), "run", run)?.card_id),
        None => None,
    })
}

/// A load that wrote nothing, with no transaction opened.
fn nothing(session: &str, trigger: &str, at: Timestamp) -> Applied<Loaded> {
    Applied {
        value: Loaded {
            session_id: session.to_owned(),
            trigger: trigger.to_owned(),
            loaded_at: at,
            notes: Vec::new(),
        },
        events: Vec::new(),
        replayed: false,
    }
}

/// The notes of the session's latest `session_notes` moment at or before
/// `at` (`None`: its latest), each once in the order first written: two
/// triggers can record the same set in one millisecond.
fn set_as_of(
    conn: &rusqlite::Connection,
    session: &str,
    at: Option<Timestamp>,
) -> Result<Vec<LoadedNote>> {
    let mut stmt = conn.prepare(
        "SELECT note_id, sha256 FROM session_notes
         WHERE session_id = ?1
           AND loaded_at = (SELECT max(loaded_at) FROM session_notes
                            WHERE session_id = ?1 AND (?2 IS NULL OR loaded_at <= ?2))
         GROUP BY note_id
         ORDER BY min(rowid)",
    )?;
    let rows = stmt.query_map(params![session, at], |row| {
        Ok(LoadedNote {
            id: row.get(0)?,
            sha256: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The rows of one load: the notes it wrote, skipping a row already there.
fn insert(
    tx: &mut Tx<'_>,
    session: &str,
    trigger: &str,
    at: Timestamp,
    notes: &[LoadedNote],
) -> Result<Vec<LoadedNote>> {
    let mut written = Vec::new();
    for note in notes {
        let rows = tx.conn().execute(
            "INSERT OR IGNORE INTO session_notes (session_id, note_id, sha256, trigger, loaded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session, note.id, note.sha256, trigger, at],
        )?;
        if rows > 0 {
            written.push(note.clone());
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::domain::ObservationSource;
    use crate::state::runs::NewObservation;
    use crate::state::tests::memory_store;
    use crate::store::Actor;

    /// A render that loaded the given notes.
    fn rendered(ids: &[&str]) -> crate::kb::Rendered {
        crate::kb::Rendered {
            kb: crate::kb::KbState::Ok,
            text: String::new(),
            hash: "hash".into(),
            entries: ids
                .iter()
                .map(|id| crate::kb::Entry {
                    id: (*id).into(),
                    title: (*id).into(),
                    kind: Some("gotcha".into()),
                    scope: "global".into(),
                    tokens: 1,
                    loaded: true,
                    reason: None,
                    sha256: format!("sha-{id}"),
                })
                .collect(),
            used: 1,
        }
    }

    fn observe(store: &mut Store, session: &str, event: &str, at: Timestamp) {
        observe_with(store, session, event, at, json!({}));
    }

    fn observe_with(store: &mut Store, session: &str, event: &str, at: Timestamp, payload: Value) {
        store
            .observation_record(
                &Ctx::new(Actor::Hook),
                NewObservation {
                    session_id: Some(session.into()),
                    run_id: None,
                    source: ObservationSource::Hook,
                    event: event.into(),
                    verdict: None,
                    observed_at: at,
                    payload,
                },
            )
            .unwrap();
    }

    /// The copies one pass with no mark makes.
    fn owed(store: &mut Store, ctx: &Ctx) -> Vec<Loaded> {
        let pass = store.mem_load_owed(ctx, None).unwrap();
        assert!(pass.failed.is_empty(), "{:?}", pass.failed);
        pass.made
    }

    fn triggers(store: &Store, session: &str) -> Vec<String> {
        store
            .conn()
            .prepare(
                "SELECT trigger FROM session_notes WHERE session_id = ?1 ORDER BY loaded_at, rowid",
            )
            .unwrap()
            .query_map([session], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// The daemon's pass copies the set once per owed observation, in
    /// order, and a second pass owes nothing. Startup, fork and a session
    /// that loaded nothing owe no copy.
    #[test]
    fn the_copy_pass_makes_each_owed_copy_once() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        let start = Timestamp::now();
        for (n, event) in [
            "SessionStart:startup",
            "SessionStart:clear",
            "SessionStart:fork",
            "SessionStart:compact",
            "SessionStart:resume",
        ]
        .into_iter()
        .enumerate()
        {
            let minutes = i64::try_from(n).unwrap() + 1;
            observe(&mut store, &session, event, start.plus_minutes(minutes));
        }
        let made = owed(&mut store, &daemon);
        assert_eq!(
            made.iter().map(|l| l.trigger.as_str()).collect::<Vec<_>>(),
            ["clear", "compact", "resume"]
        );
        assert_eq!(
            triggers(&store, &session),
            ["startup", "clear", "compact", "resume"]
        );
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session).len(), 4);
    }

    /// A `SessionStart` whose bind the hook refused names a Claude process
    /// Tiller did not bind, so it owes no copy (`memory.md` section 7).
    #[test]
    fn a_refused_bind_owes_no_copy() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        let at = Timestamp::now().plus_minutes(1);
        observe_with(
            &mut store,
            &session,
            "SessionStart:clear",
            at,
            json!({"bind_refused": "the session has ended"}),
        );
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// A `/clear` in the same millisecond as the launch's record still gets
    /// its own `clear` rows, and is not owed again.
    #[test]
    fn a_copy_in_the_launch_millisecond_is_recorded_once() {
        let (mut store, session) = memory_store();
        let at = Timestamp::now();
        let daemon = Ctx::at(Actor::Daemon, at);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        observe(&mut store, &session, "SessionStart:clear", at);
        let made = owed(&mut store, &daemon);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].notes[0].id, "R-alpha");
        assert_eq!(triggers(&store, &session), ["startup", "clear"]);
        assert_eq!(store.mem_loaded(&session).unwrap(), ["R-alpha"]);
        assert!(owed(&mut store, &daemon).is_empty());
    }

    /// A copy that fails is reported, and the copies after it are still
    /// made.
    #[test]
    fn a_failing_copy_does_not_block_the_others() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let start = Timestamp::now();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:compact",
            start.plus_minutes(2),
        );
        let pass = store.mem_load_owed(&daemon, None).unwrap();
        assert_eq!(
            pass.made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert_eq!(pass.failed.len(), 1);
        assert_eq!(pass.failed[0].trigger, "clear");
        assert!(
            pass.failed[0].error.to_string().contains("no clear copies"),
            "{}",
            pass.failed[0].error
        );
        assert_eq!(triggers(&store, &session), ["startup", "compact"]);
    }

    /// A pass given the last pass's mark reads only the observations
    /// recorded since, less the slack; one with no mark reads them all.
    #[test]
    fn a_marked_pass_reads_only_new_observations() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        let mark = store.mem_load_owed(&daemon, None).unwrap().mark;
        assert_eq!(mark.read_from, daemon.now);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            mark.read_from.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:compact",
            mark.read_from.plus_minutes(2),
        );
        let old = mark.read_from.plus_minutes(-MARK_SLACK_MINUTES - 1);
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = ?1 WHERE event = 'SessionStart:clear'",
                [old],
            )
            .unwrap();
        let later = Ctx::at(Actor::Daemon, mark.read_from.plus_minutes(3));
        let pass = store.mem_load_owed(&later, Some(mark)).unwrap();
        assert_eq!(
            pass.made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert_eq!(pass.mark.read_from, later.now);
        assert_eq!(
            owed(&mut store, &later)
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["clear"]
        );
    }

    /// A failed copy holds the mark at its observation, so a pass long
    /// after it still reads it and makes the copy once the write goes in.
    #[test]
    fn a_failed_copy_is_tried_again_on_every_later_pass() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let first = store.mem_load_owed(&daemon, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(10));
        let second = store.mem_load_owed(&later, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the failed copy is read again");
        assert!(second.mark.read_from < later.now, "{:?}", second.mark);
        store
            .conn()
            .execute_batch("DROP TRIGGER no_clear;")
            .unwrap();
        let last = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(20));
        let third = store.mem_load_owed(&last, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
        assert_eq!(
            third
                .made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["clear"]
        );
        assert_eq!(third.mark.read_from, last.now);
    }

    /// A failed copy of an observation recorded longer than the copy window
    /// ago, as the first pass after a start meets one, is given up at once.
    /// It holds the mark back not at all, so the next pass does not read the
    /// window's history again for a copy it would not try.
    #[test]
    fn a_failed_copy_past_the_window_is_given_up_at_once() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(120));
        let pass = store.mem_load_owed(&later, None).unwrap();
        assert_eq!(pass.failed.len(), 1);
        assert!(pass.failed[0].given_up, "{:?}", pass.failed);
        assert_eq!(pass.mark.read_from, later.now);
    }

    /// A copy that never goes in is tried again until an hour after its
    /// observation was recorded. The pass that tries it last gives it up,
    /// and the pass after it does not read it.
    #[test]
    fn a_failed_copy_is_given_up_an_hour_after_its_observation() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let recorded: Timestamp = store
            .conn()
            .query_row(
                "SELECT recorded_at FROM observations WHERE event = 'SessionStart:clear'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let within = Ctx::at(
            Actor::Daemon,
            recorded.plus_minutes(COPY_WINDOW_MINUTES - 1),
        );
        let first = store.mem_load_owed(&within, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        assert!(!first.failed[0].given_up, "{:?}", first.failed);
        assert_eq!(first.mark.read_from, recorded);
        let past = Ctx::at(Actor::Daemon, recorded.plus_minutes(COPY_WINDOW_MINUTES));
        let second = store.mem_load_owed(&past, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the last try");
        assert!(second.failed[0].given_up, "{:?}", second.failed);
        assert_eq!(
            second.mark.read_from, past.now,
            "a copy given up holds no mark"
        );
        let next = Ctx::at(Actor::Daemon, past.now.plus_minutes(1));
        let third = store.mem_load_owed(&next, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
    }

    /// A copy given up while an earlier-held mark still covers it is not
    /// read, or reported, again: the last pass read it past its window.
    #[test]
    fn a_copy_given_up_is_not_reported_again() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let start = daemon.now;
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(2),
        );
        let recorded = start.plus_minutes(10);
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = CASE
                     WHEN observed_at = ?1 THEN ?2 ELSE ?3 END
                 WHERE event = 'SessionStart:clear'",
                params![start.plus_minutes(1), recorded, recorded.plus_minutes(3)],
            )
            .unwrap();
        let first = Ctx::at(Actor::Daemon, recorded.plus_minutes(COPY_WINDOW_MINUTES));
        let pass = store.mem_load_owed(&first, None).unwrap();
        assert_eq!(
            pass.failed.iter().map(|f| f.given_up).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(pass.mark.read_from, recorded.plus_minutes(3));
        let next = Ctx::at(Actor::Daemon, first.now.plus_minutes(1));
        let pass = store.mem_load_owed(&next, Some(pass.mark)).unwrap();
        assert_eq!(
            pass.failed
                .iter()
                .map(|f| f.observed_at)
                .collect::<Vec<_>>(),
            [start.plus_minutes(2)],
            "only the copy still in its window"
        );
    }

    /// A copy whose session ended is given up an hour after the end, and
    /// that last try is reported: the session's window does not drop it
    /// without a word.
    #[test]
    fn a_failed_copy_of_an_ended_session_is_given_up_when_reported() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_notes
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let ended = daemon.now.plus_minutes(2);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = ?1 WHERE event = 'SessionStart:clear'",
                [ended.plus_minutes(10)],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
                params![ended, session],
            )
            .unwrap();
        let within = Ctx::at(Actor::Daemon, ended.plus_minutes(30));
        let first = store.mem_load_owed(&within, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        assert!(!first.failed[0].given_up, "{:?}", first.failed);
        let past = Ctx::at(Actor::Daemon, ended.plus_minutes(COPY_WINDOW_MINUTES + 1));
        let second = store.mem_load_owed(&past, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the last try is reported");
        assert!(second.failed[0].given_up, "{:?}", second.failed);
        assert_eq!(second.mark.read_from, past.now);
        let next = Ctx::at(Actor::Daemon, past.now.plus_minutes(1));
        let third = store.mem_load_owed(&next, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
    }

    /// A poller that did not run for longer than the copy window does not
    /// make a copy for a session that ended over an hour ago: the drop is
    /// reported as given up.
    #[test]
    fn a_stalled_pass_gives_up_a_copy_whose_session_hour_passed() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        let mark = store.mem_load_owed(&daemon, None).unwrap().mark;
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let ended = daemon.now.plus_minutes(2);
        store
            .conn()
            .execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
                params![ended, session],
            )
            .unwrap();
        let late = Ctx::at(Actor::Daemon, ended.plus_minutes(COPY_WINDOW_MINUTES + 1));
        let pass = store.mem_load_owed(&late, Some(mark)).unwrap();
        assert!(pass.made.is_empty(), "{:?}", pass.made);
        assert_eq!(pass.failed.len(), 1);
        assert!(pass.failed[0].given_up, "{:?}", pass.failed);
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// An observation from before the session's first load owes no copy:
    /// the session had no set then. The pass does not select it, so it is
    /// not read on every pass with no mark.
    #[test]
    fn an_observation_before_the_first_load_owes_no_copy() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(-1),
        );
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        assert_eq!(store.owed_copies(daemon.now, None).unwrap(), Vec::new());
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// A copy made late copies the set the session had at the observation,
    /// not a set recorded after it.
    #[test]
    fn a_late_copy_copies_the_set_of_its_moment() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &rendered(&["R-alpha"]))
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let relaunch = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(3));
        store
            .mem_load(&relaunch, &session, &rendered(&["R-beta"]))
            .unwrap();
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(5));
        assert_eq!(owed(&mut store, &later).len(), 1);
        let copied: Vec<String> = store
            .conn()
            .prepare(
                "SELECT note_id FROM session_notes WHERE session_id = ?1 AND trigger = 'clear'",
            )
            .unwrap()
            .query_map([&session], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(copied, ["R-alpha"]);
        assert_eq!(store.mem_loaded(&session).unwrap(), ["R-beta"]);
    }

    /// The launch's event names the store's state, each note with its
    /// hash, each skipped note with its reason, and the section's hash; a
    /// launch that loaded nothing still records the state, and owes no
    /// copy.
    #[test]
    fn a_launch_records_the_kb_state_and_what_it_skipped() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        let mut render = rendered(&["a-note", "too-big"]);
        render.entries[1].loaded = false;
        render.entries[1].reason = Some(crate::kb::OVER_BUDGET.into());
        let loaded = store.mem_load(&daemon, &session, &render).unwrap();
        assert_eq!(loaded.value.notes.len(), 1);
        let payload: Value = serde_json::from_str(
            &store
                .conn()
                .query_row(
                    "SELECT payload FROM events WHERE kind = 'session.memory_loaded'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            payload,
            json!({"trigger": "startup", "kb": "ok", "hash": "hash",
                   "notes": [{"id": "a-note", "sha256": "sha-a-note"}],
                   "skipped": [{"id": "too-big", "reason": "over budget"}]})
        );

        let (mut store, session) = memory_store();
        let missing = crate::kb::render(&crate::kb::Read::missing(None), &[], 0, 7_500);
        store.mem_load(&daemon, &session, &missing).unwrap();
        let kb: String = store
            .conn()
            .query_row(
                "SELECT json_extract(payload, '$.kb') FROM events WHERE kind = 'session.memory_loaded'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kb, "missing");
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        assert!(owed(&mut store, &daemon).is_empty());
    }
}
