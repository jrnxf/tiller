//! Poller sources that enqueue outbox messages: the owner's annotations on
//! a worker's live review page (`protocol.md` 3.5; T2.5).
//!
//! Each annotation becomes one keyed `steer` for the live session of the run
//! that published the page, so the worker hears it through the outbox and
//! never has to poll the page. A page whose run has ended sends a `note` to
//! the project's orchestrator instead; one whose card is closed, or that has
//! nobody to tell, only records the entry on the card.
//!
//! Reading a page's queue consumes it, so the entries are stored in the
//! same pass that reads them. The key is `page:<artifact>:<entry id>`: a
//! source that can hand the same entry out again (a fake in a test, a tool
//! that keeps its queue) is delivered once, and an implementation whose read
//! consumes the queue mints a fresh id for every entry it returns. A crash
//! between the read and the store loses that batch; the owner sees the page
//! unanswered and annotates again.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::{Artifact, Card, OutboxKind, Session};
use tiller_core::state::outbox::{NewMessage, live_orchestrator};
use tiller_core::state::runs::live_session;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::poller::{PollError, Source};

/// How often the pages are read: every 30 s (`protocol.md` 3.5).
pub const PAGES_INTERVAL: Duration = Duration::from_secs(30);

/// The longest an annotation's element text is kept in a message, so a page
/// full of prose never makes a long body.
const ELEMENT_TEXT_CHARS: usize = 200;

/// One annotation from a review page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// Stable for this entry: the same entry read twice has the same id.
    pub id: String,
    /// What the owner wrote.
    pub prompt: String,
    /// The CSS selector of the element it annotates, when it annotates one.
    pub selector: String,
    pub tag: String,
    /// The element's text.
    pub text: String,
}

/// What reading a page's queue found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Feedback {
    /// Nothing new, or someone else is already listening.
    Quiet,
    /// The owner ended the review session; the page takes no more.
    Ended,
    Entries(Vec<Entry>),
}

/// The review page tool, as far as answering goes.
pub trait Pages: Send + 'static {
    /// Reads the annotations queued on the page at `page`.
    fn feedback(&mut self, page: &Path) -> Result<Feedback, String>;
}

/// Delivers the review-page annotations of every live page artifact.
pub struct PageAnswers<P: Pages> {
    store: Store,
    pages: P,
    /// Pages the owner ended: they are not read again until the daemon
    /// restarts.
    ended: HashSet<PathBuf>,
}

impl<P: Pages> PageAnswers<P> {
    pub fn new(store: Store, pages: P) -> Self {
        Self {
            store,
            pages,
            ended: HashSet::new(),
        }
    }

