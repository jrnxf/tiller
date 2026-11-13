//! A holder whose panes each run a stand-in Claude Code, for tests (feature
//! `testing`). [`FakeClaude`] implements [`Panes`] and keeps its world in a
//! JSON file, so a child process killed mid-call leaves exactly what a real
//! pane would show, and a test's other processes share the same panes.
//!
//! The screens are drawn the way Claude Code 2.1 draws them (see
//! `crates/panes/tests/fixtures/claude`), so delivery and control read them
//! through the real [`composer::classify`](crate::composer::classify). Typed
//! text lands in the composer, Enter submits it, Escape ends a turn, and
//! `/exit` ends the pane's child.
//!
//! [`FakePanes`](crate::FakePanes) is the in-memory holder with the live
//! streams; this one has no `Live` side.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{PanesError, Result};
use crate::keys::Key;
use crate::message::{
    Cursor, DEFAULT_SIZE, ErrorCode, Exit, Format, Listing, Meta, Pane, PaneId, PaneState, Read,
    Screen as ScreenText, Signal, Spawn,
};
use crate::panes::Panes;
use crate::testing::{Fleet, Place};

/// Set to `<point>:<n>:<marker path>`, makes the `n`th time the fake reaches
/// `<point>` (counted across processes, from 1) write the marker and hang,
/// so a test can kill the process there. A point is a call's verb (before
/// it does anything, such as `input` or `keys`) or `<verb>.after` for
/// `input` and `keys`.
pub const PAUSE_ENV: &str = "TILLER_FAKE_CLAUDE_PAUSE";

/// The first fake pid, above any real one, as [`FakePanes`](crate::FakePanes)
/// has it.
const FIRST_PID: u32 = 1 << 31;

/// What a running pane's Claude shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Screen {
    /// Claude at its composer.
    Idle,
    /// Claude mid-turn.
    Working,
    /// Claude's permission dialog.
    Permission,
}

/// One pane: the holder's record and what its Claude holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakePane {
    pub pane: Pane,
    pub screen: Screen,
    /// What the composer holds.
    pub composer: String,
    /// Lines submitted with Enter, oldest first.
    pub submitted: Vec<String>,
}

impl FakePane {
    pub fn running(&self) -> bool {
        self.pane.state == PaneState::Running
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct World {
    instance: String,
    panes: Vec<FakePane>,
    next_num: u32,
    next_pid: u32,
    unreachable: bool,
    /// Verbs the holder answers with an error.
    refused: Vec<String>,
    calls: Vec<String>,
    counts: BTreeMap<String, u32>,
    /// Panes whose Claude takes `/exit` and stays up.
    stubborn: Vec<String>,
    /// Enters each pane's Claude reads as a newline in the composer, as one
    /// just resumed does with an Enter sent right after typed text.
    newline_enters: BTreeMap<String, u32>,
    attach_clients: usize,
}

/// A handle on the fake holder's file. Clones share the holder.
#[derive(Clone, Debug)]
pub struct FakeClaude {
    path: PathBuf,
}

impl FakeClaude {
    /// A new, empty holder in `path`, or the one already there.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let fake = Self { path: path.into() };
        if !fake.path.exists() {
            fake.locked(|| {
                fake.save(&World {
                    instance: ulid::Ulid::generate().to_string(),
                    next_num: 1,
                    next_pid: FIRST_PID,
                    ..World::default()
                });
            });
        }
        fake
    }

    /// A handle on an existing fake holder's file.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> World {
        self.locked(|| self.read())
    }

    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn read(&self) -> World {
        serde_json::from_slice(&std::fs::read(&self.path).expect("fake holder state")).unwrap()
    }

