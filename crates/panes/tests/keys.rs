//! The key names the board sends parse as `Key`s.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiller_panes::Key;

/// Every name the board's `typedKeys` can send but a printable character
/// past ASCII. `terminal-keys.test.ts` keeps it equal to the board's list.
const BOARD: &str = include_str!("fixtures/board-key-names.json");

#[test]
fn every_name_the_board_sends_parses() {
    let mut names: Vec<String> = serde_json::from_str(BOARD).unwrap();
    assert!(names.len() > 300, "only {} names", names.len());
    // Past ASCII a character goes as itself: a C1 control, a no-break
    // space, Latin, CJK and one outside the BMP. terminal-keys.test.ts
    // checks the board sends these samples as they are.
    for char in ["\u{85}", "\u{a0}", "é", "ß", "中", "🦀"] {
        names.push(char.to_owned());
        names.push(format!("alt+{char}"));
    }
    for name in &names {
        let key: Key = name
            .parse()
            .unwrap_or_else(|error| panic!("{name:?}: {error}"));
        // The canonical spelling parses to the same key.
        assert_eq!(key.to_string().parse::<Key>(), Ok(key), "{name:?}");
        // Only Alt with a control name is out of the canonical order.
        if !name.starts_with("alt+ctrl+") {
            assert_eq!(key.to_string(), *name);
        }
    }
}
