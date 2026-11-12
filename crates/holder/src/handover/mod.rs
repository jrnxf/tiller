//! The live upgrade's handover record (`terminal-holder.md` 6.2): what the
//! old program image hands the new one across `execve`, in a byte form of
//! the holder's own.
//!
//! The record starts with [`MAGIC`] and its version, and ends with a
//! checksum of everything before it. Each pane goes in as one
//! length-prefixed part with a checksum of its own, so a reader can tell
//! which pane a damaged record lost. Every length is checked against a
//! bound before anything is allocated: a record is read from a file
//! another program wrote.
//!
//! A new version of the record is a new [`VERSION`]. A build reads every
//! version from [`OLDEST`] to [`VERSION`], so an upgrade from the build
//! before, and a rollback to it, both work. The golden fixture test holds
//! version 1's bytes still.

pub mod codec;
pub mod term;

use std::os::fd::RawFd;
use std::time::Duration;

use jiff::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tiller_panes::Size;
use tiller_panes::message::{Exit, Pane};

use crate::input::{Queued, Refused};
use crate::model::{
    MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, ModelState, NOTIFY_MAX, TEXT_MAX, TITLE_STACK,
    TellerState, Told,
};
use crate::scanner::{HELD_MAX, ScannerState};
use codec::{Reader, Writer, fnv1a};

/// What every record starts with.
pub const MAGIC: &[u8; 16] = b"tiller-handover\0";
/// The record version this build writes.
pub const VERSION: u32 = 1;
/// The oldest record version this build reads.
pub const OLDEST: u32 = 1;
/// The largest record a build reads. A holder at every limit could need
/// more; one that does refuses the upgrade and keeps running.
pub const RECORD_MAX: usize = 1 << 30;

/// The most bytes of a pane's open synchronized update: `vte`'s buffer
/// holds less.
const SYNC_MAX: usize = 2 * 1024 * 1024;
/// The most bytes of a pane record or an exit as JSON.
const JSON_MAX: usize = 16 * 1024 * 1024;
/// The most input frames, and bytes in one, a pane's queue holds.
const INPUT_FRAMES_MAX: usize = 1 << 20;
const INPUT_FRAME_MAX: usize = 64 * 1024 * 1024;
/// The most panes in a record.
const PANES_MAX: usize = 1 << 16;
/// The most bytes of the instance, the boot id or a version.
const NAME_MAX: usize = 1024;

/// Why a record cannot be written or read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("the record ends early")]
    Short,
    #[error("the record has a bad {0}")]
    Bad(&'static str),
    #[error("the record's {0} is over its limit")]
    Limit(&'static str),
    #[error("handover version {0} is not one this build reads ({OLDEST} to {VERSION})")]
    Version(u32),
    #[error("pane {0} does not match its checksum")]
    Checksum(usize),
    #[error("the record does not match its checksum")]
    RecordChecksum,
    /// A state the record cannot hold, or one that does not read back as
    /// it was written.
    #[error("{0}")]
    Unfit(String),
}

/// Everything the old program image hands over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub instance: String,
    pub boot_id: String,
    pub started_at: Timestamp,
    /// The `num` the next spawn takes.
    pub next_num: u32,
    /// The listening socket's fd.
    pub listener: RawFd,
    /// The `holder.lock` fd.
    pub lock: RawFd,
    /// The connection that asked for the upgrade, which the new image
    /// answers.
    pub requester: Option<Requester>,
    pub panes: Vec<PaneRecord>,
}

/// The connection that asked for the upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requester {
    /// A copy of the connection's socket fd.
    pub fd: RawFd,
    /// The `upgrade` request's id.
    pub id: u64,
}

/// One pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRecord {
    /// The pane as `list` shows it, with no subscribers or attach
    /// clients: every connection ends at the upgrade.
    pub pane: Pane,
    /// The PTY master's fd.
    pub master: RawFd,
    /// Every holder of the PTY's slave has closed it: the master reads no
    /// more.
    pub output_ended: bool,
    /// The child was reaped, and its exit is not in `pane` yet: it waits
    /// for the last output.
    pub pending_exit: Option<Exit>,
    /// A close's grace left: what is left of the child's group then is
    /// killed.
    pub close_in: Option<Duration>,
    pub input: Queued,
    pub model: ModelState,
}

