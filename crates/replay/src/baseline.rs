//! The bundle's `baseline/replay.jsonl`: one owner ask per line, with what
//! Firstmate did about it (`docs/firstmate-export.md`). The per-card fields
//! (`kind`, `model`, `effort`) run parallel to `cards`; `handling` lists the
//! distinct ways the ask was handled.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct ReplayAsk {
    pub ask: String,
    pub ts: String,
    #[serde(default)]
    pub cards: Vec<String>,
    #[serde(default)]
    pub handling: Vec<String>,
    #[serde(default)]
    pub kind: Vec<Option<String>>,
    #[serde(default)]
    pub model: Vec<Option<String>>,
    #[serde(default)]
    pub effort: Vec<Option<String>>,
    #[serde(default)]
    pub holds: Vec<BaselineHold>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct BaselineHold {
    pub card: String,
    pub kind: String,
    pub until: Option<String>,
    pub reason: Option<String>,
}

impl ReplayAsk {
    /// What Firstmate did, without the ask's words: the part of a line a
    /// decision record may carry.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "ts": self.ts,
            "cards": self.cards,
            "handling": self.handling,
            "kind": self.kind,
            "model": self.model,
            "effort": self.effort,
            "holds": self.holds.iter().map(|h| serde_json::json!({
                "card": h.card,
                "kind": h.kind,
                "until": h.until,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Reads `replay.jsonl`, refusing a line that does not parse.
pub fn read(path: &Path) -> Result<Vec<ReplayAsk>, Error> {
    let text =
        fs::read_to_string(path).map_err(|e| Error::Input(format!("{}: {e}", path.display())))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| {
            serde_json::from_str(line)
                .map_err(|e| Error::Input(format!("{} line {}: {e}", path.display(), n + 1)))
        })
        .collect()
}
