//! `tiller attach` end to end (`terminal-holder.md` 5.2, 9.5): the real
//! binary in a PTY that stands in for the owner's terminal, against a real
//! holder on a throwaway home. What attach writes to that PTY is fed to the
//! holder's own screen model, so a test reads the owner's screen and modes
//! as a terminal would show them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Running, World};
use nix::sys::signal::{Signal as Sig, kill};
use nix::unistd::Pid;
use rustix::termios::{LocalModes, tcgetattr};
use tiller_holder::model::Model;
use tiller_panes::message::{Format, Read, Screen, Size};
use tiller_panes::{HolderClient, Meta, Pane, Panes, Spawn, socket_path};

/// How long a test waits for anything attach or the holder does.
const WAIT: Duration = Duration::from_secs(10);

/// What the owner's screen showed before attach ran.
const OWNER_LINE: &str = "owner-line";

/// The legacy prefix, `Ctrl-\`, and the same key in the kitty encoding.
const PREFIX: &[u8] = b"\x1c";
const KITTY_PREFIX: &[u8] = b"\x1b[92;5u";

/// A running holder on `world`'s home, and a client of it.
struct Holder {
    _running: Running,
    client: HolderClient,
}

impl Holder {
    fn start(world: &World) -> Self {
        let log = world.dir.path().join("holder.log");
        let running = Running::new(
            world
                .bare_command()
                .arg("holder")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let socket = socket_path(&world.home());
        let deadline = Instant::now() + WAIT;
        let client = loop {
            match HolderClient::connect(&socket, "daemon") {
                Ok(client) => break client,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(err) => panic!("the holder did not listen: {err}"),
            }
        };
        Self {
            _running: running,
            client,
        }
    }

    /// A pane that runs `script` in `sh`, labelled `label`.
    fn spawn(&self, world: &World, label: &str, script: &str) -> Pane {
        let meta = Meta::from([
            ("label".to_owned(), label.to_owned()),
            ("project".to_owned(), "alpha".to_owned()),
            ("session".to_owned(), format!("session-{label}")),
        ]);
        let spawn = Spawn {
            key: format!("key-{label}"),
            argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            cwd: world.dir.path().display().to_string(),
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
            size: None,
            meta,
        };
        self.client.spawn(&spawn).unwrap()
    }

    fn pane(&self, pane: &Pane) -> Pane {
        self.client.get(&pane.pane).unwrap().unwrap()
    }

    /// Waits until the pane's visible text has a row that holds `text`.
    fn shows(&self, pane: &Pane, text: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let rows = self
                .client
                .screen(&pane.pane, Read::Visible, Format::Text)
                .unwrap()
                .rows_text;
            if rows.iter().any(|row| row.contains(text)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no {text:?} in the pane: {rows:#?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the pane's size is `size`.
    fn sized(&self, pane: &Pane, size: (u16, u16)) {
        let deadline = Instant::now() + WAIT;
        loop {
            let got = self.pane(pane);
            if (got.cols, got.rows) == size {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the pane stayed at {}x{}, not {}x{}",
                got.cols,
                got.rows,
                size.0,
                size.1
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The owner's terminal: a PTY whose far end is `tiller attach`'s stdin,
/// stdout and stderr. The test keeps its own handle on that end to read
/// the terminal's modes.
struct Outer {
    controller: OwnedFd,
    terminal: OwnedFd,
    size: (u16, u16),
    written: Arc<Mutex<Vec<u8>>>,
    child: Option<Running>,
}

impl Outer {
    fn new(cols: u16, rows: u16) -> Self {
        use rustix::pty::OpenptFlags;
        let controller = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        rustix::pty::grantpt(&controller).unwrap();
        rustix::pty::unlockpt(&controller).unwrap();
        let path = rustix::pty::ptsname(&controller, Vec::new()).unwrap();
        // Never the test's controlling terminal.
        let flags = rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY;
        let terminal = rustix::fs::open(path.as_c_str(), flags, rustix::fs::Mode::empty()).unwrap();
        let outer = Self {
            controller,
            terminal,
            size: (cols, rows),
            written: Arc::default(),
            child: None,
        };
        outer.resize(cols, rows);
        let mut reader = std::fs::File::from(outer.controller.try_clone().unwrap());
        let written = Arc::clone(&outer.written);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(len) = reader.read(&mut buf) {
                if len == 0 {
                    break;
                }
                written.lock().unwrap().extend_from_slice(&buf[..len]);
            }
        });
        outer
    }

    fn resize(&self, cols: u16, rows: u16) {
        let size = rustix::termios::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(&self.controller, size).unwrap();
    }

    /// Runs `tiller attach <args>` on this terminal as the owner, with
    /// `env` added.
    fn attach(&mut self, world: &World, args: &[&str], env: &[(&str, &str)]) {
        let mut command = world.bare_command();
        command
            .arg("attach")
            .args(args)
            // The stand-in for an owner with no Claude Code behind them:
            // these tests may themselves run under one.
            .env("TILLER_TEST_OWNER", "1")
            .envs(env.iter().copied())
            .stdin(self.terminal.try_clone().unwrap())
            .stdout(self.terminal.try_clone().unwrap())
            .stderr(self.terminal.try_clone().unwrap());
        self.child = Some(Running::new(command.spawn().unwrap()));
    }

    /// Types `bytes` on the owner's keyboard.
    fn press(&self, bytes: &[u8]) {
        let mut file = std::fs::File::from(self.controller.try_clone().unwrap());
        file.write_all(bytes).unwrap();
    }

    /// Everything attach wrote so far, as text.
    fn written(&self) -> String {
        String::from_utf8_lossy(&self.written.lock().unwrap()).into_owned()
    }

    /// The owner's screen as a terminal shows it: the owner's own line,
    /// then everything attach wrote.
    fn screen(&self) -> Screen {
        let mut model = Model::new(Size {
            cols: self.size.0,
            rows: self.size.1,
        });
        model.feed(format!("{OWNER_LINE}\r\n").as_bytes()).unwrap();
        model.feed(&self.written.lock().unwrap()).unwrap();
        model.screen(Read::Visible, Format::Text).unwrap()
    }

    /// Waits until the owner's screen passes `done`, and returns it.
    fn until(&self, what: &str, done: impl Fn(&Screen) -> bool) -> Screen {
        let deadline = Instant::now() + WAIT;
        loop {
            let screen = self.screen();
            if done(&screen) {
                return screen;
            }
            assert!(
                Instant::now() < deadline,
                "no {what} on the owner's screen: {:#?}\nwritten: {:?}",
                screen.rows_text,
                self.written()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until a row of the owner's screen holds `text`.
    fn shows(&self, text: &str) -> Screen {
        self.until(text, |screen| {
            screen.rows_text.iter().any(|row| row.contains(text))
        })
    }

    /// Whether the owner's terminal is in raw mode: no line editing, no
    /// echo.
    fn raw(&self) -> bool {
        let modes = tcgetattr(&self.terminal).unwrap().local_modes;
        !modes.contains(LocalModes::ICANON) && !modes.contains(LocalModes::ECHO)
    }

    /// Waits for attach to exit, and returns its code.
    fn exit(&mut self) -> i32 {
        let child = self.child.as_mut().expect("attach runs");
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "attach did not exit: {:?}",
                self.written()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A file a pane's `cat` writes the keys it gets to.
fn keys_file(world: &World, name: &str) -> PathBuf {
    world.dir.path().join(name)
}

/// A pane script that takes keys raw and writes them, byte for byte, to
/// `file`.
fn cat_to(file: &Path) -> String {
    format!("stty raw -echo; echo ready; exec cat > {}", file.display())
}

/// Waits until `file` holds `want`.
fn holds(file: &Path, want: &[u8]) {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = std::fs::read(file).unwrap_or_default();
        if got == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{} holds {:?}, not {:?}",
            file.display(),
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(want)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// 9.5: attach puts the terminal in raw mode and shows the pane with the
// modes its app turned on. Detach gives the owner's terminal back as it
// was: line mode, every mode off, the cursor shown, the main screen with
// the owner's own line.
#[test]
fn attach_runs_raw_and_detach_gives_the_terminal_back() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(
        &world,
        "builder",
        // Bracketed paste, the mouse, app cursor keys, the kitty flags, the
        // alternate screen, and the cursor hidden.
        "printf '\\033[?1049h\\033[?2004h\\033[?1000h\\033[?1006h\\033[?1h\\033[>1u\\033[?25l'; \
         echo pane-ready; exec cat",
    );
    holder.shows(&pane, "pane-ready");
    let mut outer = Outer::new(80, 24);
    assert!(!outer.raw());
    outer.attach(&world, &["session-builder"], &[]);
    let screen = outer.shows("pane-ready");
    assert!(outer.raw(), "attach left the terminal in line mode");
    for mode in ["bracketed_paste", "mouse_click", "mouse_sgr", "app_cursor"] {
        assert!(
            screen.modes.iter().any(|m| m == mode),
            "{mode}: {:?}",
            screen.modes
        );
    }
    assert!(!screen.cursor.visible);
    // A writing client sets the size to its window.
    holder.sized(&pane, (80, 24));

    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
    assert!(!outer.raw(), "detach left the terminal raw");
    let screen = outer.screen();
    // What a fresh terminal has on, and nothing more.
    let fresh = Outer::new(80, 24).screen().modes;
    assert_eq!(screen.modes, fresh, "modes left on after detach");
    assert!(screen.cursor.visible, "the cursor stayed hidden");
    assert_eq!(screen.rows_text[0], OWNER_LINE, "{:#?}", screen.rows_text);
    assert!(
        !screen.text().contains("pane-ready"),
        "{:#?}",
        screen.rows_text
    );
    // The pane runs on.
    assert_eq!(holder.pane(&pane).state, tiller_panes::PaneState::Running);
}

// 9.5: the prefix works in the kitty encoding as in the legacy one: it
// opens the help, and it detaches.
#[test]
fn the_prefix_works_in_the_kitty_encoding() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, "builder", "echo pane-ready; exec cat");
    holder.shows(&pane, "pane-ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-builder"], &[]);
    outer.shows("pane-ready");
    outer.press(KITTY_PREFIX);
    // A terminal that reports key releases sends the `?` and its release.
    outer.press(b"?\x1b[63;1:3u");
    outer.shows("tiller attach");
    std::thread::sleep(Duration::from_millis(100));
    outer.shows("tiller attach");
    outer.press(b"x");
    outer.until("the help closed", |screen| {
        !screen.text().contains("tiller attach")
    });
    outer.press(KITTY_PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
    assert!(!outer.raw());
}

// 9.5: the prefix twice sends one prefix, in the encoding it was typed in,
// and every other key reaches the pane as typed.
#[test]
fn the_prefix_twice_sends_one_prefix_to_the_pane() {
    let world = World::new();
    let holder = Holder::start(&world);
    let file = keys_file(&world, "keys");
    let pane = holder.spawn(&world, "builder", &cat_to(&file));
    holder.shows(&pane, "ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-builder"], &[]);
    outer.shows("ready");
    outer.press(b"a\x1c\x1cb");
    outer.press(KITTY_PREFIX);
    outer.press(KITTY_PREFIX);
    outer.press(b"\x1b[A");
    holds(&file, b"a\x1cb\x1b[92;5u\x1b[A");
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
}

// D3, 9.5: `Ctrl-\ ?` lists every prefix key, and the next key only closes
// the help: the pane never gets it.
#[test]
fn the_help_lists_every_key_and_swallows_the_key_that_closes_it() {
    let world = World::new();
    let holder = Holder::start(&world);
    let file = keys_file(&world, "keys");
    let pane = holder.spawn(&world, "builder", &cat_to(&file));
    holder.shows(&pane, "ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-builder"], &[]);
    outer.shows("ready");
    outer.press(PREFIX);
    outer.press(b"?");
    let screen = outer.shows("tiller attach");
    let text = screen.text();
    for row in [
        "Ctrl-\\ d       detach",
        "Ctrl-\\ s       the list of agents",
        "Ctrl-\\ n       the next agent",
        "Ctrl-\\ p       the previous agent",
        "Ctrl-\\ Ctrl-\\  send one Ctrl-\\ to the agent",
        "Ctrl-\\ ?       this help",
        "any key closes this help",
    ] {
        assert!(text.contains(row), "no {row:?} in the help:\n{text}");
    }
    outer.press(b"z");
    outer.until("the help closed", |screen| {
        !screen.text().contains("tiller attach")
    });
    outer.press(b"y");
    holds(&file, b"y");
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
}

// 9.5: two clients on one pane see the same output, and the pane takes the
// size of the client that typed last.
#[test]
fn two_clients_share_a_pane_and_the_size_follows_the_last_typer() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, "builder", "echo pane-ready; exec cat");
    holder.shows(&pane, "pane-ready");
    let mut small = Outer::new(80, 24);
    small.attach(&world, &["session-builder"], &[]);
    small.shows("pane-ready");
    holder.sized(&pane, (80, 24));
    let mut large = Outer::new(100, 30);
    large.attach(&world, &["session-builder"], &[]);
    large.shows("pane-ready");
    holder.sized(&pane, (100, 30));
    assert_eq!(holder.pane(&pane).attach_clients, 2);

    small.press(b"from-small\r");
    small.shows("from-small");
    large.shows("from-small");
    holder.sized(&pane, (80, 24));
    large.press(b"from-large\r");
    small.shows("from-large");
    holder.sized(&pane, (100, 30));

    // The one that stays holds the size.
    large.press(PREFIX);
    large.press(b"d");
    assert_eq!(large.exit(), 0);
    holder.sized(&pane, (80, 24));
    assert_eq!(holder.pane(&pane).attach_clients, 1);
    small.press(PREFIX);
    small.press(b"d");
    assert_eq!(small.exit(), 0);
}

// 9.5: a read-only client shows the pane, sends no key and takes no lease:
// the pane keeps its size.
#[test]
fn read_only_sends_no_keys_and_takes_no_lease() {
    let world = World::new();
    let holder = Holder::start(&world);
    let file = keys_file(&world, "keys");
    let pane = holder.spawn(&world, "builder", &cat_to(&file));
    holder.shows(&pane, "ready");
    let start = (holder.pane(&pane).cols, holder.pane(&pane).rows);
    assert_ne!(start, (80, 24));
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-builder", "--read-only"], &[]);
    outer.shows("ready");
    assert_eq!(holder.pane(&pane).attach_clients, 1);
    outer.press(b"typed");
    outer.resize(90, 20);
    // Past a size poll and a key's way to the holder.
    std::thread::sleep(Duration::from_millis(400));
    holder.client.raw_input(pane.num, b"z").unwrap();
    holds(&file, b"z");
    assert_eq!((holder.pane(&pane).cols, holder.pane(&pane).rows), start);
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
    assert!(!outer.raw());
}

// 9.5: the switcher lists the agents from the holder alone, so it works
// with no daemon: it opens with no target, and Enter attaches.
#[test]
fn the_switcher_lists_the_agents_and_attaches_with_the_daemon_down() {
    let world = World::new();
    let holder = Holder::start(&world);
    let first = holder.spawn(&world, "builder-api", "echo api-ready; exec cat");
    let second = holder.spawn(&world, "builder-board", "echo board-ready; exec cat");
    holder.shows(&first, "api-ready");
    holder.shows(&second, "board-ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &[], &[]);
    let screen = outer.shows("agents");
    let text = screen.text();
    for row in [
        "alpha",
        "●  builder-api",
        "●  builder-board",
        "↑↓ move  enter attach  esc close",
    ] {
        assert!(text.contains(row), "no {row:?} in the switcher:\n{text}");
    }
    outer.press(b"\x1b[B");
    outer.press(b"\r");
    outer.shows("board-ready");
    outer.until("the switcher closed", |screen| {
        !screen.text().contains("enter attach")
    });

    // From a pane, the prefix opens it again, and Escape closes it.
    outer.press(PREFIX);
    outer.press(b"s");
    outer.shows("enter attach");
    outer.press(b"\x1b");
    outer.until("the switcher closed", |screen| {
        !screen.text().contains("enter attach")
    });
    outer.shows("board-ready");
    // `n` moves to the next agent, which wraps around to the first.
    outer.press(PREFIX);
    outer.press(b"n");
    outer.shows("api-ready");
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
}

// 9.5: a target that names no running pane opens the switcher with a note.
#[test]
fn an_ended_or_unknown_target_opens_the_switcher_with_a_note() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, "done", "exit 3");
    let deadline = Instant::now() + WAIT;
    while holder.pane(&pane).state == tiller_panes::PaneState::Running {
        assert!(Instant::now() < deadline, "the pane did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-done"], &[]);
    let text = outer.shows("agents").text();
    assert!(text.contains("session-done has ended"), "{text}");
    assert!(
        text.contains("✕  done") && text.contains("exited 3"),
        "{text}"
    );
    // Escape with no pane shown detaches.
    outer.press(b"\x1b");
    assert_eq!(outer.exit(), 0);
    assert!(!outer.raw());

    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["nothing-by-this-name"], &[]);
    outer.shows("no agent matches nothing-by-this-name");
    outer.press(b"\x1b");
    assert_eq!(outer.exit(), 0);
}

// D12: an agent never attaches. A fleet caller is refused in the binary,
// before the terminal changes.
#[test]
fn attach_refuses_a_fleet_caller() {
    let world = World::new();
    let _holder = Holder::start(&world);
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &[], &[("CLAUDECODE", "1")]);
    assert_eq!(outer.exit(), 6, "{}", outer.written());
    let written = outer.written();
    assert!(written.contains("forbidden"), "{written}");
    assert!(written.contains("CLAUDECODE is set"), "{written}");
    assert!(!outer.raw());

    // From a pipe, as an agent's shell runs it, it is refused too.
    let out = world
        .bare_command()
        .args(["--json", "attach"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["error"]["code"], "forbidden", "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stdin is not a terminal"),
        "{json}"
    );
}

// D12: a notification click runs attach for the owner with the session's
// `TILLER_SESSION_ID` set: it passes, and shows that pane.
#[test]
fn attach_lets_a_notification_click_through() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, "builder", "echo pane-ready; exec cat");
    holder.shows(&pane, "pane-ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(
        &world,
        &["session-builder"],
        &[("TILLER_SESSION_ID", "session-builder")],
    );
    outer.shows("pane-ready");
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
}