    /// One pass over every page that is still served.
    pub fn pass(&mut self, now: Timestamp) -> Result<Report, tiller_core::Error> {
        let artifacts: Vec<Artifact> = list(
            self.store.conn(),
            "kind = 'page' AND retired_at IS NULL AND path IS NOT NULL AND run_id IS NOT NULL
             ORDER BY created_at, id",
            &[],
        )?;
        let mut report = Report::default();
        for artifact in artifacts {
            let Some(path) = artifact.path.clone().map(PathBuf::from) else {
                continue;
            };
            if self.ended.contains(&path) {
                continue;
            }
            let entries = match self.pages.feedback(&path) {
                Ok(Feedback::Quiet) => continue,
                Ok(Feedback::Ended) => {
                    self.ended.insert(path);
                    continue;
                }
                Ok(Feedback::Entries(entries)) => entries,
                Err(error) => {
                    report.errors.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            report.pages += 1;
            for entry in entries {
                match self.answer(now, &artifact, &entry)? {
                    Fate::Steered => report.steered += 1,
                    Fate::Noted => report.noted += 1,
                    Fate::Recorded => report.recorded += 1,
                }
            }
        }
        Ok(report)
    }

    fn answer(
        &mut self,
        now: Timestamp,
        artifact: &Artifact,
        entry: &Entry,
    ) -> Result<Fate, tiller_core::Error> {
        let ctx = Ctx::at(Actor::Daemon, now);
        let key = format!("page:{}:{}", artifact.id, entry.id);
        let card: Card = get(self.store.conn(), "card", &artifact.card_id)?;
        if card.closed_at.is_none() {
            let body = body(artifact, entry);
            let message = |target: &Session, kind| NewMessage {
                target_session_id: target.id.clone(),
                kind,
                body: body.clone(),
                card_id: Some(card.id.clone()),
                reply_to: None,
            };
            let owner = match &artifact.run_id {
                Some(run) => live_session(self.store.conn(), run)?,
                None => None,
            };
            if let Some(session) = owner {
                self.store.outbox_enqueue(
                    &ctx,
                    Some(&key),
                    message(&session, OutboxKind::Steer),
                )?;
                return Ok(Fate::Steered);
            }
            if let Some(orchestrator) = live_orchestrator(self.store.conn(), &card.project_id)? {
                self.store.outbox_enqueue(
                    &ctx,
                    Some(&key),
                    message(&orchestrator, OutboxKind::Note),
                )?;
                return Ok(Fate::Noted);
            }
        }
        self.store.artifact_feedback(
            &ctx,
            &key,
            &artifact.id,
            json!({
                "id": entry.id,
                "prompt": entry.prompt,
                "selector": entry.selector,
                "tag": entry.tag,
                "text": entry.text,
            }),
        )?;
        Ok(Fate::Recorded)
    }
}

/// What became of one entry.
enum Fate {
    Steered,
    Noted,
    Recorded,
}

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Pages that had entries.
    pub pages: usize,
    /// Entries sent to the worker that owns the page.
    pub steered: usize,
    /// Entries sent to the orchestrator because the worker's run ended.
    pub noted: usize,
    /// Entries only recorded on the card.
    pub recorded: usize,
    /// Pages that could not be read, by path.
    pub errors: Vec<String>,
}

/// The message body: the owner's words and the element they are about, never
/// the page.
fn body(artifact: &Artifact, entry: &Entry) -> String {
    let mut body = format!(
        "Review page \"{}\": {}",
        artifact.title,
        entry.prompt.trim()
    );
    let text: String = entry.text.chars().take(ELEMENT_TEXT_CHARS).collect();
    let element = [entry.selector.as_str(), entry.tag.as_str(), text.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if !element.is_empty() {
        body.push_str("\nElement: ");
        body.push_str(&element.join(" | "));
    }
    body
}

impl<P: Pages> Source for PageAnswers<P> {
    fn name(&self) -> String {
        "pages".to_owned()
    }

    fn interval(&self) -> Duration {
        PAGES_INTERVAL
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let report = self.pass(now)?;
        match report.errors.first() {
            // One unreadable page does not fail the pass while another was
            // read; only a pass that read nothing is a failed poll.
            Some(first) if report.pages == 0 => {
                for error in &report.errors {
                    tracing::warn!("review page: {error}");
                }
                Err(PollError::State(tiller_core::Error::Unavailable(format!(
                    "{} review pages unreadable; first: {first}",
                    report.errors.len()
                ))))
            }
            Some(_) => {
                for error in &report.errors {
                    tracing::warn!("review page: {error}");
                }
                Ok(())
            }
            None => Ok(()),
        }
    }
}

// -- lavish-axi -----------------------------------------------------------------

/// How long one read waits for the review server, in milliseconds.
const READ_MS: &str = "1500";
/// The listener name a read shows in the review server's session listing.
const OWNER: &str = "tiller";

/// The `lavish-axi` CLI: `poll <page>` with a short timeout and no takeover,
/// so a worker that polls its own page keeps the listener and this source
/// reads nothing until it stops.
pub struct Lavish {
    program: Vec<OsString>,
    /// Ids are `<start>-<n>`: a read consumes the queue, so every entry it
    /// returns is new.
    start: u64,
    next: AtomicU64,
}

impl Lavish {
    pub fn new(program: Vec<OsString>) -> Self {
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis().try_into().unwrap_or(u64::MAX));
        Self {
            program,
            start,
            next: AtomicU64::new(0),
        }
    }

    fn mint(&self) -> String {
        format!(
            "{}-{}",
            self.start,
            self.next.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl Pages for Lavish {
    fn feedback(&mut self, page: &Path) -> Result<Feedback, String> {
        let (program, leading) = self
            .program
            .split_first()
            .ok_or_else(|| "the lavish command is empty".to_owned())?;
        let out = Command::new(program)
            .args(leading)
            .arg("poll")
            .arg(page)
            .args(["--owner", OWNER, "--timeout-ms", READ_MS])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("lavish-axi poll: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Someone else is polling the page: they hear the owner, not us.
        if stdout.contains("code: LISTENER_ACTIVE") || stderr.contains("code: LISTENER_ACTIVE") {
            return Ok(Feedback::Quiet);
        }
        if !out.status.success() {
            return Err(format!(
                "lavish-axi poll exited {}: {}",
                out.status,
                first_line(&stderr, &stdout)
            ));
        }
        parse_poll(&stdout, || self.mint())
    }
}

fn first_line<'a>(stderr: &'a str, stdout: &'a str) -> &'a str {
    [stderr, stdout]
        .into_iter()
        .map(str::trim)
        .find(|text| !text.is_empty())
        .and_then(|text| text.lines().next())
        .unwrap_or("no output")
}

/// Reads `lavish-axi poll`'s output, a TOON document: a `session:` block
/// whose `status:` is `feedback`, `waiting` or `ended`, and for `feedback` a
/// `prompts[N]{uid,prompt,selector,tag,text}:` table.
fn parse_poll(output: &str, mut mint: impl FnMut() -> String) -> Result<Feedback, String> {
    let status = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("status:"))
        .map(str::trim)
        .ok_or("lavish-axi poll named no status")?;
    match status {
        "waiting" => return Ok(Feedback::Quiet),
        "ended" => return Ok(Feedback::Ended),
        "feedback" => {}
        other => return Err(format!("lavish-axi poll status {other}")),
    }
    let mut lines = output.lines();
    let Some((header, columns)) = lines.find_map(|line| {
        let line = line.trim();
        let columns = line
            .strip_prefix("prompts[")?
            .split_once("]{")?
            .1
            .strip_suffix("}:")?;
        Some((line, columns))
    }) else {
        // Feedback that is not prompts (a browser that went away, an
        // artifact failure) has no one to answer.
        return Ok(Feedback::Quiet);
    };
    let columns: Vec<String> = split_row(columns)?;
    let field = |row: &[String], name: &str| -> String {
        columns
            .iter()
            .position(|column| column == name)
            .and_then(|at| row.get(at))
            .cloned()
            .unwrap_or_default()
    };
    let mut entries = Vec::new();
    for line in lines {
        // The table is the indented rows right under its header.
        if !line.starts_with("  ") {
            break;
        }
        let row = split_row(line.trim())
            .map_err(|e| format!("lavish-axi poll row under {header}: {e}"))?;
        entries.push(Entry {
            id: mint(),
            prompt: field(&row, "prompt"),
            selector: field(&row, "selector"),
            tag: field(&row, "tag"),
            text: field(&row, "text"),
        });
    }
    Ok(Feedback::Entries(entries))
}

/// Splits one TOON row: comma separated, a field in double quotes when it
/// holds a comma, a quote, a colon or edge space, with `\"`, `\\`, `\n`, `\r`
/// and `\t` escapes.
fn split_row(row: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = row.chars();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => match chars.next() {
                Some('n') => field.push('\n'),
                Some('r') => field.push('\r'),
                Some('t') => field.push('\t'),
                Some(other) => field.push(other),
                None => return Err("a row ends inside an escape".into()),
            },
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            other => field.push(other),
        }
    }
    if quoted {
        return Err("a row ends inside a quoted field".into());
    }
    fields.push(field);
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("e{n}")
        }
    }

