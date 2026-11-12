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
    /// of `\ ] ^ _`, as `[attach] prefix` gives it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let refuse = || {
            format!(
                "[attach] prefix is ctrl- and a letter or one of \\ ] ^ _, such as ctrl-\\ or ctrl-a; not {text:?}"
            )
        };
        let lower = text.trim().to_ascii_lowercase();
        let key = lower
            .strip_prefix("ctrl-")
            .or_else(|| lower.strip_prefix("c-"))
            .ok_or_else(refuse)?;
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(chord), None) if chord.is_ascii_lowercase() || "\\]^_".contains(chord) => {
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
    /// Anything else, a paste's bytes among them.
    Other,
}

/// One key and the bytes the terminal sent for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub kind: Kind,
    pub bytes: Vec<u8>,
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
                    keys.push(Key {
                        kind,
                        bytes: buf[at..at + len].to_vec(),
                    });
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
        vec![Key { kind, bytes }]
    }

    /// The first key in `buf` and its length, or None when `buf` holds only
    /// the start of a sequence.
    fn next_key(&mut self, buf: &[u8]) -> Option<(Kind, usize)> {
        let first = buf[0];
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
            Some(_) => Some((Kind::Other, 2)),
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
            return Kind::Other;
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
        match mods {
            CTRL if code == self.prefix.code() => Kind::Prefix,
            0 if code == 13 => Kind::Enter,
            0 if code == 27 => Kind::Escape,
            0 => printable(code),
            SHIFT => match shifted {
                Some(shifted) => printable(shifted),
                None => printable(code).upper(),
            },
            _ => Kind::Other,
        }
    }
}

impl Kind {
    fn upper(self) -> Self {
        match self {
            Kind::Char(c) => Kind::Char(c.to_ascii_uppercase()),
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

/// Whether the prefix came and waits for its key.
#[derive(Debug, Default)]
pub struct Prefixed {
    armed: bool,
}

impl Prefixed {
    pub fn on_key(&mut self, key: Key) -> Option<Act> {
        if !self.armed {
            if key.kind == Kind::Prefix {
                self.armed = true;
                return None;
            }
            return Some(Act::Send(key.bytes));
        }
        // The prefix's own release comes before the next key.
        if key.kind == Kind::Release {
            return None;
        }
        self.armed = false;
        match key.kind {
            Kind::Prefix => Some(Act::Send(key.bytes)),
            Kind::Char('d') => Some(Act::Detach),
            Kind::Char('s') => Some(Act::Switcher),
            Kind::Char('n') => Some(Act::Next),
            Kind::Char('p') => Some(Act::Prev),
            Kind::Char('?') => Some(Act::Help),
            _ => None,
        }
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
    fn a_prefix_setting_reads_ctrl_and_one_key() {
        assert_eq!(Prefix::parse("ctrl-\\").unwrap(), Prefix::default());
        let a = Prefix::parse("C-a").unwrap();
        assert_eq!((a.byte(), a.code(), a.label()), (1, 97, "Ctrl-a".into()));
        assert!(Prefix::parse("ctrl-[").is_err());
        assert!(Prefix::parse("alt-a").is_err());
        assert!(Prefix::parse("ctrl-ab").is_err());
    }
}
