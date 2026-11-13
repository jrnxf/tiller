//! No test child may find the developer's own home: a child that loses
//! `TILLER_HOME` falls back to `$HOME/.tiller`, and a pane resumed there
//! once bound a test worker into the live fleet.

mod common;

use std::path::Path;

use common::World;

/// The value `command` sets for `name`, if it sets one.
fn set(command: &std::process::Command, name: &str) -> Option<std::path::PathBuf> {
    command
        .get_envs()
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| value.map(Into::into))
}

#[test]
fn every_world_child_gets_the_test_home_for_each_home_name() {
    let world = World::new();
    let command = world.bare_command();
    let home = world.user_home();
    assert!(home.starts_with(world.dir.path()), "{}", home.display());
    for (name, dir) in common::USER_HOME_ENV {
        let value = set(&command, name).unwrap_or_else(|| panic!("{name} is not set"));
        assert_eq!(
            value,
            if dir.is_empty() {
                home.clone()
            } else {
                home.join(dir)
            },
            "{name}"
        );
    }
    let real = std::env::var_os("HOME").unwrap_or_default();
    assert_ne!(set(&command, "HOME").unwrap(), Path::new(&real));
    assert_eq!(set(&command, "TILLER_HOME").unwrap(), world.home());
}

#[test]
fn a_child_without_tiller_home_falls_back_into_the_test_home() {
    let world = World::initialized();
    // The fallback `$HOME/.tiller` is this world's fleet: a child that
    // reached any other home would not find project `alpha`.
    std::fs::create_dir_all(world.user_home()).unwrap();
    std::os::unix::fs::symlink(world.home(), world.user_home().join(".tiller")).unwrap();
    let out = world
        .bare_command()
        .env_remove("TILLER_HOME")
        .args(["--json", "project", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("\"alpha\""), "{text}");
}

#[test]
#[should_panic(expected = "would get the real home")]
fn the_guard_refuses_the_real_home() {
    let real = std::env::var_os("HOME").unwrap();
    common::assert_not_the_real_home(Path::new(&real));
}
