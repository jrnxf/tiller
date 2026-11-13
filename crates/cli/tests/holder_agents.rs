//! Agents on the real terminal holder, with `fake-agent` in place of Claude
//! Code (`terminal-holder.md` 9.2): a run launched into a holder pane takes
//! a message once, stops, resumes and takes an interrupt, the notifier gets
//! the orchestrator's pane, and a daemon killed or restarted mid-work types
//! nothing twice and leaves the pane's output whole.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::holder::{View, WAIT, cookie, fake_agent, script, start_holder};
use common::{World, git_only_daemon};
use serde_json::Value;
use tiller_core::state::runs::{NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx};
use tiller_panes::composer::{Composer, classify};
use tiller_panes::{Format, HolderClient, Key, Live, PaneId, Panes, Read, Subscribe};

/// A home whose launches run `fake-agent` as Claude Code. The `claude` the
/// launch line names fires the `SessionStart` hook Claude Code would, under
/// the Claude session id `claude-<session>`, then becomes `fake-agent`
/// logging to a file per session, which [`Agents::events`] reads. Its turns
/// run `busy_ms`, and a `--replay` plays a frame each [`FRAME_MS`].
struct Agents {
    world: World,
    worktree: PathBuf,
    logs: PathBuf,
}

/// The replay's frame pace: longer than the holder's 500 ms `quiet`, so the
/// holder source reads the screen again between frames as it does in a real
/// stream, and well under the 3 s settle of the output-quiet guard.
const FRAME_MS: u64 = 800;

impl Agents {
    fn new(busy_ms: u64) -> Self {
        Self::with_args(busy_ms, "")
    }

    /// [`Agents::new`] whose `fake-agent` takes `args` first, such as
    /// `--counter`.
    fn with_args(busy_ms: u64, args: &str) -> Self {
        let world = World::initialized();
        let root = world.dir.path().to_owned();
        let checkout = root.join("checkouts/alpha");
        std::fs::create_dir_all(&checkout).unwrap();
        world.must(&[
            "config",
            "set",
            "checkouts.alpha",
            checkout.to_str().unwrap(),
        ]);
        for (key, dir) in [
            ("spawn.pool_root", "pool"),
            ("spawn.scratch_root", "scratch"),
            ("spawn.orchestrator_home", "orchestrators"),
            ("spawn.checkouts_root", "checkouts"),
        ] {
            let dir = root.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            world.must(&["config", "set", key, dir.to_str().unwrap()]);
        }
        // No herdr runs: a launch or a delivery that reached it would fail.
        world.set_env("TILLER_HERDR", "/nonexistent/herdr");
        let logs = root.join("fake-agent");
        std::fs::create_dir_all(&logs).unwrap();
        let claude = root.join("claude-stub");
        script(
            &claude,
            &format!(
                "if [ \"$1\" = --version ]; then exec {agent} --version; fi\n\
                 {{ env; echo --; }} >> {logs}/\"$TILLER_SESSION_ID\".env\n\
                 id=claude-$TILLER_SESSION_ID\n\
                 if [ \"$1\" = --resume ]; then id=$id-resumed; fi\n\
                 printf '{{\"session_id\":\"%s\",\"source\":\"startup\"}}' \"$id\" \
                 | {tiller} hook SessionStart >/dev/null 2>&1\n\
                 exec env FAKE_AGENT_LOG={logs}/\"$TILLER_SESSION_ID\".jsonl FAKE_AGENT_BUSY_MS={busy_ms} FAKE_AGENT_FRAME_MS={FRAME_MS} {agent} {args} \"$@\"\n",
                agent = fake_agent().display(),
                tiller = env!("CARGO_BIN_EXE_tiller"),
                logs = logs.display(),
            ),
        );
        world.must(&["config", "set", "spawn.claude", claude.to_str().unwrap()]);
        let worktree = root.join("work/alpha-1");
        std::fs::create_dir_all(&worktree).unwrap();
        for args in [
            &["init", "-q", "-b", "alpha-1-build"][..],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "start",
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&worktree)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        Self {
            world,
            worktree,
            logs,
        }
    }

    /// The project's orchestrator, started in a holder pane. Returns its
    /// session and its pane.
    fn orchestrator(&self) -> (String, String) {
        let started = self.world.must(&["orchestrator", "start", "alpha"]);
        let session = started.data()["session"]["id"].as_str().unwrap().to_owned();
        let pane = self.pane(&session);
        assert!(tiller_core::ids::is_holder_pane(&pane), "{pane}");
        (session, pane)
    }

