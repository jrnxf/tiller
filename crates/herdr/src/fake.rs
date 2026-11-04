//! A herdr stand-in for tests (feature `testing`): workspaces, panes with a
//! Claude Code screen, and a log of every call, kept in a JSON file so a
//! child process killed mid-call leaves exactly what a real pane would show.
//!
//! The screens are drawn the way Claude Code 2.1 draws them (see
//! `tests/fixtures/claude`), so delivery reads them through the real
//! [`composer::classify`](crate::composer::classify).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::{Created, Herdr, HerdrError, PaneInfo, Result, Snapshot, WorkspaceInfo};

/// Set to `<point>:<n>:<marker path>`, makes the `n`th time the fake reaches
/// `<point>` (counted across processes, from 1) write the marker and hang,
/// so a test can kill the process there. A point is a call's name (before
/// it does anything) or `<name>.after` for `send_text`, `send_enter` and
/// `send_escape`.
pub const PAUSE_ENV: &str = "TILLER_FAKE_HERDR_PAUSE";

/// What a pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Screen {
    /// Claude at an empty composer.
    Idle,
    /// Claude mid-turn.
    Working,
    /// Claude's permission dialog.
    Permission,
    /// Claude exited; a shell prompt.
    Shell,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakePane {
    pub pane_id: String,
    pub workspace_id: String,
    pub screen: Screen,
    /// What the composer holds.
    pub composer: String,
    /// Lines submitted with Enter, oldest first.
    pub submitted: Vec<String>,
    pub tokens: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct World {
    workspaces: Vec<WorkspaceInfo>,
    panes: Vec<FakePane>,
    unreachable: bool,
    /// Calls the server answers with an error, as an older herdr does.
    refused: Vec<String>,
    calls: Vec<String>,
    counts: BTreeMap<String, u32>,
    /// Panes whose Claude takes `/exit` and stays up.
    #[serde(default)]
    stubborn: Vec<String>,
    /// Enters each pane's Claude reads as a newline in the composer, as one
    /// just relaunched in the same pane does with an Enter sent right after
    /// typed text.
    #[serde(default)]
    newline_enters: BTreeMap<String, u32>,
}

/// A handle on the fake server's file. Clones share the server.
#[derive(Clone, Debug)]
pub struct FakeHerdr {
    path: PathBuf,
}

impl FakeHerdr {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let fake = Self { path: path.into() };
        if !fake.path.exists() {
            fake.locked(|| fake.save(&World::default()));
        }
        fake
    }

    /// A handle on an existing fake server's file.
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
        serde_json::from_slice(&std::fs::read(&self.path).expect("fake herdr state")).unwrap()
    }

    /// Runs `f` holding the server's lock, as herdr serves one call at a
    /// time: a test's own threads and processes share the file.
    #[allow(clippy::expect_used)]
    fn locked<T>(&self, f: impl FnOnce() -> T) -> T {
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))
            .expect("open fake herdr lock");
        lock.lock().expect("lock fake herdr");
        f()
    }

    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn save(&self, world: &World) {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(world).unwrap()).expect("write fake herdr");
        std::fs::rename(tmp, &self.path).expect("replace fake herdr");
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
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    /// Logs a call and fails while unreachable. The pause point before a
    /// call is its name; the one after a call that changed a pane is
    /// `<name>.after`.
    fn call(&self, name: &str, detail: String) -> Result<()> {
        self.pause_point(name);
        let world = self.load();
        if world.unreachable {
            return Err(HerdrError::Unreachable("fake herdr is down".into()));
        }
        if world.refused.iter().any(|r| r == name) {
            return Err(HerdrError::Api {
                call: name.into(),
                code: "unknown_method".into(),
                message: "refused by the fake".into(),
            });
        }
        self.edit(|w| w.calls.push(detail));
        Ok(())
    }

    pub fn add_workspace(&self, id: &str, label: &str) {
        self.edit(|w| {
            w.workspaces.push(WorkspaceInfo {
                workspace_id: id.into(),
                label: label.into(),
            });
        });
    }

    pub fn add_pane(&self, workspace_id: &str, pane_id: &str, screen: Screen) {
        self.edit(|w| {
            w.panes.push(FakePane {
                pane_id: pane_id.into(),
                workspace_id: workspace_id.into(),
                screen,
                composer: String::new(),
                submitted: Vec::new(),
                tokens: BTreeMap::new(),
            });
        });
    }

    pub fn remove_pane(&self, pane_id: &str) {
        self.edit(|w| w.panes.retain(|p| p.pane_id != pane_id));
    }

    pub fn set_screen(&self, pane_id: &str, screen: Screen) {
        self.edit(|w| {
            if let Some(p) = w.panes.iter_mut().find(|p| p.pane_id == pane_id) {
                p.screen = screen;
            }
        });
    }

    /// The owner types into the composer.
    pub fn type_draft(&self, pane_id: &str, text: &str) {
        self.edit(|w| {
            if let Some(p) = w.panes.iter_mut().find(|p| p.pane_id == pane_id) {
                p.composer.push_str(text);
            }
        });
    }

    pub fn set_tokens(&self, pane_id: &str, tokens: BTreeMap<String, String>) {
        self.edit(|w| {
            if let Some(p) = w.panes.iter_mut().find(|p| p.pane_id == pane_id) {
                p.tokens = tokens;
            }
        });
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

    /// Makes the server answer every `call` (a call name such as `move`)
    /// with an error.
    pub fn refuse(&self, call: &str) {
        self.edit(|w| w.refused.push(call.into()));
    }

    /// A server restart: every token is lost and the workspaces come back in
    /// `order`.
    pub fn restart(&self, order: &[&str]) {
        self.edit(|w| {
            for p in &mut w.panes {
                p.tokens.clear();
            }
            w.workspaces
                .sort_by_key(|ws| order.iter().position(|id| *id == ws.workspace_id));
        });
    }

    #[allow(clippy::expect_used)]
    pub fn fake_pane(&self, pane_id: &str) -> FakePane {
        self.load()
            .panes
            .into_iter()
            .find(|p| p.pane_id == pane_id)
            .expect("fake pane")
    }

    /// Every call that reached the server, oldest first.
    pub fn calls(&self) -> Vec<String> {
        self.load().calls
    }

    fn render(pane: &FakePane) -> String {
        let rule = "\u{1b}[38;5;244m".to_owned() + &"─".repeat(80) + "\u{1b}[39m";
        match pane.screen {
            Screen::Shell => {
                "Resume this session with:\nclaude --resume 0\nhost ~ \n\u{1b}[38;5;5m❯\u{1b}[0m \n"
                    .to_owned()
            }
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
                let footer = if pane.screen == Screen::Working {
                    "  \u{1b}[38;5;246m✻ Thinking… (esc to interrupt)\u{1b}[39m"
                } else {
                    "  \u{1b}[2m? for shortcuts\u{1b}[22m"
                };
                let draft = if pane.composer.is_empty() {
                    "\u{1b}[2mTry \"fix the tests\"\u{1b}[22m".to_owned()
                } else {
                    pane.composer.clone()
                };
                format!("● Done.\n\n{rule}\n❯\u{a0}{draft}\n{rule}\n{footer}\n")
            }
        }
    }

    fn status(screen: Screen) -> Option<String> {
        match screen {
            Screen::Idle => Some("idle".into()),
            Screen::Working => Some("working".into()),
            Screen::Permission => Some("blocked".into()),
            Screen::Shell => None,
        }
    }

    fn info(pane: &FakePane) -> PaneInfo {
        PaneInfo {
            pane_id: pane.pane_id.clone(),
            workspace_id: pane.workspace_id.clone(),
            agent: (pane.screen != Screen::Shell).then(|| "claude".into()),
            agent_status: Self::status(pane.screen),
            tokens: pane.tokens.clone(),
        }
    }

    fn with_pane<T>(&self, pane_id: &str, f: impl FnOnce(&mut FakePane) -> T) -> Result<T> {
        self.edit(|w| w.panes.iter_mut().find(|p| p.pane_id == pane_id).map(f))
            .ok_or_else(|| HerdrError::NotFound {
                what: "pane",
                id: pane_id.into(),
            })
    }
}

