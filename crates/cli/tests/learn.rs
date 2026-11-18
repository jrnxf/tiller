//! `tiller retro` and `tiller learn` end to end: the real binary and one
//! `state.db` (`docs/design/learning-loop.md`, sections 10 and 11).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use common::{Out, World, snap};
use serde_json::{Value, json};
use tiller_panes::testing::gone;

fn card(world: &World, title: &str) -> (String, String) {
    let out = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", title,
    ]);
    (
        out.data()["id"].as_str().unwrap().to_owned(),
        out.data()["display_id"].as_str().unwrap().to_owned(),
    )
}

/// Closes a card straight in the table, the way a landed one ends up.
fn close(world: &World, id: &str, outcome: &str) {
    world
        .store()
        .conn()
        .execute(
            "UPDATE cards SET state = 'closed', outcome = ?2, closed_at = '2026-11-03T10:00:00.000Z',
                 created_at = '2026-11-03T08:00:00.000Z', closed_by = 'owner' WHERE id = ?1",
            rusqlite::params![id, outcome],
        )
        .unwrap();
}

/// Runs `tiller --json` as the curator of `retro`.
fn run_as_curator(world: &World, retro: &str, args: &[&str]) -> Out {
    let output = common::tiller(world.dir.path())
        .env("TILLER_HOME", world.home())
        .env("TILLER_RETRO_ID", retro)
        .env_remove("TILLER_SESSION_ID")
        .env_remove("CLAUDECODE")
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    Out {
        code: output.status.code().unwrap(),
        json: serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}")),
    }
}

