//! Desktop notifications for the owner (`protocol.md` 3.4, P36): one when an
//! attention item opens, including the `merge` item for a PR ready for the
//! owner's word, and never for a routine turn.
//!
//! The record comes first. `notification.record` writes `attention.notified`
//! for the item, once, and only a call that applied sends: a restart or a
//! second pass finds the event and stays quiet, and a send that fails is
//! logged, never repeated. The record only says the daemon tried, so the send
//! then writes its outcome: `attention.notify_sent` (`notification.sent`) when
//! it went out, `attention.notify_failed` (`notification.fail`) when the
//! send failed. A crash between the record and the outcome leaves the record
//! alone, and the card timeline reads it as a try. Only items opened within
//! [`NEW_MINUTES`] and still current count as new, so a daemon started on an
//! old database does not announce its backlog.
//!
//! The send goes through [`Notify`]: the owner's `[notify] command`
//! ([`Command`], `terminal-holder.md` 7).

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serde_json::json;
use tiller_core::domain::{Attention, AttentionKind, Card, Host, HostKind, Project};
use tiller_core::state::attention::stale;
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_poll::{PollError, Source};

/// How long after it opens an attention item still counts as new.
pub const NEW_MINUTES: i64 = 60;

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The attention items a notification was sent for.
    pub notified: Vec<String>,
    /// `(item, error)` for each send that failed.
    pub failed: Vec<(String, String)>,
}

/// How long the notify command has to exit 0 (`terminal-holder.md` 7).
pub const COMMAND_SECS: u64 = 10;

/// How long after the notify command exits its stderr may still arrive.
const STDERR_GRACE: Duration = Duration::from_millis(200);

/// How much of the notify command's stderr an error keeps.
const STDERR_BYTES: usize = 64 * 1024;

/// One notification for the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub title: String,
    pub message: String,
    /// The project's live orchestrator, which a click opens.
    pub session_id: Option<String>,
    /// That orchestrator's pane.
    pub pane: Option<String>,
    /// The project's checkout.
    pub cwd: Option<String>,
    /// The label of the host the notifier runs on.
    pub host: String,
}

/// Sends a notification to the owner. The error is what the owner's log
/// and the card timeline show.
pub trait Notify {
    fn notify(&self, note: &Note) -> Result<(), String>;
}

/// The owner's `[notify] command`: the note goes to its stdin as a Claude
/// Code `Notification` hook event, and exit 0 within [`COMMAND_SECS`] is a
/// send.
#[derive(Clone, Debug)]
pub struct Command {
    argv: Vec<String>,
    limit: Duration,
}

impl Command {
    /// None when `argv` names no program.
    pub fn new(argv: Vec<String>) -> Option<Self> {
        (!argv.is_empty() && !argv[0].is_empty()).then_some(Self {
            argv,
            limit: Duration::from_secs(COMMAND_SECS),
        })
    }

    /// A shorter limit, for tests.
    #[must_use]
    pub fn with_limit(mut self, limit: Duration) -> Self {
        self.limit = limit;
        self
    }
}

