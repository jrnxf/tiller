//! A stand-in for Claude Code in a terminal holder pane, for end-to-end
//! tests only (`terminal-holder.md` 9.2). It draws a composer the way Claude
//! Code 2.x does, so `composer::classify` reads it, and echoes what is typed
//! into it. Enter submits, but an Enter that comes within the paste window
//! of the last typed byte is a newline, as Claude reads a pasted line. A
//! submit shows the turn busy for a while; Escape interrupts it, and
//! `/exit` ends the program.
//!
//! `fake-agent --counter` prints `count <n>` every 50 ms instead, for the
//! restart tests' no-gap check.
//!
//! `fake-agent --replay <file> <from> <to> <cols>x<rows>` plays a raw
//! recording of Claude Code's output (`tests/fixtures/claude-raw/`) once its
//! window has that size: the first `from` bytes at once, then one frame up
//! to byte `to` each `FAKE_AGENT_FRAME_MS` (default 400), logging any input
//! that comes meanwhile. Then it goes on as the stand-in above.
//!
//! Environment:
//! - `FAKE_AGENT_LOG`: a file each start, read, submit, interrupt, tick,
//!   replayed and exit is appended to as a JSON line.
//! - `FAKE_AGENT_BUSY_MS`: how long a turn runs (default 300).
//! - `FAKE_AGENT_PASTE_MS`: the paste window (default 100).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{OptionalActions, tcgetattr, tcgetwinsize, tcsetattr};
use serde_json::{Value, json};

const PROMPT: &str = "\u{276f}\u{a0}";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("2.1.0 (Claude Code)");
        return;
    }
    log(&json!({"event": "start", "argv": args, "pid": std::process::id()}));
    if args.first().map(String::as_str) == Some("--counter") {
        counter();
    }
    if args.first().map(String::as_str) == Some("--replay") {
        replay(&args[1..5]);
    }
    Agent::new(args).run();
}

/// Appends `entry` to `FAKE_AGENT_LOG`, when it is set.
fn log(entry: &Value) {
    let Some(path) = std::env::var_os("FAKE_AGENT_LOG") else {
        return;
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    // One write per line, so lines from two processes never interleave.
    file.write_all(format!("{entry}\n").as_bytes()).unwrap();
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn env_ms(key: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default),
    )
}

fn counter() -> ! {
    let mut out = std::io::stdout();
    let mut n: u64 = 0;
    loop {
        log(&json!({"event": "tick", "n": n, "ms": now_ms()}));
        // A pane that is gone takes the output with it; the hup ends us.
        let _ = writeln!(out, "count {n}");
        let _ = out.flush();
        std::thread::sleep(Duration::from_millis(50));
        n += 1;
    }
}

/// Where a frame ends: Claude Code paints each one between the
/// synchronized output marks.
const FRAME_END: &[u8] = b"\x1b[?2026l";

fn replay(args: &[String]) {
    let raw = std::fs::read(&args[0]).unwrap();
    let from: usize = args[1].parse().unwrap();
    let to: usize = args[2].parse().unwrap();
    let (cols, rows) = args[3].split_once('x').unwrap();
    let size: (u16, u16) = (cols.parse().unwrap(), rows.parse().unwrap());
    let pace = env_ms("FAKE_AGENT_FRAME_MS", 400);
    let stdin = std::io::stdin();
    if let Ok(mut term) = tcgetattr(stdin.as_fd()) {
        term.make_raw();
        let _ = tcsetattr(stdin.as_fd(), OptionalActions::Now, &term);
    }
    while tcgetwinsize(stdin.as_fd()).map(|w| (w.ws_col, w.ws_row)) != Ok(size) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = std::io::stdout();
    let mut at = from;
    out.write_all(&raw[..at]).unwrap();
    out.flush().unwrap();
    while at < to {
        let end = raw[at..to]
            .windows(FRAME_END.len())
            .position(|w| w == FRAME_END)
            .map_or(to, |i| at + i + FRAME_END.len());
        out.write_all(&raw[at..end]).unwrap();
        out.flush().unwrap();
        at = end;
        let until = Instant::now() + pace;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: i64::try_from(left.as_nanos()).unwrap_or(0),
            };
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            if matches!(poll(&mut fds, Some(&timeout)), Ok(n) if n > 0) {
                let mut buf = [0_u8; 4096];
                if let Ok(read) = rustix::io::read(&stdin, &mut buf) {
                    log(
                        &json!({"event": "read", "bytes": String::from_utf8_lossy(&buf[..read]), "ms": now_ms()}),
                    );
                }
            }
        }
    }
    log(&json!({"event": "replayed", "ms": now_ms()}));
}

struct Agent {
    history: Vec<String>,
    composer: String,
    /// When the running turn ends.
    busy_until: Option<Instant>,
    last_typed: Option<Instant>,
    /// Inside a bracketed paste.
    pasting: bool,
    /// Bytes of an escape sequence not finished in the last read.
    pending: Vec<u8>,
    paste_window: Duration,
    busy_for: Duration,
    size: (u16, u16),
}

impl Agent {
    fn new(args: Vec<String>) -> Self {
        Self {
            history: vec![format!("fake-agent {}", args.join(" "))],
            composer: String::new(),
            busy_until: None,
            last_typed: None,
            pasting: false,
            pending: Vec::new(),
            paste_window: env_ms("FAKE_AGENT_PASTE_MS", 100),
            busy_for: env_ms("FAKE_AGENT_BUSY_MS", 300),
            size: (0, 0),
        }
    }

