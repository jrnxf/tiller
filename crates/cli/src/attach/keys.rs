//! The owner's keys, as `tiller attach` reads them (`terminal-holder.md`
//! 5.2): the prefix in both encodings, the keys after it, and the keys the
//! help screen and the switcher take. Every key keeps the bytes the terminal
//! sent, so a key that goes to the pane goes exactly as typed.

/// The prefix key: a Ctrl chord. The terminal sends it as one control byte,
/// or as `CSI <code>;5u` once the agent pushed kitty keyboard flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefix {
    /// The character after `Ctrl-`, in lower case.
    chord: char,
}

impl Default for Prefix {
    fn default() -> Self {
        Self { chord: '\\' }
    }
}

impl Prefix {
    /// Reads `ctrl-<key>` (or `C-<key>`), where the key is a letter or one
    /// of `\ ]`, as `[attach] prefix` gives it. Ctrl-i, Ctrl-j and Ctrl-m
    /// send the bytes of Tab and Enter, and Ctrl-^ and Ctrl-_ need Shift in
    /// the kitty encoding, so none of them can be the prefix.
    pub fn parse(text: &str) -> Result<Self, String> {
        let refuse = || {
            format!(
                "[attach] prefix is ctrl- and a letter but i, j or m, or one of \\ ], such as ctrl-\\ or ctrl-a; not {text:?}"
            )
        };
        let lower = text.trim().to_ascii_lowercase();
        let key = lower
            .strip_prefix("ctrl-")
            .or_else(|| lower.strip_prefix("c-"))
            .ok_or_else(refuse)?;
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(chord), None)
                if (chord.is_ascii_lowercase() && !"ijm".contains(chord))
                    || "\\]".contains(chord) =>
            {
                Ok(Self { chord })
            }
            _ => Err(refuse()),
        }
    }

    /// The control byte the legacy encoding sends.
    fn byte(self) -> u8 {
        (self.chord as u8) & 0x1f
    }

    /// The key code the kitty encoding sends.
    fn code(self) -> u32 {
        u32::from(self.chord)
    }

    /// The prefix as the help screen names it: `Ctrl-\`.
    pub fn label(self) -> String {
        format!("Ctrl-{}", self.chord)
    }
}

/// What a key is, as far as attach cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Prefix,
    /// A printable key with no modifier but Shift.
    Char(char),
    Up,
    Down,
    Enter,
    Escape,
    /// A kitty key release event, which only follows a press.
    Release,
    /// A kitty press of a modifier key alone, as Shift before `?`.
    Modifier,
    /// Anything else, a paste's bytes among them.
    Other,
}

/// One key and the bytes the terminal sent for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub kind: Kind,
    pub bytes: Vec<u8>,
    /// Which key it is, the same for its press and its kitty release.
    id: Option<Id>,
}

/// A key as kitty names it in a release: its unshifted code, or 1 for a
/// key such as Up, and the final byte of its sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Id {
    code: u32,
    last: u8,
}

impl Id {
    fn char(code: u32) -> Self {
        let code = char::from_u32(code).map_or(code, |c| u32::from(unshifted(c)));
        Self { code, last: b'u' }
    }
}

/// Splits the owner's bytes into keys. A sequence cut at the end of a read
/// waits for the next read, or for [`Reader::flush`] when none comes soon:
/// a lone Escape looks like the start of a sequence until then.
#[derive(Debug, Default)]
pub struct Reader {
    prefix: Prefix,
    pending: Vec<u8>,
    /// Inside a bracketed paste: nothing in it is a key to attach.
    paste: bool,
}

const ESC: u8 = 0x1b;

impl Reader {
    pub fn new(prefix: Prefix) -> Self {
        Self {
            prefix,
            ..Self::default()
        }
    }