    #[test]
    fn a_feedback_poll_yields_its_entries() {
        let output = "session:\n  file: /home/user/page.html\n  status: feedback\n\
            prompts[2]{uid,prompt,selector,tag,text}:\n  \"\",\"Ship the \\\"blue\\\" one, please\",\"\",\"\",\"\"\n  el-3,Make it larger,h1,h1,Title\n\
            next_step: \"Apply the requested changes.\"\ndom_snapshot: \"\"\n";
        let Feedback::Entries(entries) = parse_poll(output, ids()).unwrap() else {
            panic!("expected entries");
        };
        assert_eq!(
            entries,
            [
                Entry {
                    id: "e1".into(),
                    prompt: "Ship the \"blue\" one, please".into(),
                    ..Entry::default()
                },
                Entry {
                    id: "e2".into(),
                    prompt: "Make it larger".into(),
                    selector: "h1".into(),
                    tag: "h1".into(),
                    text: "Title".into(),
                },
            ]
        );
    }

    #[test]
    fn waiting_and_ended_polls_carry_no_entries() {
        let waiting = "session:\n  file: /home/user/page.html\n  status: waiting\n";
        assert_eq!(parse_poll(waiting, ids()), Ok(Feedback::Quiet));
        let ended = "session:\n  file: /home/user/page.html\n  status: ended\n  ended_by: user\n";
        assert_eq!(parse_poll(ended, ids()), Ok(Feedback::Ended));
        assert!(parse_poll("nothing", ids()).is_err());
    }

