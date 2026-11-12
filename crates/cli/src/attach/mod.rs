//! `tiller attach` (`terminal-holder.md` 5.2): one agent's pane in the
//! owner's terminal, with the `Ctrl-\` prefix, a help screen and the
//! switcher. It talks to the holder alone, so it works while the daemon is
//! down. Only the owner at a terminal may run it (D12): a fleet caller is
//! refused, and a notification click, which carries `TILLER_SESSION_ID`,
//! passes.

mod agents;
mod keys;
mod term;

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use tiller_core::authority::principal;
use tiller_panes::message::SyncMode;
use tiller_panes::{
    Event, Exit, HolderClient, Live, Pane, PaneId, PaneState, Panes, PanesError, StreamHandle,
    Subscribe, Update, socket_path,
};

use crate::app::Failure;
use keys::{Act, Key, Kind, Prefix, Prefixed, Reader};
use term::{Line, Raw, Screens};

#[derive(clap::Args)]
pub struct Args {
    /// The run, session, card or label of the agent to show (default: the
    /// list of agents)
    target: Option<String>,
    /// Watch only: send no keys and take no size lease
    #[arg(long)]
    read_only: bool,
}

/// How long a cut key sequence waits for the rest: a lone Escape is the
/// Escape key after this.
const ESC_WAIT: Duration = Duration::from_millis(25);

/// How often the window's size is read. Polling needs no signal handler.
const SIZE_POLL: Duration = Duration::from_millis(100);

/// `db` is the `--db` or `TILLER_DB` given, if any.
pub fn run(args: &Args, db: Option<&Path>, json: bool) -> ExitCode {
    match attach(args, db) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let exit = failure.exit;
            crate::emit(failure.into_response(), json);
            ExitCode::from(exit)
        }
    }
}

/// D12: `tiller attach` runs only for the owner at a terminal: no
/// `CLAUDECODE`, stdin a terminal, and no Claude Code process among the
/// ancestors (`principal::owner_at_terminal`). `TILLER_SESSION_ID` plays no
/// part, so a notification click that carries it passes.
fn refusal(why: String) -> Failure {
    Failure::forbidden(format!(
        "tiller attach runs only for the owner at a terminal: {why}"
    ))
    .hint("an agent never attaches; the owner's own shell or a notification click may")
}

fn attach(args: &Args, db: Option<&Path>) -> Result<(), Failure> {
    principal::owner_at_terminal().map_err(refusal)?;
    // The owner check stands in for stdin in tests, so both ends are checked.
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(refusal("stdin and stdout must be a terminal".into()));
    }
    let home = crate::app::App::from_env(None).home;
    // Attach reads the home's holder and its daemon's state: a `--db`
    // other than the home's own would suggest another.
    if db.is_some_and(|db| !crate::daemon::is_home_db(db, &home)) {
        return Err(Failure::usage(format!(
            "--db and TILLER_DB do not apply to attach: it shows the agents of the home {}",
            home.display()
        ))
        .hint("set TILLER_HOME to choose another"));
    }
    let prefix = prefix(&home)?;
    let client = connect(&home)?;
    let window = term::window_size().unwrap_or((80, 24));
    let raw = Raw::enter().map_err(|error| {
        Failure::unavailable(format!("the terminal would not go into raw mode: {error}"))
    })?;
    let (tx, rx) = mpsc::channel();
    read_keys(tx.clone());
    watch_size(tx.clone(), window);
    watch_holder(&client, tx.clone());
    let mut session = Session {
        client: &client,
        home: &home,
        read_only: args.read_only,
        prefix,
        tx,
        window,
        view: View::Pane,
        stream: None,
        generation: 0,
        screens: Screens::default(),
        prefixed: Prefixed::default(),
    };
    let ran = term::write(term::ENTER)
        .map_err(gone)
        .and_then(|()| session.start(args.target.as_deref()))
        .and_then(|()| session.run(rx, Reader::new(prefix)));
    // The lease goes first, then the terminal comes back.
    session.end_stream();
    raw.leave();
    ran
}