    /// Whether a cut sequence waits for more bytes.
    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The keys in `bytes`, after what an earlier read left waiting.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.pending);
        let mut keys = Vec::new();
        let mut at = 0;
        while at < buf.len() {
            match self.next_key(&buf[at..]) {
                Some((kind, len)) => {
                    let bytes = buf[at..at + len].to_vec();
                    let id = self.id(&bytes);
                    keys.push(Key { kind, bytes, id });
                    at += len;
                }
                None => {
                    self.pending = buf[at..].to_vec();
                    break;
                }
            }
        }
        keys
    }

    /// What waits, as keys: no more bytes came, so a lone `ESC` was the
    /// Escape key, and any other cut sequence passes as it is.
    pub fn flush(&mut self) -> Vec<Key> {
        let bytes = std::mem::take(&mut self.pending);
        if bytes.is_empty() {
            return Vec::new();
        }
        let kind = match (bytes.as_slice(), self.paste) {
            ([ESC], false) => Kind::Escape,
            _ => Kind::Other,
        };
        let id = self.id(&bytes);
        vec![Key { kind, bytes, id }]
    }

    /// Which key `bytes` is, or None for a paste's bytes.
    fn id(&self, bytes: &[u8]) -> Option<Id> {
        if self.paste {
            return None;
        }
        match bytes {
            [byte] => Some(Id::char(match *byte {
                0 => 32,
                byte @ 1..=26 if !matches!(byte, 8 | 9 | 13) => u32::from(byte) + 96,
                byte @ 28..=31 => u32::from(byte) + 64,
                byte => u32::from(byte),
            })),
            [ESC, b'O', last] => Some(Id {
                code: 1,
                last: *last,
            }),
            [ESC, b'[', params @ .., last] => {
                let code = params
                    .split(|byte| matches!(byte, b';' | b':'))
                    .next()
                    .and_then(|code| std::str::from_utf8(code).ok()?.parse().ok())
                    .unwrap_or(1);
                Some(match last {
                    b'u' => Id::char(code),
                    _ => Id { code, last: *last },
                })
            }
            [ESC, rest @ ..] | rest => {
                let mut chars = std::str::from_utf8(rest).ok()?.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Some(Id::char(u32::from(c))),
                    _ => None,
                }
            }
        }
    }

    /// The first key in `buf` and its length, or None when `buf` holds only
    /// the start of a sequence.
    fn next_key(&mut self, buf: &[u8]) -> Option<(Kind, usize)> {
        let first = buf[0];
        if first >= 0x80 {
            let len = char_len(buf)?;
            let kind = match std::str::from_utf8(&buf[..len]) {
                Ok(text) if !self.paste => text
                    .chars()
                    .next()
                    .filter(|c| !c.is_control())
                    .map_or(Kind::Other, Kind::Char),
                _ => Kind::Other,
            };
            return Some((kind, len));
        }
        if first != ESC {
            let kind = if self.paste {
                Kind::Other
            } else if first == self.prefix.byte() {
                Kind::Prefix
            } else if first == b'\r' || first == b'\n' {
                Kind::Enter
            } else if (0x20..0x7f).contains(&first) {
                Kind::Char(char::from(first))
            } else {
                Kind::Other
            };
            return Some((kind, 1));
        }
        match buf.get(1) {
            None => None,
            // Escape pressed twice: the first is a key of its own.
            Some(&ESC) => Some((self.plain(Kind::Escape), 1)),
            Some(b'[') => {
                let len = csi_len(buf)?;
                Some((self.csi(&buf[2..len - 1], buf[len - 1]), len))
            }
            Some(b'O') => {
                let last = *buf.get(2)?;
                let kind = match last {
                    b'A' => Kind::Up,
                    b'B' => Kind::Down,
                    _ => Kind::Other,
                };
                Some((self.plain(kind), 3))
            }
            // Alt and a key.
            Some(_) => Some((Kind::Other, 1 + char_len(&buf[1..])?)),
        }
    }

    /// `kind`, or `Other` inside a paste.
    fn plain(&self, kind: Kind) -> Kind {
        if self.paste { Kind::Other } else { kind }
    }

    /// A CSI sequence with these parameters and this final byte.
    fn csi(&mut self, params: &[u8], last: u8) -> Kind {
        let params = String::from_utf8_lossy(params);
        if last == b'~' {
            match params.as_ref() {
                "200" => self.paste = true,
                "201" => self.paste = false,
                _ => {}
            }
        }
        if self.paste {
            return Kind::Other;
        }
        let mut fields = params.split(';');
        let codes = fields.next().unwrap_or_default();
        let (mods, event) = modifiers(fields.next().unwrap_or_default());
        if event == Some(3) {
            return Kind::Release;
        }
        match last {
            b'A' => Kind::Up,
            b'B' => Kind::Down,
            b'u' => self.kitty_key(codes, mods),
            _ => Kind::Other,
        }
    }

    /// A kitty `CSI code[:shifted] ; mods u` key.
    fn kitty_key(&self, codes: &str, mods: u32) -> Kind {
        let mut codes = codes.split(':');
        let Some(code) = codes.next().and_then(|code| code.parse::<u32>().ok()) else {
            return Kind::Other;
        };
        let shifted = codes.next().and_then(|code| code.parse::<u32>().ok());
        const SHIFT: u32 = 1;
        const CTRL: u32 = 4;
        // Shift, Ctrl, Alt, Super, Hyper, Meta and the ISO level shifts.
        if (57441..=57454).contains(&code) {
            return Kind::Modifier;
        }
        match mods {
            CTRL if code == self.prefix.code() => Kind::Prefix,
            0 if code == 13 => Kind::Enter,
            0 if code == 27 => Kind::Escape,
            0 => printable(code),
            // With no shifted key reported, the US layout's is the guess.
            SHIFT => match shifted {
                Some(shifted) => printable(shifted),
                None => printable(code).shifted(),
            },
            _ => Kind::Other,
        }
    }
}

