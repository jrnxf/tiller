//! Transcript tailer (`protocol.md` section 5).
//!
//! It reads the lines appended to a session's Claude Code transcript and
//! records what hooks cannot see: interrupts on every host (Stop does not
//! fire on them), and on a host with `intake_source = transcript` the turn
//! boundaries hooks would have given. The daemon drives [`Tailer::poll`] from
//! a file watch. No transcript text is ever copied into a payload.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tiller_core::domain::{Activity, AskSource, ObservationSource, Session};
use tiller_core::state::asks::NewAsk;
use tiller_core::state::get;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx, Store, Timestamp};

use crate::wake;

/// The user entries Claude Code writes when the owner interrupts a turn
/// (host-capabilities, seen in real transcripts).
pub const INTERRUPT_MARKERS: &[&str] = &[
    "[Request interrupted by user]",
    "[Request interrupted by user for tool use]",
];

/// Where Claude Code writes a session's transcript:
/// `<config dir>/projects/<slug>/<claude session id>.jsonl`, where the slug
/// is the physical working directory with every character that is not a
/// letter or digit turned into `-`.
pub fn transcript_path(config_dir: &Path, cwd: &str, claude_session_id: &str) -> PathBuf {
    let slug: String = cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    config_dir
        .join("projects")
        .join(slug)
        .join(format!("{claude_session_id}.jsonl"))
}

/// What one transcript entry means, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub event: &'static str,
    pub verdict: Activity,
    /// The outbox id of a wake-up line.
    pub outbox_id: Option<String>,
}

/// Classifies one transcript entry. `transcript_intake` is true on a host
/// whose `intake_source` is `transcript`.
pub fn classify(entry: &Value, transcript_intake: bool) -> Option<Detected> {
    let detected = |event, verdict| {
        Some(Detected {
            event,
            verdict,
            outbox_id: None,
        })
    };
    match entry.get("type").and_then(Value::as_str) {
        Some("user") => {
            let content = entry.pointer("/message/content");
            let texts = texts(content);
            if texts
                .iter()
                .any(|text| INTERRUPT_MARKERS.contains(&text.trim()))
            {
                return detected("interrupt", Activity::Idle);
            }
            let meta = entry.get("isMeta").and_then(Value::as_bool) == Some(true);
            if !transcript_intake || meta || is_tool_result(content) || texts.is_empty() {
                return None;
            }
            if let [text] = texts.as_slice()
                && let Some(id) = wake::outbox_id(text)
            {
                return Some(Detected {
                    event: "wake",
                    verdict: Activity::Working,
                    outbox_id: Some(id.to_owned()),
                });
            }
            detected("transcript:user", Activity::Working)
        }
        Some("assistant") if transcript_intake => {
            if entry.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                return detected("transcript:api_error", Activity::Errored);
            }
            match entry
                .pointer("/message/stop_reason")
                .and_then(Value::as_str)
            {
                Some("end_turn") => detected("transcript:turn_end", Activity::Idle),
                _ => None,
            }
        }
        _ => None,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn texts(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str).map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn is_tool_result(content: Option<&Value>) -> bool {
    matches!(content, Some(Value::Array(items))
        if items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("tool_result")))
}

/// Tails one session's transcript.
pub struct Tailer {
    session_id: String,
    run_id: Option<String>,
    path: PathBuf,
    offset: u64,
    transcript_intake: bool,
    /// Set after a truncation: entries may be read a second time, so each is
    /// checked against the recorded uuids.
    dedupe: bool,
}