/// `[attach] prefix` from `config.toml`, or `Ctrl-\`.
fn prefix(home: &Path) -> Result<Prefix, Failure> {
    let path = home.join("config.toml");
    let table = tiller_core::config_file::load(&path)
        .map_err(|error| Failure::usage(format!("{}: {error}", path.display())))?;
    match tiller_core::config_file::get(&table, "attach.prefix") {
        None => Ok(Prefix::default()),
        Some(toml::Value::String(text)) => Prefix::parse(text).map_err(Failure::usage),
        Some(other) => Err(Failure::usage(format!(
            "[attach] prefix is a string such as \"ctrl-\\\\\", not {other}"
        ))),
    }
}

fn no_holder() -> Failure {
    Failure::unavailable(
        "no holder runs on this host (start it with `tiller holder`, or run `tiller doctor`)",
    )
}

/// The holder's client. When none answers, it starts one with `tiller
/// holder --detach` and connects to that.
fn connect(home: &Path) -> Result<HolderClient, Failure> {
    let socket = socket_path(home);
    match HolderClient::connect(&socket, "attach") {
        Ok(client) => return Ok(client),
        Err(PanesError::Unreachable(_)) => {}
        Err(error) => {
            return Err(Failure::unavailable(format!(
                "the terminal holder: {error}"
            )));
        }
    }
    let started = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .args(["holder", "--detach"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
    });
    match started {
        Ok(status) if status.success() => {
            HolderClient::connect(&socket, "attach").map_err(|_| no_holder())
        }
        _ => Err(no_holder()),
    }
}

fn gone(error: std::io::Error) -> Failure {
    Failure::unavailable(format!("the terminal went away: {error}"))
}

fn holder_down(error: &PanesError) -> Failure {
    Failure::unavailable(format!("the terminal holder stopped answering: {error}"))
        .hint("tiller doctor")
}

/// What the main loop hears.
enum Ev {
    Keys(Vec<u8>),
    /// Stdin closed.
    KeysEnd,
    /// An update of the stream with this generation. None when it ended.
    Update(u64, Option<Result<Update, PanesError>>),
    Size((u16, u16)),
    /// A `switch` asked every attach client to show this pane.
    Show(PaneId),
    /// A pane's PTY took this size, on any client's lease.
    PaneSize(PaneId, (u16, u16)),
    /// A pane started, exited or changed: an open switcher reads the list
    /// again.
    Changed,
}

fn read_keys(tx: Sender<Ev>) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    if tx.send(Ev::Keys(buf[..len].to_vec())).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(Ev::KeysEnd);
    });
}

fn watch_size(tx: Sender<Ev>, mut last: (u16, u16)) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(SIZE_POLL);
            if let Some(size) = term::window_size()
                && size != last
            {
                last = size;
                if tx.send(Ev::Size(size)).is_err() {
                    return;
                }
            }
        }
    });
}

/// The holder's events on an attach watch: `show` from a `switch`, each
/// pane's new size, and the changes an open switcher shows. Without a
/// watch, attach still works; it only misses `switch` and another client's
/// resize.
fn watch_holder(client: &HolderClient, tx: Sender<Ev>) {
    let Ok(mut events) = client.watch() else {
        return;
    };
    std::thread::spawn(move || {
        while let Ok(Some(event)) = events.recv() {
            let ev = match event {
                Event::Show { pane } => Ev::Show(pane),
                Event::Resized { pane, cols, rows } => Ev::PaneSize(pane, (cols, rows)),
                Event::Spawned { .. }
                | Event::Exited { .. }
                | Event::Meta { .. }
                | Event::Forgotten { .. } => Ev::Changed,
                _ => continue,
            };
            if tx.send(ev).is_err() {
                return;
            }
        }
    });
}

/// What is drawn over the pane.
enum View {
    Pane,
    Help,
    Switcher(agents::Switcher),
}

/// The pane attach shows now.
struct Stream {
    pane: Pane,
    handle: StreamHandle,
    /// The PTY's size, from the repaint, this client's resizes and the
    /// watch's `resized` events: whoever set it.
    size: Option<(u16, u16)>,
    /// The size the PTY took on this client's last resize. While that
    /// resize waits for its answer it is `Pending`.
    ours: Ours,
    exited: Option<Exit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ours {
    None,
    Pending,
    Size((u16, u16)),
}

/// Whether the loop goes on.
#[derive(PartialEq, Eq)]
enum Flow {
    Go,
    Detach,
}

struct Session<'a> {
    client: &'a HolderClient,
    home: &'a PathBuf,
    read_only: bool,
    prefix: Prefix,
    tx: Sender<Ev>,
    window: (u16, u16),
    view: View,
    stream: Option<Stream>,
    /// Counts the streams, so a late update of an ended one is ignored.
    generation: u64,
    screens: Screens,
    prefixed: Prefixed,
}