const PLAIN: &str = "`1234567890-=[]\\;',./";
const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// The key that gives `c` on a US layout, with no Shift.
fn unshifted(c: char) -> char {
    SHIFTED
        .find(c)
        .and_then(|at| PLAIN.chars().nth(at))
        .unwrap_or(c.to_ascii_lowercase())
}

impl Kind {
    /// The key Shift gives on a US layout.
    fn shifted(self) -> Self {
        match self {
            Kind::Char(c) => Kind::Char(
                PLAIN
                    .find(c)
                    .and_then(|at| SHIFTED.chars().nth(at))
                    .unwrap_or(c.to_ascii_uppercase()),
            ),
            other => other,
        }
    }
}

fn printable(code: u32) -> Kind {
    match char::from_u32(code) {
        Some(c) if c.is_ascii_graphic() || c == ' ' => Kind::Char(c),
        _ => Kind::Other,
    }
}

/// A kitty modifier field, `mods[:event]`: the modifier bits less Caps Lock
/// and Num Lock, and the event type.
fn modifiers(field: &str) -> (u32, Option<u32>) {
    let mut parts = field.split(':');
    let mods = parts
        .next()
        .and_then(|mods| mods.parse::<u32>().ok())
        .map_or(0, |mods| mods.saturating_sub(1) & !(64 | 128));
    let event = parts.next().and_then(|event| event.parse().ok());
    (mods, event)
}

/// The length of the UTF-8 character at the start of `buf`, 1 for a byte
/// that starts none, or None when it is cut.
fn char_len(buf: &[u8]) -> Option<usize> {
    let len = match buf[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some(1),
    };
    if buf[1..buf.len().min(len)]
        .iter()
        .any(|byte| byte & 0xc0 != 0x80)
    {
        return Some(1);
    }
    (buf.len() >= len).then_some(len)
}