    /// Runs `f` holding the holder's lock, as the holder serves one request
    /// at a time: a test's own threads and processes share the file.
    #[allow(clippy::expect_used)]
    fn locked<T>(&self, f: impl FnOnce() -> T) -> T {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))
            .expect("open fake holder lock");
        lock.lock().expect("lock fake holder");
        f()
    }

    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn save(&self, world: &World) {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(world).unwrap()).expect("write fake holder");
        std::fs::rename(tmp, &self.path).expect("replace fake holder");
    }

    fn edit<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        self.locked(|| {
            let mut world = self.read();
            let out = f(&mut world);
            self.save(&world);
            out
        })
    }

    /// Counts a pause point and hangs there when [`PAUSE_ENV`] names it.
    fn pause_point(&self, name: &str) {
        let n = self.edit(|w| {
            let n = w.counts.entry(name.to_owned()).or_default();
            *n += 1;
            *n
        });
        if let Ok(spec) = std::env::var(PAUSE_ENV)
            && let Some((point, rest)) = spec.split_once(':')
            && let Some((nth, marker)) = rest.split_once(':')
            && point == name
            && nth.parse() == Ok(n)
        {
            let _ = std::fs::write(marker, name);
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }

    /// Logs a call as `<verb> <args>` and fails while unreachable or when
    /// the verb is refused. The pause point before a call is its verb.
    fn call(&self, verb: &'static str, detail: String) -> Result<()> {
        self.pause_point(verb);
        let world = self.load();
        if world.unreachable {
            return Err(PanesError::Unreachable("the fake holder is down".into()));
        }
        if world.refused.iter().any(|r| r == verb) {
            return Err(PanesError::Refused {
                request: verb,
                code: ErrorCode::Internal,
                message: "refused by the fake".into(),
            });
        }
        self.edit(|w| w.calls.push(detail));
        Ok(())
    }

    /// A running pane `pane_id` for session `key`, whose Claude shows
    /// `screen`.
    pub fn add_pane(&self, pane_id: &str, key: &str, screen: Screen) -> PaneId {
        let id = PaneId::from(pane_id);
        self.edit(|w| {
            let pane = w.record(id.clone(), key, Vec::new(), String::new(), Meta::new());
            w.panes.push(FakePane {
                pane,
                screen,
                composer: String::new(),
                submitted: Vec::new(),
            });
        });
        id
    }

    /// Drops a pane from the holder, as if it was never there.
    pub fn remove_pane(&self, pane_id: &str) {
        self.edit(|w| w.panes.retain(|p| p.pane.pane.as_str() != pane_id));
    }

    pub fn set_screen(&self, pane_id: &str, screen: Screen) {
        self.with(pane_id, |p| p.screen = screen);
    }

    /// Merges `meta` into the pane's metadata, as a spawn sets it.
    pub fn set_meta(&self, pane_id: &str, meta: &[(&str, &str)]) {
        self.with(pane_id, |p| {
            for (k, v) in meta {
                p.pane.meta.insert((*k).to_owned(), (*v).to_owned());
            }
        });
    }

    /// When the pane last printed: `quiet_for` counts from it.
    pub fn set_last_output(&self, pane_id: &str, at: Option<Timestamp>) {
        self.with(pane_id, |p| p.pane.last_output_at = at);
    }

    /// The owner types into the composer.
    pub fn type_draft(&self, pane_id: &str, text: &str) {
        self.with(pane_id, |p| p.composer.push_str(text));
    }

    /// The pane's child exits with `code`, as Claude does after `/exit`.
    pub fn exit(&self, pane_id: &str, code: i32) {
        self.with(pane_id, |p| end(p, Some(code), None));
    }

    /// Makes the Claude in `pane_id` ignore `/exit`.
    pub fn ignore_exit(&self, pane_id: &str) {
        self.edit(|w| w.stubborn.push(pane_id.into()));
    }

    /// Makes the next `n` Enters that reach text in `pane_id`'s composer add
    /// a newline instead of submitting it.
    pub fn enter_as_newline(&self, pane_id: &str, n: u32) {
        self.edit(|w| {
            w.newline_enters.insert(pane_id.into(), n);
        });
    }

    pub fn set_unreachable(&self, down: bool) {
        self.edit(|w| w.unreachable = down);
    }

    /// Makes the holder answer every call of `verb` (such as `input`) with
    /// an error.
    pub fn refuse(&self, verb: &str) {
        self.edit(|w| w.refused.push(verb.into()));
    }

    /// Sets how many attach clients `switch` tells.
    pub fn set_attach_clients(&self, clients: usize) {
        self.edit(|w| w.attach_clients = clients);
    }

    /// The holder `instance` every listing names.
    pub fn instance(&self) -> String {
        self.load().instance
    }

    #[allow(clippy::expect_used)]
    pub fn fake_pane(&self, pane_id: &str) -> FakePane {
        self.load()
            .panes
            .into_iter()
            .find(|p| p.pane.pane.as_str() == pane_id)
            .expect("fake pane")
    }

    /// Every pane, in spawn order.
    pub fn panes(&self) -> Vec<FakePane> {
        self.load().panes
    }

    /// Every call that reached the holder, oldest first, as `<verb> <args>`.
    pub fn calls(&self) -> Vec<String> {
        self.load().calls
    }

    fn with(&self, pane_id: &str, f: impl FnOnce(&mut FakePane)) {
        self.edit(|w| {
            if let Some(p) = w.find(pane_id) {
                f(p);
            }
        });
    }

    fn with_pane<T>(&self, pane_id: &str, f: impl FnOnce(&mut FakePane) -> Result<T>) -> Result<T> {
        self.edit(|w| match w.find(pane_id) {
            Some(p) => f(p),
            None => Err(PanesError::NotFound(pane_id.to_owned())),
        })
    }

    /// One key, as Claude reads it.
    fn press(&self, pane_id: &str, key: &Key) -> Result<()> {
        let name = key.to_string();
        self.edit(|w| {
            let stubborn = w.stubborn.iter().any(|p| p == pane_id);
            let newline = w.newline_enters.get(pane_id).copied().unwrap_or(0);
            let p = w
                .find(pane_id)
                .ok_or_else(|| PanesError::NotFound(pane_id.to_owned()))?;
            if !p.running() {
                return Err(PanesError::Exited(pane_id.to_owned()));
            }
            let mut used_newline = false;
            match name.as_str() {
                "enter" if p.screen == Screen::Idle && !p.composer.is_empty() => {
                    if newline > 0 {
                        used_newline = true;
                        p.composer.push('\n');
                    } else {
                        let line = std::mem::take(&mut p.composer);
                        // Claude's `/exit` ends the child.
                        let exits = line.trim() == "/exit" && !stubborn;
                        p.submitted.push(line);
                        if exits {
                            end(p, Some(0), None);
                        }
                    }
                }
                "esc" if p.screen == Screen::Working => p.screen = Screen::Idle,
                _ => {}
            }
            if used_newline && let Some(n) = w.newline_enters.get_mut(pane_id) {
                *n -= 1;
            }
            Ok(())
        })
    }
}

