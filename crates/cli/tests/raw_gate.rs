//! The raw stream gate (`terminal-holder.md` 9.1, 12): each recording of real
//! Claude Code output in `crates/panes/tests/fixtures/claude-raw/` is fed to
//! the holder's screen model in the order the PTY produced it, with the
//! recorded resizes in place. At each mark of its sidecar the screen must give
//! the composer verdict and the prompt parse the sidecar names.
//!
//! The recordings and sidecars are the contract here. A mark sits on a frame a
//! person checked by eye, so a changed verdict is a regression in the model,
//! the composer reader or the prompt parser, never a snapshot to refresh.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use tiller_daemon::prompt::{Dialog, Wait, read};
use tiller_holder::model::Model;
use tiller_panes::Size;
use tiller_panes::composer::{Composer, classify};
use tiller_panes::message::{Format, Read};

/// The recordings the sidecars name, with the states each one is there for.
const RECORDINGS: [&str; 9] = [
    "edit-and-bash-dialogs",
    "idle-dwell",
    "queued-message",
    "resize",
    "short-and-long-reply",
    "sleep-then-essay",
    "task-list",
    "trust-and-popup",
    "write-dialog-and-sleep",
];

fn dir() -> PathBuf {
    PathBuf::from(format!(
        "{}/../panes/tests/fixtures/claude-raw",
        env!("CARGO_MANIFEST_DIR")
    ))
}

fn size(cols: &Value, rows: &Value) -> Size {
    Size {
        cols: u16::try_from(cols.as_u64().unwrap()).unwrap(),
        rows: u16::try_from(rows.as_u64().unwrap()).unwrap(),
    }
}

fn offset(value: &Value) -> usize {
    usize::try_from(value["offset"].as_u64().unwrap()).unwrap()
}

/// The composer verdict as the sidecar writes it.
fn verdict(composer: &Composer) -> Value {
    match composer {
        Composer::Empty => "Empty".into(),
        Composer::Busy => "Busy".into(),
        Composer::Dialog => "Dialog".into(),
        Composer::Unknown => "Unknown".into(),
        Composer::Holds(text) => serde_json::json!({ "Holds": text }),
    }
}

/// The prompt reads at a screen: the tool or `input` that gave a dialog, with
/// the dialog as JSON and without its screen hash. None when nothing reads.
fn prompt(screen: &str) -> Value {
    let mut found = Value::Null;
    let mut keep = |wait: &str, dialog: Dialog| {
        let mut json = serde_json::to_value(&dialog.prompt).unwrap();
        json.as_object_mut().unwrap().remove("screen");
        found = serde_json::json!({ "wait": wait, "dialog": json });
    };
    for tool in ["Bash", "Write", "Edit"] {
        if let Some(dialog) = read(screen, &Wait::Permission(tool.to_owned())) {
            keep(tool, dialog);
        }
    }
    if let Some(dialog) = read(screen, &Wait::Input) {
        keep("input", dialog);
    }
    found
}

/// Replays one recording and checks every mark. Returns the verdicts and
/// prompt waits it saw, for the coverage test.
fn replay(name: &str) -> Seen {
    let side: Value =
        serde_json::from_slice(&std::fs::read(dir().join(format!("{name}.json"))).unwrap())
            .unwrap();
    let raw = std::fs::read(dir().join(side["raw"].as_str().unwrap())).unwrap();
    let mut model = Model::new(size(&side["cols"], &side["rows"]));
    let mut resizes = side["resizes"].as_array().unwrap().iter().peekable();
    let mut seen = Seen {
        resizes: side["resizes"].as_array().unwrap().len(),
        ..Seen::default()
    };
    let mut pos = 0;
    let mut last = 0;
    for mark in side["marks"].as_array().unwrap() {
        let at = offset(mark);
        let note = mark["note"].as_str().unwrap();
        assert!(at >= last, "{name} / {note}: marks are not in order");
        assert!(at <= raw.len(), "{name} / {note}: the mark is past the end");
        last = at;
        // A resize at the mark's offset happens before the mark is read.
        while let Some(resize) = resizes.next_if(|r| offset(r) <= at) {
            model.feed(&raw[pos..offset(resize)]).unwrap();
            pos = offset(resize);
            model
                .resize(size(&resize["cols"], &resize["rows"]))
                .unwrap();
        }
        model.feed(&raw[pos..at]).unwrap();
        pos = at;
        let screen = model.screen(Read::Visible, Format::Ansi).unwrap().text();

        let got = verdict(&classify(&screen));
        assert_eq!(
            got, mark["composer"],
            "{name} / {note} (offset {at}): composer"
        );
        let got = prompt(&screen);
        assert_eq!(got, mark["prompt"], "{name} / {note} (offset {at}): prompt");

        seen.composers
            .insert(mark["composer"].as_str().unwrap_or("Holds").to_owned());
        if let Some(wait) = got["wait"].as_str() {
            seen.waits.insert(wait.to_owned());
        }
        if mark["composer"] == "Busy" && note.contains("task list") {
            seen.task_list_busy = true;
        }
        if note.contains("queued") && mark["composer"] == "Busy" {
            seen.queued_busy = true;
        }
    }
    seen
}

#[derive(Default)]
struct Seen {
    composers: BTreeSet<String>,
    waits: BTreeSet<String>,
    resizes: usize,
    task_list_busy: bool,
    queued_busy: bool,
}

macro_rules! gate {
    ($($test:ident => $name:literal),* $(,)?) => {$(
        #[test]
        fn $test() {
            replay($name);
        }
    )*};
}

gate! {
    edit_and_bash_dialogs => "edit-and-bash-dialogs",
    idle_dwell => "idle-dwell",
    queued_message => "queued-message",
    resize => "resize",
    short_and_long_reply => "short-and-long-reply",
    sleep_then_essay => "sleep-then-essay",
    task_list => "task-list",
    trust_and_popup => "trust-and-popup",
    write_dialog_and_sleep => "write-dialog-and-sleep",
}

/// A recording left out of the gate, or a gate with no recording, fails here.
#[test]
fn every_recording_has_a_gate_and_the_other_way_round() {
    let mut on_disk: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter_map(|file| file.strip_suffix(".raw").map(str::to_owned))
        .collect();
    on_disk.sort();
    assert_eq!(on_disk, RECORDINGS);
    for name in RECORDINGS {
        assert!(
            dir().join(format!("{name}.json")).is_file(),
            "{name}: no sidecar"
        );
    }
}

/// The recordings together reach every state the gate is there to hold.
#[test]
fn the_recordings_cover_the_states_the_gate_is_for() {
    let mut composers = BTreeSet::new();
    let mut waits = BTreeSet::new();
    let mut resizes = 0;
    let (mut task_list_busy, mut queued_busy) = (false, false);
    for name in RECORDINGS {
        let seen = replay(name);
        composers.extend(seen.composers);
        waits.extend(seen.waits);
        resizes += seen.resizes;
        task_list_busy |= seen.task_list_busy;
        queued_busy |= seen.queued_busy;
    }
    let want = |items: &[&str]| {
        items
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        composers,
        want(&["Busy", "Dialog", "Empty", "Holds", "Unknown"])
    );
    // Bash, Write and Edit permission dialogs, and the trust dialog.
    assert_eq!(waits, want(&["Bash", "Edit", "Write", "input"]));
    assert!(resizes >= 3, "the recordings have {resizes} resizes");
    assert!(task_list_busy, "no Busy mark over a task list");
    assert!(queued_busy, "no Busy mark over a queued message");
}