impl Record {
    /// The record's bytes, or [`RecordError::Limit`] when they are more
    /// than a build reads.
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        let mut w = Writer::new();
        w.raw(MAGIC);
        w.uvar(u64::from(VERSION));
        w.str(&self.instance);
        w.str(&self.boot_id);
        w.str(&self.started_at.to_string());
        w.uvar(u64::from(self.next_num));
        w.ivar(i64::from(self.listener));
        w.ivar(i64::from(self.lock));
        w.option(self.requester, |w, requester| {
            w.ivar(i64::from(requester.fd));
            w.uvar(requester.id);
        });
        w.usize(self.panes.len());
        for pane in &self.panes {
            let bytes = pane.encode();
            w.bytes(&bytes);
            w.raw(&fnv1a(&bytes).to_le_bytes());
        }
        let mut out = w.into_bytes();
        let sum = fnv1a(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        if out.len() > RECORD_MAX {
            return Err(RecordError::Limit("size"));
        }
        Ok(out)
    }

    /// A record from its bytes. Each pane's model state is read, not
    /// rebuilt: [`crate::model::Model::import`] rebuilds it.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        if bytes.len() > RECORD_MAX {
            return Err(RecordError::Limit("size"));
        }
        let (body, sum) = bytes.split_last_chunk::<8>().ok_or(RecordError::Short)?;
        let mut r = Reader::new(body);
        if r.raw(MAGIC.len())? != MAGIC {
            return Err(RecordError::Bad("start"));
        }
        let version = u32::try_from(r.uvar()?).map_err(|_| RecordError::Bad("version"))?;
        if !(OLDEST..=VERSION).contains(&version) {
            return Err(RecordError::Version(version));
        }
        if fnv1a(body) != u64::from_le_bytes(*sum) {
            return Err(RecordError::RecordChecksum);
        }
        let instance = r.str(NAME_MAX, "instance")?;
        let boot_id = r.str(NAME_MAX, "boot id")?;
        let started_at = r
            .str(NAME_MAX, "start time")?
            .parse()
            .map_err(|_| RecordError::Bad("start time"))?;
        let next_num = get_u32(&mut r, "next num")?;
        let listener = get_fd(&mut r)?;
        let lock = get_fd(&mut r)?;
        let requester = r.option(|r| {
            Ok(Requester {
                fd: get_fd(r)?,
                id: r.uvar()?,
            })
        })?;
        let count = r.len(PANES_MAX, "pane count")?;
        let mut panes = Vec::new();
        for index in 0..count {
            let part = r.bytes(RECORD_MAX, "pane")?;
            let sum = r.raw(8)?;
            if fnv1a(&part).to_le_bytes() != sum {
                return Err(RecordError::Checksum(index));
            }
            panes.push(PaneRecord::decode(&part)?);
        }
        if !r.done() {
            return Err(RecordError::Bad("end"));
        }
        Ok(Self {
            instance,
            boot_id,
            started_at,
            next_num,
            listener,
            lock,
            requester,
            panes,
        })
    }
}

impl PaneRecord {
    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        put_json(&mut w, &self.pane);
        w.ivar(i64::from(self.master));
        w.bool(self.output_ended);
        w.option(self.pending_exit.as_ref(), put_json);
        w.option(self.close_in, |w, left| {
            w.uvar(u64::try_from(left.as_millis()).unwrap_or(u64::MAX));
        });
        put_input(&mut w, &self.input);
        put_model(&mut w, &self.model);
        w.into_bytes()
    }

    fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut r = Reader::new(bytes);
        let pane: Pane = get_json(&mut r, "pane")?;
        let master = get_fd(&mut r)?;
        let output_ended = r.bool()?;
        let pending_exit = r.option(|r| get_json(r, "exit"))?;
        let close_in = r.option(|r| Ok(Duration::from_millis(r.uvar()?)))?;
        let input = get_input(&mut r)?;
        let model = get_model(&mut r)?;
        if !r.done() {
            return Err(RecordError::Bad("pane end"));
        }
        if (pane.cols, pane.rows) != (model.size.cols, model.size.rows) {
            return Err(RecordError::Bad("pane size"));
        }
        Ok(Self {
            pane,
            master,
            output_ended,
            pending_exit,
            close_in,
            input,
            model,
        })
    }
}

fn put_json<T: Serialize>(w: &mut Writer, value: &T) {
    // The types written here are plain records with string keys, which
    // always serialize.
    w.str(&serde_json::to_string(value).unwrap_or_default());
}

fn get_json<T: DeserializeOwned>(r: &mut Reader<'_>, what: &'static str) -> Result<T, RecordError> {
    serde_json::from_str(&r.str(JSON_MAX, what)?).map_err(|_| RecordError::Bad(what))
}

fn get_u32(r: &mut Reader<'_>, what: &'static str) -> Result<u32, RecordError> {
    u32::try_from(r.uvar()?).map_err(|_| RecordError::Bad(what))
}

fn get_fd(r: &mut Reader<'_>) -> Result<RawFd, RecordError> {
    RawFd::try_from(r.ivar()?)
        .ok()
        .filter(|fd| *fd >= 0)
        .ok_or(RecordError::Bad("fd"))
}