impl World {
    fn find(&mut self, pane_id: &str) -> Option<&mut FakePane> {
        self.panes
            .iter_mut()
            .find(|p| p.pane.pane.as_str() == pane_id)
    }

    fn record(
        &mut self,
        pane: PaneId,
        key: &str,
        argv: Vec<String>,
        cwd: String,
        meta: Meta,
    ) -> Pane {
        let record = Pane {
            pane,
            num: self.next_num,
            key: key.to_owned(),
            pid: self.next_pid,
            argv,
            cwd,
            started_at: Timestamp::now(),
            state: PaneState::Running,
            exit: None,
            cols: DEFAULT_SIZE.cols,
            rows: DEFAULT_SIZE.rows,
            meta,
            title: String::new(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 0,
        };
        self.next_num += 1;
        self.next_pid += 1;
        record
    }
}

/// Records the exit of a running pane's child.
fn end(pane: &mut FakePane, code: Option<i32>, signal: Option<i32>) {
    if !pane.running() {
        return;
    }
    pane.pane.state = PaneState::Exited;
    pane.pane.exit = Some(Exit {
        code,
        signal,
        at: Timestamp::now(),
    });
}

/// What the pane's screen shows, top row first.
fn render(pane: &FakePane) -> String {
    if !pane.running() {
        return "Resume this session with:\nclaude --resume 0\n".to_owned();
    }
    pane.screen.draw(&pane.composer)
}

impl Screen {
    /// What a running pane's Claude shows on this screen at an empty
    /// composer, with its colours, top row first.
    pub fn ansi(self) -> String {
        self.draw("")
    }

