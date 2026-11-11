//! Framing on the holder socket (`terminal-holder.md` 2.4).
//!
//! Each frame is `[len: u32 big-endian][kind: u8][body]`, and `len` counts
//! `kind` and `body`. A frame over [`MAX_FRAME_LEN`] ends the connection.
//!
//! | `kind` | Body |
//! |---|---|
//! | 0 | one JSON message (UTF-8): requests, replies and events |
//! | 1 | `[stream: u32 big-endian][bytes]`: pane output to a subscriber |
//! | 2 | `[num: u32 big-endian][bytes]`: raw input to the pane with that `num` |
//!
//! [`Decoder`] takes bytes as they arrive, in any split, so a socket read of
//! any size works. [`read_frame`] is the blocking form over a [`Read`].

use std::io::{self, Read, Write};

/// The largest `len` a frame may carry: 16 MiB, counting `kind` and `body`.
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

/// The kind byte of a JSON message.
pub const KIND_MESSAGE: u8 = 0;
/// The kind byte of pane output to a subscriber.
pub const KIND_OUTPUT: u8 = 1;
/// The kind byte of raw input to a pane.
pub const KIND_INPUT: u8 = 2;

/// The `len` prefix.
const LEN_BYTES: usize = 4;
/// The `stream` or `num` prefix of a kind 1 or kind 2 body.
const ADDRESS_BYTES: usize = 4;

/// One frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Kind 0: one JSON message, as UTF-8 text.
    Message(String),
    /// Kind 1: pane output for the subscription `stream`.
    Output { stream: u32, bytes: Vec<u8> },
    /// Kind 2: raw input for the pane `num`.
    Input { num: u32, bytes: Vec<u8> },
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// `len` is over [`MAX_FRAME_LEN`]. The connection ends.
    #[error("a frame of {len} bytes is over the {MAX_FRAME_LEN}-byte limit")]
    TooLarge { len: u64 },

    /// `len` is 0, so the frame has no kind byte.
    #[error("a frame has length 0 and no kind")]
    Empty,

    #[error("a frame has the unknown kind {0}")]
    UnknownKind(u8),

    /// A kind 1 or kind 2 body shorter than its 4-byte address.
    #[error("a kind {kind} frame has a {len}-byte body, shorter than its 4-byte address")]
    NoAddress { kind: u8, len: usize },

    #[error("a kind 0 frame is not UTF-8: {0}")]
    NotUtf8(#[from] std::string::FromUtf8Error),

    /// The stream ended inside a frame.
    #[error("the connection ended inside a frame ({buffered} bytes of it read)")]
    Truncated { buffered: usize },

    #[error(transparent)]
    Io(#[from] io::Error),
}

impl Frame {
    pub fn kind(&self) -> u8 {
        match self {
            Frame::Message(_) => KIND_MESSAGE,
            Frame::Output { .. } => KIND_OUTPUT,
            Frame::Input { .. } => KIND_INPUT,
        }
    }

    /// The frame's `len`: the kind byte and the body.
    fn len(&self) -> u64 {
        let body = match self {
            Frame::Message(text) => text.len(),
            Frame::Output { bytes, .. } | Frame::Input { bytes, .. } => ADDRESS_BYTES + bytes.len(),
        };
        1 + body as u64
    }

    /// Appends the frame's wire bytes to `out`. A frame over the limit is
    /// refused here, so a sender never writes one the peer must reject.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), FrameError> {
        let len = self.len();
        let wire = u32::try_from(len)
            .ok()
            .filter(|len| *len <= MAX_FRAME_LEN)
            .ok_or(FrameError::TooLarge { len })?;
        out.reserve(LEN_BYTES + wire as usize);
        out.extend_from_slice(&wire.to_be_bytes());
        out.push(self.kind());
        match self {
            Frame::Message(text) => out.extend_from_slice(text.as_bytes()),
            Frame::Output {
                stream: address,
                bytes,
            }
            | Frame::Input {
                num: address,
                bytes,
            } => {
                out.extend_from_slice(&address.to_be_bytes());
                out.extend_from_slice(bytes);
            }
        }
        Ok(())
    }

    /// The frame's wire bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, FrameError> {
        let mut out = Vec::new();
        self.encode(&mut out)?;
        Ok(out)
    }

    /// Builds a frame from its kind and body, as read off the wire. The
    /// body becomes the frame's bytes in place, with no copy.
    fn from_parts(kind: u8, mut body: Vec<u8>) -> Result<Self, FrameError> {
        match kind {
            KIND_MESSAGE => Ok(Frame::Message(String::from_utf8(body)?)),
            KIND_OUTPUT | KIND_INPUT => {
                let Some(address) = body.first_chunk::<ADDRESS_BYTES>() else {
                    return Err(FrameError::NoAddress {
                        kind,
                        len: body.len(),
                    });
                };
                let address = u32::from_be_bytes(*address);
                body.drain(..ADDRESS_BYTES);
                let bytes = body;
                Ok(if kind == KIND_OUTPUT {
                    Frame::Output {
                        stream: address,
                        bytes,
                    }
                } else {
                    Frame::Input {
                        num: address,
                        bytes,
                    }
                })
            }
            other => Err(FrameError::UnknownKind(other)),
        }
    }
}

