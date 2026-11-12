//! "Open in herdr": focus an agent's pane, then put herdr in front of the
//! person at this machine's screen (`docs/spec/protocol.md` 7.4).
//!
//! With a terminal app configured, an attached herdr client means the app
//! is raised; no attached client means the app opens a new one, and the
//! focus waits for it. Without one, the focus is all there is, and the
//! answer says whether any client on this machine can show it. The app goes
//! through [`Terminal`] so tests never open one.

use std::ffi::OsString;
use std::io;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use tiller_panes::open::{MacApp, Opened, Terminal, Wait};

use crate::client::{Cli, HerdrError};

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Herdr(#[from] HerdrError),

    #[error("{app} did not open: {source}")]
    Terminal { app: String, source: io::Error },

    #[error("no herdr client attached in {app} within {within:?}")]
    NotAttached { app: String, within: Duration },
}

/// The local herdr server the screen shows.
pub trait Screen {
    fn client_attached(&self) -> Result<bool, HerdrError>;
    fn attach_command(&self) -> Vec<OsString>;
}

impl Screen for Cli {
    fn client_attached(&self) -> Result<bool, HerdrError> {
        Cli::client_attached(self)
    }

    fn attach_command(&self) -> Vec<OsString> {
        Cli::attach_command(self)
    }
}

/// How long the screen's probe may take. herdr answers it at once; one
/// that does not is taken as attached, so a stuck probe costs this much.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One open at a time: a second click while a client is still attaching
/// waits for it, then raises the app instead of opening another window.
static OPENING: Mutex<()> = Mutex::new(());