impl Notify for Command {
    fn notify(&self, note: &Note) -> Result<(), String> {
        let payload = json!({
            "hook_event_name": "Notification",
            "notification_type": "tiller",
            "title": note.title,
            "message": note.message,
            "session_id": note.session_id,
            "cwd": note.cwd,
        });
        let mut command = Process::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .env(
                "TILLER_SESSION_ID",
                note.session_id.as_deref().unwrap_or(""),
            )
            .env("TILLER_PANE", note.pane.as_deref().unwrap_or(""))
            .env("TILLER_HOST", &note.host)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(cwd) = note
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
        {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("{} did not start: {error}", self.argv[0]))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A command that exits without reading its stdin is its own
            // business: its exit status decides.
            let payload = payload.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(payload.as_bytes());
            });
        }
        let stderr = child.stderr.take();
        let text = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = mpsc::channel();
        let shared = Arc::clone(&text);
        std::thread::spawn(move || {
            if let Some(mut stderr) = stderr {
                let mut chunk = [0; 4096];
                while let Ok(read @ 1..) = stderr.read(&mut chunk) {
                    let mut text = shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let room = STDERR_BYTES.saturating_sub(text.len());
                    text.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
            let _ = sender.send(());
        });
        let deadline = Instant::now() + self.limit;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        // A process the command left behind can hold stderr open long after
        // it exits, so the wait for the text ends with the limit.
        let grace = deadline
            .saturating_duration_since(Instant::now())
            .min(STDERR_GRACE);
        let _ = receiver.recv_timeout(grace);
        let stderr = String::from_utf8_lossy(&text.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned();
        let stderr = stderr.trim();
        match status {
            Some(status) if status.success() => Ok(()),
            Some(status) if stderr.is_empty() => Err(format!("{} {status}", self.argv[0])),
            Some(status) => Err(format!("{} {status}: {stderr}", self.argv[0])),
            None => Err(format!(
                "{} did not exit within {} s",
                self.argv[0],
                self.limit.as_secs()
            )),
        }
    }
}

impl<N: Notify + ?Sized> Notify for Box<N> {
    fn notify(&self, note: &Note) -> Result<(), String> {
        (**self).notify(note)
    }
}

/// The notification loop for the local host.
pub struct Notifier<N: Notify> {
    store: Store,
    notify: N,
    checkouts: BTreeMap<String, PathBuf>,
}

impl<N: Notify> Notifier<N> {
    pub fn new(store: Store, notify: N) -> Self {
        Self {
            store,
            notify,
            checkouts: BTreeMap::new(),
        }
    }

    /// Each project's local checkout, by project id: a note's `cwd`.
    #[must_use]
    pub fn with_checkouts(mut self, checkouts: BTreeMap<String, PathBuf>) -> Self {
        self.checkouts = checkouts;
        self
    }

    /// One pass: a notification for every new open item not yet notified.
    pub fn pass(&mut self, now: Timestamp) -> tiller_core::Result<Report> {
        let since = now.plus_minutes(-NEW_MINUTES);
        let items: Vec<Attention> = list(
            self.store.conn(),
            "answered_at IS NULL AND withdrawn_at IS NULL
               AND id IN (SELECT entity_id FROM events
                          WHERE kind = 'attention.opened' AND at >= ?1)
               AND id NOT IN (SELECT entity_id FROM events WHERE kind = 'attention.notified')
             ORDER BY opened_at, id",
            &[&since],
        )?;
        let ctx = Ctx::at(Actor::Daemon, now);
        let mut report = Report::default();
        for item in items {
            if stale(self.store.conn(), &item)?.is_some() {
                continue;
            }
            let note = note(&self.store, &self.checkouts, &item)?;
            if self.store.notification_record(&ctx, &item.id)?.replayed {
                continue;
            }
            // The send happened (or was refused) either way: an outcome
            // that cannot be written is logged, and the pass goes on to the
            // other items, which it would otherwise never notify.
            match self.notify.notify(&note) {
                Ok(()) => {
                    if let Err(error) = self.store.notification_sent(&ctx, &item.id) {
                        tracing::warn!(item = %item.id, %error, "recording a sent notification failed");
                    }
                    report.notified.push(item.id);
                }
                Err(error) => {
                    tracing::warn!(item = %item.id, %error, "owner notification failed");
                    if let Err(write) = self.store.notification_fail(&ctx, &item.id, &error) {
                        tracing::warn!(item = %item.id, error = %write, "recording a failed notification failed");
                    }
                    report.failed.push((item.id, error));
                }
            }
        }
        Ok(report)
    }
}

/// The notification for `item`: the project and the card it names, and the
/// project's orchestrator for a click to open.
fn note(
    store: &Store,
    checkouts: &BTreeMap<String, PathBuf>,
    item: &Attention,
) -> tiller_core::Result<Note> {
    let conn = store.conn();
    let card: Card = get(conn, "card", &item.card_id)?;
    let project: Project = get(conn, "project", &card.project_id)?;
    let orchestrator = live_orchestrator(conn, &project.id)?;
    let hosts: Vec<Host> = list(conn, "1 = 1 ORDER BY id", &[])?;
    let host = hosts
        .into_iter()
        .find(|host| host.kind == HostKind::Local)
        .map(|host| host.label)
        .unwrap_or_default();
    let what = match item.kind {
        AttentionKind::Merge => "ready for your merge",
        AttentionKind::Decision => "needs a decision",
        AttentionKind::Credential => "needs a credential",
        AttentionKind::Review => "has a review for you",
        AttentionKind::Recovery => "needs recovery",
        AttentionKind::Report => "has a report",
    };
    Ok(Note {
        title: format!("{}: {what}", project.name),
        message: format!("[{}] {}", card.display_id, item.question),
        session_id: orchestrator.as_ref().map(|session| session.id.clone()),
        pane: orchestrator.and_then(|session| session.pane_id),
        cwd: checkouts
            .get(&project.id)
            .map(|path| path.display().to_string()),
        host,
    })
}

impl<N: Notify + Send + 'static> Source for Notifier<N> {
    fn name(&self) -> String {
        "notify".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        self.pass(now).map(drop).map_err(PollError::State)
    }
}
