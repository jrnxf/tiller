//! Frozen `prompt` parse results on the Claude Code fixtures.
//!
//! Each snapshot holds the permission parse for a `Bash` hook and the screen
//! read for an answer no hook asked for, with every field. The snapshots are
//! named by fixture, so a later model gate can assert against the same ones.

#![allow(clippy::unwrap_used)]

use tiller_daemon::prompt::{Wait, parse, read};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../panes/tests/fixtures/claude/{name}.ansi",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

macro_rules! frozen {
    ($($test:ident => $name:literal),* $(,)?) => {$(
        #[test]
        fn $test() {
            let screen = fixture($name);
            insta::assert_debug_snapshot!(
                $name,
                (
                    parse(&screen, "Bash"),
                    read(&screen, &Wait::Permission("Bash".to_owned())),
                    read(&screen, &Wait::Input),
                )
            );
        }
    )*};
}

frozen! {
    draft => "draft",
    empty_manual_mode => "empty-manual-mode",
    empty => "empty",
    exited_after_popup => "exited-after-popup",
    exited => "exited",
    permission_dialog => "permission-dialog",
    trust_dialog => "trust-dialog",
    wake_line => "wake-line",
    working => "working",
}