/// The length of the CSI sequence at the start of `buf`, or None when it is
/// cut. A byte that cannot be in one ends it there.
fn csi_len(buf: &[u8]) -> Option<usize> {
    buf.iter()
        .enumerate()
        .skip(2)
        .find(|(_, byte)| !(0x20..0x40).contains(*byte))
        .map(|(at, _)| at + 1)
}

/// What a key does while the pane shows: the prefix and the key after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Bytes for the pane.
    Send(Vec<u8>),
    Detach,
    Switcher,
    Next,
    Prev,
    Help,
}

/// Whether the prefix came and waits for its key, and which keys attach
/// used up: the pane gets a kitty release only when it got the press.
#[derive(Debug, Default)]
pub struct Prefixed {
    armed: bool,
    used: Vec<Id>,
}

impl Prefixed {
    pub fn on_key(&mut self, key: Key) -> Option<Act> {
        if key.kind == Kind::Release {
            let at = self.used.iter().position(|id| Some(*id) == key.id);
            return match at {
                Some(at) => {
                    self.used.swap_remove(at);
                    None
                }
                None => Some(Act::Send(key.bytes)),
            };
        }
        if !self.armed {
            if key.kind == Kind::Prefix {
                self.armed = true;
                self.swallow(&key);
                return None;
            }
            self.sent(&key);
            return Some(Act::Send(key.bytes));
        }
        // A Shift held for `?` comes before the next key.
        if key.kind == Kind::Modifier {
            self.swallow(&key);
            return None;
        }
        self.armed = false;
        if key.kind == Kind::Prefix {
            self.sent(&key);
            return Some(Act::Send(key.bytes));
        }
        self.swallow(&key);
        match key.kind {
            Kind::Char('d') => Some(Act::Detach),
            Kind::Char('s') => Some(Act::Switcher),
            Kind::Char('n') => Some(Act::Next),
            Kind::Char('p') => Some(Act::Prev),
            Kind::Char('?') => Some(Act::Help),
            _ => None,
        }
    }

    /// A key the help or the switcher took: its release is dropped too.
    pub fn swallow(&mut self, key: &Key) {
        if let Some(id) = key.id
            && !self.used.contains(&id)
        {
            self.used.push(id);
        }
    }

