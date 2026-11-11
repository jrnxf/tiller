//! Frozen `composer::classify` results on the Claude Code fixtures.
//!
//! The snapshots are named by fixture, so a later model gate can classify
//! the same fixtures through another screen source and assert against them.

#![allow(clippy::unwrap_used)]

use tiller_herdr::composer::classify;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/claude/{name}.ansi",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

macro_rules! frozen {
    ($($test:ident => $name:literal),* $(,)?) => {$(
        #[test]
        fn $test() {
            insta::assert_debug_snapshot!($name, classify(&fixture($name)));
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