impl Tailer {
    /// A tailer for `session` on the transcript at `path`. It resumes after
    /// the newest offset recorded for that same file, so a restarted daemon
    /// never records an entry twice. An offset recorded for another file
    /// (the session's transcript before `/clear` or a forked resume) or for
    /// no named file (rows from before the path was stored) says nothing
    /// about this one: the file is then read from the start and each entry
    /// is checked against the uuids already recorded for the session.
    pub fn resume(
        store: &Store,
        session: &Session,
        path: impl Into<PathBuf>,
        transcript_intake: bool,
    ) -> tiller_core::Result<Self> {
        let path = path.into();
        let (offset, others): (Option<i64>, bool) = store.conn().query_row(
            "SELECT max(CASE WHEN json_extract(payload, '$.transcript_path') = ?2
                         THEN json_extract(payload, '$.transcript_offset') END),
                    count(*) > 0
             FROM observations WHERE session_id = ?1 AND source = 'transcript'",
            rusqlite::params![session.id, path.to_string_lossy()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let offset = offset.and_then(|o| u64::try_from(o).ok());
        Ok(Self {
            session_id: session.id.clone(),
            run_id: session.run_id.clone(),
            path,
            offset: offset.unwrap_or(0),
            transcript_intake,
            dedupe: offset.is_none() && others,
        })
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads every complete line appended since the last poll and records
    /// what it detects. Returns how many observations it wrote. A partial
    /// last line waits for the next poll; a file shorter than the offset was
    /// truncated or replaced and is read again from the start.
    pub fn poll(&mut self, store: &mut Store) -> tiller_core::Result<usize> {
        let io = |error: std::io::Error| tiller_core::Error::Internal(error.to_string());
        let Ok(mut file) = File::open(&self.path) else {
            return Ok(0);
        };
        let len = file.metadata().map_err(io)?.len();
        if len < self.offset {
            self.offset = 0;
            self.dedupe = true;
        }
        file.seek(SeekFrom::Start(self.offset)).map_err(io)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut written = 0;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line).map_err(io)?;
            if read == 0 || line.last() != Some(&b'\n') {
                // End of file, or a line Claude Code is still writing.
                return Ok(written);
            }
            let line_end = self.offset + read as u64;
            if self.record_line(store, &line, line_end)? {
                written += 1;
            }
            // Only past a line once it is recorded (or needs nothing), so an
            // error mid-poll re-reads from the first line not yet written.
            self.offset = line_end;
        }
    }

