//! `tiller doctor` end to end: the real binary against fake tools. Every tool
//! is a small script under the world's `bin`, found through `PATH` and the
//! `TILLER_*` variables, so no test touches a real herdr, forge or Claude.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::World;
use serde_json::Value;
use tiller_core::domain::MergeGate;
use tiller_core::{Actor, Ctx};

fn script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A world whose tools are all fakes that answer. Returns the `bin` dir.
fn world_with_tools(world: &World) -> std::path::PathBuf {
    let bin = world.dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    script(&bin, "git", "echo 'git version 9.9.9'");
    script(&bin, "treehouse", "echo 'v1.2.3'");
    script(&bin, "claude", "echo '2.1.312 (Claude Code)'");
    script(&bin, "gh", "echo 'gh version 2.0.0'");
    script(&bin, "gh-axi", "echo '0.1.0'");
    script(&bin, "quota-axi", "echo '0.2.0'");
    script(
        &bin,
        "no-mistakes",
        "case \"$1\" in daemon) echo 'daemon running (pid 1)';; *) echo 'no-mistakes version v1.0.0';; esac",
    );
    script(
        &bin,
        "herdr",
        "echo '{\"result\":{\"snapshot\":{\"version\":\"0.9.5\",\"workspaces\":[],\"panes\":[]}}}'",
    );
    // The no-mistakes daemon runs the same binary as its CLI, found through
    // its pid file and the process list.
    let nm_home = world.dir.path().join("no-mistakes-home");
    fs::create_dir_all(&nm_home).unwrap();
    fs::write(nm_home.join("daemon.pid"), r#"{"pid":4242}"#).unwrap();
    script(
        &bin,
        "ps",
        &format!(
            "case \"$1\" in -axo) ;; *) echo '{} daemon run --root /home/user/.no-mistakes';; esac",
            bin.join("no-mistakes").display()
        ),
    );
    world.set_env("TILLER_NO_MISTAKES_HOME", nm_home.to_str().unwrap());
    let claude_dir = world.dir.path().join("claude");
    fs::create_dir_all(&claude_dir).unwrap();
    world.set_env("PATH", bin.to_str().unwrap());
    world.set_env("CLAUDE_CONFIG_DIR", claude_dir.to_str().unwrap());
    world.set_env("TILLER_HERDR", bin.join("herdr").to_str().unwrap());
    world.set_env("TILLER_QUOTA_AXI", bin.join("quota-axi").to_str().unwrap());
    world.set_env("TILLER_GH", bin.join("gh").to_str().unwrap());
    bin
}

fn check<'a>(data: &'a Value, name: &str) -> &'a Value {
    data["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check {name}: {data}"))
}

#[test]
fn healthy_host_reports_every_tool_and_exits_zero() {
    let world = World::initialized();
    world_with_tools(&world);
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    let data = out.data();
    assert_eq!(data["ok"], true);
    for (name, version) in [
        ("git", "git version 9.9.9"),
        ("treehouse", "v1.2.3"),
        ("herdr", "0.9.5"),
        ("claude", "2.1.312 (Claude Code)"),
        ("gh", "gh version 2.0.0"),
        ("gh-axi", "0.1.0"),
        ("quota-axi", "0.2.0"),
        ("no-mistakes", "no-mistakes version v1.0.0"),
    ] {
        let c = check(data, name);
        assert_eq!(
            (c["status"].as_str(), c["version"].as_str()),
            (Some("ok"), Some(version)),
            "{name}"
        );
    }
    assert_eq!(check(data, "hook registration")["status"], "ok");
    assert_eq!(check(data, "no-mistakes daemon")["status"], "ok");
}

#[test]
fn missing_required_tool_exits_five_with_the_report() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fs::remove_file(bin.join("treehouse")).unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    let report = &out.json["error"]["current"];
    assert_eq!(report["ok"], false);
    assert_eq!(check(report, "treehouse")["status"], "fail");
    assert_eq!(check(report, "git")["status"], "ok");
}