    fn run(mut self) -> ! {
        let stdin = std::io::stdin();
        if let Ok(mut term) = tcgetattr(stdin.as_fd()) {
            term.make_raw();
            let _ = tcsetattr(stdin.as_fd(), OptionalActions::Now, &term);
        }
        self.draw();
        let mut buf = [0_u8; 4096];
        loop {
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 50_000_000,
            };
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => {}
                Ok(_) => match rustix::io::read(&stdin, &mut buf) {
                    Ok(0) | Err(_) => self.exit(),
                    Ok(read) => self.take(&buf[..read]),
                },
                Err(_) => self.exit(),
            }
            if self.busy_until.is_some_and(|until| Instant::now() >= until) {
                self.busy_until = None;
                self.history.push("\u{25cf} done".into());
                self.draw();
            }
            if self.window() != self.size {
                self.draw();
            }
        }
    }

    fn window(&self) -> (u16, u16) {
        match tcgetwinsize(std::io::stdin()) {
            Ok(size) if size.ws_col > 0 && size.ws_row > 0 => (size.ws_col, size.ws_row),
            _ => (80, 24),
        }
    }

    /// Takes one read of input.
    fn take(&mut self, bytes: &[u8]) {
        log(&json!({"event": "read", "bytes": String::from_utf8_lossy(bytes), "ms": now_ms()}));
        let mut bytes: Vec<u8> = std::mem::take(&mut self.pending)
            .into_iter()
            .chain(bytes.iter().copied())
            .collect();
        let mut text = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            at += 1;
            match byte {
                0x1b => {
                    let rest = &bytes[at..];
                    if rest.is_empty() {
                        self.typed(&mut text);
                        self.escape();
                        continue;
                    }
                    if rest[0] != b'[' {
                        // Alt plus a key: nothing this stand-in knows.
                        at += 1;
                        continue;
                    }
                    let Some(end) = rest[1..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                        // The sequence goes on in the next read.
                        self.pending = bytes.split_off(at - 1);
                        break;
                    };
                    let sequence = &rest[1..=end + 1];
                    match sequence {
                        b"200~" => self.pasting = true,
                        b"201~" => self.pasting = false,
                        _ => {}
                    }
                    at += end + 2;
                }
                b'\r' | b'\n' if self.pasting => text.push(b'\n'),
                b'\r' | b'\n' => {
                    self.typed(&mut text);
                    let fast = self
                        .last_typed
                        .is_some_and(|at| at.elapsed() < self.paste_window);
                    if fast {
                        self.composer.push('\n');
                        self.last_typed = Some(Instant::now());
                    } else {
                        self.submit();
                    }
                }
                0x7f | 0x08 => {
                    self.typed(&mut text);
                    self.composer.pop();
                }
                0x03 => {
                    text.clear();
                    self.composer.clear();
                }
                byte if byte >= 0x20 || byte == b'\t' => text.push(byte),
                _ => {}
            }
        }
        self.typed(&mut text);
        self.draw();
    }

    /// Adds `text` to the composer.
    fn typed(&mut self, text: &mut Vec<u8>) {
        if text.is_empty() {
            return;
        }
        self.composer.push_str(&String::from_utf8_lossy(text));
        text.clear();
        self.last_typed = Some(Instant::now());
    }

    fn escape(&mut self) {
        if self.busy_until.take().is_some() {
            log(&json!({"event": "interrupt"}));
            self.history.push("  \u{23bf} Interrupted".into());
        }
    }

    fn submit(&mut self) {
        let text = std::mem::take(&mut self.composer);
        if text.trim().is_empty() {
            return;
        }
        if text.trim() == "/exit" {
            self.exit();
        }
        log(&json!({"event": "submit", "text": text}));
        for (i, line) in text.lines().enumerate() {
            self.history
                .push(format!("{} {line}", if i == 0 { ">" } else { " " }));
        }
        self.busy_until = Some(Instant::now() + self.busy_for);
    }

    fn exit(&self) -> ! {
        log(&json!({"event": "exit"}));
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[H\x1b[2J");
        let _ = out.flush();
        std::process::exit(0)
    }

    /// Paints the whole screen: the history, then the composer between its
    /// rules, then the footer.
    fn draw(&mut self) {
        self.size = self.window();
        let (cols, rows) = self.size;
        let rule = "\u{2500}".repeat(usize::from(cols));
        let mut composer: Vec<String> = Vec::new();
        for (i, line) in self.composer.split('\n').enumerate() {
            composer.push(if i == 0 {
                format!("{PROMPT}{line}")
            } else {
                format!("  {line}")
            });
        }
        let busy = self.busy_until.is_some();
        let mut below = vec![rule.clone()];
        below.extend(composer);
        below.push(rule);
        below.push(if busy {
            "  esc to interrupt".into()
        } else {
            "  ? for shortcuts".into()
        });
        let mut above = self.history.clone();
        above.push(String::new());
        if busy {
            above.push("\u{273b} Working\u{2026}".into());
        }
        let room = usize::from(rows).saturating_sub(below.len());
        let skip = above.len().saturating_sub(room);
        let screen: Vec<&String> = above[skip..].iter().chain(below.iter()).collect();
        let mut out = std::io::stdout();
        let mut paint = String::from("\x1b[H\x1b[2J");
        for (i, row) in screen.iter().enumerate() {
            if i > 0 {
                paint.push_str("\r\n");
            }
            paint.push_str(&row.chars().take(usize::from(cols)).collect::<String>());
        }
        let _ = out.write_all(paint.as_bytes());
        let _ = out.flush();
    }
}