    /// Records one complete line, if it means anything. Returns whether it
    /// wrote an observation.
    fn record_line(
        &self,
        store: &mut Store,
        line: &[u8],
        line_end: u64,
    ) -> tiller_core::Result<bool> {
        // On a hook host only interrupts matter: skip parsing anything else.
        if !self.transcript_intake && !contains(line, b"[Request interrupted by user") {
            return Ok(false);
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            return Ok(false);
        };
        let Some(detected) = classify(&entry, self.transcript_intake) else {
            return Ok(false);
        };
        let uuid = entry.get("uuid").and_then(Value::as_str);
        if self.dedupe && self.already_recorded(store, uuid)? {
            return Ok(false);
        }
        let mut event = detected.event;
        let mut payload = json!({
            "transcript_path": self.path.to_string_lossy(),
            "transcript_offset": line_end,
            "uuid": uuid,
        });
        if let Some(id) = &detected.outbox_id {
            // The same check the UserPromptSubmit hook makes (protocol.md 3.2).
            let target: Option<String> = store
                .conn()
                .query_row(
                    "SELECT target_session_id FROM outbox WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if target.as_deref() != Some(self.session_id.as_str()) {
                event = "wake_mismatch";
            }
            payload["outbox_id"] = json!(id);
        }
        let observed_at = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<Timestamp>().ok())
            .unwrap_or_else(Timestamp::now);
        if event == "transcript:user" {
            // A busy database retries the line; any other refusal is noted
            // and the tailer moves on, so one bad entry never stalls it.
            match self.record_ask(store, &entry, uuid, observed_at) {
                Ok(Some(ask_id)) => payload["ask_id"] = json!(ask_id),
                Ok(None) => {}
                Err(error @ tiller_core::Error::Unavailable(_)) => return Err(error),
                Err(error) => payload["ask_error"] = json!(error.to_string()),
            }
        }
        store.observation_record(
            &Ctx::new(Actor::Daemon),
            NewObservation {
                session_id: Some(self.session_id.clone()),
                run_id: self.run_id.clone(),
                source: ObservationSource::Transcript,
                event: event.to_owned(),
                verdict: Some(detected.verdict),
                observed_at,
                payload,
            },
        )?;
        Ok(true)
    }

    /// On a hook-less host the tailer is the ask's source (`protocol.md`
    /// 2.2), keyed by the entry so a re-read never records it twice. An
    /// entry stamped after the session ended opens no ask: the session it
    /// would land on is gone.
    fn record_ask(
        &self,
        store: &mut Store,
        entry: &Value,
        uuid: Option<&str>,
        observed_at: Timestamp,
    ) -> tiller_core::Result<Option<String>> {
        let Some(uuid) = uuid else {
            return Ok(None);
        };
        let text = texts(entry.pointer("/message/content")).join("\n\n");
        if text.trim().is_empty() {
            return Ok(None);
        }
        let session: Session = get(store.conn(), "session", &self.session_id)?;
        if session.ended_at.is_some_and(|ended| observed_at > ended) {
            return Ok(None);
        }
        let claude_id = session.claude_session_id.as_deref().unwrap_or(&session.id);
        let key = format!("transcript:{claude_id}:{uuid}");
        let recorded = store.ask_record(
            &Ctx::new(Actor::Daemon),
            Some(&key),
            NewAsk {
                project_id: session.project_id.clone(),
                session_id: Some(session.id.clone()),
                source: AskSource::Prompt,
                text,
            },
        )?;
        Ok(Some(recorded.value.ask.id))
    }

    fn already_recorded(&self, store: &Store, uuid: Option<&str>) -> tiller_core::Result<bool> {
        let Some(uuid) = uuid else {
            return Ok(false);
        };
        let found: Option<i64> = store
            .conn()
            .query_row(
                "SELECT 1 FROM observations WHERE session_id = ?1 AND source = 'transcript'
                 AND json_extract(payload, '$.uuid') = ?2 LIMIT 1",
                [&self.session_id, uuid],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::Path;

    use serde_json::{Value, json};
    use tiller_core::Store;
    use tiller_core::domain::Activity;

    use super::*;
    use crate::testing::{count, fleet, session};

    fn append(path: &Path, entries: &[Value]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for entry in entries {
            writeln!(file, "{entry}").unwrap();
        }
    }

    fn user(uuid: &str, at: &str, content: Value) -> Value {
        json!({"type": "user", "uuid": uuid, "timestamp": at, "message": {"role": "user", "content": content}})
    }

    fn assistant(uuid: &str, at: &str, stop_reason: &str) -> Value {
        json!({"type": "assistant", "uuid": uuid, "timestamp": at,
               "message": {"role": "assistant", "stop_reason": stop_reason, "content": [{"type": "text", "text": "a secret reply"}]}})
    }

    #[test]
    fn interrupt_marker_sets_idle() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        append(
            &path,
            &[
                user("u1", "2026-11-02T10:00:00Z", json!("fix the login bug")),
                assistant("a1", "2026-11-02T10:00:01Z", "tool_use"),
                user(
                    "u2",
                    "2026-11-02T10:00:02Z",
                    json!([{"type": "tool_result", "content": "ok"}]),
                ),
            ],
        );
        let mut tailer = Tailer::resume(&store, &orchestrator, &path, false).unwrap();
        // A hook host records nothing of an ordinary turn from the transcript.
        assert_eq!(tailer.poll(&mut store).unwrap(), 0);

        for (uuid, marker, hook_at, at) in [
            (
                "u3",
                INTERRUPT_MARKERS[1],
                "2026-11-02T10:00:03Z",
                "2026-11-02T10:00:04Z",
            ),
            (
                "u4",
                INTERRUPT_MARKERS[0],
                "2026-11-02T10:00:05Z",
                "2026-11-02T10:00:06Z",
            ),
        ] {
            store
                .observation_record(
                    &tiller_core::Ctx::new(tiller_core::Actor::Hook),
                    NewObservation {
                        session_id: Some(fleet.orchestrator.clone()),
                        run_id: None,
                        source: ObservationSource::Hook,
                        event: "PreToolUse".into(),
                        verdict: Some(Activity::Working),
                        observed_at: hook_at.parse().unwrap(),
                        payload: json!({}),
                    },
                )
                .unwrap();
            assert_eq!(
                session(&store, &fleet.orchestrator).activity,
                Activity::Working
            );
            append(
                &path,
                &[user(uuid, at, json!([{"type": "text", "text": marker}]))],
            );
            assert_eq!(tailer.poll(&mut store).unwrap(), 1, "{marker}");
            let after = session(&store, &fleet.orchestrator);
            assert_eq!(after.activity, Activity::Idle, "{marker}");
            assert_eq!(after.activity_source, Some(ObservationSource::Transcript));
        }
        let stored: String = store
            .conn()
            .query_row(
                "SELECT group_concat(payload) FROM observations WHERE source = 'transcript'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !stored.contains("interrupted"),
            "no transcript text in payloads: {stored}"
        );
    }

    #[test]
    fn restart_resumes_after_the_newest_offset_and_waits_for_whole_lines() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        append(
            &path,
            &[user(
                "u1",
                "2026-11-02T10:00:00Z",
                json!(INTERRUPT_MARKERS[0]),
            )],
        );
        let partial = user("u2", "2026-11-02T10:00:09Z", json!(INTERRUPT_MARKERS[0])).to_string();
        let (head, tail) = partial.split_at(20);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(head.as_bytes())
            .unwrap();

        let mut first = Tailer::resume(&store, &orchestrator, &path, false).unwrap();
        assert_eq!(first.poll(&mut store).unwrap(), 1);
        drop(first);

        // A new tailer (a restarted daemon) picks up where the rows say.
        let mut second = Tailer::resume(&store, &orchestrator, &path, false).unwrap();
        assert_eq!(
            second.poll(&mut store).unwrap(),
            0,
            "u1 is not recorded twice"
        );
        writeln!(
            OpenOptions::new().append(true).open(&path).unwrap(),
            "{tail}"
        )
        .unwrap();
        assert_eq!(
            second.poll(&mut store).unwrap(),
            1,
            "the completed line is read"
        );
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM observations WHERE source = 'transcript'"
            ),
            2
        );
    }

    #[test]
    fn truncated_transcript_restarts_and_dedupes_by_uuid() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        let first = user("u1", "2026-11-02T10:00:00Z", json!(INTERRUPT_MARKERS[0]));
        append(
            &path,
            &[
                first.clone(),
                user("u2", "2026-11-02T10:00:01Z", json!(INTERRUPT_MARKERS[0])),
            ],
        );
        let mut tailer = Tailer::resume(&store, &orchestrator, &path, false).unwrap();
        assert_eq!(tailer.poll(&mut store).unwrap(), 2);

        // Replaced by a shorter file that repeats u1, then grows with u3.
        std::fs::write(&path, "").unwrap();
        append(&path, &[first]);
        assert_eq!(
            tailer.poll(&mut store).unwrap(),
            0,
            "u1 is not recorded twice"
        );
        append(
            &path,
            &[user(
                "u3",
                "2026-11-02T10:00:02Z",
                json!(INTERRUPT_MARKERS[1]),
            )],
        );
        assert_eq!(tailer.poll(&mut store).unwrap(), 1, "only u3 is new");
    }

    #[test]
    fn transcript_intake_host_records_each_user_entry_as_one_ask() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        let ask = user("u1", "2026-11-02T10:00:00Z", json!("add a dark mode"));
        let wake = user(
            "u2",
            "2026-11-02T10:00:01Z",
            json!("tiller inbox 01JC2ZB6N0E2K4M6P8R0S2T4V6"),
        );
        let blank = user("u3", "2026-11-02T10:00:02Z", json!("  \n"));
        append(&path, &[ask.clone(), wake, blank]);
        let mut tailer = Tailer::resume(&store, &orchestrator, &path, true).unwrap();
        assert_eq!(tailer.poll(&mut store).unwrap(), 3);
        let asks =
            "SELECT count(*) FROM asks WHERE source = 'prompt' AND disposition = 'unresolved'";
        assert_eq!(
            count(&store, asks),
            1,
            "neither the wake-up line nor a blank entry is an ask"
        );

        // A replaced file that repeats u1 records nothing new, and a fresh
        // tailer (a daemon restart) on the same entry neither.
        std::fs::write(&path, "").unwrap();
        append(&path, std::slice::from_ref(&ask));
        tailer.poll(&mut store).unwrap();
        let mut fresh = Tailer::resume(&store, &orchestrator, &path, true).unwrap();
        let other = dir.path().join("other.jsonl");
        append(&other, &[ask]);
        fresh.path = other;
        fresh.offset = 0;
        fresh.poll(&mut store).unwrap();
        assert_eq!(count(&store, asks), 1);
    }

    #[test]
    fn transcript_intake_host_records_turns_wakes_and_api_errors() {
        let wake = "tiller inbox 01JC2ZB6N0E2K4M6P8R0S2T4V6 # steer alpha-1";
        let cases = [
            (
                user("u1", "2026-11-02T10:00:00Z", json!("add a dark mode")),
                Some(("transcript:user", Activity::Working)),
            ),
            (
                user("u2", "2026-11-02T10:00:00Z", json!(wake)),
                Some(("wake", Activity::Working)),
            ),
            (
                assistant("a1", "2026-11-02T10:00:01Z", "end_turn"),
                Some(("transcript:turn_end", Activity::Idle)),
            ),
            (assistant("a2", "2026-11-02T10:00:01Z", "tool_use"), None),
            (
                json!({"type": "assistant", "isApiErrorMessage": true, "message": {"content": "API Error: 529"}}),
                Some(("transcript:api_error", Activity::Errored)),
            ),
            (
                user(
                    "u3",
                    "2026-11-02T10:00:02Z",
                    json!([{"type": "tool_result", "content": "x"}]),
                ),
                None,
            ),
            (
                json!({"type": "user", "isMeta": true, "message": {"content": "caveat"}}),
                None,
            ),
            (json!({"type": "summary", "summary": "x"}), None),
        ];
        for (entry, want) in cases {
            let got = classify(&entry, true).map(|d| (d.event, d.verdict));
            assert_eq!(got, want, "{entry}");
        }
        // Without transcript intake only the interrupt is recorded.
        assert_eq!(classify(&cases_user(), false), None);
    }

    fn cases_user() -> Value {
        user("u9", "2026-11-02T10:00:00Z", json!("hello"))
    }

    /// On a transcript-intake host a wake-up line gets the same target check
    /// the hook makes, and never reads as an ask.
    #[test]
    fn transcript_wake_line_is_checked_against_the_outbox() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut ids = Vec::new();
        for target in [&fleet.orchestrator, &fleet.worker] {
            let id = tiller_core::ids::new_ulid();
            store
                .conn()
                .execute(
                    "INSERT INTO outbox (id, target_session_id, kind, body, created_at, created_by)
                     VALUES (?1, ?2, 'wake', 'b', '2026-11-02T10:00:00.000Z', 'daemon')",
                    [&id, target],
                )
                .unwrap();
            ids.push(id);
        }
        append(
            &path,
            &[
                user(
                    "u1",
                    "2026-11-02T10:00:00Z",
                    json!(format!("tiller inbox {} # wake", ids[0])),
                ),
                user(
                    "u2",
                    "2026-11-02T10:00:01Z",
                    json!(format!("tiller inbox {}", ids[1])),
                ),
            ],
        );
        let orchestrator = session(&store, &fleet.orchestrator);
        let mut tailer = Tailer::resume(&store, &orchestrator, &path, true).unwrap();
        assert_eq!(tailer.poll(&mut store).unwrap(), 2);
        let rows: Vec<(String, String)> = store
            .conn()
            .prepare("SELECT event, json_extract(payload, '$.outbox_id') FROM observations ORDER BY observed_at")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("wake".to_owned(), ids[0].clone()),
                ("wake_mismatch".to_owned(), ids[1].clone())
            ]
        );
        assert_eq!(count(&store, "SELECT count(*) FROM asks"), 0);
    }

    /// Lines that mean nothing on a hook host, `n` of them, to make a file
    /// long.
    fn filler(prefix: &str, n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| assistant(&format!("{prefix}{i}"), "2026-11-02T10:00:00Z", "tool_use"))
            .collect()
    }

    fn transcript_uuids(store: &Store) -> Vec<String> {
        store
            .conn()
            .prepare(
                "SELECT json_extract(payload, '$.uuid') FROM observations
                 WHERE source = 'transcript' ORDER BY recorded_at, id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// `/clear` or a forked resume gives the session a new transcript. A
    /// tailer on it, after a restart too, starts from the offset recorded
    /// for that file, never from the old file's: the interrupt at the head
    /// of the new file is recorded.
    #[test]
    fn resume_takes_the_offset_of_its_own_path_only() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.jsonl");
        let new = dir.path().join("new.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        append(&old, &filler("o", 10));
        append(
            &old,
            &[user(
                "u1",
                "2026-11-02T10:00:00Z",
                json!(INTERRUPT_MARKERS[0]),
            )],
        );
        let mut first = Tailer::resume(&store, &orchestrator, &old, false).unwrap();
        assert_eq!(first.poll(&mut store).unwrap(), 1);
        let old_offset = first.offset();

        // The new file is longer than the old offset, its interrupt first.
        append(
            &new,
            &[user(
                "n1",
                "2026-11-02T10:01:00Z",
                json!(INTERRUPT_MARKERS[1]),
            )],
        );
        append(&new, &filler("n", 20));
        assert!(std::fs::metadata(&new).unwrap().len() > old_offset);
        let mut second = Tailer::resume(&store, &orchestrator, &new, false).unwrap();
        assert_eq!(second.offset(), 0, "no offset is recorded for this file");
        assert_eq!(second.poll(&mut store).unwrap(), 1, "the head interrupt");

        // A restart on each file resumes where that file's rows say: after
        // its last recorded entry.
        let mut again = Tailer::resume(&store, &orchestrator, &new, false).unwrap();
        assert!(again.offset() > 0 && again.offset() < second.offset());
        assert_eq!(again.poll(&mut store).unwrap(), 0);
        append(
            &new,
            &[user(
                "n2",
                "2026-11-02T10:02:00Z",
                json!(INTERRUPT_MARKERS[0]),
            )],
        );
        assert_eq!(again.poll(&mut store).unwrap(), 1);
        let back = Tailer::resume(&store, &orchestrator, &old, false).unwrap();
        assert_eq!(back.offset(), old_offset);
        assert_eq!(transcript_uuids(&store), ["u1", "n1", "n2"]);
    }

    /// Rows written before the tailer stored its path name no file. Their
    /// offset is not trusted: the file is read from the start, and each
    /// entry already recorded is skipped by its uuid.
    #[test]
    fn rows_without_a_path_are_read_again_and_deduped_by_uuid() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let orchestrator = session(&store, &fleet.orchestrator);
        append(
            &path,
            &[user(
                "u1",
                "2026-11-02T10:00:00Z",
                json!(INTERRUPT_MARKERS[0]),
            )],
        );
        append(&path, &filler("f", 5));
        append(
            &path,
            &[user(
                "u2",
                "2026-11-02T10:00:05Z",
                json!(INTERRUPT_MARKERS[0]),
            )],
        );
        // An older daemon recorded u1, with an offset and no path.
        store
            .observation_record(
                &Ctx::new(Actor::Daemon),
                NewObservation {
                    session_id: Some(fleet.orchestrator.clone()),
                    run_id: None,
                    source: ObservationSource::Transcript,
                    event: "interrupt".into(),
                    verdict: Some(Activity::Idle),
                    observed_at: "2026-11-02T10:00:00Z".parse().unwrap(),
                    payload: json!({"transcript_offset": 999_999, "uuid": "u1"}),
                },
            )
            .unwrap();
        let mut tailer = Tailer::resume(&store, &orchestrator, &path, false).unwrap();
        assert_eq!(tailer.offset(), 0);
        assert_eq!(tailer.poll(&mut store).unwrap(), 1, "only u2 is new");
        assert_eq!(transcript_uuids(&store), ["u1", "u2"]);
    }

    #[test]
    fn path_slug_turns_every_non_alphanumeric_into_a_dash() {
        let path = transcript_path(
            Path::new("/home/user/.claude"),
            "/home/user/my_app.v2",
            "abc",
        );
        assert_eq!(
            path,
            Path::new("/home/user/.claude/projects/-home-user-my-app-v2/abc.jsonl")
        );
    }
}
