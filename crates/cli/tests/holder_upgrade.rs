//! The holder's live upgrade end to end (`terminal-holder.md` 6.2 and
//! 9.3): the real binary re-executes itself under the panes it holds.
//!
//! The handover record is read through a wrapper binary: a script that
//! hands every call to the real binary, and keeps a copy of each record a
//! dry run reads. A dry run after the upgrade gives the record of the new
//! image, so the two are compared field by field.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::holder::fake_agent;
use common::{Out, Running, World};
use tiller_holder::handover::{PaneRecord, Record};
use tiller_panes::{
    Format, HolderClient, Live, Meta, Pane, PaneId, PaneState, Panes, Read, Spawn, Subscribe,
    Subscription, Update, socket_path,
};

/// How long a test waits for anything the holder does.
const WAIT: Duration = Duration::from_secs(15);

/// The real binary.
const TILLER: &str = env!("CARGO_BIN_EXE_tiller");

/// A running holder on `world`'s home.
struct Holder {
    running: Running,
    log: PathBuf,
}

impl Holder {
    fn start(world: &World) -> Self {
        let log = world.dir.path().join("holder.log");
        let running = Running::new(
            world
                .bare_command()
                .arg("holder")
                .env("TILLER_LOG", "debug")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let holder = Self { running, log };
        holder.client(world);
        holder
    }

    /// A new client, once the holder listens.
    fn client(&self, world: &World) -> HolderClient {
        let socket = socket_path(&world.home());
        let deadline = Instant::now() + WAIT;
        loop {
            match HolderClient::connect(&socket, "daemon") {
                Ok(client) => return client,
                Err(err) => {
                    assert!(
                        Instant::now() < deadline,
                        "the holder did not listen: {err}\n{}",
                        self.log()
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    fn pid(&self) -> u32 {
        self.running.id()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn spawn_of(world: &World, key: &str, script: &str) -> Spawn {
    Spawn {
        key: key.to_owned(),
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: world.dir.path().display().to_string(),
        env: BTreeMap::from([("PATH".to_owned(), common::pane_path())]),
        size: None,
        meta: Meta::default(),
    }
}

fn spawn(client: &HolderClient, world: &World, key: &str, script: &str) -> Pane {
    client.spawn(&spawn_of(world, key, script)).unwrap()
}

/// The pane's visible screen in `format`.
fn screen(client: &HolderClient, pane: &Pane, format: Format) -> String {
    client
        .screen(&pane.pane, Read::Visible, format)
        .unwrap()
        .text()
}

/// Waits until the pane's text holds `text`, and gives it.
fn shows(holder: &Holder, client: &HolderClient, pane: &Pane, read: Read, text: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let shown = client
            .screen(&pane.pane, read, Format::Text)
            .unwrap()
            .text();
        if shown.contains(text) {
            return shown;
        }
        assert!(
            Instant::now() < deadline,
            "pane {} never showed {text:?}: {shown:?}\n{}",
            pane.key,
            holder.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A wrapper binary in the test's directory: every call goes to the real
/// binary, each record a dry run reads is kept as `record-<n>`, and the
/// record the new image resumes from as `record-exec`. A
/// pid in `kill-pid` gets `SIGTERM` at each dry run, so a child exits
/// while the upgrade runs.
fn wrapper(world: &World) -> PathBuf {
    let dir = world.dir.path();
    script(
        &dir.join("tiller-wrapper"),
        &format!(
            r#"dir='{dir}'
if [ "$2" = "--dry-run-handover" ]; then
  n=$(cat "$dir/count" 2>/dev/null || echo 0)
  n=$((n + 1))
  echo "$n" > "$dir/count"
  cat > "$dir/record-$n"
  if [ -e "$dir/kill-pid" ]; then kill -TERM "$(cat "$dir/kill-pid")"; fi
  exec '{TILLER}' "$@" < "$dir/record-$n"
fi
if [ "$2" = "--resume" ]; then cat "/dev/fd/$3" > "$dir/record-exec"; fi
exec '{TILLER}' "$@"
"#,
            dir = dir.display()
        ),
    )
}

/// An executable `/bin/sh` script at `path`.
fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_owned()
}

/// The record the wrapper kept as `record-<name>`.
fn record(world: &World, name: &str) -> Record {
    let bytes = std::fs::read(world.dir.path().join(format!("record-{name}"))).unwrap();
    Record::decode(&bytes).unwrap()
}

fn upgrade(world: &World, binary: &Path, dry_run: bool) -> Out {
    let binary = binary.to_str().unwrap();
    let mut args = vec!["holder", "upgrade", "--binary", binary];
    if dry_run {
        args.push("--dry-run");
    }
    world.run(&args)
}

/// The record's pane `pane`.
fn pane_of<'a>(record: &'a Record, pane: &Pane) -> &'a PaneRecord {
    record
        .panes
        .iter()
        .find(|record| record.pane.pane == pane.pane)
        .unwrap_or_else(|| panic!("pane {} is not in the record", pane.key))
}

/// The pane's exit, once the holder has it.
fn exit_code(holder: &Holder, client: &HolderClient, pane: &PaneId) -> Option<i32> {
    let deadline = Instant::now() + WAIT;
    loop {
        let record = client.get(pane).unwrap().unwrap();
        if record.state == PaneState::Exited {
            return record.exit.unwrap().code;
        }
        assert!(
            Instant::now() < deadline,
            "pane {pane} never exited\n{}",
            holder.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the holder says the subscription's pane is `cols` by
/// `rows`.
fn resized(holder: &Holder, subscription: &mut Subscription, cols: u16, rows: u16) {
    loop {
        match subscription.recv().unwrap() {
            Some(Update::Resized { cols: c, rows: r }) if (c, r) == (cols, rows) => return,
            Some(_) => {}
            None => panic!("the subscription ended\n{}", holder.log()),
        }
    }
}

/// The bytes the input pane is sent while its app reads none.
fn queued_input() -> Vec<u8> {
    (0..300_000u32).map(|i| (i % 251) as u8).collect()
}

/// What `cksum` prints for `bytes`.
fn cksum(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut child = std::process::Command::new("cksum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn an_upgrade_keeps_the_pids_the_instance_and_every_pane_field_by_field() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let wrapper = wrapper(&world);

    let alt = client
        .spawn(&Spawn {
            meta: Meta::from([
                ("project".to_owned(), "alpha".to_owned()),
                ("label".to_owned(), "the alternate screen".to_owned()),
            ]),
            ..spawn_of(
                &world,
                "alt",
                r"stty -echo; printf 'primary-text\n'; printf '\033[?1049h\033[2J\033[Halt-text'; read -r _; printf '\033[?1049l'; sleep 3600",
            )
        })
        .unwrap();
    let kitty = spawn(
        &client,
        &world,
        "kitty",
        r"printf '\033[>5u\033[>1ukitty-ready'; sleep 3600",
    );
    let sync = spawn(
        &client,
        &world,
        "sync",
        // Each start inside the open frame moves its deadline on, so the
        // frame stays open until the test ends it.
        r"printf 'sync-before\033[?2026hinside-the-frame'; while [ ! -e end-sync ]; do printf '\033[?2026h'; sleep 0.01; done; printf '\033[?2026l'; sleep 3600",
    );
    let regions = spawn(
        &client,
        &world,
        "regions",
        r"printf '\033[1;35;42mpen\0337\033[3;8r\033)0\033[20G\033H\033[5;1Hregions-ready\016'; sleep 3600",
    );
    let csi = spawn(
        &client,
        &world,
        "csi",
        r"stty -echo; printf 'csi-a\033[3'; read -r _; printf '1mX\n'; sleep 3600",
    );
    let utf8 = spawn(
        &client,
        &world,
        "utf8",
        r"stty -echo; printf 'utf8-b\342\202'; read -r _; printf '\254\n'; sleep 3600",
    );
    let input = spawn(
        &client,
        &world,
        "input",
        r"stty raw -echo; echo input-ready; while [ ! -e go ]; do sleep 0.05; done; head -c 300000 | cksum; sleep 3600",
    );
    for (pane, marker) in [
        (&alt, "alt-text"),
        (&kitty, "kitty-ready"),
        (&sync, "sync-before"),
        (&regions, "regions-ready"),
        (&csi, "csi-a"),
        (&utf8, "utf8-b"),
        (&input, "input-ready"),
    ] {
        shows(&holder, &client, pane, Read::Visible, marker);
    }
    // More than the PTY takes while its app reads nothing: the rest waits
    // in the pane's input queue.
    let sent = queued_input();
    for chunk in sent.chunks(16 * 1024) {
        client.raw_input(input.num, chunk).unwrap();
    }
    // The holder reads a connection's frames in order, so once the ping
    // is answered every frame is in the queue.
    client.ping().unwrap();
    let before = client.list().unwrap();

    let out = upgrade(&world, &wrapper, false);
    assert_eq!(
        out.data()["upgrade"],
        "started",
        "{}\n{}",
        out.json,
        holder.log()
    );
    let client = holder.client(&world);
    let after = client.list().unwrap();

    // The same process and instance, and the same panes with their pids
    // and meta.
    assert!(
        holder.log().contains("the holder re-executed"),
        "{}",
        holder.log()
    );
    assert_eq!(client.welcome().instance, before.instance);
    assert_eq!(after.instance, before.instance);
    let pid = std::fs::read_to_string(world.home().join("run/holder.lock")).unwrap();
    assert_eq!(pid.trim(), holder.pid().to_string());
    // The sync pane writes all the time, so only its output time moves.
    let strip = |panes: &[Pane]| -> Vec<Pane> {
        panes
            .iter()
            .map(|pane| Pane {
                subscribers: 0,
                attach_clients: 0,
                last_output_at: if pane.pane == sync.pane {
                    None
                } else {
                    pane.last_output_at
                },
                ..pane.clone()
            })
            .collect()
    };
    assert_eq!(strip(&after.panes), strip(&before.panes));

    // Every field of every pane across the exec: the record a dry run read
    // before, the one the new image resumed from, and the one a dry run
    // read after. That is the models, the held tails, the kitty stack and
    // the queued input.
    let out = upgrade(&world, &wrapper, true);
    assert_eq!(out.data()["upgrade"], "ready", "{}", out.json);
    let (dry, exec, new) = (
        record(&world, "1"),
        record(&world, "exec"),
        record(&world, "2"),
    );
    let header = |record: &Record| {
        (
            record.instance.clone(),
            record.boot_id.clone(),
            record.started_at,
            record.next_num,
        )
    };
    assert_eq!(header(&exec), header(&dry));
    assert_eq!(header(&new), header(&exec));
    assert!(exec.requester.is_some() && new.requester.is_none());
    for pane in [&alt, &kitty, &regions, &csi, &utf8] {
        assert_eq!(
            pane_of(&exec, pane),
            pane_of(&dry, pane),
            "pane {}",
            pane.key
        );
        assert_eq!(
            pane_of(&new, pane),
            pane_of(&exec, pane),
            "pane {}",
            pane.key
        );
    }
    // The PTY can take more of the queue at any time, so each record holds
    // the rest of what the one before it held.
    let queued = |record: &Record| pane_of(record, &input).input.chunks.concat();
    let (before_exec, at_exec, after_exec) = (queued(&dry), queued(&exec), queued(&new));
    assert!(!after_exec.is_empty(), "no input was queued");
    assert!(
        before_exec.ends_with(&at_exec),
        "{} then {} bytes",
        before_exec.len(),
        at_exec.len()
    );
    assert!(
        at_exec.ends_with(&after_exec),
        "{} then {} bytes",
        at_exec.len(),
        after_exec.len()
    );
    assert_eq!(
        PaneRecord {
            input: pane_of(&exec, &input).input.clone(),
            ..pane_of(&new, &input).clone()
        },
        *pane_of(&exec, &input)
    );
    // The open frame grows while the test runs, and each record holds all
    // the one before it did.
    let frame = |record: &Record| {
        pane_of(record, &sync)
            .model
            .sync
            .clone()
            .unwrap_or_default()
    };
    let (open, at_exec, still_open) = (frame(&dry), frame(&exec), frame(&new));
    assert!(
        String::from_utf8_lossy(&open).contains("inside-the-frame"),
        "{} bytes: {:?}",
        open.len(),
        String::from_utf8_lossy(&open[..open.len().min(80)])
    );
    assert!(
        at_exec.starts_with(&open),
        "{} then {} bytes",
        open.len(),
        at_exec.len()
    );
    assert!(
        still_open.starts_with(&at_exec),
        "{} then {} bytes",
        at_exec.len(),
        still_open.len()
    );
    // The compared fields hold what each pane set, so equal is not empty.
    let term = |pane: &Pane| pane_of(&exec, pane).model.term.clone();
    assert!(format!("{:?}", term(&alt).mode).contains("ALT_SCREEN"));
    assert!(
        term(&kitty).keyboard.len() >= 2,
        "{:?}",
        term(&kitty).keyboard
    );
    let (plain, set) = (term(&kitty), term(&regions));
    assert_ne!(set.scroll_region, plain.scroll_region);
    assert_ne!(set.tabs, plain.tabs);
    assert_ne!(set.active_charset, plain.active_charset);
    assert_ne!(set.primary.saved, plain.primary.saved);
    for pane in [&csi, &utf8] {
        assert_ne!(
            pane_of(&exec, pane).model.scanner.state,
            0,
            "pane {}",
            pane.key
        );
    }

    // The open frame is not drawn until it ends, then it is drawn whole.
    assert!(!screen(&client, &sync, Format::Text).contains("inside-the-frame"));
    std::fs::write(world.dir.path().join("end-sync"), "").unwrap();
    shows(
        &holder,
        &client,
        &sync,
        Read::Visible,
        "sync-beforeinside-the-frame",
    );

    // An app that leaves the alternate screen shows its old primary one.
    client.raw_input(alt.num, b"\n").unwrap();
    shows(&holder, &client, &alt, Read::Visible, "primary-text");
    assert!(!screen(&client, &alt, Format::Text).contains("alt-text"));

    // A read that ended inside a CSI sequence or a UTF-8 character goes on
    // as if there had been no upgrade.
    let csi_whole = spawn(
        &client,
        &world,
        "csi-whole",
        r"stty -echo; printf 'csi-a\033[31mX\n'; sleep 3600",
    );
    let utf8_whole = spawn(
        &client,
        &world,
        "utf8-whole",
        r"stty -echo; printf 'utf8-b\342\202\254\n'; sleep 3600",
    );
    client.raw_input(csi.num, b"\n").unwrap();
    client.raw_input(utf8.num, b"\n").unwrap();
    shows(&holder, &client, &csi, Read::Visible, "csi-aX");
    shows(&holder, &client, &utf8, Read::Visible, "utf8-b\u{20ac}");
    shows(&holder, &client, &csi_whole, Read::Visible, "csi-aX");
    shows(
        &holder,
        &client,
        &utf8_whole,
        Read::Visible,
        "utf8-b\u{20ac}",
    );
    for (pane, whole) in [(&csi, &csi_whole), (&utf8, &utf8_whole)] {
        let screen_of = |pane: &Pane| {
            client
                .screen(&pane.pane, Read::Visible, Format::Ansi)
                .unwrap()
        };
        let (read, expected) = (screen_of(pane), screen_of(whole));
        assert_eq!(read.rows_text, expected.rows_text, "pane {}", pane.key);
        assert_eq!(read.cursor, expected.cursor, "pane {}", pane.key);
    }

    // The queued input reaches the app whole and in order.
    std::fs::write(world.dir.path().join("go"), "").unwrap();
    let expected = cksum(&sent);
    shows(&holder, &client, &input, Read::Visible, &expected);

    // The leases ended with their connections: a new client sizes a pane.
    let mut subscription = client.subscribe(&kitty.pane, Subscribe::default()).unwrap();
    let handle = subscription.handle();
    assert!(matches!(
        subscription.recv().unwrap(),
        Some(Update::Repaint { .. })
    ));
    handle.resize(100, 30).unwrap();
    resized(&holder, &mut subscription, 100, 30);
    let resized = client.get(&kitty.pane).unwrap().unwrap();
    assert_eq!((resized.cols, resized.rows), (100, 30));
    handle.end();
}

#[test]
fn a_counter_runs_through_the_upgrade_and_a_child_that_exits_in_it_keeps_its_code() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let wrapper = wrapper(&world);

    let counter = spawn(
        &client,
        &world,
        "counter",
        r#"i=1; while [ $i -le 300 ]; do echo "n$i"; i=$((i + 1)); sleep 0.01; done; echo counter-done; sleep 3600"#,
    );
    let exits = spawn(
        &client,
        &world,
        "exits",
        r"trap 'exit 7' TERM; echo exits-ready; while :; do sleep 0.05; done",
    );
    shows(&holder, &client, &exits, Read::Visible, "exits-ready");
    std::fs::write(world.dir.path().join("kill-pid"), exits.pid.to_string()).unwrap();
    shows(&holder, &client, &counter, Read::Visible, "n20");

    let out = upgrade(&world, &wrapper, false);
    assert_eq!(
        out.data()["upgrade"],
        "started",
        "{}\n{}",
        out.json,
        holder.log()
    );
    let client = holder.client(&world);
    assert!(
        holder.log().contains("the holder re-executed"),
        "{}",
        holder.log()
    );

    // The child that the dry run's `SIGTERM` ended exits 7, and the new
    // image reports it.
    assert_eq!(exit_code(&holder, &client, &exits.pane), Some(7));

    // Every number once, in order.
    let text = shows(
        &holder,
        &client,
        &counter,
        Read::Recent(1_000),
        "counter-done",
    );
    let numbers: Vec<u32> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix('n')?.parse().ok())
        .collect();
    assert_eq!(numbers, (1..=300).collect::<Vec<_>>());
}

#[test]
fn a_close_under_way_at_the_upgrade_still_kills_a_child_that_outlives_its_grace() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let wrapper = wrapper(&world);

    // A child that lets go of its terminal, so the pane's output ends, and
    // outlives the close's `SIGHUP`.
    let lingers = spawn(
        &client,
        &world,
        "lingers",
        r"trap 'touch hupped' HUP; echo lingers-ready; exec </dev/null >/dev/null 2>&1; while :; do sleep 0.05; done",
    );
    shows(&holder, &client, &lingers, Read::Visible, "lingers-ready");
    let closer = {
        let client = holder.client(&world);
        let pane = lingers.pane.clone();
        std::thread::spawn(move || {
            // The upgrade ends the connection before the reply.
            let _ = client.close(&pane, Duration::from_secs(2));
        })
    };
    let deadline = Instant::now() + WAIT;
    while !world.dir.path().join("hupped").exists() {
        assert!(Instant::now() < deadline, "{}", holder.log());
        std::thread::sleep(Duration::from_millis(20));
    }
    // Dry runs until the record has the close under way and the output
    // ended.
    for n in 1.. {
        let out = upgrade(&world, &wrapper, true);
        assert_eq!(out.data()["upgrade"], "ready", "{}", out.json);
        let record = record(&world, &n.to_string());
        let pane = pane_of(&record, &lingers);
        if pane.output_ended && pane.close_in.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "{pane:?}\n{}", holder.log());
        std::thread::sleep(Duration::from_millis(20));
    }

    let out = upgrade(&world, &wrapper, false);
    assert_eq!(
        out.data()["upgrade"],
        "started",
        "{}\n{}",
        out.json,
        holder.log()
    );
    closer.join().unwrap();
    let client = holder.client(&world);
    let deadline = Instant::now() + WAIT;
    let exit = loop {
        let record = client.get(&lingers.pane).unwrap().unwrap();
        if let Some(exit) = record.exit {
            break exit;
        }
        assert!(Instant::now() < deadline, "{}", holder.log());
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(exit.signal, Some(9), "{}", holder.log());
}

#[test]
fn a_dry_run_changes_nothing() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let wrapper = wrapper(&world);

    let shell = spawn(
        &client,
        &world,
        "shell",
        "stty -echo; echo shell-ready; exec /bin/sh",
    );
    shows(&holder, &client, &shell, Read::Visible, "shell-ready");
    let mut subscription = client.subscribe(&shell.pane, Subscribe::default()).unwrap();
    let handle = subscription.handle();
    assert!(matches!(
        subscription.recv().unwrap(),
        Some(Update::Repaint { .. })
    ));
    handle.resize(90, 20).unwrap();
    resized(&holder, &mut subscription, 90, 20);
    let before = client.list().unwrap();

    for _ in 0..2 {
        let out = upgrade(&world, &wrapper, true);
        assert_eq!(
            out.data()["upgrade"],
            "ready",
            "{}\n{}",
            out.json,
            holder.log()
        );
        assert_eq!(out.data()["dry_run"], true);
    }
    assert!(!holder.log().contains("re-executed"), "{}", holder.log());

    // The same connection, its subscription and its lease all go on.
    assert_eq!(client.list().unwrap(), before);
    client.ping().unwrap();
    client
        .raw_input(shell.num, b"echo still-$((6*7))\n")
        .unwrap();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("still-42") {
        match subscription.recv().unwrap() {
            Some(Update::Output(bytes)) => seen.extend(bytes),
            Some(_) => {}
            None => panic!("the subscription ended\n{}", holder.log()),
        }
    }
    handle.resize(91, 21).unwrap();
    // And the two records it read are the same.
    assert_eq!(record(&world, "1"), record(&world, "2"));
    handle.end();
}

/// After a refused upgrade: the same holder, and its pane still runs and
/// answers.
fn still_serves(world: &World, holder: &Holder, shell: &Pane, round: u32) {
    let client = holder.client(world);
    let pid = std::fs::read_to_string(world.home().join("run/holder.lock")).unwrap();
    assert_eq!(pid.trim(), holder.pid().to_string());
    assert!(!holder.log().contains("re-executed"), "{}", holder.log());
    let pane = client.get(&shell.pane).unwrap().unwrap();
    assert_eq!((pane.state, pane.pid), (PaneState::Running, shell.pid));
    client
        .raw_input(
            shell.num,
            format!("echo round-$(({round}*11))\n").as_bytes(),
        )
        .unwrap();
    shows(
        holder,
        &client,
        shell,
        Read::Visible,
        &format!("round-{}", round * 11),
    );
}

#[test]
fn every_refused_upgrade_leaves_the_old_holder_serving_its_panes() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let dir = world.dir.path();
    let shell = spawn(
        &client,
        &world,
        "shell",
        "stty -echo; echo shell-ready; exec /bin/sh",
    );
    shows(&holder, &client, &shell, Read::Visible, "shell-ready");

    let not_executable = dir.join("not-executable");
    std::fs::write(&not_executable, "#!/bin/sh\n").unwrap();
    let fails = script(&dir.join("fails"), "echo 'it broke' >&2; exit 1\n");
    let skewed = script(
        &dir.join("skewed"),
        r#"echo '{"handover":{"oldest":99,"version":99},"proto":[1,1],"holder":"9.9.9"}'
"#,
    );
    let misreads = script(
        &dir.join("misreads"),
        &format!(
            r#"if [ "$2" = "--dry-run-handover" ]; then cat > /dev/null; echo '{{"checksum":"0000000000000000"}}'; exit 0; fi
exec '{TILLER}' "$@"
"#
        ),
    );
    // Its dry run passes, and it is gone when the holder executes it.
    let vanishes = script(
        &dir.join("vanishes"),
        &format!(
            r#"if [ "$2" = "--dry-run-handover" ]; then rm -f "$0"; fi
exec '{TILLER}' "$@"
"#
        ),
    );
    let cases: [(&Path, &str); 5] = [
        (&not_executable, "preflight: "),
        (&fails, "preflight: "),
        (&skewed, "version: "),
        (&misreads, "dry_run: "),
        (&vanishes, "exec: "),
    ];
    let mut round = 1;
    for (binary, reason) in cases {
        let out = upgrade(&world, binary, false);
        assert_eq!(out.code, 5, "{}: {}", binary.display(), out.json);
        assert_eq!(out.error_code(), "upgrade_refused", "{}", out.json);
        let message = out.json["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("keeps running as it is: {reason}")),
            "{}: {message}",
            binary.display()
        );
        still_serves(&world, &holder, &shell, round);
        round += 1;
    }

    // A pane whose state no record holds: an OSC over the held tail's
    // limit, still open.
    let open_osc = spawn(
        &client,
        &world,
        "open-osc",
        r"printf 'osc-ready\033]0;'; head -c 70000 /dev/zero | tr '\000' a; sleep 3600",
    );
    shows(&holder, &client, &open_osc, Read::Visible, "osc-ready");
    // The model has the whole OSC once a dry run cannot record it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let out = upgrade(&world, Path::new(TILLER), true);
        if out.json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("keeps running as it is: record: "))
        {
            break;
        }
        assert!(Instant::now() < deadline, "{}", out.json);
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = upgrade(&world, Path::new(TILLER), false);
    assert_eq!(out.error_code(), "upgrade_refused", "{}", out.json);
    let message = out.json["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("keeps running as it is: record: "),
        "{message}"
    );
    still_serves(&world, &holder, &shell, round);
    let alive = client.get(&open_osc.pane).unwrap().unwrap();
    assert_eq!(alive.state, PaneState::Running);

    // Once that pane is gone, the same holder upgrades: the refusals left
    // nothing behind.
    client
        .close(&open_osc.pane, Duration::from_millis(200))
        .unwrap();
    client.forget(&open_osc.pane).unwrap();
    let out = upgrade(&world, Path::new(TILLER), false);
    assert_eq!(
        out.data()["upgrade"],
        "started",
        "{}\n{}",
        out.json,
        holder.log()
    );
    let client = holder.client(&world);
    assert!(
        holder.log().contains("the holder re-executed"),
        "{}",
        holder.log()
    );
    let pane = client.get(&shell.pane).unwrap().unwrap();
    assert_eq!((pane.state, pane.pid), (PaneState::Running, shell.pid));
}

/// A `fake-agent` pane, which logs to `log`.
fn agent(client: &HolderClient, world: &World, key: &str, args: &[&str], log: &Path) -> Pane {
    let mut argv = vec![fake_agent().display().to_string()];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    client
        .spawn(&Spawn {
            key: key.to_owned(),
            argv,
            cwd: world.dir.path().display().to_string(),
            env: BTreeMap::from([
                ("PATH".to_owned(), common::pane_path()),
                ("FAKE_AGENT_LOG".to_owned(), log.display().to_string()),
            ]),
            size: None,
            meta: Meta::default(),
        })
        .unwrap()
}

/// The events of `kind` in a `fake-agent` log.
fn agent_events(log: &Path, kind: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"] == kind)
        .collect()
}

// Section 9.3 with agents: a `fake-agent` holding a half-typed draft and a
// `fake-agent --counter` run through the upgrade. The draft is still in the
// composer and its raw mode still holds, so one Enter submits it once; the
// counter has no gap; neither agent starts again or exits.
#[test]
fn fake_agents_run_through_the_upgrade_with_their_draft_and_counter_whole() {
    let world = World::new();
    let holder = Holder::start(&world);
    let client = holder.client(&world);
    let wrapper = wrapper(&world);
    let drafts = world.dir.path().join("drafts.jsonl");
    let counts = world.dir.path().join("counts.jsonl");

    let drafting = agent(&client, &world, "drafting", &[], &drafts);
    let counting = agent(&client, &world, "counting", &["--counter"], &counts);
    shows(
        &holder,
        &client,
        &drafting,
        Read::Visible,
        "? for shortcuts",
    );
    client.input(&drafting.pane, "half a thought").unwrap();
    shows(&holder, &client, &drafting, Read::Visible, "half a thought");
    shows(&holder, &client, &counting, Read::Visible, "count 20");

    let out = upgrade(&world, &wrapper, false);
    assert_eq!(
        out.data()["upgrade"],
        "started",
        "{}\n{}",
        out.json,
        holder.log()
    );
    let client = holder.client(&world);
    assert!(
        holder.log().contains("the holder re-executed"),
        "{}",
        holder.log()
    );

    // The draft is whole in the new image's screen, and the agent still
    // reads keys raw: a carriage return submits.
    shows(&holder, &client, &drafting, Read::Visible, "half a thought");
    client.input(&drafting.pane, " finished").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    client
        .keys(&drafting.pane, &[tiller_panes::Key::ENTER])
        .unwrap();
    shows(
        &holder,
        &client,
        &drafting,
        Read::Visible,
        "> half a thought finished",
    );
    let submits = agent_events(&drafts, "submit");
    assert_eq!(submits.len(), 1, "{submits:#?}");
    assert_eq!(submits[0]["text"], "half a thought finished");

    // The counter's ticks before, during and after the upgrade.
    let last = agent_events(&counts, "tick").len();
    shows(
        &holder,
        &client,
        &counting,
        Read::Visible,
        &format!("count {}", last + 20),
    );
    let ticks: Vec<u64> = agent_events(&counts, "tick")
        .iter()
        .map(|tick| tick["n"].as_u64().unwrap())
        .collect();
    assert_eq!(ticks, (0..ticks.len() as u64).collect::<Vec<_>>());
    let text = client
        .screen(&counting.pane, Read::Recent(10_000), Format::Text)
        .unwrap()
        .text();
    let shown: Vec<u64> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("count ")?.parse().ok())
        .collect();
    assert!(shown.len() > 40, "{text}");
    assert_eq!(
        shown,
        (shown[0]..shown[0] + shown.len() as u64).collect::<Vec<_>>(),
        "the counter's output has a gap"
    );

    for log in [&drafts, &counts] {
        assert_eq!(agent_events(log, "start").len(), 1, "{}", log.display());
        assert!(agent_events(log, "exit").is_empty(), "{}", log.display());
    }
    for pane in [&drafting, &counting] {
        let now = client.get(&pane.pane).unwrap().unwrap();
        assert_eq!(now.state, PaneState::Running);
        assert_eq!(now.pid, pane.pid);
    }
}
