//! Key names (`terminal-holder.md` 4.3): what the board, the CLI and the
//! daemon send in `keys`, and the holder encodes for the app's keyboard
//! mode.
//!
//! The list is the one `board/src/components/board/terminal-keys.ts` and
//! `crates/daemon/src/prompt.rs` send, with herdr's spelling, so the board
//! does not change. A name is optional modifier prefixes, `ctrl+`, `alt+`
//! and `shift+`, and then a named key or one printable character. Space is
//! always `space`, never a literal space.
//!
//! A key prints its prefixes in that order. It parses them in any order,
//! each at most once, because the board sends Alt with a control key as
//! `alt+ctrl+a`: `alt+` in front of the name the control byte has.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A key with a name of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Named {
    Enter,
    Tab,
    Backspace,
    Esc,
    Space,
    Up,
    Down,
    Left,
    Right,
    /// F1 to F12.
    F(u8),
}

impl Named {
    const WORDS: &[(&str, Named)] = &[
        ("enter", Named::Enter),
        ("tab", Named::Tab),
        ("backspace", Named::Backspace),
        ("esc", Named::Esc),
        ("space", Named::Space),
        ("up", Named::Up),
        ("down", Named::Down),
        ("left", Named::Left),
        ("right", Named::Right),
    ];

    fn parse(name: &str) -> Option<Named> {
        if let Some((_, named)) = Self::WORDS.iter().find(|(word, _)| *word == name) {
            return Some(*named);
        }
        let n = name.strip_prefix('f')?;
        // `f01` is not a name: the number has no leading zero.
        if n.starts_with('0') {
            return None;
        }
        n.parse()
            .ok()
            .filter(|n| (1..=12).contains(n))
            .map(Named::F)
    }
}

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Named::F(n) => write!(f, "f{n}"),
            named => {
                let word = Self::WORDS
                    .iter()
                    .find(|(_, other)| other == named)
                    .map_or("", |(word, _)| word);
                f.write_str(word)
            }
        }
    }
}

/// What a key is, under its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Base {
    Named(Named),
    /// One printable character: any code point above space other than DEL.
    Char(char),
}

/// One key press, with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub base: Base,
}

/// A name that is not a key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a key name")]
pub struct BadKey(pub String);

impl Key {
    pub const ENTER: Key = Key::named(Named::Enter);
    pub const ESC: Key = Key::named(Named::Esc);

    /// A named key with no modifiers.
    pub const fn named(named: Named) -> Key {
        Key {
            ctrl: false,
            alt: false,
            shift: false,
            base: Base::Named(named),
        }
    }

    /// A printable character with no modifiers, or None when `c` is not one.
    pub fn char(c: char) -> Option<Key> {
        printable(c).then_some(Key {
            ctrl: false,
            alt: false,
            shift: false,
            base: Base::Char(c),
        })
    }
}

fn printable(c: char) -> bool {
    c > ' ' && c != '\u{7f}'
}

impl FromStr for Key {
    type Err = BadKey;

