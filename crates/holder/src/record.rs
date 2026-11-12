//! The fixture recorder (`terminal-holder.md` 12): behind the `record`
//! feature, which no release build turns on.
//!
//! For each pane the holder writes two files in the output directory:
//! - `<pane>.raw`: every byte the child wrote to its PTY, exactly as the PTY
//!   read it, before the holder cuts any query from it.
//! - `<pane>.jsonl`: one JSON object per line - `start` with the size, then
//!   `output` (time, offset and length of each read) and `resize` (time,
//!   offset and new size) as they happen, and `exit`. An offset counts bytes
//!   of the `.raw` file, so a resize sits between two reads.
//!
//! A write that fails is logged once and ends that pane's recording, so the
//! files never disagree on an offset. A failure never reaches the pane: a
//! recording never changes what a pane does.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::json;
use tiller_panes::{PaneId, Size};

/// Where a run's recordings go.
#[derive(Debug)]
pub struct Recorder {
    dir: PathBuf,
}

impl Recorder {
    /// Creates `dir` when it is missing.
    pub fn create(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    /// The files of a new pane. None, after a log line, when they cannot be
    /// created: the pane runs unrecorded.
    pub(crate) fn pane(&self, pane: &PaneId, size: Size) -> Option<PaneTap> {
        match PaneTap::create(&self.dir, pane, size) {
            Ok(tap) => Some(tap),
            Err(err) => {
                tracing::warn!(%pane, %err, "the pane's recording could not start");
                None
            }
        }
    }
}

/// One pane's two files.
#[derive(Debug)]
pub(crate) struct PaneTap {
    pane: String,
    started: Instant,
    files: Mutex<Files>,
    failed: AtomicBool,
}

#[derive(Debug)]
struct Files {
    raw: BufWriter<File>,
    events: BufWriter<File>,
    /// Bytes of the raw file so far.
    offset: u64,
}

impl PaneTap {
    fn create(dir: &Path, pane: &PaneId, size: Size) -> io::Result<Self> {
        // A pane id is a fresh ULID. `create_new` keeps a reused directory's
        // earlier recording from being truncated.
        let create = |ext: &str| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join(format!("{pane}.{ext}")))
        };
        let raw = create("raw")?;
        let events = create("jsonl")?;
        let tap = Self {
            pane: pane.to_string(),
            started: Instant::now(),
            files: Mutex::new(Files {
                raw: BufWriter::new(raw),
                events: BufWriter::new(events),
                offset: 0,
            }),
            failed: AtomicBool::new(false),
        };
        tap.event("start", |_| json!({"cols": size.cols, "rows": size.rows}));
        Ok(tap)
    }

    /// One PTY read.
    pub(crate) fn output(&self, bytes: &[u8]) {
        if self.failed.load(Ordering::Relaxed) {
            return;
        }
        let mut files = self.files();
        let result = files.raw.write_all(bytes).and_then(|()| {
            let line = self.line(
                "output",
                json!({"offset": files.offset, "len": bytes.len()}),
            );
            files.offset += bytes.len() as u64;
            writeln!(files.events, "{line}")?;
            // A crash of the holder keeps what was read.
            files.raw.flush()?;
            files.events.flush()
        });
        self.settle(result);
    }

    /// The PTY took a new size.
    pub(crate) fn resize(&self, size: Size) {
        self.event(
            "resize",
            |offset| json!({"offset": offset, "cols": size.cols, "rows": size.rows}),
        );
    }

    /// The child's exit has been recorded.
    pub(crate) fn exit(&self) {
        self.event("exit", |offset| json!({"offset": offset}));
    }

    fn event(&self, name: &str, fields: impl FnOnce(u64) -> serde_json::Value) {
        if self.failed.load(Ordering::Relaxed) {
            return;
        }
        let mut files = self.files();
        let line = self.line(name, fields(files.offset));
        let result = writeln!(files.events, "{line}").and_then(|()| files.events.flush());
        self.settle(result);
    }

    fn line(&self, name: &str, mut fields: serde_json::Value) -> String {
        if let Some(map) = fields.as_object_mut() {
            map.insert("event".to_owned(), json!(name));
            map.insert(
                "t_ms".to_owned(),
                json!(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            );
        }
        fields.to_string()
    }

    fn files(&self) -> std::sync::MutexGuard<'_, Files> {
        self.files
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn settle(&self, result: io::Result<()>) {
        if let Err(err) = result
            && !self.failed.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(pane = %self.pane, %err, "a recording write failed");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn offsets_count_the_raw_bytes_and_a_resize_sits_between_two_reads() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::create(&dir.path().join("out")).unwrap();
        let pane = PaneId::from("p1");
        let tap = recorder.pane(&pane, Size { cols: 80, rows: 24 }).unwrap();
        tap.output(b"abc");
        tap.resize(Size {
            cols: 100,
            rows: 30,
        });
        tap.output(b"de");
        tap.exit();

        let out = dir.path().join("out");
        assert_eq!(std::fs::read(out.join("p1.raw")).unwrap(), b"abcde");
        let events: Vec<serde_json::Value> = std::fs::read_to_string(out.join("p1.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let shape: Vec<_> = events
            .iter()
            .map(|e| (e["event"].as_str().unwrap(), e["offset"].as_u64()))
            .collect();
        assert_eq!(
            shape,
            [
                ("start", None),
                ("output", Some(0)),
                ("resize", Some(3)),
                ("output", Some(3)),
                ("exit", Some(5)),
            ]
        );
        assert_eq!(events[2]["cols"], 100);
        assert_eq!(events[3]["len"], 2);
    }
}
