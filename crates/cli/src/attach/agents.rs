//! The switcher (layout A, `terminal-holder.md` 5.2): every pane by project
//! block, with a status glyph, the label and the age. It reads the holder's
//! `list`, so it works while the daemon is down. When the daemon runs, its
//! activity gives the glyph.

use std::collections::HashMap;
use std::path::Path;

use jiff::Timestamp;
use tiller_panes::{Pane, PaneId, PaneState};

use super::term::Line;

/// The panes in the order the switcher and the board show them: by
/// project, then role, then start time.
pub fn order(mut panes: Vec<Pane>) -> Vec<Pane> {
    panes.sort_by(|a, b| {
        (project(a), meta(a, "role"), a.started_at).cmp(&(
            project(b),
            meta(b, "role"),
            b.started_at,
        ))
    });
    panes
}

fn meta<'a>(pane: &'a Pane, key: &str) -> &'a str {
    pane.meta.get(key).map_or("", String::as_str)
}

fn project(pane: &Pane) -> &str {
    match meta(pane, "project") {
        "" => "other",
        project => project,
    }
}

/// The name a pane goes by: its label, else its title, else its key.
pub fn label(pane: &Pane) -> &str {
    [meta(pane, "label"), pane.title.as_str(), pane.key.as_str()]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_default()
}

/// The daemon's activity by session id, when a daemon runs on this home.
/// Empty otherwise, and on any failure: the holder's state stands in.
pub fn activity(home: &Path) -> HashMap<String, String> {
    use tiller_agents::land::{DaemonLock, LOCK_FILE};
    let db = home.join("state.db");
    // The probe creates the lock file, so it runs only where one exists.
    if !home.join(LOCK_FILE).exists() || !db.exists() || !DaemonLock::held(home) {
        return HashMap::new();
    }
    read_activity(&db).unwrap_or_default()
}

fn read_activity(db: &Path) -> rusqlite::Result<HashMap<String, String>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_millis(200))?;
    let mut stmt = conn.prepare("SELECT session_id, effective_activity FROM session_activity")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// The switcher's state: the panes, the selected one, and a note on top.
pub struct Switcher {
    pub panes: Vec<Pane>,
    pub selected: usize,
    pub note: Option<String>,
    pub activity: HashMap<String, String>,
}

impl Switcher {
    /// Selects `current` when it is listed.
    pub fn new(
        panes: Vec<Pane>,
        current: Option<&PaneId>,
        activity: HashMap<String, String>,
    ) -> Self {
        let panes = order(panes);
        let selected = current
            .and_then(|current| panes.iter().position(|pane| &pane.pane == current))
            .unwrap_or(0);
        Self {
            panes,
            selected,
            note: None,
            activity,
        }
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        if self.selected + 1 < self.panes.len() {
            self.selected += 1;
        }
    }

    pub fn chosen(&self) -> Option<&Pane> {
        self.panes.get(self.selected)
    }

    /// The box's rows at `now`, at most `room` of them: when the panes do
    /// not fit, the rows shown scroll to keep the selected one.
    pub fn lines(&self, now: Timestamp, room: usize) -> Vec<Line> {
        let mut lines = Vec::new();
        if let Some(note) = &self.note {
            lines.push(Line::plain(note.clone()));
        }
        if self.panes.is_empty() {
            lines.push(Line::plain("  no agents run on this host"));
        }
        let width = self
            .panes
            .iter()
            .map(|pane| label(pane).chars().count())
            .max()
            .unwrap_or(0);
        let mut block = None;
        let mut body = Vec::new();
        for (n, pane) in self.panes.iter().enumerate() {
            if block != Some(project(pane)) {
                block = Some(project(pane));
                body.push(Line::plain(format!("  {}", project(pane))));
            }
            let (glyph, state) = self.status(pane);
            body.push(Line {
                text: format!(
                    "   {glyph}  {:width$}  {state:10} {}",
                    label(pane),
                    age(pane, now)
                ),
                selected: n == self.selected,
            });
        }
        // The note and the hint stay, and the panes get what is left.
        let fits = room.saturating_sub(lines.len() + 1).max(1);
        if body.len() > fits {
            let at = body.iter().position(|line| line.selected).unwrap_or(0);
            let start = (at + 1).saturating_sub(fits);
            body.truncate(start + fits);
            body.drain(..start);
        }
        lines.extend(body);
        lines.push(Line::plain("  ↑↓ move  enter attach  esc close"));
        lines
    }