    /// The prefix no longer waits for its key, as when another pane shows.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// A key the pane got: its release goes there too.
    fn sent(&mut self, key: &Key) {
        self.used.retain(|id| Some(*id) != key.id);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn kinds(reader: &mut Reader, bytes: &[u8]) -> Vec<Kind> {
        reader.feed(bytes).into_iter().map(|key| key.kind).collect()
    }

    #[test]
    fn the_prefix_is_read_in_both_encodings() {
        let mut reader = Reader::default();
        assert_eq!(kinds(&mut reader, b"\x1c"), [Kind::Prefix]);
        assert_eq!(kinds(&mut reader, b"\x1b[92;5u"), [Kind::Prefix]);
        assert_eq!(kinds(&mut reader, b"\x1b[92;5:1u"), [Kind::Prefix]);
        // Caps Lock on still counts.
        assert_eq!(kinds(&mut reader, b"\x1b[92;69u"), [Kind::Prefix]);
        assert_eq!(kinds(&mut reader, b"\x1b[92;5:3u"), [Kind::Release]);
        // Ctrl-Shift-\ is not the prefix.
        assert_eq!(kinds(&mut reader, b"\x1b[92;6u"), [Kind::Other]);
    }

    #[test]
    fn keys_keep_their_bytes_and_a_cut_sequence_waits() {
        let mut reader = Reader::default();
        let keys = reader.feed(b"ab\x1b[9");
        assert_eq!(keys.len(), 2);
        assert!(reader.waiting());
        let keys = reader.feed(b"2;5u\x1b[A");
        assert_eq!(keys[0].bytes, b"\x1b[92;5u");
        assert_eq!(keys[0].kind, Kind::Prefix);
        assert_eq!(keys[1].kind, Kind::Up);
        // A lone Escape waits, then is the Escape key.
        assert!(reader.feed(b"\x1b").is_empty());
        let flushed = reader.flush();
        assert_eq!(flushed[0].kind, Kind::Escape);
        assert!(!reader.waiting());
    }

    #[test]
    fn kitty_keys_decode_as_their_characters() {
        let mut reader = Reader::default();
        assert_eq!(kinds(&mut reader, b"\x1b[100u"), [Kind::Char('d')]);
        assert_eq!(kinds(&mut reader, b"\x1b[47:63;2u"), [Kind::Char('?')]);
        assert_eq!(kinds(&mut reader, b"\x1b[47;2u"), [Kind::Char('?')]);
        assert_eq!(kinds(&mut reader, b"\x1b[97;2u"), [Kind::Char('A')]);
        assert_eq!(kinds(&mut reader, b"\x1b[57441;2u"), [Kind::Modifier]);
        assert_eq!(kinds(&mut reader, b"\x1b[13u"), [Kind::Enter]);
        assert_eq!(kinds(&mut reader, b"\x1b[27u"), [Kind::Escape]);
        assert_eq!(kinds(&mut reader, b"\x1b[1;1:3A"), [Kind::Release]);
        assert_eq!(kinds(&mut reader, b"\x1bOB"), [Kind::Down]);
    }

    #[test]
    fn nothing_in_a_paste_is_the_prefix() {
        let mut reader = Reader::default();
        let keys = kinds(&mut reader, b"\x1b[200~a\x1c\x1b[92;5u\x1b[201~\x1c");
        assert_eq!(
            keys,
            [
                Kind::Other,
                Kind::Other,
                Kind::Other,
                Kind::Other,
                Kind::Other,
                Kind::Prefix
            ]
        );
    }

    #[test]
    fn the_prefix_twice_sends_the_second_as_typed() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        let acts: Vec<_> = reader
            .feed(b"x\x1c\x1c\x1b[92;5u\x1b[92;5:3u\x1b[92;5u\x1cq\x1c?")
            .into_iter()
            .filter_map(|key| prefixed.on_key(key))
            .collect();
        assert_eq!(
            acts,
            [
                Act::Send(b"x".to_vec()),
                Act::Send(b"\x1c".to_vec()),
                Act::Send(b"\x1b[92;5u".to_vec()),
                Act::Help
            ]
        );
    }

