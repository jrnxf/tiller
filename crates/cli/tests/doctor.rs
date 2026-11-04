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