    fn pane(&self, session: &str) -> String {
        self.world
            .store()
            .conn()
            .query_row(
                "SELECT pane_id FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// A builder run on a new card, with its brief and no session yet,
    /// under a live orchestrator. Returns the run id.
    fn run(&self) -> String {
        self.orchestrator();
        self.world.must(&[
            "card", "create", "alpha", "--kind", "ship", "--title", "Fake",
        ]);
        let run = self
            .world
            .store()
            .run_start(
                &Ctx::new(Actor::Owner),
                None,
                NewRun {
                    card: "alpha-1".into(),
                    role: tiller_core::domain::RunRole::Builder,
                    model: "test-model".into(),
                    effort: tiller_core::domain::Effort::Medium,
                    host_id: None,
                    worktree_path: Some(self.worktree.to_str().unwrap().to_owned()),
                    branch: Some("alpha-1-build".into()),
                    base_branch: Some("main".into()),
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value
            .run
            .id;
        let scratch = self.world.dir.path().join("scratch").join(&run);
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join("brief.md"), "Make the tests pass.\n").unwrap();
        run
    }

    /// The run's live session and its pane.
    fn live(&self, run: &str) -> (String, String) {
        let session: String = self
            .world
            .store()
            .conn()
            .query_row(
                "SELECT id FROM sessions WHERE run_id = ?1 AND ended_at IS NULL",
                [run],
                |row| row.get(0),
            )
            .unwrap();
        let pane = self.pane(&session);
        (session, pane)
    }

    /// Every line the session's `fake-agent` logged so far.
    fn events(&self, session: &str) -> Vec<Value> {
        std::fs::read_to_string(self.logs.join(format!("{session}.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The environment of each start of `session`'s `claude`, oldest first.
    fn envs(&self, session: &str) -> Vec<std::collections::BTreeMap<String, String>> {
        std::fs::read_to_string(self.logs.join(format!("{session}.env")))
            .unwrap_or_default()
            .split("--\n")
            .filter(|block| !block.is_empty())
            .map(|block| {
                block
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect()
            })
            .collect()
    }

    fn of(&self, session: &str, event: &str) -> Vec<Value> {
        self.events(session)
            .into_iter()
            .filter(|e| e["event"] == event)
            .collect()
    }

    /// Waits until `done` holds for the log, or fails naming `what`.
    fn until(&self, session: &str, what: &str, done: impl Fn(&[Value]) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(&self.events(session)) {
            assert!(
                Instant::now() < deadline,
                "no {what} from fake-agent: {:#?}",
                self.events(session)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `tiller agent relaunch` of `run`, which starts its first session in
    /// a holder pane. Returns the session and the pane.
    fn launch(&self, run: &str) -> (String, String) {
        let done = self
            .world
            .must(&["agent", "relaunch", run, "--note", "first start"]);
        assert_eq!(done.data()["started"], "confirmed", "{:?}", done.data());
        let (session, pane) = self.live(run);
        assert!(tiller_core::ids::is_holder_pane(&pane), "{pane}");
        (session, pane)
    }
}

/// The text of the pane's visible screen.
fn screen(client: &HolderClient, pane: &str) -> String {
    client
        .screen(&pane.into(), Read::Visible, Format::Text)
        .unwrap()
        .text()
}

/// Types `line` into the pane the way an owner does: the text, then an
/// Enter after `fake-agent`'s paste window.
fn submit(client: &HolderClient, pane: &str, line: &str) {
    let pane = PaneId::from(pane);
    client.input(&pane, line).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    client.keys(&pane, &[Key::ENTER]).unwrap();
}

/// Waits until the pane's PTY is `cols` by `rows`.
fn sized(client: &HolderClient, pane: &str, cols: u16, rows: u16, what: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = client.get(&PaneId::from(pane)).unwrap().unwrap();
        if (now.cols, now.rows) == (cols, rows) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: the pane is {}x{}, not {cols}x{rows}",
            now.cols,
            now.rows
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the pane's screen holds `text`.
fn shows(client: &HolderClient, pane: &str, text: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = screen(client, pane);
        if now.contains(text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {text:?} on the screen:\n{now}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn has(events: &[Value], event: &str) -> bool {
    events.iter().any(|e| e["event"] == event)
}

/// Spec 9.2 with `fake-agent`: an interrupt reaches a working turn, a
/// message is typed once and submitted once, an `/exit` ends the agent, and
/// a resume starts `claude --resume` in a new holder pane.
#[test]
fn a_run_in_a_holder_pane_takes_an_interrupt_a_message_once_and_a_resume() {
    let agents = Agents::new(60_000);
    let (_holder, client) = start_holder(&agents.world);
    let run = agents.run();
    let (session, pane) = agents.launch(&run);
    agents.until(&session, "start", |events| has(events, "start"));

    // A long turn, which only the interrupt ends.
    submit(&client, &pane, "work on it");
    shows(&client, &pane, "esc to interrupt");
    let interrupted = agents.world.must(&["agent", "interrupt", &run]);
    assert_eq!(interrupted.data()["pane_id"], pane.as_str());
    agents.until(&session, "interrupt", |events| has(events, "interrupt"));
    shows(&client, &pane, "? for shortcuts");

    let sent = agents.world.must(&[
        "send",
        &session,
        "--kind",
        "steer",
        "--text",
        "look at the tests",
    ]);
    let id = sent.data()["id"].as_str().unwrap().to_owned();
    let (daemon, _port) = git_only_daemon(&agents.world);
    agents.until(&session, "the message", |events| {
        events.iter().filter(|e| e["event"] == "submit").count() == 2
    });
    // The agent reads the line once: no second copy comes later.
    std::thread::sleep(Duration::from_secs(4));
    drop(daemon);
    let submits = agents.of(&session, "submit");
    assert_eq!(submits.len(), 2, "{submits:#?}");
    let text = submits[1]["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("tiller inbox {id}")),
        "{text} {}",
        sent.data()
    );

    // The agent leaves, and a resume brings the same Claude session back.
    // Escape ends the message's turn first. A wait for the idle footer
    // keeps it apart from `/exit`, which in the same read is Alt+/.
    client
        .keys(&PaneId::from(pane.as_str()), &[Key::ESC])
        .unwrap();
    shows(&client, &pane, "? for shortcuts");
    submit(&client, &pane, "/exit");
    agents.until(&session, "exit", |events| has(events, "exit"));
    let resumed = agents.world.must(&["agent", "resume", &run]);
    let (again, new_pane) = agents.live(&run);
    assert_ne!(new_pane, pane, "{}", resumed.data());
    assert!(tiller_core::ids::is_holder_pane(&new_pane), "{new_pane}");
    // The resume goes on in the same session, with its Claude session.
    assert_eq!(again, session, "{}", resumed.data());
    agents.until(&session, "the resumed start", |events| {
        events.iter().filter(|e| e["event"] == "start").count() == 2
    });
    let argv = agents.of(&session, "start")[1]["argv"].clone();
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = argv.iter().position(|a| *a == "--resume").unwrap();
    assert_eq!(argv[at + 1], format!("claude-{session}"), "{argv:?}");
}

/// `tiller agent stop` of an orchestrator in a holder pane types `/exit`
/// at its empty composer, and the agent leaves.
#[test]
fn a_stop_of_an_orchestrator_in_a_holder_pane_ends_its_agent() {
    let agents = Agents::new(300);
    let (_holder, client) = start_holder(&agents.world);
    let (session, pane) = agents.orchestrator();
    agents.until(&session, "start", |events| has(events, "start"));
    shows(&client, &pane, "? for shortcuts");
    let stopped = agents.world.must(&["agent", "stop", &session]);
    agents.until(&session, "exit", |events| has(events, "exit"));
    assert_eq!(agents.of(&session, "submit").len(), 0, "{}", stopped.data());
    let ended: Option<String> = agents
        .world
        .store()
        .conn()
        .query_row(
            "SELECT ended_at FROM sessions WHERE id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended.is_some(), "{}", stopped.data());
}

/// Spec 7: the owner's `[notify] command` runs for a new attention item,
/// with the project's orchestrator session and holder pane, and the host's
/// label, in its environment.
#[test]
fn the_notify_command_gets_the_orchestrator_holder_pane_for_a_new_question() {
    let agents = Agents::new(300);
    let (_holder, _client) = start_holder(&agents.world);
    let (session, pane) = agents.orchestrator();
    let root = agents.world.dir.path();
    let out = root.join("notified");
    let notify = root.join("notify");
    script(
        &notify,
        &format!(
            // The daemon's PATH holds only git: a shell builtin writes it.
            "echo \"$TILLER_SESSION_ID $TILLER_PANE $TILLER_HOST\" > {}\n",
            out.display()
        ),
    );
    let config = agents.world.home().join("config.toml");
    let mut table = std::fs::read_to_string(&config).unwrap_or_default();
    table.push_str(&format!(
        "\n[notify]\ncommand = [\"{}\"]\n",
        notify.display()
    ));
    std::fs::write(&config, table).unwrap();
    agents.world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "Ask",
    ]);
    agents
        .world
        .store()
        .attention_open(
            &Ctx::new(Actor::Owner),
            None,
            tiller_core::state::attention::NewAttention {
                card: "alpha-1".into(),
                kind: tiller_core::domain::AttentionKind::Decision,
                question: "Ship it?".into(),
                ..Default::default()
            },
        )
        .unwrap();

    let (_daemon, _port) = git_only_daemon(&agents.world);
    let deadline = Instant::now() + WAIT;
    while !std::fs::read_to_string(&out).is_ok_and(|env| env.ends_with('\n')) {
        assert!(Instant::now() < deadline, "the notify command never ran");
        std::thread::sleep(Duration::from_millis(50));
    }
    let env = std::fs::read_to_string(&out).unwrap();
    let host: String = agents
        .world
        .store()
        .conn()
        .query_row("SELECT label FROM hosts WHERE kind = 'local'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(env.trim(), format!("{session} {pane} {host}"));
}

/// A board GET of `path` on the daemon at `port`: the response body.
fn get(port: u16, cookie: &str, path: &str) -> Value {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
        .write_all(
            format!(
                "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nCookie: {}={cookie}\r\n\r\n",
                tiller_daemon::auth::COOKIE
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

/// R21 on the real holder: a holder killed under a running worker comes
/// back as a new instance, the daemon resumes the worker with its Claude
/// session in a pane of the new holder, the owner's `[notify] command`
/// hears of it once, and the Machines row shows the restart. A daemon
/// restart after that sends no second notification.
#[test]
fn a_lost_holder_resumes_its_worker_and_notifies_the_owner_once() {
    let agents = Agents::new(300);
    let (mut holder, _client) = start_holder(&agents.world);
    let run = agents.run();
    let (session, pane) = agents.launch(&run);
    agents.until(&session, "start", |events| has(events, "start"));

    let root = agents.world.dir.path();
    let out = root.join("notified");
    let notify = root.join("notify");
    script(
        &notify,
        &format!(
            // The daemon's PATH holds only git: shell builtins copy stdin.
            "while IFS= read -r line || [ -n \"$line\" ]; do printf '%s' \"$line\"; done >> {out}\necho >> {out}\n",
            out = out.display()
        ),
    );
    let config = agents.world.home().join("config.toml");
    let mut table = std::fs::read_to_string(&config).unwrap_or_default();
    table.push_str(&format!(
        "\n[notify]\ncommand = [\"{}\"]\n",
        notify.display()
    ));
    std::fs::write(&config, table).unwrap();
    let count = |sql: &str| -> i64 {
        agents
            .world
            .store()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    };

    // The resumed agent starts with the daemon's PATH and runs `claude`
    // from it, and the claude stub runs `env`: both sit beside the git the
    // daemon's PATH holds.
    let bin = root.join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    let found = std::process::Command::new("sh")
        .args(["-c", "command -v env"])
        .output()
        .unwrap();
    std::os::unix::fs::symlink(
        String::from_utf8(found.stdout).unwrap().trim(),
        bin.join("env"),
    )
    .unwrap();
    std::os::unix::fs::symlink(root.join("claude-stub"), bin.join("claude")).unwrap();
    let (daemon, port) = git_only_daemon(&agents.world);
    let cookie = cookie(&agents.world);
    // The holder source records the pane and its pid first.
    let deadline = Instant::now() + WAIT;
    while count("SELECT count(*) FROM panes WHERE holder_instance IS NOT NULL AND pid IS NOT NULL")
        < 2
    {
        assert!(Instant::now() < deadline, "the holder source never listed");
        std::thread::sleep(Duration::from_millis(100));
    }
    let machines = get(port, &cookie, "/api/v1/machines");
    assert!(
        machines["machines"][0]["holder_restarted_at"].is_null(),
        "{machines}"
    );

    holder.kill().unwrap();
    holder.wait().unwrap();
    drop(holder);
    let (_holder, client) = start_holder(&agents.world);

    let deadline = Instant::now() + Duration::from_secs(120);
    let notes = |out: &PathBuf| -> Vec<Value> {
        std::fs::read_to_string(out)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|note: &Value| {
                note["title"]
                    .as_str()
                    .is_some_and(|t| t.contains("terminal holder"))
            })
            .collect()
    };
    while notes(&out).is_empty() {
        if Instant::now() >= deadline {
            let store = agents.world.store();
            let mut stmt = store
                .conn()
                .prepare("SELECT kind || ' ' || substr(payload, 1, 400) FROM events WHERE seq > 20 ORDER BY seq")
                .unwrap();
            let rows: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            panic!(
                "no R21 notification: {}\n{}",
                std::fs::read_to_string(&out).unwrap_or_default(),
                rows.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let (again, new_pane) = agents.live(&run);
    assert_eq!(again, session, "the resume keeps the session row");
    assert_ne!(new_pane, pane);
    assert!(
        client
            .list()
            .unwrap()
            .panes
            .iter()
            .any(|p| p.pane.as_str() == new_pane),
        "the worker runs in a pane of the new holder"
    );
    agents.until(&session, "the resumed start", |events| {
        events.iter().filter(|e| e["event"] == "start").count() == 2
    });
    let argv = agents.of(&session, "start")[1]["argv"].clone();
    let argv: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = argv.iter().position(|a| *a == "--resume").unwrap();
    assert_eq!(argv[at + 1], format!("claude-{session}"), "{argv:?}");
    // The resumed agent gets the Tiller env of its first launch: its hooks
    // find the daemon's home, and the resume binds the same row.
    let envs = agents.envs(&session);
    assert_eq!(envs.len(), 2, "{envs:#?}");
    let (first, resumed) = (&envs[0], &envs[1]);
    let home = agents.world.home();
    assert_eq!(first["TILLER_HOME"], home.to_str().unwrap(), "{first:#?}");
    for (name, value) in first {
        if (name.starts_with("TILLER_") || name.starts_with("CLAUDE_")) && name != "TILLER_PANE" {
            assert_eq!(
                resumed.get(name),
                Some(value),
                "the resumed pane's {name}: {resumed:#?}"
            );
        }
    }
    assert_eq!(
        resumed["HOME"],
        agents.world.user_home().to_str().unwrap(),
        "{resumed:#?}"
    );
    // A reconcile resume puts `--resume` first, and the stub then binds a new
    // Claude id: the bind lands only when the hook found the daemon's home.
    let deadline = Instant::now() + WAIT;
    while count(&format!(
        "SELECT count(*) FROM events WHERE kind = 'session.bound' AND entity_id = '{session}' \
         AND json_extract(payload, '$.claude_session_id') = 'claude-{session}-resumed'"
    )) < 1
    {
        assert!(Instant::now() < deadline, "the resumed session never bound");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !agents.world.user_home().join(".tiller").exists(),
        "a hook wrote to $HOME/.tiller in place of TILLER_HOME"
    );
    // R21 notifies before it writes holder.lost, so the note can come first.
    let deadline = Instant::now() + WAIT;
    while count("SELECT count(*) FROM events WHERE kind = 'holder.lost'") < 1 {
        assert!(Instant::now() < deadline, "R21 never wrote holder.lost");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        count("SELECT count(*) FROM events WHERE kind = 'holder.lost'"),
        1
    );
    let machines = get(port, &cookie, "/api/v1/machines");
    assert!(
        machines["machines"][0]["holder_restarted_at"].is_string(),
        "{machines}"
    );
    eprintln!("R21 note: {}", notes(&out)[0]);
    eprintln!("machines: {machines}");

    // A daemon restart on the same new holder notifies nothing again.
    drop(daemon);
    let (_daemon, _port) = git_only_daemon(&agents.world);
    std::thread::sleep(Duration::from_secs(8));
    assert_eq!(notes(&out).len(), 1, "{:?}", notes(&out));
    assert_eq!(
        count("SELECT count(*) FROM events WHERE kind = 'holder.lost'"),
        1
    );
}

/// Where a 9.2 kill lands.
#[derive(Clone, Copy, Debug)]
enum Kill {
    /// While the board streams the pane.
    Stream,
    /// While the board's Fit holds the pane at its size.
    Fit,
    /// After delivery typed the wake line and before its Enter.
    BeforeEnter,
}

/// Spec 9.2: SIGKILL the daemon at `kill` while a message goes to a run's
/// `fake-agent`. The agent runs on in its pane, a restarted daemon delivers
/// the message once, nothing is typed twice, and a dead daemon's Fit holds
/// nothing: its lease ends with its connection, and the size falls back to
/// the lease under it, an outside client's 80 by 24.
fn killed_daemon_delivers_once(kill: Kill) {
    let agents = Agents::new(300);
    let (_holder, client) = start_holder(&agents.world);
    let run = agents.run();
    let (session, pane) = agents.launch(&run);
    shows(&client, &pane, "? for shortcuts");
    let cookie = cookie(&agents.world);
    // An outside client, such as an attach, holds a lease under the Fit.
    let base = matches!(kill, Kill::Fit).then(|| {
        let base = client
            .subscribe(&PaneId::from(pane.as_str()), Subscribe::default())
            .unwrap();
        base.resize(80, 24).unwrap();
        sized(&client, &pane, 80, 24, "the outside lease");
        base
    });

    let (mut daemon, port) = git_only_daemon(&agents.world);
    let mut view = match kill {
        Kill::Stream => Some(View::open(port, &cookie, &session)),
        Kill::Fit => Some(View::open_with(
            port,
            &cookie,
            &session,
            r#"{"fit": {"cols": 100, "rows": 30}}"#,
        )),
        Kill::BeforeEnter => None,
    };
    if let Some(view) = view.as_mut() {
        view.until("the repaint", |view| {
            view.lines
                .iter()
                .any(|l| l["type"] == "frame" && l["full"] == true)
        });
    }
    if matches!(kill, Kill::Fit) {
        sized(&client, &pane, 100, 30, "the Fit");
    }
    let sent = agents.world.must(&[
        "send",
        &session,
        "--kind",
        "steer",
        "--text",
        "look at the tests",
    ]);
    let id = sent.data()["id"].as_str().unwrap().to_owned();
    if matches!(kill, Kill::BeforeEnter) {
        // The line is in the composer, and its Enter comes half a second
        // later (the pacing's settle).
        let deadline = Instant::now() + WAIT;
        while !composer_holds(&screen(&client, &pane), "tiller inbox") {
            assert!(Instant::now() < deadline, "delivery never typed the line");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    drop(daemon);
    drop(view);
    if matches!(kill, Kill::BeforeEnter) {
        assert!(
            agents.of(&session, "submit").is_empty(),
            "the Enter came before the kill"
        );
    }
    if matches!(kill, Kill::Fit) {
        // The dead daemon's connection ended, and its lease with it.
        sized(&client, &pane, 80, 24, "after the kill");
    }
    let (_daemon, port) = git_only_daemon(&agents.world);
    if matches!(kill, Kill::Fit) {
        // A new Fit takes its own lease, and gives it back when it ends.
        let mut view = View::open_with(
            port,
            &cookie,
            &session,
            r#"{"fit": {"cols": 90, "rows": 25}}"#,
        );
        view.until("the repaint", |view| {
            view.lines
                .iter()
                .any(|l| l["type"] == "frame" && l["full"] == true)
        });
        sized(&client, &pane, 90, 25, "a new Fit");
        drop(view);
        sized(&client, &pane, 80, 24, "after the new Fit");
    }
    drop(base);
    // A delivery pass that meets a busy screen, a working holder status or an
    // unreadable composer defers by design for 30 s, which outlasts WAIT. The
    // wait covers one such deferral. 30 is BUSY_DEFER_SECS in
    // tiller_delivery::outbox, which this crate does not depend on.
    const BUSY_DEFER: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + WAIT + BUSY_DEFER;
    while !has(&agents.events(&session), "submit") {
        assert!(
            Instant::now() < deadline,
            "{kill:?}: the message never reached fake-agent: {:#?}\ndeferrals: {:#?}",
            agents.events(&session),
            deferrals(&agents.world, &id)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let deadline = Instant::now() + WAIT;
    while agents
        .world
        .store()
        .conn()
        .query_row("SELECT state FROM outbox WHERE id = ?1", [&id], |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        != "delivered"
    {
        assert!(Instant::now() < deadline, "the message never settled");
        std::thread::sleep(Duration::from_millis(50));
    }
    // No second copy comes later.
    std::thread::sleep(Duration::from_secs(4));
    let submits = agents.of(&session, "submit");
    assert_eq!(
        submits.len(),
        1,
        "{kill:?}: {submits:#?}\n{:#?}",
        agents.of(&session, "read")
    );
    let text = submits[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("tiller inbox {id} ")),
        "{kill:?}: {text}"
    );
    assert_eq!(
        text.matches("tiller inbox").count(),
        1,
        "{kill:?}: typed twice: {text}"
    );
    assert!(
        !text.contains('\n'),
        "{kill:?}: {text:?}\n{:#?}",
        agents.of(&session, "read")
    );
    // The same agent all along.
    assert_eq!(agents.of(&session, "start").len(), 1, "{kill:?}");
    assert!(agents.of(&session, "exit").is_empty(), "{kill:?}");
    assert_eq!(agents.live(&run), (session, pane));
}

/// Each `outbox.deferred` of message `id`: when, until when, and why.
fn deferrals(world: &World, id: &str) -> Vec<(String, String)> {
    let store = world.store();
    let mut rows = store
        .conn()
        .prepare("SELECT at, payload FROM events WHERE kind = 'outbox.deferred' AND entity_id = ?1 ORDER BY seq")
        .unwrap();
    rows.query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Whether the composer row, between its rules, holds `text`.
fn composer_holds(screen: &str, text: &str) -> bool {
    screen
        .lines()
        .any(|row| row.starts_with(PROMPT) && row.contains(text))
}

const PROMPT: &str = "\u{276f}\u{a0}";

#[test]
fn a_daemon_killed_during_a_stream_delivers_the_message_once() {
    killed_daemon_delivers_once(Kill::Stream);
}

#[test]
fn a_daemon_killed_during_a_fit_delivers_the_message_once() {
    killed_daemon_delivers_once(Kill::Fit);
}

#[test]
fn a_daemon_killed_between_a_typed_line_and_its_enter_submits_it_once() {
    // It times the wall clock, so `.config/nextest.toml` runs it alone.
    if std::env::var_os("NEXTEST_RUN_ID").is_some() {
        assert_eq!(
            std::env::var("NEXTEST_TEST_GROUP").as_deref(),
            Ok("wall-clock"),
            "the nextest override that runs this test alone no longer matches it"
        );
    }
    killed_daemon_delivers_once(Kill::BeforeEnter);
}

/// Spec 9.2: 20 daemon restarts in a row, each streaming the pane, while
/// the agent writes a counter. The agent never stalls, and the pane's
/// output holds every number once, in order.
#[test]
fn twenty_daemon_restarts_leave_no_gap_in_a_counter() {
    // It times the wall clock, so `.config/nextest.toml` runs it alone.
    if std::env::var_os("NEXTEST_RUN_ID").is_some() {
        assert_eq!(
            std::env::var("NEXTEST_TEST_GROUP").as_deref(),
            Ok("wall-clock"),
            "the nextest override that runs this test alone no longer matches it"
        );
    }
    let agents = Agents::with_args(300, "--counter");
    let (_holder, client) = start_holder(&agents.world);
    let (session, pane) = agents.orchestrator();
    agents.until(&session, "a tick", |events| has(events, "tick"));
    let cookie = cookie(&agents.world);
    for round in 0..20 {
        let (daemon, port) = git_only_daemon(&agents.world);
        let mut view = View::open(port, &cookie, &session);
        let last = agents.of(&session, "tick").len();
        view.until("a later count", |view| {
            view.shows(&format!("count {}", last + 1))
        });
        drop(view);
        drop(daemon);
        assert!(agents.of(&session, "exit").is_empty(), "round {round}");
    }
    let ticks = agents.of(&session, "tick");
    let numbers: Vec<u64> = ticks.iter().map(|t| t["n"].as_u64().unwrap()).collect();
    let want: Vec<u64> = (0..numbers.len() as u64).collect();
    assert_eq!(numbers, want);
    let times: Vec<u64> = ticks.iter().map(|t| t["ms"].as_u64().unwrap()).collect();
    let stall = times.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    // A tick every 50 ms: a write the holder held up would show here.
    assert!(stall < 1_000, "the counter stalled for {stall} ms");
    assert_eq!(agents.of(&session, "start").len(), 1);

    // Every number reached the pane, in order and once.
    let output = client
        .screen(
            &PaneId::from(pane.as_str()),
            Read::Recent(10_000),
            Format::Text,
        )
        .unwrap()
        .text();
    let seen: Vec<u64> = output
        .lines()
        .filter_map(|row| row.trim().strip_prefix("count ")?.parse().ok())
        .collect();
    assert!(seen.len() > 20 * 2, "{} counts", seen.len());
    let first = seen[0];
    let want: Vec<u64> = (first..first + seen.len() as u64).collect();
    assert_eq!(seen, want, "a gap or a repeat in the pane's output");
}

/// H08's `sleep-then-essay` recording, which the replaying `fake-agent`
/// plays.
fn essay() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../panes/tests/fixtures/claude-raw/sleep-then-essay.raw"
    ))
}

/// The output-quiet guard, on H08's `sleep-then-essay` recording. Its
/// essay streams with the spinner off screen, so for 13 frames the composer
/// reads Empty while Claude still prints, and the holder source reads the
/// pane idle. A message sent then is not typed into the stream: delivery
/// waits until the pane has been quiet, then types the line once.
#[test]
fn a_message_waits_out_a_reply_that_streams_with_an_empty_composer() {
    // It paces frames on the wall clock, so `.config/nextest.toml` runs it
    // alone.
    if std::env::var_os("NEXTEST_RUN_ID").is_some() {
        assert_eq!(
            std::env::var("NEXTEST_TEST_GROUP").as_deref(),
            Ok("wall-clock"),
            "the nextest override that runs this test alone no longer matches it"
        );
    }
    // From the essay's first Empty frame (it ends at byte 9808) to the idle
    // composer after the essay (the frame that ends at byte 16181). The
    // holder source reads the screen at first sight, and only on `quiet`
    // after that: a start in the Bash run would read its spinner, and the
    // session's `working` would hold the message before the guard is asked.
    // From here the holder reads idle while the essay still streams, the
    // case the guard is for.
    let agents = Agents::with_args(
        300,
        &format!("--replay {} 9808 16181 120x40", essay().display()),
    );
    let (_holder, client) = start_holder(&agents.world);
    let (session, pane) = agents.orchestrator();
    agents.until(&session, "start", |events| has(events, "start"));
    let (_daemon, port) = git_only_daemon(&agents.world);
    let mut view = View::open_with(
        port,
        &cookie(&agents.world),
        &session,
        r#"{"fit": {"cols": 120, "rows": 40}}"#,
    );
    view.until("the repaint", |view| {
        view.lines
            .iter()
            .any(|l| l["type"] == "frame" && l["full"] == true)
    });

    // The stream reaches the frames that read Empty.
    let deadline = Instant::now() + WAIT;
    loop {
        let shown = client
            .screen(&PaneId::from(pane.as_str()), Read::Visible, Format::Ansi)
            .unwrap()
            .text();
        if classify(&shown) == Composer::Empty {
            break;
        }
        assert!(Instant::now() < deadline, "the replay never read Empty");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        agents.of(&session, "replayed").is_empty(),
        "the replay ended"
    );
    let sent = agents.world.must(&[
        "send",
        &session,
        "--kind",
        "steer",
        "--text",
        "look at the tests",
    ]);
    let id = sent.data()["id"].as_str().unwrap().to_owned();

    // Nothing reaches the pane while it streams, and the line lands once
    // the stand-in sits at its own idle composer.
    let deadline = Instant::now() + 3 * WAIT;
    while !has(&agents.events(&session), "submit") {
        assert!(
            Instant::now() < deadline,
            "the message never landed: {:#?}",
            agents.events(&session)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let replayed = agents.of(&session, "replayed");
    assert_eq!(replayed.len(), 1);
    // The holder's answers to the recording's terminal queries are read
    // too; only the wake line is delivery's.
    let reads = agents.of(&session, "read");
    let typed: Vec<&Value> = reads
        .iter()
        .filter(|read| read["bytes"].as_str().unwrap().contains("tiller inbox"))
        .collect();
    assert!(!typed.is_empty(), "{reads:#?}");
    assert!(
        typed
            .iter()
            .all(|read| read["ms"].as_u64() > replayed[0]["ms"].as_u64()),
        "delivery typed into the stream: {reads:#?}"
    );
    let reasons: Vec<String> = {
        let store = agents.world.store();
        let mut statement = store
            .conn()
            .prepare(
                "SELECT json_extract(payload, '$.reason') FROM events \
                 WHERE kind = 'outbox.deferred' AND entity_id = ?1",
            )
            .unwrap();
        statement
            .query_map([&id], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        reasons.iter().any(|reason| reason == "pane still printing"),
        "the guard never held the message: {reasons:?}"
    );
    std::thread::sleep(Duration::from_secs(2));
    let submits = agents.of(&session, "submit");
    assert_eq!(submits.len(), 1, "{submits:#?}");
    let text = submits[0]["text"].as_str().unwrap();
    assert!(text.starts_with(&format!("tiller inbox {id} ")), "{text}");
    drop(view);
}
