//! Which herdr the commands drive: `TILLER_HERDR` first, then the `[spawn]`
//! table of `config.toml`, and the daemon follows a changed table without a
//! restart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::World;

/// A herdr stand-in that appends its arguments to `calls` and fails.
fn recorder(path: &Path, calls: &Path) {
    std::fs::write(
        path,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n", calls.display()),
    )
    .unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `tiller orchestrator start` goes through the same tools as `tiller spawn`
/// and reaches herdr before any other program.
#[test]
fn spawn_tools_prefer_tiller_herdr_over_the_config() {
    let world = World::initialized();
    let checkout = world.dir.path().join("checkouts/alpha");
    std::fs::create_dir_all(&checkout).unwrap();
    let (from_env, from_config) = (
        world.dir.path().join("env-calls"),
        world.dir.path().join("config-calls"),
    );
    let (env_herdr, config_herdr) = (
        world.dir.path().join("env-herdr"),
        world.dir.path().join("config-herdr"),
    );
    recorder(&env_herdr, &from_env);
    recorder(&config_herdr, &from_config);
    world.must(&[
        "config",
        "set",
        "spawn.herdr",
        &format!("[\"{}\"]", config_herdr.display()),
    ]);
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
        let dir = world.dir.path().join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        world.must(&["config", "set", key, dir.to_str().unwrap()]);
    }
    world.set_env("TILLER_HERDR", env_herdr.to_str().unwrap());

    world.run(&["orchestrator", "start", "alpha"]);

    assert!(from_env.exists(), "TILLER_HERDR was not driven");
    assert!(!from_config.exists(), "spawn.herdr beat TILLER_HERDR");
}