    fn draw(self, composer: &str) -> String {
        let rule = "\u{1b}[38;5;244m".to_owned() + &"─".repeat(80) + "\u{1b}[39m";
        match self {
            Screen::Permission => [
                "╭──────────────────────────────────────────────╮",
                "│ Bash command                                 │",
                "│   cargo test                                 │",
                "│ Do you want to proceed?                      │",
                "│ ❯ 1. Yes                                     │",
                "│   2. No, and tell Claude what to do          │",
                "╰──────────────────────────────────────────────╯",
                "   Esc to cancel",
                "",
            ]
            .join("\n"),
            Screen::Idle | Screen::Working => {
                let footer = if self == Screen::Working {
                    "  \u{1b}[38;5;246m✻ Thinking… (esc to interrupt)\u{1b}[39m"
                } else {
                    "  \u{1b}[2m? for shortcuts\u{1b}[22m"
                };
                let draft = if composer.is_empty() {
                    "\u{1b}[2mTry \"fix the tests\"\u{1b}[22m".to_owned()
                } else {
                    composer.to_owned()
                };
                format!("● Done.\n\n{rule}\n❯\u{a0}{draft}\n{rule}\n{footer}\n")
            }
        }
    }
}

const SIGHUP: i32 = 1;
const SIGKILL: i32 = 9;

impl Panes for FakeClaude {
    /// A new pane's Claude starts idle at an empty composer.
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        self.call("spawn", format!("spawn {} {}", req.key, req.argv.join(" ")))?;
        if !req.argv.first().is_some_and(|a| Path::new(a).is_absolute()) {
            return Err(PanesError::Refused {
                request: "spawn",
                code: ErrorCode::BadRequest,
                message: "argv[0] must be an absolute path".into(),
            });
        }
        Ok(self.edit(|w| {
            if let Some(running) = w
                .panes
                .iter()
                .find(|p| p.pane.key == req.key && p.running())
            {
                return running.pane.clone();
            }
            let pane = w.record(
                PaneId::generate(),
                &req.key,
                req.argv.clone(),
                req.cwd.clone(),
                req.meta.clone(),
            );
            w.panes.push(FakePane {
                pane: pane.clone(),
                screen: Screen::Idle,
                composer: String::new(),
                submitted: Vec::new(),
            });
            pane
        }))
    }

    fn list(&self) -> Result<Listing> {
        self.call("list", "list".into())?;
        let world = self.load();
        Ok(Listing {
            instance: world.instance,
            panes: world.panes.into_iter().map(|p| p.pane).collect(),
        })
    }

    fn get(&self, pane: &PaneId) -> Result<Option<Pane>> {
        self.call("get", format!("get {pane}"))?;
        Ok(self
            .load()
            .panes
            .into_iter()
            .find(|p| p.pane.pane == *pane)
            .map(|p| p.pane))
    }

    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<ScreenText> {
        self.call("screen", format!("screen {pane} {read:?} {format:?}"))?;
        self.with_pane(pane.as_str(), |p| {
            Ok(ScreenText {
                cols: p.pane.cols,
                rows: p.pane.rows,
                cursor: Cursor {
                    col: 0,
                    row: 0,
                    visible: true,
                },
                modes: Vec::new(),
                rows_text: render(p).split('\n').map(str::to_owned).collect(),
            })
        })
    }

    fn input(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.call("input", format!("input {pane} {text}"))?;
        self.with_pane(pane.as_str(), |p| {
            if !p.running() {
                return Err(PanesError::Exited(pane.to_string()));
            }
            p.composer.push_str(text);
            Ok(())
        })?;
        self.pause_point("input.after");
        Ok(())
    }

    fn paste(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.call("paste", format!("paste {pane} {text}"))?;
        self.with_pane(pane.as_str(), |p| {
            if !p.running() {
                return Err(PanesError::Exited(pane.to_string()));
            }
            p.composer.push_str(text);
            Ok(())
        })
    }

    /// Each key is one keystroke, as the holder writes them in order.
    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        let names: Vec<String> = keys.iter().map(Key::to_string).collect();
        self.call("keys", format!("keys {pane} {}", names.join(" ")))?;
        for key in keys {
            self.press(pane.as_str(), key)?;
        }
        self.pause_point("keys.after");
        Ok(())
    }

    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane> {
        self.call("meta", format!("meta {pane} {set:?} {remove:?}"))?;
        self.with_pane(pane.as_str(), |p| {
            p.pane.meta.extend(set.clone());
            for key in remove {
                p.pane.meta.remove(*key);
            }
            Ok(p.pane.clone())
        })
    }

    /// `kill` ends the child; any other signal is only logged.
    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()> {
        self.call("signal", format!("signal {pane} {signal:?}"))?;
        self.with_pane(pane.as_str(), |p| {
            if !p.running() {
                return Err(PanesError::Exited(pane.to_string()));
            }
            if signal == Signal::Kill {
                end(p, None, Some(SIGKILL));
            }
            Ok(())
        })
    }

    /// The child ends at once, by `hup`. An exited pane answers with its
    /// exit.
    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit> {
        self.call("close", format!("close {pane} {}", grace.as_millis()))?;
        self.with_pane(pane.as_str(), |p| {
            end(p, None, Some(SIGHUP));
            p.pane
                .exit
                .ok_or_else(|| PanesError::Protocol("an ended pane has no exit".into()))
        })
    }

    fn forget(&self, pane: &PaneId) -> Result<()> {
        self.call("forget", format!("forget {pane}"))?;
        self.edit(|w| {
            let at = w
                .panes
                .iter()
                .position(|p| p.pane.pane == *pane)
                .ok_or_else(|| PanesError::NotFound(pane.to_string()))?;
            if w.panes[at].running() {
                return Err(PanesError::Refused {
                    request: "forget",
                    code: ErrorCode::BadRequest,
                    message: format!("{pane} is running"),
                });
            }
            w.panes.remove(at);
            Ok(())
        })
    }

    fn switch(&self, pane: &PaneId) -> Result<usize> {
        self.call("switch", format!("switch {pane}"))?;
        let world = self.load();
        match world.panes.iter().find(|p| p.pane.pane == *pane) {
            None => Err(PanesError::NotFound(pane.to_string())),
            Some(p) if !p.running() => Err(PanesError::Exited(pane.to_string())),
            Some(_) => Ok(world.attach_clients),
        }
    }
}