/// Checks a frame's `len` prefix.
fn check_len(len: u32) -> Result<usize, FrameError> {
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { len: len.into() });
    }
    Ok(len as usize)
}

/// Checks a frame's kind before its body is read.
fn check_kind(kind: u8) -> Result<u8, FrameError> {
    match kind {
        KIND_MESSAGE | KIND_OUTPUT | KIND_INPUT => Ok(kind),
        other => Err(FrameError::UnknownKind(other)),
    }
}

/// Writes one frame and flushes it.
pub fn write_frame<W: Write>(mut out: W, frame: &Frame) -> Result<(), FrameError> {
    out.write_all(&frame.to_bytes()?)?;
    out.flush()?;
    Ok(())
}

/// Reads one frame. `Ok(None)` is a clean end of the stream between frames;
/// an end inside a frame is [`FrameError::Truncated`]. A bad `len` or kind
/// is rejected before the body is read, and the body's buffer grows as its
/// bytes arrive, so a bare `len` prefix never claims 16 MiB.
pub fn read_frame<R: Read>(mut input: R) -> Result<Option<Frame>, FrameError> {
    let mut head = [0u8; LEN_BYTES + 1];
    let got = fill(&mut input, &mut head)?;
    if got == 0 {
        return Ok(None);
    }
    if got < LEN_BYTES {
        return Err(FrameError::Truncated { buffered: got });
    }
    let len = check_len(u32::from_be_bytes([head[0], head[1], head[2], head[3]]))?;
    if got < head.len() {
        return Err(FrameError::Truncated { buffered: got });
    }
    let kind = check_kind(head[LEN_BYTES])?;
    let want = len - 1;
    let mut body = Vec::new();
    input.by_ref().take(want as u64).read_to_end(&mut body)?;
    if body.len() < want {
        return Err(FrameError::Truncated {
            buffered: head.len() + body.len(),
        });
    }
    Frame::from_parts(kind, body).map(Some)
}

/// Reads until `buf` is full or the stream ends, and says how much it read.
fn fill<R: Read>(input: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match input.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(got)
}