#[test]
fn unreachable_herdr_fails_the_check() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    script(&bin, "herdr", "exit 1");
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 5, "{}", out.json);
    assert_eq!(
        check(&out.json["error"]["current"], "herdr")["status"],
        "fail"
    );
}

/// Hung tools cost one timeout together, not one each, and a hung process
/// list is cut at the report's deadline: a peer's report reaches its board
/// host inside the 10 s the board waits.
#[test]
fn hung_tools_do_not_hold_the_report_past_its_deadline() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    // The world's PATH is its fake tools only: `sleep` is named in full.
    let sleep = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("sleep"))
        .find(|path| path.is_file())
        .unwrap();
    script(&bin, "hang", &format!("exec '{}' 30", sleep.display()));
    let hang = bin.join("hang");
    world.set_env("TILLER_GH", hang.to_str().unwrap());
    world.set_env("TILLER_QUOTA_AXI", hang.to_str().unwrap());
    world.set_env("TILLER_HERDR", hang.to_str().unwrap());
    world.set_env("TILLER_PS", hang.to_str().unwrap());
    let started = std::time::Instant::now();
    let out = world.run(&["doctor"]);
    let took = started.elapsed();
    // Before, the probes ran one after another: 15 s for the three tools,
    // then 30 s for the process list. Now they end at the 7 s deadline; the
    // margin is for a loaded machine, and the hung tools sleep 30 s, so a
    // report that waited for any of them would still fail.
    assert!(
        took < std::time::Duration::from_secs(15),
        "took {took:?}: {}",
        out.json
    );
    assert_eq!(out.code, 5, "{}", out.json);
    let report = &out.json["error"]["current"];
    for name in ["gh", "quota-axi"] {
        let detail = check(report, name)["detail"].as_str().unwrap().to_owned();
        assert!(
            detail.contains("did not answer within 5s"),
            "{name}: {detail}"
        );
    }
    assert_eq!(check(report, "herdr")["status"], "fail");
    assert_eq!(check(report, "git")["status"], "ok");
    let list = check(report, "firstmate");
    assert_eq!(list["code"], "processes_unreadable", "{list}");
    // The order is the report's, not the order the probes answered in.
    let names: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        &names[..9],
        [
            "git",
            "treehouse",
            "herdr",
            "claude",
            "gh",
            "gh-axi",
            "quota-axi",
            "no-mistakes",
            "no-mistakes daemon"
        ]
    );
}

#[test]
fn a_no_mistakes_daemon_older_than_its_cli_warns_with_the_fix() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    // The CLI was upgraded; the daemon's process still runs the old binary.
    script(&bin, "no-mistakes-old", "echo 'no-mistakes version v0.9.0'");
    script(
        &bin,
        "ps",
        &format!(
            "case \"$1\" in -axo) ;; *) echo '{} daemon run --root /home/user/.no-mistakes';; esac",
            bin.join("no-mistakes-old").display()
        ),
    );
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "a skew warns, it does not fail: {}", out.json);
    let daemon = check(out.data(), "no-mistakes daemon");
    assert_eq!(daemon["status"], "warn");
    assert_eq!(daemon["version"], "no-mistakes version v0.9.0");
    let detail = daemon["detail"].as_str().unwrap();
    assert!(
        detail.contains("v0.9.0, older than its CLI v1.0.0"),
        "{detail}"
    );
    assert!(detail.contains("no-mistakes daemon restart"), "{detail}");
}

#[test]
fn missing_optional_tool_only_warns() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fs::remove_file(bin.join("gh-axi")).unwrap();
    fs::remove_file(bin.join("no-mistakes")).unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(check(out.data(), "gh-axi")["status"], "warn");
    assert_eq!(check(out.data(), "no-mistakes")["status"], "warn");
    assert_eq!(check(out.data(), "no-mistakes daemon")["status"], "warn");
}

#[test]
fn quota_axi_is_optional_when_quota_is_off() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fs::remove_file(bin.join("quota-axi")).unwrap();
    assert_eq!(world.run(&["doctor"]).code, 5);
    world.must(&["config", "set", "quota.enabled", "false"]);
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(check(out.data(), "quota-axi")["status"], "warn");
}

