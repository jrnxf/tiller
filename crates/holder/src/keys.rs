//! Named keys, encoded for the app's keyboard mode (`terminal-holder.md`
//! 4.3): legacy bytes, application cursor keys (DECCKM), or the kitty
//! keyboard flags the app pushed.
//!
//! The kitty encoding follows the protocol's rules for a key press. With
//! only "disambiguate" on, Enter, Tab and Backspace with no modifiers, and
//! text with at most Shift, stay as they were; Esc and every other key with
//! Ctrl or Alt become `CSI code ; mods u`. With "report all keys" on, every
//! key does, and "associated text" adds the text it types. The holder
//! answers no kitty query yet (H04), so apps learn the mode from their own
//! pushes only.

use std::fmt::Write as _;

use alacritty_terminal::term::TermMode;
use tiller_panes::keys::{Base, Key, Named};

/// How the app reads keys, from its modes.
#[derive(Clone, Copy, Debug)]
struct Modes {
    app_cursor: bool,
    newline: bool,
    disambiguate: bool,
    alternates: bool,
    all_keys: bool,
    text: bool,
}

impl Modes {
    fn of(mode: TermMode) -> Modes {
        let all_keys = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
        Modes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            newline: mode.contains(TermMode::LINE_FEED_NEW_LINE),
            disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            alternates: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
            all_keys,
            // The protocol sends text only with every key as a code.
            text: all_keys && mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
        }
    }

    fn kitty(&self) -> bool {
        self.disambiguate || self.all_keys
    }
}

/// The bytes for `keys`, in order, for an app with `mode` on.
pub fn encode(keys: &[Key], mode: TermMode) -> Vec<u8> {
    let modes = Modes::of(mode);
    let mut out = String::new();
    for key in keys {
        if modes.kitty() {
            kitty(&mut out, *key, modes);
        } else {
            legacy(&mut out, *key, modes);
        }
    }
    out.into_bytes()
}

/// The modifier parameter of a CSI sequence: 1 plus the modifier bits.
fn mods(key: Key) -> u8 {
    1 + u8::from(key.shift) + 2 * u8::from(key.alt) + 4 * u8::from(key.ctrl)
}

/// A cursor or function key's CSI sequence: `CSI number ; mods final`.
/// With no modifiers, `CSI final` (or `CSI number ~`).
fn csi(out: &mut String, number: u8, last: char, mods: u8) {
    match (number, mods) {
        (1, 1) => _ = write!(out, "\x1b[{last}"),
        (number, 1) => _ = write!(out, "\x1b[{number}{last}"),
        (number, mods) => _ = write!(out, "\x1b[{number};{mods}{last}"),
    }
}

/// A cursor or function key: its CSI number and final character, and the
/// SS3 final a legacy app takes for it unmodified.
fn function(named: Named) -> Option<(u8, char, Option<char>)> {
    Some(match named {
        Named::Up => (1, 'A', Some('A')),
        Named::Down => (1, 'B', Some('B')),
        Named::Right => (1, 'C', Some('C')),
        Named::Left => (1, 'D', Some('D')),
        Named::F(n @ 1..=4) => (1, char::from(b'P' + n - 1), Some(char::from(b'P' + n - 1))),
        Named::F(n @ 5..=12) => (
            [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)],
            '~',
            None,
        ),
        _ => return None,
    })
}

fn legacy(out: &mut String, key: Key, modes: Modes) {
    let alt = |out: &mut String| {
        if key.alt {
            out.push('\x1b');
        }
    };
    match key.base {
        Base::Named(named) => {
            if let Some((number, last, ss3)) = function(named) {
                let mods = mods(key);
                match ss3 {
                    // Arrows honor application cursor keys; F1 to F4 are
                    // always SS3.
                    Some(ss3)
                        if mods == 1 && (modes.app_cursor || matches!(named, Named::F(_))) =>
                    {
                        _ = write!(out, "\x1bO{ss3}");
                    }
                    _ => csi(out, number, last, mods),
                }
                return;
            }
            match named {
                Named::Enter => {
                    alt(out);
                    out.push_str(if modes.newline { "\r\n" } else { "\r" });
                }
                Named::Tab if key.shift => {
                    alt(out);
                    out.push_str("\x1b[Z");
                }
                Named::Tab => {
                    alt(out);
                    out.push('\t');
                }
                Named::Backspace => {
                    alt(out);
                    out.push(if key.ctrl { '\x08' } else { '\x7f' });
                }
                Named::Esc => {
                    alt(out);
                    out.push('\x1b');
                }
                Named::Space => {
                    alt(out);
                    out.push(if key.ctrl { '\0' } else { ' ' });
                }
                _ => {}
            }
        }
        Base::Char(c) => {
            alt(out);
            let c = if key.shift { shifted(c) } else { c };
            out.push(if key.ctrl { control(c).unwrap_or(c) } else { c });
        }
    }
}