// From H06: with no holder on the host, attach starts one, then shows the
// switcher from it.
#[test]
fn attach_starts_a_holder_when_none_runs() {
    let world = World::new();
    // The detached holder is no child of the test: stop it by its pid.
    let _stop = Stop(world.home().join("run/holder.lock"));
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &[], &[]);
    outer.shows("no agents run on this host");
    assert!(HolderClient::connect(socket_path(&world.home()), "cli").is_ok());
    outer.press(b"\x1b");
    assert_eq!(outer.exit(), 0);
}

/// Stops the holder its lock file names, when it drops.
struct Stop(PathBuf);

impl Drop for Stop {
    fn drop(&mut self) {
        let Some(pid) = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .map(Pid::from_raw)
        else {
            return;
        };
        let _ = kill(pid, Sig::SIGTERM);
        let deadline = Instant::now() + WAIT;
        while kill(pid, None).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if kill(pid, None).is_ok() {
            let _ = kill(pid, Sig::SIGKILL);
        }
    }
}

// A `switch`, as the board's attach button sends it, moves every attach
// client to that pane.
#[test]
fn a_switch_moves_the_attach_client_to_the_pane() {
    let world = World::new();
    let holder = Holder::start(&world);
    let first = holder.spawn(&world, "builder-api", "echo api-ready; exec cat");
    let second = holder.spawn(&world, "builder-board", "echo board-ready; exec cat");
    holder.shows(&first, "api-ready");
    holder.shows(&second, "board-ready");
    let mut outer = Outer::new(80, 24);
    outer.attach(&world, &["session-builder-api"], &[]);
    outer.shows("api-ready");
    let deadline = Instant::now() + WAIT;
    while holder.client.switch(&second.pane).unwrap() == 0 {
        assert!(Instant::now() < deadline, "attach never watched");
        std::thread::sleep(Duration::from_millis(20));
    }
    outer.shows("board-ready");
    outer.press(PREFIX);
    outer.press(b"d");
    assert_eq!(outer.exit(), 0);
}