impl Session<'_> {
    /// Shows the target's pane, or the switcher: with no target, when none
    /// matches, and when its session has ended.
    fn start(&mut self, target: Option<&str>) -> Result<(), Failure> {
        let Some(target) = target else {
            return self.open_switcher(None);
        };
        let listing = self.client.list().map_err(|error| holder_down(&error))?;
        match find(&listing.panes, target) {
            Some(pane) if pane.state == PaneState::Running => self.show(pane.clone()),
            Some(_) => self.open_switcher(Some(format!("  {target} has ended"))),
            None => self.open_switcher(Some(format!("  no agent matches {target}"))),
        }
    }

    fn run(&mut self, rx: Receiver<Ev>, mut reader: Reader) -> Result<(), Failure> {
        // When the cut sequence waiting began: pane output must not hold
        // back a lone Escape.
        let mut since: Option<Instant> = None;
        loop {
            let ev = if reader.waiting() {
                let began = *since.get_or_insert_with(Instant::now);
                let left = (began + ESC_WAIT).saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(ev) => ev,
                    Err(RecvTimeoutError::Timeout) => {
                        since = None;
                        let keys = reader.flush();
                        if self.keys(keys)? == Flow::Detach {
                            return Ok(());
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                match rx.recv() {
                    Ok(ev) => ev,
                    Err(_) => return Ok(()),
                }
            };
            let flow = match ev {
                Ev::Keys(bytes) => {
                    since = None;
                    let keys = reader.feed(&bytes);
                    self.keys(keys)?
                }
                Ev::KeysEnd => Flow::Detach,
                Ev::Update(generation, update) if generation == self.generation => {
                    self.update(update)?;
                    Flow::Go
                }
                Ev::Update(..) => Flow::Go,
                Ev::Size(size) => {
                    self.resized(size)?;
                    Flow::Go
                }
                Ev::Show(pane) => {
                    if let Ok(Some(pane)) = self.client.get(&pane) {
                        self.show(pane)?;
                    }
                    Flow::Go
                }
                Ev::PaneSize(pane, size) => {
                    if let Some(stream) = &mut self.stream
                        && stream.pane.pane == pane
                    {
                        stream.size = Some(size);
                    }
                    Flow::Go
                }
                Ev::Changed => {
                    if let View::Switcher(_) = self.view {
                        self.refresh_switcher()?;
                    }
                    Flow::Go
                }
            };
            if flow == Flow::Detach {
                return Ok(());
            }
        }
    }

    fn keys(&mut self, keys: Vec<Key>) -> Result<Flow, Failure> {
        let mut send = Vec::new();
        for key in keys {
            match &mut self.view {
                // A kitty key's release is no key press: the release of the
                // `?` that opened the help must not close it. A modifier
                // pressed alone is none either.
                View::Help | View::Switcher(_)
                    if matches!(key.kind, Kind::Release | Kind::Modifier) => {}
                // Any key closes the help, and is swallowed.
                View::Help => self.repaint()?,
                View::Switcher(switcher) => match key.kind {
                    Kind::Up => {
                        switcher.up();
                        self.draw_switcher()?;
                    }
                    Kind::Down => {
                        switcher.down();
                        self.draw_switcher()?;
                    }
                    Kind::Enter => {
                        if let Some(pane) = switcher.chosen().cloned() {
                            self.show(pane)?;
                        }
                    }
                    Kind::Escape if self.stream.is_some() => self.repaint()?,
                    Kind::Escape => return Ok(Flow::Detach),
                    _ => {}
                },
                View::Pane => {
                    let Some(act) = self.prefixed.on_key(key) else {
                        continue;
                    };
                    if let Act::Send(bytes) = act {
                        send.extend(bytes);
                        continue;
                    }
                    self.send(&std::mem::take(&mut send))?;
                    match act {
                        Act::Send(_) => {}
                        Act::Detach => return Ok(Flow::Detach),
                        Act::Switcher => self.open_switcher(None)?,
                        Act::Next => self.step(1)?,
                        Act::Prev => self.step(-1)?,
                        Act::Help => self.open_help()?,
                    }
                }
            }
        }
        self.send(&send)?;
        Ok(Flow::Go)
    }

    /// Sends the owner's keys to the pane, and first moves this client's
    /// lease to the top when another one set the size.
    fn send(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        if bytes.is_empty() || self.read_only {
            return Ok(());
        }
        let window = self.window;
        let Some(stream) = &mut self.stream else {
            return Ok(());
        };
        if stream.exited.is_some() {
            return Ok(());
        }
        if let Ours::Size(ours) = stream.ours
            && stream.size != Some(ours)
        {
            take_lease(stream, window);
        }
        self.client
            .raw_input(stream.pane.num, bytes)
            .map_err(|error| holder_down(&error))
    }

    fn resized(&mut self, size: (u16, u16)) -> Result<(), Failure> {
        self.window = size;
        if !self.read_only
            && let Some(stream) = &mut self.stream
        {
            take_lease(stream, size);
        }
        match self.view {
            View::Pane => Ok(()),
            View::Help => self.open_help(),
            View::Switcher(_) => self.draw_switcher(),
        }
    }

    fn update(&mut self, update: Option<Result<Update, PanesError>>) -> Result<(), Failure> {
        let showing = matches!(self.view, View::Pane);
        let Some(stream) = &mut self.stream else {
            return Ok(());
        };
        match update {
            None => Ok(()),
            Some(Ok(Update::Repaint { cols, rows, bytes })) => {
                stream.size = Some((cols, rows));
                if showing {
                    let (bytes, _) = self.screens.filter(&bytes);
                    term::write(&bytes).map_err(gone)?;
                }
                Ok(())
            }
            Some(Ok(Update::Output(bytes))) => {
                if !showing {
                    return Ok(());
                }
                let (bytes, switched) = self.screens.filter(&bytes);
                term::write(&bytes).map_err(gone)?;
                if switched { self.repaint() } else { Ok(()) }
            }
            Some(Ok(Update::Resized { cols, rows })) => {
                stream.size = Some((cols, rows));
                if stream.ours == Ours::Pending {
                    stream.ours = Ours::Size((cols, rows));
                }
                Ok(())
            }
            Some(Ok(Update::Exited(exit))) => {
                stream.exited = Some(exit);
                if showing {
                    self.exit_note(exit)?;
                }
                Ok(())
            }
            // The holder cut the stream: a new one repaints.
            Some(Ok(Update::Lagged)) => self.repaint(),
            Some(Err(error @ PanesError::Unreachable(_))) => Err(holder_down(&error)),
            // A refused resize: the size stays as the holder has it.
            Some(Err(_)) => {
                if stream.ours == Ours::Pending {
                    stream.ours = Ours::None;
                }
                Ok(())
            }
        }
    }

    fn exit_note(&self, exit: Exit) -> Result<(), Failure> {
        let how = match (exit.code, exit.signal) {
            (Some(code), _) => format!("exited with code {code}"),
            (None, Some(signal)) => format!("was ended by signal {signal}"),
            (None, None) => "exited".into(),
        };
        let prefix = self.prefix.label();
        let text = format!("the agent {how}. {prefix} s for the list, {prefix} d to detach");
        term::write(&term::note(&text, self.window)).map_err(gone)
    }

    /// Shows `pane`: a new stream, which starts with a repaint, and this
    /// client's lease at the window's size. The old stream ends after the
    /// new lease is on top, so a repaint of the same pane never lets
    /// another lease resize it in between.
    fn show(&mut self, pane: Pane) -> Result<(), Failure> {
        let old = self.stream.take();
        let end_old = || {
            if let Some(old) = old {
                old.handle.end();
            }
        };
        self.generation += 1;
        self.view = View::Pane;
        self.prefixed = Prefixed::default();
        self.screens.reset();
        let subscribed = self.client.subscribe(
            &pane.pane,
            Subscribe {
                sync: SyncMode::Pass,
            },
        );
        let mut subscription = match subscribed {
            Ok(subscription) => subscription,
            Err(PanesError::NotFound(_)) => {
                end_old();
                let note = format!("  {} is gone", agents::label(&pane));
                return self.open_switcher(Some(note));
            }
            Err(error) => {
                end_old();
                return Err(holder_down(&error));
            }
        };
        let handle = subscription.handle();
        let (tx, generation) = (self.tx.clone(), self.generation);
        std::thread::spawn(move || {
            loop {
                let update = match subscription.recv() {
                    Ok(Some(update)) => Some(Ok(update)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                };
                let end = update.is_none();
                if tx.send(Ev::Update(generation, update)).is_err() || end {
                    return;
                }
            }
        });
        let mut stream = Stream {
            pane,
            handle,
            size: None,
            ours: Ours::None,
            exited: None,
        };
        if !self.read_only {
            take_lease(&mut stream, self.window);
        }
        end_old();
        self.stream = Some(stream);
        Ok(())
    }

    /// Asks for a fresh repaint of the pane shown: a new stream on it.
    fn repaint(&mut self) -> Result<(), Failure> {
        match self.stream.as_ref().map(|stream| stream.pane.clone()) {
            Some(pane) => self.show(pane),
            None => self.open_switcher(None),
        }
    }

    /// Ends the stream shown, and with it this client's lease on the pane.
    fn end_stream(&mut self) {
        if let Some(stream) = self.stream.take() {
            stream.handle.end();
        }
    }

    fn open_help(&mut self) -> Result<(), Failure> {
        self.view = View::Help;
        let prefix = self.prefix.label();
        let keys = [
            (format!("{prefix} d"), "detach"),
            (format!("{prefix} s"), "the list of agents"),
            (format!("{prefix} n"), "the next agent"),
            (format!("{prefix} p"), "the previous agent"),
            (
                format!("{prefix} {prefix}"),
                "send one {prefix} to the agent",
            ),
            (format!("{prefix} ?"), "this help"),
        ];
        let width = keys.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
        let mut lines: Vec<Line> = keys
            .iter()
            .map(|(key, what)| {
                Line::plain(format!(
                    " {key:width$}  {}",
                    what.replace("{prefix}", &prefix)
                ))
            })
            .collect();
        lines.push(Line::plain(""));
        lines.push(Line::plain(" any key closes this help"));
        term::write(&term::draw_box("tiller attach", &lines, self.window)).map_err(gone)
    }

    fn open_switcher(&mut self, note: Option<String>) -> Result<(), Failure> {
        let listing = self.client.list().map_err(|error| holder_down(&error))?;
        let current = self.stream.as_ref().map(|stream| &stream.pane.pane);
        let mut switcher =
            agents::Switcher::new(listing.panes, current, agents::activity(self.home));
        switcher.note = note;
        if self.stream.is_none() {
            term::write(b"\x1b[0m\x1b[H\x1b[2J").map_err(gone)?;
        }
        self.view = View::Switcher(switcher);
        self.draw_switcher()
    }

    /// Reads the list again and keeps the selection on the same pane.
    fn refresh_switcher(&mut self) -> Result<(), Failure> {
        let View::Switcher(old) = &self.view else {
            return Ok(());
        };
        let chosen = old.chosen().map(|pane| pane.pane.clone());
        let note = old.note.clone();
        let listing = self.client.list().map_err(|error| holder_down(&error))?;
        let mut switcher =
            agents::Switcher::new(listing.panes, chosen.as_ref(), agents::activity(self.home));
        switcher.note = note;
        self.view = View::Switcher(switcher);
        // The list may have shrunk: what the old box covered is drawn again.
        term::write(b"\x1b[0m\x1b[H\x1b[2J").map_err(gone)?;
        self.draw_switcher()
    }

    fn draw_switcher(&self) -> Result<(), Failure> {
        let View::Switcher(switcher) = &self.view else {
            return Ok(());
        };
        // The box's borders take two rows.
        let room = usize::from(self.window.1).saturating_sub(2);
        let lines = switcher.lines(Timestamp::now(), room);
        term::write(&term::draw_box("agents", &lines, self.window)).map_err(gone)
    }

    /// The next (`1`) or the previous (`-1`) running agent, in the
    /// switcher's order.
    fn step(&mut self, by: isize) -> Result<(), Failure> {
        let listing = self.client.list().map_err(|error| holder_down(&error))?;
        let current = self.stream.as_ref().map(|stream| stream.pane.pane.clone());
        let running: Vec<Pane> = agents::order(listing.panes)
            .into_iter()
            .filter(|pane| pane.state == PaneState::Running || Some(&pane.pane) == current.as_ref())
            .collect();
        if running.is_empty() {
            return Ok(());
        }
        let at = current
            .and_then(|current| running.iter().position(|pane| pane.pane == current))
            .map_or(0, |at| {
                (at as isize + by).rem_euclid(running.len() as isize) as usize
            });
        match running.into_iter().nth(at) {
            Some(pane) => self.show(pane),
            None => Ok(()),
        }
    }
}

/// Moves this client's lease to the top at `window`'s size. Its answer
/// comes through the stream.
fn take_lease(stream: &mut Stream, (cols, rows): (u16, u16)) {
    if stream.handle.resize(cols, rows).is_ok() {
        stream.ours = Ours::Pending;
    }
}

/// The pane `target` names: its pane id, its key (the session), or its
/// `session`, `run`, `card` or `label` metadata. A running pane comes
/// before an exited one, then the latest.
fn find<'a>(panes: &'a [Pane], target: &str) -> Option<&'a Pane> {
    let names = |pane: &Pane| {
        pane.pane.as_str() == target
            || pane.key == target
            || ["session", "run", "card", "label"]
                .iter()
                .any(|key| pane.meta.get(*key).is_some_and(|value| value == target))
    };
    panes
        .iter()
        .filter(|pane| names(pane))
        .max_by_key(|pane| (pane.state == PaneState::Running, pane.started_at))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use tiller_core::authority::principal::Process;

    fn start_check(
        claudecode: bool,
        stdin_terminal: bool,
        ancestors: impl FnOnce() -> std::io::Result<Vec<Process>>,
    ) -> Result<(), Failure> {
        principal::owner_check(claudecode, stdin_terminal, ancestors)
            .map(drop)
            .map_err(refusal)
    }

    fn process(pid: u32, comm: &str) -> Process {
        Process {
            pid,
            comm: comm.into(),
            args: comm.into(),
        }
    }

    // D12: a fleet caller is refused in the binary.
    #[test]
    fn attach_refuses_a_fleet_caller() {
        let err = start_check(true, true, || Ok(Vec::new())).unwrap_err();
        assert_eq!(err.code, "forbidden");
        assert!(err.message.contains("CLAUDECODE is set"), "{}", err.message);
        let chain = vec![process(10, "zsh"), process(11, "claude")];
        let err = start_check(false, true, || Ok(chain)).unwrap_err();
        assert!(
            err.message.contains("Claude Code (pid 11)"),
            "{}",
            err.message
        );
        let err = start_check(false, false, || Ok(Vec::new())).unwrap_err();
        assert!(
            err.message.contains("stdin is not a terminal"),
            "{}",
            err.message
        );
    }

    // D12: a notification click runs from the owner's desktop with
    // `TILLER_SESSION_ID` set and no Claude Code behind it: it passes, as the
    // check never reads that variable.
    #[test]
    fn attach_lets_a_notification_click_through() {
        let chain = vec![process(20, "notifier"), process(21, "launchd")];
        start_check(false, true, || Ok(chain)).unwrap();
    }

    fn pane(id: &str, key: &str, state: PaneState, started: i64) -> Pane {
        Pane {
            pane: PaneId::from(id),
            num: 1,
            key: key.into(),
            pid: 1,
            argv: vec![],
            cwd: "/home/user".into(),
            started_at: Timestamp::from_second(started).unwrap(),
            state,
            exit: None,
            cols: 80,
            rows: 24,
            meta: [("run".to_owned(), "r1".to_owned())].into(),
            title: String::new(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 0,
        }
    }

    #[test]
    fn a_target_names_the_running_pane_first() {
        let panes = [
            pane("p1", "s1", PaneState::Exited, 30),
            pane("p2", "s1", PaneState::Running, 10),
            pane("p3", "s3", PaneState::Running, 20),
        ];
        assert_eq!(find(&panes, "s1").unwrap().pane.as_str(), "p2");
        assert_eq!(find(&panes, "p3").unwrap().pane.as_str(), "p3");
        assert_eq!(find(&panes, "r1").unwrap().pane.as_str(), "p3");
        assert!(find(&panes, "nope").is_none());
    }
}