#[test]
fn newer_claude_warns() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    script(&bin, "claude", "echo '2.2.0 (Claude Code)'");
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(check(out.data(), "claude")["status"], "warn");
}

#[test]
fn tiller_hooks_in_user_settings_warn() {
    let world = World::initialized();
    world_with_tools(&world);
    let settings = world.dir.path().join("claude/settings.json");
    fs::write(
        &settings,
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"tiller hook Stop"}]}]}}"#,
    )
    .unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    let c = check(out.data(), "hook registration");
    assert_eq!(c["status"], "warn");
    assert!(c["detail"].as_str().unwrap().contains("user scope"));
}

#[test]
fn merge_gate_is_reported_per_project() {
    let world = World::initialized();
    world_with_tools(&world);
    let out = world.run(&["doctor"]);
    let gates = out.data()["merge_gates"].as_array().unwrap();
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0]["project"], "alpha");
    assert_eq!(gates[0]["merge_gate"], "detect");
    assert_eq!(gates[0]["status"], "warn");

    world
        .store()
        .project_set_merge_gate(
            &Ctx::new(Actor::Daemon),
            "alpha",
            MergeGate::Enforced,
            "ruleset found",
        )
        .unwrap();
    let out = world.run(&["doctor"]);
    let gate = &out.data()["merge_gates"][0];
    assert_eq!(
        (gate["merge_gate"].as_str(), gate["status"].as_str()),
        (Some("enforced"), Some("ok"))
    );
    assert!(gate["last_changed_at"].is_string(), "{gate}");
}

#[test]
fn merge_app_key_is_checked_without_showing_it() {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let world = World::initialized();
    world_with_tools(&world);
    let merge_app = |world: &World| {
        let out = world.run(&["doctor"]);
        assert_eq!(out.code, 0, "an optional check: {}", out.json);
        check(out.data(), "merge app").clone()
    };
    let c = merge_app(&world);
    assert_eq!(c["status"], "warn");
    assert!(
        c["detail"].as_str().unwrap().contains("no merge App key"),
        "{c}"
    );

    let dir = world.home().join("forge-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("app-id"), "4242\n").unwrap();
    let key = dir.join("private-key.pem");
    let pem = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .unwrap()
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .unwrap();
    fs::write(&key, pem.as_bytes()).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
    // A leaked key fails the check, yet doctor still exits 0: the check is
    // optional.
    let c = merge_app(&world);
    assert_eq!(c["status"], "fail");
    assert!(c["detail"].as_str().unwrap().contains("chmod 600"), "{c}");

    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let c = merge_app(&world);
    assert_eq!(c["status"], "ok", "{c}");
    assert!(c["detail"].as_str().unwrap().contains("App 4242"), "{c}");

    fs::write(
        &key,
        "-----BEGIN RSA PRIVATE KEY-----\nc2VjcmV0\n-----END RSA PRIVATE KEY-----\n",
    )
    .unwrap();
    let out = world.run(&["doctor"]);
    let c = check(out.data(), "merge app");
    assert_eq!(c["status"], "warn");
    assert!(
        c["detail"]
            .as_str()
            .unwrap()
            .contains("not a usable RSA key"),
        "{c}"
    );
    assert!(!out.json.to_string().contains("c2VjcmV0"), "the key leaked");
}

#[test]
fn hand_edited_config_is_flagged() {
    let world = World::initialized();
    world_with_tools(&world);
    let drift = |world: &World| check(world.run(&["doctor"]).data(), "config drift").clone();
    // Never set through tiller: nothing to compare.
    fs::write(
        world.home().join("config.toml"),
        "[quota]\nenabled = true\n",
    )
    .unwrap();
    assert_eq!(drift(&world)["status"], "ok");
    world.must(&["config", "set", "quota.enabled", "false"]);
    assert_eq!(drift(&world)["status"], "ok");
    let file = fs::File::options()
        .write(true)
        .open(world.home().join("config.toml"))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    assert_eq!(drift(&world)["status"], "warn");
}

#[test]
fn broken_config_fails_the_config_check() {
    let world = World::initialized();
    world_with_tools(&world);
    fs::write(world.home().join("config.toml"), "not = [valid").unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 5, "{}", out.json);
    assert_eq!(
        check(&out.json["error"]["current"], "config")["status"],
        "fail"
    );
}