    /// A `lavish-axi` that prints what its first line says to, so the
    /// command line and the exit paths are the real ones.
    fn stub(dir: &Path, script: &str) -> Lavish {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("lavish-axi");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Lavish::new(vec![path.into_os_string()])
    }

    #[test]
    fn lavish_polls_briefly_without_taking_over() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        let mut lavish = stub(
            dir.path(),
            &format!(
                "echo \"$@\" > {}\nprintf 'session:\\n  status: feedback\\nprompts[1]{{uid,prompt,selector,tag,text}}:\\n  ,Hello,,,\\n'",
                args.display()
            ),
        );
        let found = lavish.feedback(Path::new("/home/user/page.html")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&args).unwrap().trim(),
            "poll /home/user/page.html --owner tiller --timeout-ms 1500"
        );
        let Feedback::Entries(entries) = found else {
            panic!("expected entries");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].prompt, "Hello");
        // A second read mints a different id: the first consumed its queue.
        let again = lavish.feedback(Path::new("/home/user/page.html")).unwrap();
        let Feedback::Entries(again) = again else {
            panic!("expected entries");
        };
        assert_ne!(entries[0].id, again[0].id);
    }

    #[test]
    fn a_page_someone_else_polls_reads_quiet_and_a_failed_tool_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut busy = stub(
            dir.path(),
            "echo 'error: already has an active poll listener'; echo 'code: LISTENER_ACTIVE'; exit 1",
        );
        assert_eq!(
            busy.feedback(Path::new("/home/user/page.html")),
            Ok(Feedback::Quiet)
        );
        let mut broken = stub(dir.path(), "echo 'code: UNKNOWN' >&2; exit 1");
        let error = broken
            .feedback(Path::new("/home/user/page.html"))
            .unwrap_err();
        assert!(error.contains("code: UNKNOWN"), "{error}");
    }

    #[test]
    fn rows_keep_escaped_newlines_and_commas() {
        assert_eq!(
            split_row("a,\"b, c\",\"line\\none\",").unwrap(),
            ["a", "b, c", "line\none", ""]
        );
        assert!(split_row("\"open").is_err());
    }
}