#[test]
fn a_sweep_schedules_retros_and_the_curator_finishes_one() {
    let world = World::initialized();
    let (landed, landed_tag) = card(&world, "the login page");
    let (failed, failed_tag) = card(&world, "the export");
    let (_, open_tag) = card(&world, "still open");
    close(&world, &landed, "landed");
    close(&world, &failed, "failed");

    let swept = world.must(&["retro", "sweep"]);
    let scheduled = swept.data()["scheduled"].as_array().unwrap();
    assert_eq!(scheduled.len(), 2);
    let by_card = |id: &str| -> Value {
        scheduled
            .iter()
            .find(|r| r["card_id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(by_card(&landed)["depth"], "light");
    assert_eq!(by_card(&landed)["outcome"], "nothing");
    assert_eq!(by_card(&failed)["depth"], "full");
    assert_eq!(by_card(&failed)["reason"], "incident");
    assert_eq!(by_card(&failed)["state"], "pending");
    snap(
        &world,
        "retro_sweep_again",
        &world.must(&["retro", "sweep"]),
    );

    let listed = world.must(&["retro", "list", "--pending"]);
    assert_eq!(listed.data()["retros"].as_array().unwrap().len(), 1);
    snap(&world, "retro_list_pending", &listed);

    // The brief: the owner and the curator read it; a fleet session may not.
    let retro = by_card(&failed)["id"].as_str().unwrap().to_owned();
    let shown = world.must(&["retro", "show", &failed_tag]);
    assert_eq!(shown.data()["retro"]["id"], retro);
    assert_eq!(shown.data()["card"]["tag"], format!("[{failed_tag}]"));
    assert!(
        shown.data()["finish"]["nothing"]
            .as_str()
            .unwrap()
            .contains(&retro)
    );
    snap(&world, "retro_show", &shown);
    let orchestrator = world.orchestrator();
    let refused = world.run_as(&orchestrator, &["retro", "show", &retro]);
    assert_eq!((refused.code, refused.error_code()), (6, "forbidden"));
    let (_, worker) = world.builder_run(&open_tag);
    let refused = world.run_as(&worker, &["retro", "show", &retro]);
    assert_eq!(refused.code, 6, "{}", refused.json);
    let refused = world.run_as(&worker, &["retro", "finish", &retro, "--nothing"]);
    assert_eq!(refused.code, 6, "{}", refused.json);
    let no_retro = world.run(&["retro", "show", &open_tag]);
    assert_eq!(no_retro.code, 3, "{}", no_retro.json);

    // A light retro cannot be finished.
    let light = world.run(&["retro", "finish", &landed_tag, "--nothing"]);
    assert_eq!((light.code, light.error_code()), (5, "refused"));
    let no_verdict = world.run(&["retro", "finish", &retro]);
    assert_eq!(no_verdict.code, 2, "{}", no_verdict.json);

    // A note needs its kind, title and body together.
    let half = run_as_curator(
        &world,
        &retro,
        &["retro", "finish", &retro, "--kind", "gotcha"],
    );
    assert_eq!(half.code, 2, "{}", half.json);
    // The curator keeps one note, which joins the pending queue.
    let done = run_as_curator(
        &world,
        &retro,
        &[
            "retro",
            "finish",
            &retro,
            "--kind",
            "gotcha",
            "--title",
            "A rebase of a pushed branch loses the reviewer's comments",
            "--body",
            "Before a push, check that the branch was not already pushed.",
        ],
    );
    assert!(done.ok(), "{}", done.json);
    assert_eq!(done.data()["outcome"], "noted");
    assert_eq!(done.data()["finished_by"], format!("curator:{retro}"));
    let pending = world.must(&["mem", "pending"]);
    let pending = pending.data()["pending"].as_array().unwrap().clone();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0]["source"], "curator");
    assert_eq!(pending[0]["retro_id"], retro.as_str());
    assert_eq!(pending[0]["kind"], "gotcha");
    // Finishing again with another verdict is refused, and a curator of
    // another retro is not this retro's curator.
    let again = run_as_curator(&world, &retro, &["retro", "finish", &retro, "--nothing"]);
    assert_eq!(again.code, 5, "{}", again.json);
    let other = run_as_curator(
        &world,
        "01JC00000000000000000000R9",
        &["retro", "finish", &retro, "--nothing"],
    );
    assert_eq!(
        (other.code, other.error_code()),
        (6, "forbidden"),
        "{}",
        other.json
    );
    let listed = world.run_as(&worker, &["retro", "list"]);
    assert_eq!(listed.code, 6, "{}", listed.json);
    let _ = landed_tag;
}

#[test]
fn retro_run_launches_the_curator_headless() {
    let world = World::initialized();
    let scratch = world.dir.path().join("scratch");
    world.must(&[
        "config",
        "set",
        "spawn.scratch_root",
        scratch.to_str().unwrap(),
    ]);
    // A stand-in claude: it records its arguments and finishes the retro
    // the way the curator does, through tiller with TILLER_RETRO_ID.
    let fake = world.dir.path().join("claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/claude-args\"\n\
             \"{tiller}\" retro finish \"$TILLER_RETRO_ID\" --nothing >/dev/null\n\
             printf '{{\"type\":\"result\",\"session_id\":\"stub\",\"result\":\"nothing worth saving\"}}\\n'\n",
            tiller = env!("CARGO_BIN_EXE_tiller")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.must(&["config", "set", "spawn.claude", fake.to_str().unwrap()]);

    let (id, tag) = card(&world, "a failed card");
    close(&world, &id, "failed");
    world.must(&["retro", "sweep"]);
    let retro = world.must(&["retro", "show", &tag]).data()["retro"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let dry = world.must(&["retro", "run", &retro, "--dry-run"]);
    assert_eq!(dry.data()["dry_run"], true);
    let argv: Vec<String> = dry.data()["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(argv[0], fake.to_str().unwrap());
    assert!(argv.contains(&"--append-system-prompt-file".to_owned()));
    assert!(argv.contains(&"--session-id".to_owned()));
    let prompt = std::fs::read_to_string(dry.data()["prompt"].as_str().unwrap()).unwrap();
    assert!(prompt.starts_with("# Curator"));
    assert!(prompt.contains(&format!("Retro `{retro}` of card [{tag}]")));
    assert!(prompt.contains("\"finish\""));
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(dry.data()["settings"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["env"]["TILLER_ROLE"], "curator");
    // The sandbox hides this home, not the default `~/.tiller`, and the
    // curator's `tiller` finds it again by the environment.
    let home = world.home().to_string_lossy().into_owned();
    for key in ["denyRead", "denyWrite"] {
        let paths = settings["sandbox"]["filesystem"][key].as_array().unwrap();
        assert!(paths.contains(&json!(home)), "{key}: {paths:?}");
    }
    assert!(!settings.to_string().contains(".tiller"), "{settings}");
    assert!(
        dry.data()["env"]
            .as_array()
            .unwrap()
            .contains(&json!(["TILLER_HOME", home])),
        "{}",
        dry.data()["env"]
    );
    // Nothing ran, nothing changed.
    assert_eq!(
        world.must(&["retro", "show", &tag]).data()["retro"]["state"],
        "pending"
    );

    let ran = world.must(&["retro", "run", &retro]);
    assert_eq!(ran.data()["exit"], 0, "{}", ran.json);
    assert_eq!(ran.data()["retro"]["state"], "done");
    assert_eq!(ran.data()["retro"]["outcome"], "nothing");
    assert_eq!(
        ran.data()["retro"]["curator_session"],
        ran.data()["session_id"]
    );
    let args = std::fs::read_to_string(world.dir.path().join("claude-args")).unwrap();
    assert!(args.contains("--permission-mode\ndontAsk"), "{args}");
    assert!(args.contains("--output-format\njson"), "{args}");
    // A retro that is done (and did not end unfinished) is not run again.
    let again = world.run(&["retro", "run", &retro]);
    assert_eq!((again.code, again.error_code()), (5, "refused"));

    // A curator that exits without finishing leaves the retro unfinished.
    std::fs::write(&fake, "#!/bin/sh\nexit 3\n").unwrap();
    let (id2, tag2) = card(&world, "another failed card");
    close(&world, &id2, "failed");
    world.must(&["retro", "sweep"]);
    let retro2 = world.must(&["retro", "show", &tag2]).data()["retro"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ran = world.must(&["retro", "run", &retro2]);
    assert_eq!(ran.data()["exit"], 3);
    assert_eq!(ran.data()["retro"]["outcome"], "unfinished");
    assert!(
        ran.data()["retro"]["learning"]
            .as_str()
            .unwrap()
            .contains("exited 3")
    );
    assert!(
        ran.json["warning"]
            .as_str()
            .unwrap()
            .contains("did not finish")
    );
}

/// A world whose `spawn.claude` is the stand-in script `body`, with one
/// closed, failed card and its pending full retro. Returns the world, the
/// script's path and the retro id.
fn curator_world(body: &str) -> (World, std::path::PathBuf, String) {
    let world = World::initialized();
    let scratch = world.dir.path().join("scratch");
    world.must(&[
        "config",
        "set",
        "spawn.scratch_root",
        scratch.to_str().unwrap(),
    ]);
    let fake = world.dir.path().join("fake-claude");
    std::fs::write(&fake, body).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.must(&["config", "set", "spawn.claude", fake.to_str().unwrap()]);
    let (id, tag) = card(&world, "a failed card");
    close(&world, &id, "failed");
    world.must(&["retro", "sweep"]);
    let retro = world.must(&["retro", "show", &tag]).data()["retro"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (world, fake, retro)
}

/// A stand-in curator that finishes its retro with `--nothing`.
fn finishing_curator() -> String {
    format!(
        "#!/bin/sh\n\"{tiller}\" retro finish \"$TILLER_RETRO_ID\" --nothing >/dev/null\n",
        tiller = env!("CARGO_BIN_EXE_tiller")
    )
}

#[test]
fn an_unfinished_retro_runs_again() {
    let (world, fake, retro) = curator_world("#!/bin/sh\nexit 3\n");
    let ran = world.must(&["retro", "run", &retro]);
    assert_eq!(ran.data()["retro"]["outcome"], "unfinished", "{}", ran.json);
    assert!(
        ran.json["warning"]
            .as_str()
            .unwrap()
            .contains(&format!("tiller retro run {retro}")),
        "{}",
        ran.json
    );

    // Once the cause is fixed, the owner runs it again, as the warning says.
    std::fs::write(&fake, finishing_curator()).unwrap();
    let again = world.must(&["retro", "run", &retro]);
    assert_eq!(again.data()["exit"], 0, "{}", again.json);
    assert_eq!(again.data()["retro"]["state"], "done");
    assert_eq!(again.data()["retro"]["outcome"], "nothing");
    assert!(again.data()["retro"]["learning"].is_null());
    let kinds: Vec<String> = {
        let store = world.store();
        let mut stmt = store
            .conn()
            .prepare("SELECT kind FROM events WHERE entity_id = ?1 ORDER BY seq")
            .unwrap();
        stmt.query_map([&retro], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(
        kinds,
        [
            "retro.scheduled",
            "retro.started",
            "retro.finished",
            "retro.reset",
            "retro.started",
            "retro.finished"
        ]
    );
    // A retro that ended any other way is not run again.
    let done = world.run(&["retro", "run", &retro]);
    assert_eq!(
        (done.code, done.error_code()),
        (5, "refused"),
        "{}",
        done.json
    );
}

/// A dry run claims nothing and writes nothing of the real run: an
/// unfinished retro keeps the failed run's output, where the warning tells
/// the owner to look first.
#[test]
fn a_dry_run_keeps_the_failed_runs_output() {
    let (world, _fake, retro) = curator_world("#!/bin/sh\necho 'the reason it failed'\nexit 3\n");
    let ran = world.must(&["retro", "run", &retro]);
    assert_eq!(ran.data()["retro"]["outcome"], "unfinished", "{}", ran.json);
    let scratch = world.dir.path().join("scratch/retros").join(&retro);
    let output = std::fs::read_to_string(scratch.join("output.json")).unwrap();
    assert!(output.contains("the reason it failed"), "{output}");

    let dry = world.must(&["retro", "run", &retro, "--dry-run"]);
    assert_eq!(dry.data()["dry_run"], true);
    assert_eq!(
        std::fs::read_to_string(scratch.join("output.json")).unwrap(),
        output
    );
    // The dry run's launch files are still there to read.
    let prompt = std::fs::read_to_string(dry.data()["prompt"].as_str().unwrap()).unwrap();
    assert!(prompt.starts_with("# Curator"), "{prompt}");
    assert_ne!(
        std::path::Path::new(dry.data()["prompt"].as_str().unwrap()).parent(),
        Some(scratch.as_path()),
        "{}",
        dry.json
    );
}

/// An unfinished end is stamped when the run ended, not when it was
/// claimed: Memory and Health show that time.
#[test]
fn an_unfinished_run_ends_at_the_time_it_ended() {
    let (world, _fake, retro) = curator_world("#!/bin/sh\nsleep 1\nexit 3\n");
    let ran = world.must(&["retro", "run", &retro]);
    let row = &ran.data()["retro"];
    assert_eq!(row["outcome"], "unfinished", "{}", ran.json);
    let started = row["curator_started_at"].as_str().unwrap().to_owned();
    let finished = row["finished_at"].as_str().unwrap().to_owned();
    let millis = |at: &str| at.parse::<jiff::Timestamp>().unwrap().as_millisecond();
    assert!(
        millis(&finished) - millis(&started) >= 1000,
        "finished {finished}, started {started}"
    );
}

/// A curator that cannot be started says why in the retro itself, with
/// the hint, so the owner need not read a log.
#[test]
fn a_run_that_cannot_start_says_why_in_the_retro() {
    let (world, fake, retro) = curator_world(&finishing_curator());
    std::fs::remove_file(&fake).unwrap();
    let failed = world.run(&["retro", "run", &retro]);
    assert_ne!(failed.code, 0, "{}", failed.json);
    let shown = world.must(&["retro", "show", &retro]);
    let learning = shown.data()["retro"]["learning"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        learning.starts_with("the curator could not be started: cannot start "),
        "{learning}"
    );
    assert!(
        learning.ends_with("; set spawn.claude in config.toml"),
        "{learning}"
    );
}

/// When the run cannot start and the retro cannot be marked unfinished
/// either, the error still names why the run failed, with its hint.
#[test]
fn a_run_that_cannot_start_keeps_its_cause_when_the_end_fails_too() {
    let (world, fake, retro) = curator_world(&finishing_curator());
    std::fs::remove_file(&fake).unwrap();
    world
        .store()
        .conn()
        .execute_batch(
            "CREATE TRIGGER no_unfinished BEFORE UPDATE OF outcome ON retros
             WHEN NEW.outcome = 'unfinished'
             BEGIN SELECT RAISE(ABORT, 'the test refuses the end'); END;",
        )
        .unwrap();
    let failed = world.run(&["retro", "run", &retro]);
    assert_ne!(failed.code, 0, "{}", failed.json);
    let error = &failed.json["error"];
    let message = error["message"].as_str().unwrap();
    assert!(message.starts_with("cannot start "), "{}", failed.json);
    assert!(
        message.contains("the test refuses the end"),
        "{}",
        failed.json
    );
    assert_eq!(
        error["hint"], "set spawn.claude in config.toml",
        "{}",
        failed.json
    );
}

#[test]
fn a_refused_run_leaves_the_running_curators_files_alone() {
    let (world, _fake, retro) = curator_world(&finishing_curator());
    // A curator is running: it claimed the retro a moment ago and works in
    // its scratch directory.
    world
        .store()
        .retro_start(&world.owner_ctx(), &retro, Some("running"), 20)
        .unwrap();
    let scratch = world.dir.path().join("scratch/retros").join(&retro);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("notes.md"), "half way").unwrap();

    for args in [
        vec!["retro", "run", retro.as_str()],
        vec!["retro", "run", retro.as_str(), "--dry-run"],
    ] {
        let second = world.run(&args);
        assert_eq!(
            (second.code, second.error_code()),
            (5, "refused"),
            "{args:?}: {}",
            second.json
        );
        assert!(
            second.json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("still running"),
            "{}",
            second.json
        );
        assert_eq!(
            std::fs::read_to_string(scratch.join("notes.md")).unwrap(),
            "half way",
            "{args:?}"
        );
    }
    let shown = world.must(&["retro", "show", &retro]);
    assert_eq!(shown.data()["retro"]["curator_session"], "running");
}

#[test]
fn a_run_that_cannot_start_leaves_the_retro_ready_to_run_again() {
    let (world, fake, retro) = curator_world(&finishing_curator());
    std::fs::remove_file(&fake).unwrap();
    let failed = world.run(&["retro", "run", &retro]);
    assert_ne!(failed.code, 0, "{}", failed.json);
    let shown = world.must(&["retro", "show", &retro]);
    assert_eq!(
        shown.data()["retro"]["outcome"],
        "unfinished",
        "{}",
        shown.json
    );
    // No curator ran, so nothing holds the retro: the next run starts at once.
    std::fs::write(&fake, finishing_curator()).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let again = world.must(&["retro", "run", &retro]);
    assert_eq!(
        again.data()["retro"]["outcome"],
        "nothing",
        "{}",
        again.json
    );
}

fn wait_for(what: impl Fn() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(10) {
        if what() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn an_interrupted_run_stops_the_curator_and_what_it_started() {
    // A curator that starts a child of its own, as claude starts MCP
    // servers, and then waits.
    let (world, _fake, retro) = curator_world(
        "#!/bin/sh\nsleep 300 &\necho $! > \"$(dirname \"$0\")/grandchild.pid\"\n\
         echo $$ > \"$(dirname \"$0\")/curator.pid\"\nwait\n",
    );
    let tiller = world.spawn(&["retro", "run", &retro]);
    let pid_file = |name: &str| world.dir.path().join(name);
    assert!(
        wait_for(|| pid_file("curator.pid").exists() && pid_file("grandchild.pid").exists()),
        "the stand-in curator never started"
    );
    let read = |name: &str| {
        std::fs::read_to_string(pid_file(name))
            .unwrap()
            .trim()
            .to_owned()
    };
    let (curator, grandchild) = (read("curator.pid"), read("grandchild.pid"));
    assert!(!gone(&curator) && !gone(&grandchild));

    // Ctrl-C reaches tiller; only tiller is signalled here.
    let status = Command::new("kill")
        .args(["-INT", &tiller.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let stopped = wait_for(|| gone(&curator) && gone(&grandchild));
    // Whatever happened, take every process this test started down with it.
    for pid in [&curator, &grandchild] {
        let _ = Command::new("kill").args(["-KILL", pid]).status();
    }
    let output = tiller.wait_with_output().unwrap();
    assert!(
        stopped,
        "the curator or its child outlived the interrupt: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(out["data"]["retro"]["outcome"], "unfinished", "{out}");
    assert!(
        out["data"]["retro"]["learning"]
            .as_str()
            .unwrap()
            .contains("interrupted"),
        "{out}"
    );
}

/// A Ctrl-C that comes while tiller records the curator's end, here
/// waiting on a busy database, does not cut that record: the retro ends
/// `unfinished` and can run again at once, not only after its claim runs
/// out.
#[test]
fn a_ctrl_c_while_the_end_is_recorded_still_records_it() {
    use std::os::unix::process::ExitStatusExt;
    let (world, _fake, retro) = curator_world(
        "#!/bin/sh\necho $$ > \"$(dirname \"$0\")/curator.pid\"\n\
         while [ ! -e \"$(dirname \"$0\")/go\" ]; do sleep 0.05; done\nexit 3\n",
    );
    let tiller = world.spawn(&["retro", "run", &retro]);
    let pid_file = world.dir.path().join("curator.pid");
    assert!(
        wait_for(|| std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n'))),
        "the stand-in curator never started"
    );
    let curator = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_owned();
    // Hold the database, so tiller waits to record the end.
    let lock = rusqlite::Connection::open(world.db()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(world.dir.path().join("go"), "").unwrap();
    assert!(wait_for(|| gone(&curator)), "the curator did not end");
    // Past tiller's 200 ms look at the curator, so it waits on the lock.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let sent = Command::new("kill")
        .args(["-INT", &tiller.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    std::thread::sleep(std::time::Duration::from_millis(300));
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    let output = tiller.wait_with_output().unwrap();
    assert_eq!(output.status.signal(), None, "{:?}", output.status);
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(out["data"]["retro"]["outcome"], "unfinished", "{out}");
}

#[test]
fn a_curator_that_exits_takes_what_it_started_with_it() {
    // A curator that starts a child of its own, which keeps the curator's
    // stdout open, finishes its retro and exits.
    let (world, _fake, retro) = curator_world("");
    let grandchild_file = world.dir.path().join("grandchild.pid");
    std::fs::write(
        world.dir.path().join("fake-claude"),
        format!(
            "{finish}sleep 300 &\necho $! > '{file}'\n",
            finish = finishing_curator(),
            file = grandchild_file.display(),
        ),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let ran = world.run(&["retro", "run", &retro]);
    let took = started.elapsed();
    let grandchild = std::fs::read_to_string(&grandchild_file)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let stopped = !grandchild.is_empty() && wait_for(|| gone(&grandchild));
    if !grandchild.is_empty() {
        let _ = Command::new("kill").args(["-KILL", &grandchild]).status();
    }
    assert_eq!(ran.data()["retro"]["outcome"], "nothing", "{}", ran.json);
    assert_eq!(ran.data()["exit"], 0, "{}", ran.json);
    // The run ends with the curator, not at its one-minute limit.
    assert!(took < std::time::Duration::from_secs(30), "took {took:?}");
    assert!(stopped, "the curator's child {grandchild} outlived it");
}

#[test]
fn config_manifests_diff_by_part() {
    let world = World::initialized();
    let mut store = world.store();
    store
        .config_manifest_record(
            &world.owner_ctx(),
            "aaa",
            &json!({"role": "r1", "settings": "s1", "model": "opus", "claude": "2.1.0"}),
        )
        .unwrap();
    store
        .config_manifest_record(
            &world.owner_ctx(),
            "bbb",
            &json!({"role": "r1", "settings": "s2", "model": "opus", "claude": "2.1.1"}),
        )
        .unwrap();
    let one = world.must(&["learn", "config", "aaa"]);
    assert_eq!(one.data()["parts"]["model"], "opus");
    snap(&world, "learn_config", &one);
    let diff = world.must(&["learn", "config", "aaa", "bbb"]);
    assert_eq!(diff.data()["differs"], json!(["claude", "settings"]));
    assert_eq!(diff.data()["parts"]["settings"]["b"], "s2");
    snap(&world, "learn_config_diff", &diff);
    let missing = world.run(&["learn", "config", "ccc"]);
    assert_eq!((missing.code, missing.error_code()), (3, "not_found"));
}