    /// The glyph and the word for a pane: the daemon's activity when it has
    /// one, else the holder's state.
    fn status(&self, pane: &Pane) -> (char, String) {
        if let (PaneState::Exited, Some(exit)) = (pane.state, pane.exit) {
            let how = match (exit.code, exit.signal) {
                (Some(code), _) => format!("exited {code}"),
                (None, Some(signal)) => format!("signal {signal}"),
                (None, None) => "exited".into(),
            };
            return ('✕', how);
        }
        if pane.state == PaneState::Exited {
            return ('✕', "exited".into());
        }
        match self.activity.get(&pane.key).map(String::as_str) {
            Some("working") => ('●', "working".into()),
            Some("waiting_permission" | "waiting_input") => ('▲', "blocked".into()),
            Some("errored") => ('▲', "errored".into()),
            Some(other) => ('○', other.into()),
            None => ('●', "running".into()),
        }
    }
}

/// How long since the pane last did something: its exit, else its last
/// output, else its start.
fn age(pane: &Pane, now: Timestamp) -> String {
    let since = pane
        .exit
        .map(|exit| exit.at)
        .or(pane.last_output_at)
        .unwrap_or(pane.started_at);
    let secs = now.duration_since(since).as_secs().max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..172_800 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use tiller_panes::{Exit, Meta};

    fn pane(id: &str, project: &str, label: &str, started: i64) -> Pane {
        let mut meta = Meta::new();
        if !project.is_empty() {
            meta.insert("project".into(), project.into());
        }
        meta.insert("label".into(), label.into());
        Pane {
            pane: PaneId::from(id),
            num: 1,
            key: format!("s-{id}"),
            pid: 1,
            argv: vec![],
            cwd: "/home/user".into(),
            started_at: Timestamp::from_second(started).unwrap(),
            state: PaneState::Running,
            exit: None,
            cols: 80,
            rows: 24,
            meta,
            title: String::new(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 0,
        }
    }

    #[test]
    fn panes_list_by_project_block_with_their_glyphs() {
        let mut gone = pane("p3", "spec", "reviewer", 30);
        gone.state = PaneState::Exited;
        gone.exit = Some(Exit {
            code: Some(1),
            signal: None,
            at: Timestamp::from_second(100).unwrap(),
        });
        let panes = vec![
            gone,
            pane("p2", "wave-4", "builder-board", 20),
            pane("p1", "wave-4", "builder-api", 10),
            pane("p4", "", "loose", 5),
        ];
        let activity = HashMap::from([
            ("s-p1".to_owned(), "working".to_owned()),
            ("s-p2".to_owned(), "waiting_permission".to_owned()),
        ]);
        let switcher = Switcher::new(panes, Some(&PaneId::from("p2")), activity);
        let now = Timestamp::from_second(220).unwrap();
        let text: Vec<_> = switcher
            .lines(now, 24)
            .into_iter()
            .map(|line| (line.text, line.selected))
            .collect();
        assert_eq!(
            text,
            [
                ("  other".to_owned(), false),
                ("   ●  loose          running    3m".to_owned(), false),
                ("  spec".to_owned(), false),
                ("   ✕  reviewer       exited 1   2m".to_owned(), false),
                ("  wave-4".to_owned(), false),
                ("   ●  builder-api    working    3m".to_owned(), false),
                ("   ▲  builder-board  blocked    3m".to_owned(), true),
                ("  ↑↓ move  enter attach  esc close".to_owned(), false),
            ]
        );
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selected_pane() {
        let panes: Vec<_> = (0..10)
            .map(|n| pane(&format!("p{n}"), "alpha", &format!("agent-{n}"), n))
            .collect();
        let mut switcher = Switcher::new(panes, None, HashMap::new());
        switcher.note = Some("  a note".into());
        let shown = |switcher: &Switcher| -> Vec<String> {
            let now = Timestamp::from_second(60).unwrap();
            let lines = switcher.lines(now, 6);
            assert_eq!(lines.len(), 6);
            assert_eq!(lines[0].text, "  a note");
            assert_eq!(lines[5].text, "  ↑↓ move  enter attach  esc close");
            lines
                .into_iter()
                .filter(|line| line.selected)
                .map(|line| line.text)
                .collect()
        };
        assert_eq!(shown(&switcher).len(), 1);
        for _ in 0..9 {
            switcher.down();
        }
        let selected = shown(&switcher);
        assert!(selected[0].contains("agent-9"), "{selected:?}");
    }
}
