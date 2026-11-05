//! The forge reconcile reads through (`recovery.md` R07, R08, R19): after a
//! daemon restart a PR merged in the meantime is seen as merged, through the
//! owner's `gh` login. A fake `gh` answers from files, like the real one
//! answers from GitHub.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../reconcile/tests/common/mod.rs"]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::*;
use tiller_agents::land::reconcile::{Local, ReconcileForge};
use tiller_agents::land::scrub::ScrubList;
use tiller_core::domain::{Checks, Mergeable, PipelineState, PrState, Project, ReviewDecision};
use tiller_core::state::get;
use tiller_reconcile::sources::{Forge, NoForge};

/// A `gh` that answers `gh api <path>` from files next to it and fails like
/// the real one when the file is missing.
struct FakeGh {
    dir: tempfile::TempDir,
}

impl FakeGh {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("gh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
dir=$(dirname "$0")
pick() { if [ -f "$dir/$1" ]; then cat "$dir/$1"; else echo "gh: Not Found (HTTP 404)" >&2; exit 1; fi; }
case "$*" in
  *pulls/7/files*) pick files.txt ;;
  *pulls/7/reviews*) pick reviews.json ;;
  *pulls/7) pick pull.json ;;
  *check-runs*) echo '[{"total_count":1,"check_runs":[{"status":"completed","conclusion":"success"}]}]' ;;
  */status*) echo '[{"statuses":[]}]' ;;
  *commits/main) pick head.json ;;
  *compare/*) pick compare.json ;;
  *) echo "gh: unexpected call: $*" >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let gh = Self { dir };
        gh.set("files.txt", "src/lib.rs\n");
        gh.set("reviews.json", "[[]]");
        gh
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("gh")
    }

    fn set(&self, name: &str, body: &str) {
        std::fs::write(self.dir.path().join(name), body).unwrap();
    }

    /// PR 7 as GitHub reports it.
    fn pull(&self, state: &str, merged: bool, extra: &str) {
        self.set(
            "pull.json",
            &format!(
                r#"{{"number":7,"state":"{state}","merged":{merged},"title":"t","body":"clean body",
                "head":{{"sha":"a1a1a1a1","ref":"fm/work"}},"base":{{"ref":"main","repo":{{"private":true}}}},
                "mergeable_state":"clean"{extra}}}"#
            ),
        );
    }
}

struct Cached {
    pipeline: PipelineState,
    terms: &'static str,
}

impl Cached {
    fn passed() -> Box<Self> {
        Box::new(Self {
            pipeline: PipelineState::Passed,
            terms: "",
        })
    }
}

impl Local for Cached {
    fn pipeline_state(&self, _repo: &str, _number: i64) -> PipelineState {
        self.pipeline
    }

    fn asks(&self, _repo: &str) -> Vec<String> {
        Vec::new()
    }

    fn scrub_list(&self) -> ScrubList {
        ScrubList::parse(self.terms)
    }
}

fn forge(gh: &FakeGh, local: Box<dyn Local>) -> ReconcileForge {
    ReconcileForge::new(gh.program().to_string_lossy(), local)
}

/// A world with PR 7 open in the cache, as a daemon left it before it went
/// down.
fn world_with_open_pr() -> (World, String) {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    (w, pr)
}

/// The bug: the daemon read the forge through `NoForge`, so after a restart a
/// PR merged while it was down stayed open and every refresh recorded "no
/// forge is configured".
#[test]
fn no_forge_leaves_a_merged_pr_open() {
    let (mut w, pr) = world_with_open_pr();
    w.use_forge(Box::new(NoForge));
    w.pass(now());
    let row = w.pr(&pr);
    assert_eq!(row.state, PrState::Open);
    assert_eq!(row.fetch_error.as_deref(), Some("no forge is configured"));
}

/// The fix: the same restart reads the merge from GitHub.
#[test]
fn a_pr_merged_while_the_daemon_was_down_is_seen_as_merged() {
    let (mut w, pr) = world_with_open_pr();
    let gh = FakeGh::new();
    gh.pull(
        "closed",
        true,
        r#","merged_at":"2026-11-05T10:00:00Z","merge_commit_sha":"e1e1e1e1","merged_by":{"login":"someone-else"}"#,
    );
    w.use_forge(Box::new(forge(&gh, Cached::passed())));
    let report = w.pass(now());
    let row = w.pr(&pr);
    assert_eq!(row.state, PrState::Merged, "{report:?}");
    assert_eq!(row.fetch_error, None);
    assert_eq!(row.merge_sha.as_deref(), Some("e1e1e1e1"));
    assert!(row.merged_at.is_some());
    assert!(report.did("R07"), "{report:?}");
    let notes = w.notes();
    assert!(
        notes
            .iter()
            .any(|n| n.body.contains("merged outside Tiller by someone-else")),
        "{notes:?}"
    );
}

#[test]
fn an_open_pr_is_read_with_its_checks_review_and_visibility() {
    let gh = FakeGh::new();
    gh.pull("open", false, "");
    gh.set(
        "reviews.json",
        r#"[[{"state":"CHANGES_REQUESTED","user":{"login":"a"}},{"state":"APPROVED","user":{"login":"a"}},{"state":"COMMENTED","user":{"login":"b"}}]]"#,
    );
    let read = forge(&gh, Cached::passed()).pr(REPO, 7).unwrap();
    assert_eq!(read.fetch.state, PrState::Open);
    assert_eq!(read.fetch.head_sha.as_deref(), Some("a1a1a1a1"));
    assert_eq!(read.fetch.base_ref.as_deref(), Some("main"));
    assert_eq!(read.fetch.checks, Checks::Success);
    assert_eq!(read.fetch.mergeable, Mergeable::Clean);
    assert_eq!(read.fetch.review_decision, ReviewDecision::Approved);
    assert_eq!(read.fetch.pipeline_state, PipelineState::Passed);
    assert_eq!(read.fetch.merge_sha, None);
    assert_eq!(read.repo_public, Some(false));
    assert_eq!(read.head_ref.as_deref(), Some("fm/work"));
    assert_eq!(read.changed_paths, Some(vec!["src/lib.rs".to_owned()]));
    assert!(read.body_leaks.is_empty());

    gh.set(
        "reviews.json",
        r#"[[{"state":"APPROVED","user":{"login":"a"}},{"state":"CHANGES_REQUESTED","user":{"login":"b"}}]]"#,
    );
    let read = forge(&gh, Cached::passed()).pr(REPO, 7).unwrap();
    assert_eq!(read.fetch.review_decision, ReviewDecision::ChangesRequested);
}

/// R19: the lines of the body that fail the scrub come back, by the host's
/// scrub list.
#[test]
fn a_leaking_body_comes_back_as_its_failing_lines() {
    let gh = FakeGh::new();
    gh.set(
        "pull.json",
        r#"{"number":7,"state":"open","merged":false,"body":"fine line\nmentions secretproject here",
        "head":{"sha":"a1a1a1a1","ref":"fm/work"},"base":{"ref":"main","repo":{"private":false}},"mergeable_state":"clean"}"#,
    );
    let local = Box::new(Cached {
        pipeline: PipelineState::Passed,
        terms: "secretproject\n",
    });
    let read = forge(&gh, local).pr(REPO, 7).unwrap();
    assert_eq!(read.repo_public, Some(true));
    assert_eq!(read.body_leaks, vec!["mentions secretproject here"]);
}

#[test]
fn a_failing_gh_is_an_error_not_an_answer() {
    let gh = FakeGh::new();
    let error = forge(&gh, Cached::passed()).pr(REPO, 7).unwrap_err();
    assert!(error.contains("Not Found"), "{error}");
    let missing = ReconcileForge::new(
        Path::new("/nonexistent/gh").to_string_lossy(),
        Cached::passed(),
    );
    assert!(missing.pr(REPO, 7).unwrap_err().contains("run gh"));
}

#[test]
fn the_default_head_and_ancestry_come_from_the_remote() {
    let w = World::new();
    let project: Project = get(w.store().conn(), "project", "alpha").unwrap();
    let gh = FakeGh::new();
    gh.set("head.json", r#"{"sha":"d4d4d4d4"}"#);
    let forge = forge(&gh, Cached::passed());
    assert_eq!(forge.default_branch_head(&project).unwrap(), "d4d4d4d4");
    gh.set("compare.json", r#"{"status":"ahead"}"#);
    assert!(forge.is_ancestor(&project, "a1a1a1a1", "d4d4d4d4").unwrap());
    gh.set("compare.json", r#"{"status":"identical"}"#);
    assert!(forge.is_ancestor(&project, "d4d4d4d4", "d4d4d4d4").unwrap());
    gh.set("compare.json", r#"{"status":"behind"}"#);
    assert!(!forge.is_ancestor(&project, "a1a1a1a1", "d4d4d4d4").unwrap());
    gh.set("compare.json", r#"{"status":"diverged"}"#);
    assert!(!forge.is_ancestor(&project, "a1a1a1a1", "d4d4d4d4").unwrap());
}