fn put_input(w: &mut Writer, input: &Queued) {
    w.usize(input.chunks.len());
    for chunk in &input.chunks {
        w.bytes(chunk);
    }
    w.u8(match input.closed {
        None => 0,
        Some(Refused::Closed) => 1,
        // A full client is cut, not the queue: it never closes with it.
        Some(Refused::Broken | Refused::Full(_)) => 2,
    });
}

fn get_input(r: &mut Reader<'_>) -> Result<Queued, RecordError> {
    let count = r.len(INPUT_FRAMES_MAX, "input frames")?;
    let mut chunks = Vec::new();
    for _ in 0..count {
        chunks.push(r.bytes(INPUT_FRAME_MAX, "input frame")?);
    }
    let closed = match r.u8()? {
        0 => None,
        1 => Some(Refused::Closed),
        2 => Some(Refused::Broken),
        _ => return Err(RecordError::Bad("input state")),
    };
    Ok(Queued { chunks, closed })
}

/// A pane's model state: its size first, which bounds the grids.
pub fn put_model(w: &mut Writer, model: &ModelState) {
    w.uvar(u64::from(model.size.cols));
    w.uvar(u64::from(model.size.rows));
    term::put(w, &model.term);
    w.option(model.preceding, Writer::char);
    w.option(model.sync.as_deref(), Writer::bytes);
    let teller = &model.teller;
    w.str(&teller.title);
    w.usize(teller.stack.len());
    for title in &teller.stack {
        w.str(title);
    }
    w.usize(teller.told.len());
    for told in &teller.told {
        match told {
            Told::Title(title) => {
                w.u8(0);
                w.str(title);
            }
            Told::Bell => w.u8(1),
            Told::Notify { title, body } => {
                w.u8(2);
                w.str(title);
                w.str(body);
            }
        }
    }
    w.option(teller.last, Writer::char);
    let scanner = &model.scanner;
    w.u8(scanner.state);
    w.bytes(&scanner.held);
    w.bytes(&scanner.intro);
    w.bool(scanner.overflowed);
}

