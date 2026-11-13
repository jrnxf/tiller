//! The terminal mirror (`protocol.md` 7.4, Terminal mirror): what every
//! view of a pane shares, whichever pane it shows (`terminal-holder.md`
//! 2.6). Views are counted against one cap, the owner's inputs spend one
//! budget, and each session's prompt answers take their turn. The stream
//! itself is the holder's ([`crate::holder_mirror`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use axum::body::Bytes;
use tokio::sync::{OwnedMutexGuard, mpsc, watch};

use crate::dto::TerminalLine;

/// Terminal views open at once across the board, every tab included.
pub const MAX_VIEWERS: usize = 8;
/// The most keys one input carries.
pub const MAX_KEYS: usize = 64;
/// The most bytes of pasted text one input carries.
pub const MAX_TEXT: usize = 4096;
/// The most rows of scrollback one history read returns.
pub const MAX_HISTORY: u16 = 1000;
/// Inputs a second the owner may send on average, and in a burst.
const INPUT_RATE: f64 = 20.0;
const INPUT_BURST: f64 = 40.0;
/// Lines waiting for a browser before its stream is dropped.
pub(crate) const QUEUE: usize = 64;
/// The sizes a Fit may ask for, in cells.
const FIT_COLS: std::ops::RangeInclusive<u16> = 20..=500;
const FIT_ROWS: std::ops::RangeInclusive<u16> = 5..=200;

/// The mirror's shared state: open views, the input budget, the Fits as
/// size leases on holder panes, and the lock each session's prompt answers
/// take in turn.
#[derive(Debug)]
pub struct Mirrors {
    viewers: Arc<AtomicUsize>,
    budget: Mutex<Budget>,
    leases: Arc<crate::holder_mirror::Leases>,
    answers: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// A token bucket: `INPUT_RATE` inputs a second, `INPUT_BURST` at once.
#[derive(Debug)]
struct Budget {
    tokens: f64,
    at: Instant,
}

/// One open view, counted until it drops.
#[derive(Debug)]
pub struct Viewer(Arc<AtomicUsize>);

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Default for Mirrors {
    fn default() -> Self {
        Self::new()
    }
}

impl Mirrors {
    pub fn new() -> Self {
        Self {
            viewers: Arc::new(AtomicUsize::new(0)),
            budget: Mutex::new(Budget {
                tokens: INPUT_BURST,
                at: Instant::now(),
            }),
            leases: Arc::default(),
            answers: Mutex::new(HashMap::new()),
        }
    }

    /// Waits for the session's turn to answer its prompt: one answer at a
    /// time, so a second tap reads the screen only after the first one's
    /// keys went. The turn ends when the guard drops.
    pub async fn answer_turn(&self, session: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
            // A lock no answer holds or waits on is dropped, so the map
            // keeps only the sessions answering now.
            answers.retain(|_, lock| Arc::strong_count(lock) > 1);
            answers.entry(session.to_owned()).or_default().clone()
        };
        lock.lock_owned().await
    }

    /// The Fits on holder panes, each a size lease (`terminal-holder.md`
    /// 2.6).
    pub fn leases(&self) -> Arc<crate::holder_mirror::Leases> {
        self.leases.clone()
    }

    /// Gives the newest lease on `pane` the board's new size. False when no
    /// view holds a Fit on it.
    pub fn refit(&self, pane: &str, fit: (u16, u16)) -> bool {
        self.leases.refit(&tiller_panes::PaneId::from(pane), fit)
    }

    /// A slot for one more view, or `None` at the cap.
    pub fn viewer(&self) -> Option<Viewer> {
        self.viewers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < MAX_VIEWERS).then_some(open + 1)
            })
            .ok()
            .map(|_| Viewer(self.viewers.clone()))
    }

    /// Views open now.
    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::Acquire)
    }

    /// Spends one input from the budget; false when it is spent.
    pub fn spend(&self) -> bool {
        self.spend_at(Instant::now())
    }

    fn spend_at(&self, now: Instant) -> bool {
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        let earned = now.saturating_duration_since(budget.at).as_secs_f64() * INPUT_RATE;
        budget.tokens = (budget.tokens + earned).min(INPUT_BURST);
        budget.at = now;
        if budget.tokens < 1.0 {
            return false;
        }
        budget.tokens -= 1.0;
        true
    }
}