/// Frames from bytes that arrive in pieces of any size.
///
/// Push what a read returned, then take frames until [`Decoder::next_frame`]
/// gives `None`. An error means the stream is broken: the connection ends.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Where the next frame starts in `buf`. Frames taken before it are
    /// dropped on the next push, in one move.
    start: usize,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame, or `None` until more bytes arrive. A bad `len`
    /// is rejected as soon as its 4 bytes are in, before the body arrives.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        let pending = &self.buf[self.start..];
        let Some(prefix) = pending.first_chunk::<LEN_BYTES>() else {
            return Ok(None);
        };
        let len = check_len(u32::from_be_bytes(*prefix))?;
        let Some(&kind) = pending.get(LEN_BYTES) else {
            return Ok(None);
        };
        let kind = check_kind(kind)?;
        let Some(body) = pending.get(LEN_BYTES + 1..LEN_BYTES + len) else {
            return Ok(None);
        };
        let body = body.to_vec();
        self.start += LEN_BYTES + len;
        Frame::from_parts(kind, body).map(Some)
    }

    /// Bytes held for a frame not yet whole. Nonzero at the end of the
    /// stream means it ended inside a frame.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.start
    }

    /// Checks the end of the stream: an error when it ended inside a frame.
    pub fn finish(&self) -> Result<(), FrameError> {
        match self.buffered() {
            0 => Ok(()),
            buffered => Err(FrameError::Truncated { buffered }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A reader that returns at most `step` bytes per read.
    struct Trickle<'a> {
        bytes: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.step.min(buf.len()).min(self.bytes.len());
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
    }

    fn samples() -> Vec<Frame> {
        vec![
            Frame::Message(r#"{"type":"ping","id":1}"#.into()),
            Frame::Message(String::new()),
            Frame::Message("{\"text\":\"é ✓\"}".into()),
            Frame::Output {
                stream: 7,
                bytes: b"\x1b[2J\x1b[Hhello".to_vec(),
            },
            Frame::Output {
                stream: u32::MAX,
                bytes: Vec::new(),
            },
            Frame::Input {
                num: 0,
                bytes: vec![0, 0xff, 3],
            },
            Frame::Input {
                num: 42,
                bytes: Vec::new(),
            },
        ]
    }

    fn decode_all(wire: &[u8]) -> Vec<Frame> {
        let mut decoder = Decoder::new();
        decoder.push(wire);
        let mut frames = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            frames.push(frame);
        }
        decoder.finish().unwrap();
        frames
    }

    #[test]
    fn every_kind_round_trips() {
        for frame in samples() {
            let wire = frame.to_bytes().unwrap();
            assert_eq!(decode_all(&wire), vec![frame.clone()]);
            assert_eq!(read_frame(&wire[..]).unwrap(), Some(frame));
        }
    }

    #[test]
    fn the_wire_layout_is_len_kind_body() {
        let wire = Frame::Output {
            stream: 0x0102_0304,
            bytes: b"ab".to_vec(),
        }
        .to_bytes()
        .unwrap();
        assert_eq!(wire, [0, 0, 0, 7, 1, 1, 2, 3, 4, b'a', b'b']);
        let wire = Frame::Message("{}".into()).to_bytes().unwrap();
        assert_eq!(wire, [0, 0, 0, 3, 0, b'{', b'}']);
        let wire = Frame::Input {
            num: 9,
            bytes: vec![b'x'],
        }
        .to_bytes()
        .unwrap();
        assert_eq!(wire, [0, 0, 0, 6, 2, 0, 0, 0, 9, b'x']);
    }

    #[test]
    fn a_frame_split_across_reads_decodes_whole() {
        let mut wire = Vec::new();
        for frame in samples() {
            frame.encode(&mut wire).unwrap();
        }
        // One byte at a time through the decoder.
        let mut decoder = Decoder::new();
        let mut frames = Vec::new();
        for byte in &wire {
            decoder.push(std::slice::from_ref(byte));
            while let Some(frame) = decoder.next_frame().unwrap() {
                frames.push(frame);
            }
        }
        decoder.finish().unwrap();
        assert_eq!(frames, samples());
        // And through a reader that returns one or three bytes per read.
        for step in [1, 3] {
            let mut input = Trickle { bytes: &wire, step };
            let mut frames = Vec::new();
            while let Some(frame) = read_frame(&mut input).unwrap() {
                frames.push(frame);
            }
            assert_eq!(frames, samples(), "step {step}");
        }
    }

    #[test]
    fn a_frame_at_the_limit_passes_and_one_over_it_is_rejected() {
        // `len` counts the kind byte, so the largest message body is one less.
        let body = "x".repeat(MAX_FRAME_LEN as usize - 1);
        let at_limit = Frame::Message(body);
        let wire = at_limit.to_bytes().unwrap();
        assert_eq!(wire[..4], MAX_FRAME_LEN.to_be_bytes());
        assert_eq!(read_frame(&wire[..]).unwrap(), Some(at_limit));

        let bytes = vec![7u8; MAX_FRAME_LEN as usize - 1 - 4];
        let at_limit = Frame::Output { stream: 1, bytes };
        assert_eq!(decode_all(&at_limit.to_bytes().unwrap()), vec![at_limit]);

        let over = Frame::Input {
            num: 1,
            bytes: vec![0; MAX_FRAME_LEN as usize - 4],
        };
        assert!(matches!(
            over.to_bytes(),
            Err(FrameError::TooLarge { len }) if len == u64::from(MAX_FRAME_LEN) + 1
        ));

        // A peer that sends a `len` over the limit is rejected at once, before
        // its body arrives.
        let mut decoder = Decoder::new();
        decoder.push(&(MAX_FRAME_LEN + 1).to_be_bytes());
        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::TooLarge { .. })
        ));
        let prefix = u32::MAX.to_be_bytes();
        assert!(matches!(
            read_frame(&prefix[..]),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn the_smallest_frames_decode() {
        // `len` 1: a kind byte and an empty JSON body.
        assert_eq!(
            read_frame(&[0, 0, 0, 1, 0][..]).unwrap(),
            Some(Frame::Message(String::new()))
        );
        // `len` 5: a kind 1 frame with its address and no bytes.
        assert_eq!(
            read_frame(&[0, 0, 0, 5, 1, 0, 0, 0, 3][..]).unwrap(),
            Some(Frame::Output {
                stream: 3,
                bytes: Vec::new()
            })
        );
    }

    #[test]
    fn malformed_frames_are_rejected() {
        type Check = fn(&FrameError) -> bool;
        let cases: [(&[u8], Check); 5] = [
            // `len` 0: no kind byte.
            (&[0, 0, 0, 0], |e| matches!(e, FrameError::Empty)),
            (&[0, 0, 0, 1, 3], |e| {
                matches!(e, FrameError::UnknownKind(3))
            }),
            (&[0, 0, 0, 4, 1, 0, 0, 0], |e| {
                matches!(e, FrameError::NoAddress { kind: 1, len: 3 })
            }),
            (&[0, 0, 0, 1, 2], |e| {
                matches!(e, FrameError::NoAddress { kind: 2, len: 0 })
            }),
            (&[0, 0, 0, 2, 0, 0xff], |e| {
                matches!(e, FrameError::NotUtf8(_))
            }),
        ];
        for (wire, expected) in cases {
            let err = read_frame(wire).unwrap_err();
            assert!(expected(&err), "{wire:?}: {err}");
            let mut decoder = Decoder::new();
            decoder.push(wire);
            let err = decoder.next_frame().unwrap_err();
            assert!(expected(&err), "{wire:?}: {err}");
        }
        // An unknown kind is refused before its body arrives.
        let head = [0, 0, 0, 200, 9];
        assert!(matches!(
            read_frame(&head[..]),
            Err(FrameError::UnknownKind(9))
        ));
        let mut decoder = Decoder::new();
        decoder.push(&head);
        assert!(matches!(
            decoder.next_frame(),
            Err(FrameError::UnknownKind(9))
        ));
    }

    #[test]
    fn one_push_of_many_frames_gives_each_in_order() {
        let frames: Vec<Frame> = (0..1000)
            .map(|n| Frame::Output {
                stream: n,
                bytes: vec![n as u8; (n % 7) as usize],
            })
            .collect();
        let mut wire = Vec::new();
        for frame in &frames {
            frame.encode(&mut wire).unwrap();
        }
        let mut decoder = Decoder::new();
        decoder.push(&wire[..wire.len() - 1]);
        let mut got = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            got.push(frame);
        }
        assert_eq!(got.len(), frames.len() - 1);
        decoder.push(&wire[wire.len() - 1..]);
        got.extend(decoder.next_frame().unwrap());
        assert_eq!(got, frames);
        assert_eq!(decoder.buffered(), 0);
        decoder.finish().unwrap();
    }

    #[test]
    fn a_stream_that_ends_inside_a_frame_is_truncated() {
        let wire = Frame::Message("{}".into()).to_bytes().unwrap();
        assert!(read_frame(&[][..]).unwrap().is_none());
        for cut in 1..wire.len() {
            assert!(
                matches!(read_frame(&wire[..cut]), Err(FrameError::Truncated { buffered }) if buffered == cut),
                "cut at {cut}"
            );
            let mut decoder = Decoder::new();
            decoder.push(&wire[..cut]);
            assert!(decoder.next_frame().unwrap().is_none());
            assert_eq!(decoder.buffered(), cut);
            assert!(matches!(
                decoder.finish(),
                Err(FrameError::Truncated { .. })
            ));
        }
    }

    fn any_frame() -> impl Strategy<Value = Frame> {
        let bytes = proptest::collection::vec(any::<u8>(), 0..512);
        prop_oneof![
            ".{0,128}".prop_map(Frame::Message),
            (any::<u32>(), bytes.clone())
                .prop_map(|(stream, bytes)| Frame::Output { stream, bytes }),
            (any::<u32>(), bytes).prop_map(|(num, bytes)| Frame::Input { num, bytes }),
        ]
    }

    proptest! {
        #[test]
        fn frames_round_trip_in_any_split(
            frames in proptest::collection::vec(any_frame(), 0..8),
            cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..16),
        ) {
            let mut wire = Vec::new();
            for frame in &frames {
                frame.encode(&mut wire).unwrap();
            }
            let mut cuts: Vec<usize> = cuts.iter().map(|cut| cut.index(wire.len() + 1)).collect();
            cuts.push(wire.len());
            cuts.sort_unstable();
            let mut decoder = Decoder::new();
            let mut decoded = Vec::new();
            let mut from = 0;
            for cut in cuts {
                decoder.push(&wire[from..cut]);
                from = cut;
                while let Some(frame) = decoder.next_frame().unwrap() {
                    decoded.push(frame);
                }
            }
            decoder.finish().unwrap();
            prop_assert_eq!(&decoded, &frames);

            let mut input = &wire[..];
            let mut read = Vec::new();
            while let Some(frame) = read_frame(&mut input).unwrap() {
                read.push(frame);
            }
            prop_assert_eq!(read, frames);
        }
    }
}