impl Herdr for FakeHerdr {
    fn snapshot(&self) -> Result<Snapshot> {
        self.call("snapshot", "snapshot".into())?;
        let w = self.load();
        Ok(Snapshot {
            version: Some("fake".into()),
            workspaces: w.workspaces,
            panes: w.panes.iter().map(Self::info).collect(),
        })
    }

    fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        self.call("pane", format!("pane {pane_id}"))?;
        Ok(self
            .load()
            .panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .map(Self::info))
    }

    fn read_visible(&self, pane_id: &str) -> Result<String> {
        self.call("read", format!("read {pane_id}"))?;
        self.with_pane(pane_id, |p| Self::render(p))
    }

    fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.call("send_text", format!("send_text {pane_id} {text}"))?;
        self.with_pane(pane_id, |p| p.composer.push_str(text))?;
        self.pause_point("send_text.after");
        Ok(())
    }

    fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.call("send_enter", format!("send_enter {pane_id}"))?;
        // One edit, as herdr delivers one keystroke at a time.
        self.edit(|w| {
            let stubborn = w.stubborn.iter().any(|p| p == pane_id);
            let p = w.panes.iter_mut().find(|p| p.pane_id == pane_id)?;
            let newline = w
                .newline_enters
                .get_mut(pane_id)
                .filter(|n| **n > 0 && p.screen == Screen::Idle && !p.composer.is_empty());
            // Claude takes a line at its composer, a shell runs one.
            let takes = matches!(p.screen, Screen::Idle | Screen::Shell);
            if let Some(n) = newline {
                *n -= 1;
                p.composer.push('\n');
            } else if takes && !p.composer.is_empty() {
                let line = std::mem::take(&mut p.composer);
                // Claude's `/exit` leaves the pane at a shell.
                if line == "/exit" && !stubborn {
                    p.screen = Screen::Shell;
                }
                p.submitted.push(line);
            }
            Some(())
        })
        .ok_or_else(|| HerdrError::NotFound {
            what: "pane",
            id: pane_id.into(),
        })?;
        self.pause_point("send_enter.after");
        Ok(())
    }

    fn send_escape(&self, pane_id: &str) -> Result<()> {
        self.call("send_escape", format!("send_escape {pane_id}"))?;
        self.with_pane(pane_id, |p| {
            if p.screen == Screen::Working {
                p.screen = Screen::Idle;
            }
        })?;
        self.pause_point("send_escape.after");
        Ok(())
    }

    fn report_tokens(&self, pane_id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        self.call("report_tokens", format!("report_tokens {pane_id}"))?;
        self.with_pane(pane_id, |p| p.tokens.extend(tokens.clone()))
    }

    fn create_workspace(&self, label: &str, _cwd: Option<&Path>) -> Result<Created> {
        self.call("create_workspace", format!("create_workspace {label}"))?;
        Ok(self.edit(|w| {
            // One past the highest id, so a closed workspace's id is never reused.
            let next = w
                .workspaces
                .iter()
                .filter_map(|ws| ws.workspace_id.strip_prefix('w')?.parse::<usize>().ok())
                .max()
                .unwrap_or(0)
                + 1;
            let workspace_id = format!("w{next}");
            let pane_id = format!("{workspace_id}:p1");
            w.workspaces.push(WorkspaceInfo {
                workspace_id: workspace_id.clone(),
                label: label.into(),
            });
            w.panes.push(FakePane {
                pane_id: pane_id.clone(),
                workspace_id: workspace_id.clone(),
                screen: Screen::Shell,
                composer: String::new(),
                submitted: Vec::new(),
                tokens: BTreeMap::new(),
            });
            Created {
                workspace_id,
                pane_id,
            }
        }))
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        self.call("rename", format!("rename {workspace_id} {label}"))?;
        self.edit(|w| {
            if let Some(ws) = w
                .workspaces
                .iter_mut()
                .find(|ws| ws.workspace_id == workspace_id)
            {
                ws.label = label.into();
            }
        });
        Ok(())
    }

    fn move_workspace(&self, workspace_id: &str, index: usize) -> Result<()> {
        self.call("move", format!("move {workspace_id} {index}"))?;
        self.edit(|w| {
            if let Some(from) = w
                .workspaces
                .iter()
                .position(|ws| ws.workspace_id == workspace_id)
            {
                let ws = w.workspaces.remove(from);
                let index = index.min(w.workspaces.len());
                w.workspaces.insert(index, ws);
            }
        });
        Ok(())
    }

    fn close_pane(&self, pane_id: &str) -> Result<()> {
        self.call("close_pane", format!("close_pane {pane_id}"))?;
        self.edit(|w| {
            let Some(at) = w.panes.iter().position(|p| p.pane_id == pane_id) else {
                return;
            };
            let workspace = w.panes.remove(at).workspace_id;
            if !w.panes.iter().any(|p| p.workspace_id == workspace) {
                w.workspaces.retain(|ws| ws.workspace_id != workspace);
            }
        });
        Ok(())
    }

    fn notify(&self, title: &str, _body: &str) -> Result<()> {
        self.call("notify", format!("notify {title}"))
    }
}