/// The character Shift types with `c`, for a letter.
fn shifted(c: char) -> char {
    single(c.to_uppercase()).unwrap_or(c)
}

/// The one character a case mapping gives, if it gives one.
fn single(mut mapped: impl Iterator<Item = char>) -> Option<char> {
    match (mapped.next(), mapped.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// The control byte Ctrl types with `c`, as xterm sends it.
fn control(c: char) -> Option<char> {
    let byte = match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(char::from(byte))
}

fn kitty(out: &mut String, key: Key, modes: Modes) {
    let mods = mods(key);
    match key.base {
        Base::Named(named) => {
            if let Some((number, last, _)) = function(named) {
                // The kitty protocol sends F3 as `CSI 13 ~`: `CSI R` reads
                // as a cursor position report.
                let (number, last) = if named == Named::F(3) {
                    (13, '~')
                } else {
                    (number, last)
                };
                csi(out, number, last, mods);
                return;
            }
            let (code, text) = match named {
                Named::Enter => (13, None),
                Named::Tab => (9, None),
                Named::Backspace => (127, None),
                Named::Esc => (27, None),
                Named::Space => (32, Some(' ')),
                _ => return,
            };
            if !modes.all_keys && named != Named::Esc {
                // Enter, Tab, Backspace and Space unmodified, and Space with
                // Shift, which types its text, stay as they were.
                if mods == 1 {
                    out.push(char::from(code));
                    return;
                }
                if named == Named::Space && mods == 2 {
                    out.push(' ');
                    return;
                }
            }
            let text = text.filter(|_| !key.ctrl && !key.alt);
            csi_u(out, u32::from(code), None, mods, text, modes);
        }
        Base::Char(c) => {
            // The code is the key's own, unshifted: a capital is the letter
            // with Shift.
            let lower = single(c.to_lowercase()).filter(|&lower| lower != c);
            let (code, shift) = match lower {
                Some(lower) if c.is_uppercase() => (lower, true),
                _ => (c, key.shift),
            };
            let key = Key { shift, ..key };
            let typed = if shift { shifted(code) } else { code };
            if !modes.all_keys && !key.ctrl && !key.alt {
                out.push(typed);
                return;
            }
            let alternate = (modes.alternates && shift && typed != code).then_some(typed);
            let text = (!key.ctrl && !key.alt).then_some(typed);
            csi_u(
                out,
                u32::from(code),
                alternate,
                self::mods(key),
                text,
                modes,
            );
        }
    }
}

/// `CSI code[:alternate] ; mods [; text] u`.
fn csi_u(
    out: &mut String,
    code: u32,
    alternate: Option<char>,
    mods: u8,
    text: Option<char>,
    modes: Modes,
) {
    _ = write!(out, "\x1b[{code}");
    if let Some(alternate) = alternate {
        _ = write!(out, ":{}", u32::from(alternate));
    }
    let text = text.filter(|_| modes.text);
    match (mods, text) {
        (1, None) => {}
        (mods, None) => _ = write!(out, ";{mods}"),
        (1, Some(text)) => _ = write!(out, ";;{}", u32::from(text)),
        (mods, Some(text)) => _ = write!(out, ";{mods};{}", u32::from(text)),
    }
    out.push('u');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(names: &str, mode: TermMode) -> String {
        let keys: Vec<Key> = names.split(' ').map(|name| name.parse().unwrap()).collect();
        String::from_utf8(encode(&keys, mode)).unwrap()
    }

    const LEGACY: TermMode = TermMode::empty();

    #[test]
    fn legacy_keys_are_the_bytes_xterm_sends() {
        let cases = [
            ("enter", "\r"),
            ("tab", "\t"),
            ("shift+tab", "\x1b[Z"),
            ("alt+shift+tab", "\x1b\x1b[Z"),
            ("backspace", "\x7f"),
            ("ctrl+backspace", "\x08"),
            ("esc", "\x1b"),
            ("space", " "),
            ("ctrl+space", "\0"),
            ("up", "\x1b[A"),
            ("left", "\x1b[D"),
            ("ctrl+right", "\x1b[1;5C"),
            ("shift+alt+down", "\x1b[1;4B"),
            ("f1", "\x1bOP"),
            ("f4", "\x1bOS"),
            ("ctrl+f2", "\x1b[1;5Q"),
            ("f5", "\x1b[15~"),
            ("f12", "\x1b[24~"),
            ("shift+f6", "\x1b[17;2~"),
            ("a", "a"),
            ("shift+a", "A"),
            ("ctrl+c", "\x03"),
            ("ctrl+[", "\x1b"),
            ("alt+x", "\x1bx"),
            ("alt+ctrl+a", "\x1b\x01"),
            ("alt+enter", "\x1b\r"),
            ("é", "é"),
        ];
        for (name, want) in cases {
            assert_eq!(bytes(name, LEGACY), want, "{name}");
        }
        assert_eq!(bytes("h i enter", LEGACY), "hi\r");
    }

    #[test]
    fn application_cursor_keys_send_ss3_arrows_unmodified() {
        assert_eq!(bytes("up", TermMode::APP_CURSOR), "\x1bOA");
        assert_eq!(bytes("ctrl+up", TermMode::APP_CURSOR), "\x1b[1;5A");
        assert_eq!(bytes("f5", TermMode::APP_CURSOR), "\x1b[15~");
        assert_eq!(bytes("enter", TermMode::LINE_FEED_NEW_LINE), "\r\n");
    }

    #[test]
    fn kitty_disambiguate_codes_esc_and_modified_keys_only() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        let cases = [
            ("esc", "\x1b[27u"),
            ("enter", "\r"),
            ("tab", "\t"),
            ("backspace", "\x7f"),
            ("space", " "),
            ("shift+enter", "\x1b[13;2u"),
            ("shift+tab", "\x1b[9;2u"),
            ("ctrl+backspace", "\x1b[127;5u"),
            ("ctrl+space", "\x1b[32;5u"),
            ("a", "a"),
            ("shift+a", "A"),
            ("A", "A"),
            ("ctrl+c", "\x1b[99;5u"),
            ("alt+x", "\x1b[120;3u"),
            ("ctrl+shift+a", "\x1b[97;6u"),
            ("up", "\x1b[A"),
            ("ctrl+left", "\x1b[1;5D"),
            ("f1", "\x1b[P"),
            ("f3", "\x1b[13~"),
            ("shift+f3", "\x1b[13;2~"),
            ("f4", "\x1b[S"),
            ("f5", "\x1b[15~"),
        ];
        for (name, want) in cases {
            assert_eq!(bytes(name, mode), want, "{name}");
        }
        // Application cursor keys do not apply once kitty flags are on.
        assert_eq!(bytes("up", mode | TermMode::APP_CURSOR), "\x1b[A");
    }

    #[test]
    fn kitty_all_keys_codes_every_key_and_adds_text_and_alternates_when_asked() {
        let all = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;
        let cases = [
            ("enter", "\x1b[13u"),
            ("tab", "\x1b[9u"),
            ("esc", "\x1b[27u"),
            ("space", "\x1b[32u"),
            ("a", "\x1b[97u"),
            ("shift+a", "\x1b[97;2u"),
            ("ctrl+c", "\x1b[99;5u"),
            // A capital past ASCII is its small letter with Shift too.
            ("É", "\x1b[233;2u"),
            ("shift+é", "\x1b[233;2u"),
        ];
        for (name, want) in cases {
            assert_eq!(bytes(name, all), want, "{name}");
        }
        let text = all | TermMode::REPORT_ASSOCIATED_TEXT | TermMode::REPORT_ALTERNATE_KEYS;
        let cases = [
            ("a", "\x1b[97;;97u"),
            ("shift+a", "\x1b[97:65;2;65u"),
            ("ctrl+a", "\x1b[97;5u"),
            ("space", "\x1b[32;;32u"),
            ("enter", "\x1b[13u"),
        ];
        for (name, want) in cases {
            assert_eq!(bytes(name, text), want, "{name}");
        }
    }
}
