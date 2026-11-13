//! Key names (`terminal-holder.md` 4.3): what the board, the CLI and the
//! daemon send in `keys`, and the holder encodes for the app's keyboard
//! mode.
//!
//! The list is the one `board/src/components/board/terminal-keys.ts` and
//! `crates/daemon/src/prompt.rs` send. A name is optional modifier prefixes, `ctrl+`, `alt+`
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
        if n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
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

/// A character a key name may hold as it is: not a space, a C0 control or
/// DEL, which have names. A C1 control passes, as the board sends what was
/// typed as it is.
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
            "f+1",
            "ctrl+f+12",
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