#[test]
fn unrelated_user_hook_text_does_not_warn() {
    let world = World::initialized();
    world_with_tools(&world);
    fs::write(
        world.dir.path().join("claude/settings.json"),
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"notify --title 'tiller hook done'"}]}]}}"#,
    )
    .unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!(check(out.data(), "hook registration")["status"], "ok");
}

#[test]
fn doctor_without_state_still_checks_tools() {
    let world = World::new();
    world_with_tools(&world);
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(check(out.data(), "state")["status"], "warn");
    assert!(out.data()["merge_gates"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_writes_nothing() {
    let world = World::initialized();
    world_with_tools(&world);
    let before = world.store().last_seq().unwrap();
    let config = fs::read(world.home().join("config.toml")).ok();
    world.run(&["doctor"]);
    assert_eq!(world.store().last_seq().unwrap(), before);
    assert_eq!(fs::read(world.home().join("config.toml")).ok(), config);
}

/// Makes `TILLER_PS` print the given process list.
fn fake_ps(world: &World, bin: &Path, list: &str) {
    script(bin, "fake-ps", &format!("printf '%s\\n' '{list}'"));
    world.set_env("TILLER_PS", bin.join("fake-ps").to_str().unwrap());
}

const FIRSTMATE_PROCESSES: &str =
    "bash /opt/fm/bin/fm-watch.sh\nbash /opt/fm/bin/fm-supervise-daemon.sh";

fn set_marker(world: &World, owner: &str) {
    fs::write(world.home().join("owner"), format!("{owner}\n")).unwrap();
}

#[test]
fn missing_owner_marker_warns_and_names_the_fix() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fake_ps(&world, &bin, "zsh");
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    let marker = check(out.data(), "owner marker");
    assert_eq!(marker["status"], "warn");
    assert!(
        marker["detail"]
            .as_str()
            .unwrap()
            .contains("tiller owner set <owner>"),
        "{marker}"
    );
    set_marker(&world, "tiller");
    assert_eq!(
        check(world.run(&["doctor"]).data(), "owner marker")["status"],
        "ok"
    );
}

#[test]
fn firstmate_marker_beside_a_running_daemon_is_an_error() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fake_ps(&world, &bin, "zsh");
    set_marker(&world, "firstmate");
    assert_eq!(world.run(&["doctor"]).code, 0, "no daemon, no clash");
    let _daemon = tiller_agents::land::DaemonLock::acquire(&world.home()).unwrap();
    let out = world.run(&["doctor"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    assert_eq!(
        check(&out.json["error"]["current"], "owner marker")["status"],
        "fail"
    );
}

#[test]
fn running_firstmate_warns_when_tiller_would_also_dispatch() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fake_ps(&world, &bin, FIRSTMATE_PROCESSES);
    let out = world.run(&["doctor"]);
    assert_eq!(out.code, 0, "{}", out.json);
    let found = check(out.data(), "firstmate");
    assert_eq!(found["status"], "warn", "{found}");
    assert!(found["detail"].as_str().unwrap().contains("fm-watch.sh"));
    set_marker(&world, "firstmate");
    assert_eq!(
        check(world.run(&["doctor"]).data(), "firstmate")["status"],
        "ok"
    );
}

#[test]
fn no_firstmate_process_is_quiet() {
    let world = World::initialized();
    let bin = world_with_tools(&world);
    fake_ps(&world, &bin, "zsh\nvim fm-watch.sh.bak");
    let out = world.run(&["doctor"]);
    assert_eq!(check(out.data(), "firstmate")["status"], "ok");
}