/// Why an input is refused before it reaches the holder.
pub fn check_keys(keys: &[String]) -> Result<(), String> {
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return Err(format!("keys holds 1 to {MAX_KEYS} keys"));
    }
    match keys.iter().find(|key| !plausible_key(key)) {
        Some(key) => Err(format!("{key:?} is not a key")),
        None => Ok(()),
    }
}

/// A key the holder might know: a name or one character, after any
/// modifiers, with no space or control character. The holder refuses the
/// rest itself. A word that starts with a dash reads as a flag on a command
/// line, so only `-` itself (the minus key) may start with one.
fn plausible_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && (key == "-" || !key.starts_with('-'))
        && !key.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn check_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(format!("text holds 1 to {MAX_TEXT} bytes"));
    }
    Ok(())
}

/// Why a Fit's size is refused before it reaches the holder.
pub fn check_fit(cols: u16, rows: u16) -> Result<(u16, u16), String> {
    if !FIT_COLS.contains(&cols) || !FIT_ROWS.contains(&rows) {
        return Err(format!(
            "fit is {} to {} cols by {} to {} rows",
            FIT_COLS.start(),
            FIT_COLS.end(),
            FIT_ROWS.start(),
            FIT_ROWS.end()
        ));
    }
    Ok((cols, rows))
}

/// Returns once the daemon is stopping; never when nothing can stop it.
pub(crate) async fn stopping(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Queues one NDJSON line without waiting; false when the browser left or
/// fell a whole queue behind, which ends its stream.
pub(crate) fn queue(tx: &mpsc::Sender<Bytes>, message: &TerminalLine) -> bool {
    let Ok(mut line) = serde_json::to_vec(message) else {
        return false;
    };
    line.push(b'\n');
    tx.try_send(Bytes::from(line)).is_ok()
}

pub(crate) fn unavailable(tx: &mpsc::Sender<Bytes>, message: &str) {
    let message = message.to_owned();
    queue(tx, &TerminalLine::Unavailable { message });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_input_budget_refills_at_its_rate() {
        let mirrors = Mirrors::new();
        let start = Instant::now();
        let spent = (0..100).filter(|_| mirrors.spend_at(start)).count();
        assert_eq!(spent, INPUT_BURST as usize);
        // Half a second earns ten more.
        let later = start + Duration::from_millis(500);
        let spent = (0..100).filter(|_| mirrors.spend_at(later)).count();
        assert_eq!(spent, 10);
    }

    #[test]
    fn the_viewer_cap_holds_and_frees() {
        let mirrors = Mirrors::new();
        let open: Vec<_> = (0..MAX_VIEWERS).filter_map(|_| mirrors.viewer()).collect();
        assert_eq!(open.len(), MAX_VIEWERS);
        assert!(mirrors.viewer().is_none());
        drop(open);
        assert_eq!(mirrors.viewers(), 0);
        assert!(mirrors.viewer().is_some());
    }

    #[test]
    fn keys_are_words_and_text_is_bounded() {
        let keys = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert!(check_keys(&keys(&["ctrl+c", "-", "a", "enter"])).is_ok());
        assert!(check_keys(&keys(&[])).is_err());
        assert!(check_keys(&keys(&["a b"])).is_err());
        assert!(check_keys(&keys(&["\u{1b}"])).is_err());
        assert!(check_keys(&keys(&["a", "--session", "other"])).is_err());
        assert!(check_keys(&keys(&["-h"])).is_err());
        assert!(check_keys(&vec!["a".to_owned(); MAX_KEYS + 1]).is_err());
        assert!(check_text("-rf").is_ok());
        assert!(check_text("").is_err());
        assert!(check_text(&"x".repeat(MAX_TEXT + 1)).is_err());
    }
}
