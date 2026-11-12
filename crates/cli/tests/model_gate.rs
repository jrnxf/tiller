//! The model gate (`terminal-holder.md` 9.1): each Claude Code fixture, fed
//! into the holder's screen model at 119 columns by its row count and read
//! back with `screen(visible, ansi)`, gives the composer and the prompt
//! parser the results H00 froze from herdr's own reads. The assertions use
//! the frozen snapshots themselves, so a changed result fails here and
//! nothing is rewritten.

#![allow(clippy::unwrap_used)]

use tiller_daemon::prompt::{Wait, parse, read};
use tiller_holder::model::Model;
use tiller_panes::Size;
use tiller_panes::composer::classify;
use tiller_panes::message::{Format, Read};

/// The fixtures' width: herdr read them from a 119-column viewport.
const COLS: u16 = 119;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../panes/tests/fixtures/claude/{name}.ansi",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

/// The fixture as the model reads it back.
fn through_the_model(name: &str) -> String {
    let fixture = fixture(name);
    // Rows are split by `\r\n`, with none after the last.
    let rows = u16::try_from(fixture.matches('\n').count() + 1).unwrap();
    let mut model = Model::new(Size { cols: COLS, rows });
    model.feed(fixture.as_bytes()).unwrap();
    // No byte of a fixture is held back for a later read.
    assert!(model.held().is_empty(), "{name}: the model held a tail");
    model.screen(Read::Visible, Format::Ansi).unwrap().text()
}

macro_rules! gate {
    ($($test:ident => $name:literal),* $(,)?) => {$(
        #[test]
        fn $test() {
            let screen = through_the_model($name);
            insta::with_settings!({
                snapshot_path => "../../herdr/tests/snapshots",
                prepend_module_to_snapshot => false,
            }, {
                insta::assert_debug_snapshot!(
                    concat!("composer_snapshots__", $name),
                    classify(&screen)
                );
            });
            insta::with_settings!({
                snapshot_path => "../../daemon/tests/snapshots",
                prepend_module_to_snapshot => false,
            }, {
                insta::assert_debug_snapshot!(
                    concat!("prompt_snapshots__", $name),
                    (
                        parse(&screen, "Bash"),
                        read(&screen, &Wait::Permission("Bash".to_owned())),
                        read(&screen, &Wait::Input),
                    )
                );
            });
        }
    )*};
}

gate! {
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