/// Lays the fleet out as the daemon would have spawned it: the
/// orchestrator's pane, an unrelated shell that already exited, then each
/// worker's pane, every one keyed by its session and idle.
impl Place for FakeClaude {
    fn place(&self, fleet: &Fleet) {
        self.add_pane(
            &fleet.orchestrator_pane,
            &fleet.orchestrator_session,
            Screen::Idle,
        );
        self.add_pane("w3:p1", "notes", Screen::Idle);
        self.exit("w3:p1", 0);
        for (session, _, pane) in &fleet.workers {
            self.add_pane(pane, session, Screen::Idle);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::composer::{Composer, classify};

    fn fake() -> (tempfile::TempDir, FakeClaude) {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeClaude::new(dir.path().join("holder.json"));
        (dir, fake)
    }

    fn read(fake: &FakeClaude, pane: &PaneId) -> Composer {
        classify(
            &fake
                .screen(pane, Read::Visible, Format::Ansi)
                .unwrap()
                .text(),
        )
    }

    #[test]
    fn typed_text_sits_in_the_composer_until_enter_submits_it() {
        let (_dir, fake) = fake();
        let pane = fake.add_pane("p1", "S1", Screen::Idle);
        assert_eq!(read(&fake, &pane), Composer::Empty);
        fake.input(&pane, "hello").unwrap();
        assert_eq!(read(&fake, &pane), Composer::Holds("hello".into()));
        fake.keys(&pane, &["enter".parse().unwrap()]).unwrap();
        assert_eq!(read(&fake, &pane), Composer::Empty);
        assert_eq!(fake.fake_pane("p1").submitted, ["hello"]);
        let typed: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|c| !c.starts_with("screen"))
            .collect();
        assert_eq!(typed, ["input p1 hello", "keys p1 enter"]);
    }

    #[test]
    fn escape_ends_a_turn_and_exit_ends_the_child() {
        let (_dir, fake) = fake();
        let pane = fake.add_pane("p1", "S1", Screen::Working);
        assert_eq!(read(&fake, &pane), Composer::Busy);
        fake.keys(&pane, &["esc".parse().unwrap()]).unwrap();
        assert_eq!(read(&fake, &pane), Composer::Empty);
        fake.input(&pane, "/exit").unwrap();
        fake.keys(&pane, &["enter".parse().unwrap()]).unwrap();
        let record = fake.get(&pane).unwrap().unwrap();
        assert_eq!(record.state, PaneState::Exited);
        assert_eq!(record.exit.map(|e| e.code), Some(Some(0)));
        assert!(fake.input(&pane, "more").is_err());
    }

    #[test]
    fn a_spawn_for_a_running_key_returns_its_pane_and_a_clone_shares_the_holder() {
        let (_dir, fake) = fake();
        let req = Spawn {
            key: "S1".into(),
            argv: vec!["/bin/sh".into()],
            cwd: "/".into(),
            env: BTreeMap::new(),
            size: None,
            meta: Meta::new(),
        };
        let first = fake.spawn(&req).unwrap();
        let other = FakeClaude::open(fake.path());
        assert_eq!(other.spawn(&req).unwrap().pane, first.pane);
        assert_eq!(other.list().unwrap().panes.len(), 1);
    }
}