/// Focuses an agent's pane with `focus` and puts herdr in front of the
/// screen. `screen` is always the local server: the window on this machine
/// is the one that matters, even when `focus` reaches a saved machine.
pub fn open_agent(
    screen: &dyn Screen,
    terminal: Option<&dyn Terminal>,
    wait: Wait,
    focus: impl Fn() -> Result<(), HerdrError>,
) -> Result<Opened, OpenError> {
    let _one = OPENING.lock().unwrap_or_else(PoisonError::into_inner);
    // A herdr that cannot answer the probe (older than the title call) is
    // taken as attached: raising an app beats opening a second window.
    let attached = screen.client_attached().unwrap_or_else(|error| {
        tracing::debug!(%error, "herdr cannot say whether a client is attached");
        true
    });
    focus()?;
    let Some(terminal) = terminal else {
        return Ok(if attached {
            Opened::Focused
        } else {
            Opened::Unseen
        });
    };
    let app = || terminal.name().to_owned();
    if attached {
        terminal
            .raise()
            .map_err(|source| OpenError::Terminal { app: app(), source })?;
        return Ok(Opened::Raised);
    }
    terminal
        .open(&screen.attach_command())
        .map_err(|source| OpenError::Terminal { app: app(), source })?;
    let deadline = Instant::now() + wait.within;
    while !matches!(screen.client_attached(), Ok(true)) {
        if Instant::now() >= deadline {
            return Err(OpenError::NotAttached {
                app: app(),
                within: wait.within,
            });
        }
        std::thread::sleep(wait.every);
    }
    // A new client opens on the focused workspace; focusing again covers a
    // client that attached on another one.
    focus()?;
    Ok(Opened::Launched)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::Mutex;

    use super::*;

    /// A local server whose probe answers from a script, one answer a call;
    /// the last answer repeats.
    struct FakeScreen {
        answers: RefCell<Vec<Result<bool, &'static str>>>,
        probes: Cell<usize>,
    }

    impl FakeScreen {
        fn new(answers: Vec<Result<bool, &'static str>>) -> Self {
            Self {
                answers: RefCell::new(answers),
                probes: Cell::new(0),
            }
        }
    }

    impl Screen for FakeScreen {
        fn client_attached(&self) -> Result<bool, HerdrError> {
            self.probes.set(self.probes.get() + 1);
            let mut answers = self.answers.borrow_mut();
            let answer = if answers.len() > 1 {
                answers.remove(0)
            } else {
                answers[0]
            };
            answer.map_err(|code| HerdrError::Api {
                call: "terminal title".into(),
                code: code.into(),
                message: String::new(),
            })
        }

        fn attach_command(&self) -> Vec<OsString> {
            vec![
                "/home/user/bin/herdr".into(),
                "--session".into(),
                "lab".into(),
            ]
        }
    }

    #[derive(Default)]
    struct FakeApp {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl FakeApp {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: String) -> io::Result<()> {
            self.calls.lock().unwrap().push(call);
            if self.fail {
                return Err(io::Error::other("no such app"));
            }
            Ok(())
        }
    }

    impl Terminal for FakeApp {
        fn name(&self) -> &str {
            "Ghostty"
        }

        fn raise(&self) -> io::Result<()> {
            self.record("raise".into())
        }

        fn open(&self, command: &[OsString]) -> io::Result<()> {
            let command: Vec<_> = command.iter().map(|w| w.to_string_lossy()).collect();
            self.record(format!("open {}", command.join(" ")))
        }
    }

    fn quick() -> Wait {
        Wait {
            every: Duration::from_millis(1),
            within: Duration::from_millis(200),
        }
    }

    fn counted(focuses: &Cell<usize>) -> impl Fn() -> Result<(), HerdrError> + '_ {
        move || {
            focuses.set(focuses.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn attached_client_raises_the_app() {
        let screen = FakeScreen::new(vec![Ok(true)]);
        let app = FakeApp::default();
        let focuses = Cell::new(0);
        let opened = open_agent(&screen, Some(&app), quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Raised);
        assert_eq!(app.calls(), ["raise"]);
        assert_eq!(focuses.get(), 1);
    }

    #[test]
    fn no_client_opens_the_app_on_herdr_and_focuses_once_it_attaches() {
        let screen = FakeScreen::new(vec![Ok(false), Ok(false), Ok(false), Ok(true)]);
        let app = FakeApp::default();
        let focuses = Cell::new(0);
        let opened = open_agent(&screen, Some(&app), quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Launched);
        assert_eq!(app.calls(), ["open /home/user/bin/herdr --session lab"]);
        assert_eq!(screen.probes.get(), 4);
        assert_eq!(focuses.get(), 2);
    }

    #[test]
    fn a_client_that_never_attaches_is_an_error_after_the_focus() {
        let screen = FakeScreen::new(vec![Ok(false)]);
        let app = FakeApp::default();
        let focuses = Cell::new(0);
        let error = open_agent(&screen, Some(&app), quick(), counted(&focuses)).unwrap_err();
        assert!(
            matches!(error, OpenError::NotAttached { ref app, .. } if app == "Ghostty"),
            "{error:?}"
        );
        assert_eq!(focuses.get(), 1);
    }

    #[test]
    fn no_terminal_focuses_and_says_whether_a_client_can_show_it() {
        let focuses = Cell::new(0);
        let attached = FakeScreen::new(vec![Ok(true)]);
        let opened = open_agent(&attached, None, quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Focused);
        let detached = FakeScreen::new(vec![Ok(false)]);
        let opened = open_agent(&detached, None, quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Unseen);
        assert_eq!(focuses.get(), 2);
    }

    #[test]
    fn a_second_open_while_the_client_attaches_raises_instead() {
        use std::sync::atomic::{AtomicBool, Ordering};

        /// A screen whose client attaches a moment after the app opens.
        #[derive(Default)]
        struct Slow {
            attached: AtomicBool,
            calls: Mutex<Vec<String>>,
        }

        impl Screen for Slow {
            fn client_attached(&self) -> Result<bool, HerdrError> {
                Ok(self.attached.load(Ordering::SeqCst))
            }

            fn attach_command(&self) -> Vec<OsString> {
                vec!["herdr".into()]
            }
        }

        impl Terminal for Slow {
            fn name(&self) -> &str {
                "Ghostty"
            }

            fn raise(&self) -> io::Result<()> {
                self.calls.lock().unwrap().push("raise".into());
                Ok(())
            }

            fn open(&self, _: &[OsString]) -> io::Result<()> {
                self.calls.lock().unwrap().push("open".into());
                std::thread::sleep(Duration::from_millis(50));
                self.attached.store(true, Ordering::SeqCst);
                Ok(())
            }
        }

        let slow = Slow::default();
        let mut opened: Vec<Opened> = std::thread::scope(|scope| {
            let clicks: Vec<_> = (0..2)
                .map(|_| scope.spawn(|| open_agent(&slow, Some(&slow), quick(), || Ok(()))))
                .collect();
            clicks
                .into_iter()
                .map(|click| click.join().unwrap().unwrap())
                .collect()
        });
        opened.sort_by_key(|opened| format!("{opened:?}"));
        assert_eq!(opened, [Opened::Launched, Opened::Raised]);
        let mut calls = slow.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(calls, ["open", "raise"]);
    }

    #[test]
    fn a_herdr_without_the_probe_is_taken_as_attached() {
        let screen = FakeScreen::new(vec![Err("unknown_method")]);
        let app = FakeApp::default();
        let focuses = Cell::new(0);
        let opened = open_agent(&screen, Some(&app), quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Raised);
        let opened = open_agent(&screen, None, quick(), counted(&focuses)).unwrap();
        assert_eq!(opened, Opened::Focused);
    }

    #[test]
    fn a_failed_focus_leaves_the_app_alone() {
        let screen = FakeScreen::new(vec![Ok(false)]);
        let app = FakeApp::default();
        let error = open_agent(&screen, Some(&app), quick(), || {
            Err(HerdrError::Unreachable("down".into()))
        })
        .unwrap_err();
        assert!(matches!(error, OpenError::Herdr(_)), "{error:?}");
        assert!(app.calls().is_empty());
    }

    #[test]
    fn an_app_that_will_not_open_names_itself() {
        let screen = FakeScreen::new(vec![Ok(true)]);
        let app = FakeApp {
            fail: true,
            ..FakeApp::default()
        };
        let focuses = Cell::new(0);
        let error = open_agent(&screen, Some(&app), quick(), counted(&focuses)).unwrap_err();
        assert_eq!(error.to_string(), "Ghostty did not open: no such app");
    }
}