    #[test]
    fn the_prefix_takes_its_command_keys() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        let acts: Vec<_> = reader
            .feed(b"\x1cd\x1cs\x1cn\x1cp\x1b[92;5u\x1b[100u")
            .into_iter()
            .filter_map(|key| prefixed.on_key(key))
            .collect();
        assert_eq!(
            acts,
            [
                Act::Detach,
                Act::Switcher,
                Act::Next,
                Act::Prev,
                Act::Detach
            ]
        );
    }

    #[test]
    fn a_modifier_pressed_after_the_prefix_keeps_it_armed() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        let acts: Vec<_> = reader
            .feed(b"\x1b[92;5u\x1b[92;5:3u\x1b[57441;2u\x1b[47;2u")
            .into_iter()
            .filter_map(|key| prefixed.on_key(key))
            .collect();
        assert_eq!(acts, [Act::Help]);
    }

    #[test]
    fn only_the_release_of_a_key_the_pane_got_goes_to_the_pane() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        let mut acts = |bytes: &[u8]| -> Vec<Act> {
            reader
                .feed(bytes)
                .into_iter()
                .filter_map(|key| prefixed.on_key(key))
                .collect()
        };
        // A key the pane got: its release follows it.
        assert_eq!(
            acts(b"a\x1b[97;1:3u"),
            [
                Act::Send(b"a".to_vec()),
                Act::Send(b"\x1b[97;1:3u".to_vec())
            ]
        );
        // The prefix, Shift and `?` open the help, and their releases,
        // in any order, do not reach the pane.
        assert_eq!(
            acts(b"\x1b[92;5u\x1b[57441;2u?\x1b[92;5:3u\x1b[63;2:3u\x1b[57441;1:3u"),
            [Act::Help]
        );
        // The commands' keys, legacy and kitty, and their late releases.
        assert_eq!(
            acts(b"\x1cn\x1b[92;1:3u\x1b[110;1:3u\x1b[92;5u\x1b[112u\x1b[92;5:3u\x1b[112;1:3u"),
            [Act::Next, Act::Prev]
        );
        // The prefix twice: the pane gets the second press and its release.
        assert_eq!(
            acts(b"\x1c\x1b[92;5:3u\x1c\x1b[92;5:3u"),
            [
                Act::Send(b"\x1c".to_vec()),
                Act::Send(b"\x1b[92;5:3u".to_vec())
            ]
        );
    }

    #[test]
    fn the_release_of_a_key_the_help_took_is_dropped() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        // The help is open. `x` closes it, Escape closes the switcher, and
        // a key the pane got before is still released there.
        for key in reader.feed(b"x\x1b[27u") {
            prefixed.swallow(&key);
        }
        let acts: Vec<_> = reader
            .feed(b"\x1b[120;1:3u\x1b[27;1:3u\x1b[1;1:3A")
            .into_iter()
            .filter_map(|key| prefixed.on_key(key))
            .collect();
        assert_eq!(acts, [Act::Send(b"\x1b[1;1:3A".to_vec())]);
        // A function key's release is a release too.
        assert_eq!(kinds(&mut reader, b"\x1b[15;1:3~"), [Kind::Release]);
    }

    #[test]
    fn a_character_of_many_bytes_is_one_key() {
        let mut reader = Reader::default();
        let mut prefixed = Prefixed::default();
        // A cut character waits for the rest of it.
        assert!(reader.feed(b"\xc3").is_empty());
        assert!(reader.waiting());
        let keys = reader.feed(b"\xa9\x1b\xe2\x82\xac");
        assert_eq!(keys[0].kind, Kind::Char('\u{e9}'));
        assert_eq!(keys[0].bytes, "\u{e9}".as_bytes());
        // Alt and a character of many bytes.
        assert_eq!(keys[1].bytes, b"\x1b\xe2\x82\xac");
        // A byte that starts no character is a key of its own.
        assert_eq!(kinds(&mut reader, b"\xa9x"), [Kind::Other, Kind::Char('x')]);
        // After the prefix, the whole character is used up, and so is its
        // release; the next one, and its release, reach the pane.
        let acts: Vec<_> = reader
            .feed("\x1c\u{e9}\x1b[233;1:3u\u{e9}\x1b[233;1:3u".as_bytes())
            .into_iter()
            .filter_map(|key| prefixed.on_key(key))
            .collect();
        assert_eq!(
            acts,
            [
                Act::Send("\u{e9}".as_bytes().to_vec()),
                Act::Send(b"\x1b[233;1:3u".to_vec())
            ]
        );
        // A cut character that no more bytes follow passes as it is.
        assert!(reader.feed(b"\xe2\x82").is_empty());
        assert_eq!(reader.flush()[0].bytes, b"\xe2\x82");
    }

    #[test]
    fn a_prefix_setting_reads_ctrl_and_one_key() {
        assert_eq!(Prefix::parse("ctrl-\\").unwrap(), Prefix::default());
        let a = Prefix::parse("C-a").unwrap();
        assert_eq!((a.byte(), a.code(), a.label()), (1, 97, "Ctrl-a".into()));
        assert!(Prefix::parse("ctrl-]").is_ok());
        for refused in ["ctrl-[", "ctrl-i", "ctrl-j", "ctrl-m", "ctrl-^", "ctrl-_"] {
            assert!(Prefix::parse(refused).is_err(), "{refused}");
        }
        assert!(Prefix::parse("alt-a").is_err());
        assert!(Prefix::parse("ctrl-ab").is_err());
    }
}