pub fn get_model(r: &mut Reader<'_>) -> Result<ModelState, RecordError> {
    let cols = u16::try_from(r.uvar()?).map_err(|_| RecordError::Bad("columns"))?;
    let rows = u16::try_from(r.uvar()?).map_err(|_| RecordError::Bad("rows"))?;
    if !(MIN_COLS..=MAX_COLS).contains(&cols) || !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
        return Err(RecordError::Bad("a screen size"));
    }
    let term = term::get(r, usize::from(rows), usize::from(cols))?;
    let preceding = r.option(Reader::char)?;
    let sync = r.option(|r| r.bytes(SYNC_MAX, "synchronized update"))?;
    let title = r.str(TEXT_MAX, "title")?;
    let count = r.len(TITLE_STACK, "title stack")?;
    let mut stack = Vec::new();
    for _ in 0..count {
        stack.push(r.str(TEXT_MAX, "title")?);
    }
    // A title, a bell and the notifications.
    let count = r.len(NOTIFY_MAX + 2, "told")?;
    let mut told = Vec::new();
    for _ in 0..count {
        told.push(match r.u8()? {
            0 => Told::Title(r.str(TEXT_MAX, "title")?),
            1 => Told::Bell,
            2 => Told::Notify {
                title: r.str(TEXT_MAX, "notification")?,
                body: r.str(TEXT_MAX, "notification")?,
            },
            _ => return Err(RecordError::Bad("told")),
        });
    }
    let last = r.option(Reader::char)?;
    let scanner = ScannerState {
        state: r.u8()?,
        held: r.bytes(HELD_MAX, "held tail")?,
        intro: r.bytes(HELD_MAX, "held tail")?,
        overflowed: r.bool()?,
    };
    Ok(ModelState {
        size: Size { cols, rows },
        term,
        preceding,
        sync,
        teller: TellerState {
            title,
            stack,
            told,
            last,
        },
        scanner,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tiller_panes::message::{PaneId, PaneState};

    use super::*;
    use crate::model::Model;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn model(bytes: &[u8]) -> ModelState {
        let mut model = Model::new(Size { cols: 20, rows: 4 });
        model.feed(bytes).unwrap();
        model.export().unwrap()
    }

    fn pane(num: u32, state: PaneState, exit: Option<Exit>) -> Pane {
        Pane {
            pane: PaneId::from(format!("p{num}")),
            num,
            key: format!("key-{num}"),
            pid: 4000 + num,
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            cwd: "/home/user".into(),
            started_at: at("2026-01-02T03:04:05Z"),
            state,
            exit,
            cols: 20,
            rows: 4,
            meta: [("role".to_owned(), "builder".to_owned())].into(),
            title: "shell".into(),
            last_output_at: Some(at("2026-01-02T03:04:06Z")),
            attach_clients: 0,
            subscribers: 0,
        }
    }

    /// A record with a part of every kind: a running pane in an open
    /// synchronized update with input waiting, and one whose child ended
    /// while it closed.
    fn record() -> Record {
        let exit = Exit {
            code: None,
            signal: Some(15),
            at: at("2026-01-02T03:05:00Z"),
        };
        Record {
            instance: "default".into(),
            boot_id: "boot-1".into(),
            started_at: at("2026-01-02T03:00:00Z"),
            next_num: 3,
            listener: 5,
            lock: 6,
            requester: Some(Requester { fd: 9, id: 42 }),
            panes: vec![
                PaneRecord {
                    pane: pane(1, PaneState::Running, None),
                    master: 7,
                    output_ended: false,
                    pending_exit: None,
                    close_in: None,
                    input: Queued {
                        chunks: vec![b"ls\r".to_vec(), vec![0, 0xff, 0x1b]],
                        closed: None,
                    },
                    model: model(
                        b"\x1b]2;title\x07\x1b[1;31mred\x1b[m\r\n\x1b]8;id=a;https://example.com\x1b\\x\x1b]8;;\x1b\\\x1b[?2026hheld\x1b[3",
                    ),
                },
                PaneRecord {
                    pane: pane(2, PaneState::Running, None),
                    master: 8,
                    output_ended: true,
                    pending_exit: Some(exit),
                    close_in: Some(Duration::from_millis(1500)),
                    input: Queued {
                        chunks: Vec::new(),
                        closed: Some(Refused::Closed),
                    },
                    model: model("\x1b[?1049h\x1b[>1ualt \u{65e5}\x1b\x07".as_bytes()),
                },
            ],
        }
    }

    #[test]
    fn a_record_reads_back_whole() {
        let record = record();
        let bytes = record.encode().unwrap();
        assert_eq!(Record::decode(&bytes).unwrap(), record);
        // Each model rebuilds from what was read.
        for pane in Record::decode(&bytes).unwrap().panes {
            assert_eq!(
                Model::import(&pane.model).unwrap().export().unwrap(),
                pane.model
            );
        }
    }

    /// `bytes` with its record checksum written again.
    fn summed(mut bytes: Vec<u8>) -> Vec<u8> {
        let end = bytes.len() - 8;
        let sum = fnv1a(&bytes[..end]);
        bytes[end..].copy_from_slice(&sum.to_le_bytes());
        bytes
    }

    #[test]
    fn a_damaged_or_foreign_record_is_refused() {
        let bytes = record().encode().unwrap();
        let mut flipped = bytes.clone();
        flipped[40] ^= 1;
        assert_eq!(Record::decode(&flipped), Err(RecordError::RecordChecksum));
        assert_eq!(
            Record::decode(&bytes[..bytes.len() - 1]),
            Err(RecordError::RecordChecksum)
        );
        assert_eq!(Record::decode(&bytes[..7]), Err(RecordError::Short));
        let mut foreign = bytes.clone();
        foreign[0] = b'x';
        assert_eq!(Record::decode(&foreign), Err(RecordError::Bad("start")));
        let mut newer = bytes.clone();
        newer[MAGIC.len()] = 2;
        assert_eq!(Record::decode(&summed(newer)), Err(RecordError::Version(2)));
        // A pane's own checksum names the pane.
        let mut pane = bytes.clone();
        let at = bytes.len() - 8 - 8 - 20;
        pane[at] ^= 1;
        assert_eq!(Record::decode(&summed(pane)), Err(RecordError::Checksum(1)));
        let mut longer = bytes[..bytes.len() - 8].to_vec();
        longer.push(0);
        longer.extend_from_slice(&[0; 8]);
        assert_eq!(
            Record::decode(&summed(longer)),
            Err(RecordError::Bad("end"))
        );
    }

    #[test]
    fn a_pane_whose_size_is_not_its_screens_is_refused() {
        let mut record = record();
        record.panes[0].pane.cols = 21;
        let bytes = record.encode().unwrap();
        assert_eq!(Record::decode(&bytes), Err(RecordError::Bad("pane size")));
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/handover-v1.bin")
    }

    /// Version 1's bytes stay the same, so a later build reads what this
    /// one writes. The version 1 file is never written again. A new
    /// version adds a fixture file of its own.
    #[test]
    fn version_1_reads_and_writes_the_bytes_it_always_has() {
        let bytes = record().encode().unwrap();
        let golden = std::fs::read(fixture()).unwrap();
        assert_eq!(Record::decode(&golden).unwrap(), record());
        assert_eq!(VERSION, 1, "a new version gets a fixture of its own");
        assert!(bytes == golden, "version 1 writes different bytes");
    }
}