    fn from_str(name: &str) -> Result<Key, BadKey> {
        let bad = || BadKey(name.to_owned());
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = name;
        // A prefix counts only when something is left after it, so `alt++`
        // is Alt with `+` and a lone `+` is the character.
        loop {
            let (flag, after) = if let Some(after) = rest.strip_prefix("ctrl+") {
                (&mut ctrl, after)
            } else if let Some(after) = rest.strip_prefix("alt+") {
                (&mut alt, after)
            } else if let Some(after) = rest.strip_prefix("shift+") {
                (&mut shift, after)
            } else {
                break;
            };
            if after.is_empty() {
                break;
            }
            if *flag {
                return Err(bad());
            }
            *flag = true;
            rest = after;
        }
        let base = match Named::parse(rest) {
            Some(named) => Base::Named(named),
            None => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if printable(c) => Base::Char(c),
                    _ => return Err(bad()),
                }
            }
        };
        Ok(Key {
            ctrl,
            alt,
            shift,
            base,
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.alt {
            f.write_str("alt+")?;
        }
        if self.shift {
            f.write_str("shift+")?;
        }
        match self.base {
            Base::Named(named) => named.fmt(f),
            Base::Char(c) => write!(f, "{c}"),
        }
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Key, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const TERMINAL_KEYS: &str =
        include_str!("../../../board/src/components/board/terminal-keys.ts");
    const PROMPT: &str = include_str!("../../daemon/src/prompt.rs");

    /// The quoted values of the TS object literal `const <name> ... = { ... };`.
    fn table(name: &str) -> Vec<String> {
        let start = TERMINAL_KEYS
            .find(&format!("const {name}:"))
            .unwrap_or_else(|| panic!("terminal-keys.ts has no table {name}"));
        let body = &TERMINAL_KEYS[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find("};").unwrap()];
        body.split(',')
            .filter_map(|entry| entry.split_once(':'))
            .map(|(_, value)| value.trim().trim_matches('"').to_owned())
            .collect()
    }

    /// The body of the TS function `function <name>(`.
    fn function(name: &str) -> &'static str {
        let start = TERMINAL_KEYS
            .find(&format!("function {name}("))
            .unwrap_or_else(|| panic!("terminal-keys.ts has no function {name}"));
        let body = &TERMINAL_KEYS[start..];
        &body[..body.find("\n}\n").unwrap()]
    }

    /// The string literals a TS function body returns, unescaped.
    fn returns(body: &str) -> Vec<String> {
        body.split("return \"")
            .skip(1)
            .map(|rest| {
                let mut escaped = false;
                let end = rest
                    .char_indices()
                    .find(|&(_, c)| {
                        let close = c == '"' && !escaped;
                        escaped = c == '\\' && !escaped;
                        close
                    })
                    .unwrap()
                    .0;
                // A TS literal with only these escapes reads as JSON.
                serde_json::from_str(&format!("\"{}\"", &rest[..end])).unwrap()
            })
            .collect()
    }

    /// Asserts the TS still has the shape a list below is built from, so a
    /// change there fails here instead of leaving the list behind.
    fn shape(body: &str, text: &str) {
        assert!(body.contains(text), "terminal-keys.ts changed: no {text:?}");
    }

    /// Every name `terminal-keys.ts` can emit.
    fn board_names() -> Vec<String> {
        let mut names = Vec::new();

        // `modifiers()` joins any mix of the three, in this order, before
        // an arrow or an F key.
        let modifiers = function("modifiers");
        shape(
            modifiers,
            r#"(value & 4 ? "ctrl+" : "") + (value & 2 ? "alt+" : "") + (value & 1 ? "shift+" : "")"#,
        );
        let mut prefixes = vec![String::new()];
        for mask in 1..8 {
            let mut prefix = String::new();
            if mask & 4 != 0 {
                prefix.push_str("ctrl+");
            }
            if mask & 2 != 0 {
                prefix.push_str("alt+");
            }
            if mask & 1 != 0 {
                prefix.push_str("shift+");
            }
            prefixes.push(prefix);
        }
        let mut modified = table("ARROWS");
        modified.extend(table("SS3_F"));
        modified.extend(table("TILDE"));
        assert_eq!(modified.len(), 4 + 4 + 12);
        for key in &modified {
            for prefix in &prefixes {
                names.push(format!("{prefix}{key}"));
            }
        }

        // `control()`: its literals, and Ctrl with each letter.
        let control = function("control");
        shape(control, "`ctrl+${String.fromCharCode(code + 0x60)}`");
        shape(control, "code >= 0x01 && code <= 0x1a");
        let mut controls = returns(control);
        controls.retain(|name| !name.is_empty());
        controls.extend(('a'..='z').map(|c| format!("ctrl+{c}")));

        // `printable()`: space by name, any other character as itself.
        let printable = function("printable");
        shape(printable, r#"char === " " ? "space" : char"#);
        let mut chars: Vec<String> = vec!["space".to_owned()];
        chars.extend(('!'..='~').map(String::from));
        // Beyond ASCII: a C1 control, Latin, CJK and a pair of UTF-16
        // surrogates, which `codePointAt` reads as one character.
        chars.extend(["\u{85}", "é", "ß", "中", "🦀"].map(String::from));

        // `escape()`: its own literals, and Alt with a control name or a
        // character.
        let escape = function("escape");
        shape(escape, "key: `alt+${named}`");
        shape(
            escape,
            "next < 0x20 || next === 0x7f ? control(next) : printable(char)",
        );
        for literal in ["shift+tab", "esc"] {
            shape(escape, &format!("\"{literal}\""));
            names.push(literal.to_owned());
        }
        for name in controls.iter().chain(&chars) {
            names.push(format!("alt+{name}"));
        }

        // `typedKeys()`: a control byte by `control()`, else `printable()`.
        shape(function("typedKeys"), "const key = control(code);");
        names.extend(controls);
        names.extend(chars);
        names
    }

    /// Every name `prompt.rs` can emit: a digit (its options run 1 to 9,
    /// one digit each), an arrow, and Enter.
    fn prompt_names() -> Vec<String> {
        for text in [
            "vec![option.to_string()]",
            r#"if option > self.cursor { "down" } else { "up" }"#,
            r#"keys.push("enter".to_owned())"#,
            "chars.next()?.to_digit(10).filter(|&n| n > 0)?",
        ] {
            assert!(PROMPT.contains(text), "prompt.rs changed: no {text:?}");
        }
        let mut names: Vec<String> = (1..=9).map(|n: u8| n.to_string()).collect();
        names.extend(["up", "down", "enter"].map(String::from));
        names
    }

    #[test]
    fn every_name_the_board_and_the_prompt_send_parses() {
        let names: Vec<String> = board_names().into_iter().chain(prompt_names()).collect();
        assert!(names.len() > 300, "only {} names", names.len());
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

    #[test]
    fn modifiers_parse_in_order_and_alt_takes_a_plus() {
        assert_eq!(
            "ctrl+alt+shift+f12".parse::<Key>().unwrap(),
            Key {
                ctrl: true,
                alt: true,
                shift: true,
                base: Base::Named(Named::F(12)),
            }
        );
        let plus = "alt++".parse::<Key>().unwrap();
        assert!(plus.alt);
        assert_eq!(plus.base, Base::Char('+'));
        assert_eq!("+".parse::<Key>().unwrap(), Key::char('+').unwrap());
        assert_eq!("ctrl+c".parse::<Key>().unwrap().base, Base::Char('c'));
        assert_eq!("enter".parse::<Key>().unwrap(), Key::ENTER);
        let board = "alt+ctrl+a".parse::<Key>().unwrap();
        assert_eq!(board.to_string(), "ctrl+alt+a");
        assert!(board.ctrl && board.alt && !board.shift);
    }

    #[test]
    fn a_name_with_a_modifier_twice_or_no_key_does_not_parse() {
        for name in [
            "",
            " ",
            "\u{7f}",
            "\t",
            "Enter",
            "ctrl+ctrl+a",
            "alt+shift+alt+x",
            "ctrl+",
            "f0",
            "f13",
            "f01",
            "home",
            "ctrl+ab",
            "super+a",
            "ctrl+ ",
        ] {
            assert_eq!(
                name.parse::<Key>(),
                Err(BadKey(name.to_owned())),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_key_is_its_name_on_the_wire() {
        let keys: Vec<Key> = serde_json::from_str(r#"["ctrl+c","esc","4"]"#).unwrap();
        assert_eq!(keys[1], Key::ESC);
        assert_eq!(
            serde_json::to_string(&keys).unwrap(),
            r#"["ctrl+c","esc","4"]"#
        );
        assert!(serde_json::from_str::<Key>(r#""meta+x""#).is_err());
    }
}
