//! Reading a `tiller-import/v1` bundle (`docs/firstmate-export.md`).
//!
//! Only the three record files the importer writes from are parsed:
//! `projects.json`, `cards.jsonl` and `memory.jsonl`. Each is checked against
//! its SHA-256 in `manifest.json`, and its record count against the
//! manifest's `counts`, before anything is planned. Unknown fields are
//! ignored, so an exporter that adds one never breaks the import.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The only schema this importer reads.
pub const SCHEMA: &str = "tiller-import/v1";

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub mode: String,
    pub host_label: String,
    #[serde(default)]
    pub generated_at: Option<String>,
    /// Absent in v1 bundles; when present it must match `meta.boundary`.
    #[serde(default)]
    pub boundary: Option<String>,
    pub counts: Counts,
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Counts {
    pub projects: usize,
    pub cards: usize,
    pub inflight: usize,
    pub memory: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BundleProject {
    pub id: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    pub delivery: String,
    #[serde(default)]
    pub capacity: Option<i64>,
    /// The project's checks, in either shape of `projects.checks`. Optional:
    /// the exporter does not write it (Firstmate keeps no per-project check
    /// list), and a bundle edited by hand may.
    #[serde(default)]
    pub checks: Option<Value>,
    #[serde(default)]
    pub added: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BundleCard {
    pub id: String,
    pub project: String,
    pub title: String,
    pub column: String,
    pub kind: String,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub hold: Option<BundleHold>,
    #[serde(default)]
    pub pr: Option<String>,
    #[serde(default)]
    pub runs: Vec<Value>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub done: Option<BundleDone>,
    /// The brief, as lines (or one string from an older exporter).
    #[serde(default)]
    pub body: Option<Value>,
    /// An earlier Firstmate id, when the exporter recorded one.
    #[serde(default)]
    pub legacy_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BundleHold {
    pub kind: String,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BundleDone {
    pub outcome: String,
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct BundleMemory {
    pub id: String,
    pub text: String,
    pub scope: String,
    #[serde(default)]
    pub section: Option<String>,
    pub class: String,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub secret_ref: bool,
    #[serde(default)]
    pub source: Option<Value>,
}

/// One `inflight/<card-id>.json`: only what the plan reports.
#[derive(Clone, Debug, Deserialize)]
pub struct BundleInflight {
    pub id: String,
    #[serde(default)]
    pub recommendation: Option<String>,
}

/// A bundle read and verified against its manifest.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// SHA-256 of `manifest.json`: what `meta.import_manifest_sha256` holds.
    pub manifest_sha256: String,
    pub projects: Vec<BundleProject>,
    pub cards: Vec<BundleCard>,
    pub memory: Vec<BundleMemory>,
    pub inflight: Vec<BundleInflight>,
}

impl Bundle {
    /// Reads the bundle at `dir`, refusing a wrong schema, a record file whose
    /// hash differs from the manifest's, or a count that does not match.
    pub fn read(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let manifest_bytes = read_file(&dir, "manifest.json")?;
        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| refused(&dir, format!("manifest.json does not parse: {error}")))?;
        if manifest.schema != SCHEMA {
            return Err(refused(
                &dir,
                format!(
                    "schema is {:?}, this tiller reads {SCHEMA}",
                    manifest.schema
                ),
            ));
        }

        let projects: Vec<BundleProject> = {
            let bytes = verified(&dir, &manifest, "projects.json")?;
            serde_json::from_slice(&bytes)
                .map_err(|error| refused(&dir, format!("projects.json does not parse: {error}")))?
        };
        let cards: Vec<BundleCard> = jsonl(
            &dir,
            &verified(&dir, &manifest, "cards.jsonl")?,
            "cards.jsonl",
        )?;
        let memory: Vec<BundleMemory> = jsonl(
            &dir,
            &verified(&dir, &manifest, "memory.jsonl")?,
            "memory.jsonl",
        )?;
        let inflight = read_inflight(&dir, &manifest)?;

        let counts = &manifest.counts;
        for (what, manifest_count, found) in [
            ("projects", counts.projects, projects.len()),
            ("cards", counts.cards, cards.len()),
            ("memory", counts.memory, memory.len()),
            ("inflight", counts.inflight, inflight.len()),
        ] {
            if manifest_count != found {
                return Err(refused(
                    &dir,
                    format!(
                        "the manifest counts {manifest_count} {what}, the bundle holds {found}"
                    ),
                ));
            }
        }
        Ok(Self {
            dir,
            manifest,
            manifest_sha256,
            projects,
            cards,
            memory,
            inflight,
        })
    }
}

fn refused(dir: &Path, fact: impl Into<String>) -> Error {
    Error::refused("bundle", dir.display().to_string(), fact)
}

fn read_file(dir: &Path, name: &str) -> Result<Vec<u8>> {
    std::fs::read(dir.join(name))
        .map_err(|error| refused(dir, format!("cannot read {name}: {error}")))
}

/// Reads `name` and checks it against the manifest's hash for it.
fn verified(dir: &Path, manifest: &Manifest, name: &str) -> Result<Vec<u8>> {
    let expected = manifest
        .files
        .get(name)
        .ok_or_else(|| refused(dir, format!("the manifest lists no hash for {name}")))?;
    let bytes = read_file(dir, name)?;
    if !sha256_hex(&bytes).eq_ignore_ascii_case(expected) {
        return Err(refused(
            dir,
            format!("{name} differs from the manifest's sha256"),
        ));
    }
    Ok(bytes)
}

fn jsonl<T: DeserializeOwned>(dir: &Path, bytes: &[u8], name: &str) -> Result<Vec<T>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| refused(dir, format!("{name} is not UTF-8")))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(n, line)| {
            serde_json::from_str(line).map_err(|error| {
                refused(
                    dir,
                    format!("{name} line {} does not parse: {error}", n + 1),
                )
            })
        })
        .collect()
}

/// Every `inflight/<id>.json` the manifest lists, verified, in name order.
fn read_inflight(dir: &Path, manifest: &Manifest) -> Result<Vec<BundleInflight>> {
    manifest
        .files
        .keys()
        .filter(|name| {
            name.strip_prefix("inflight/")
                .is_some_and(|rest| !rest.contains('/') && rest.ends_with(".json"))
        })
        .map(|name| {
            let bytes = verified(dir, manifest, name)?;
            serde_json::from_slice(&bytes)
                .map_err(|error| refused(dir, format!("{name} does not parse: {error}")))
        })
        .collect()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
