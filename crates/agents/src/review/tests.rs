#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;

use super::*;

const PATHS: &[&str] = &[
    "src/a.rs",
    "src/b.rs",
    "src/c.rs",
    "src/net/d.rs",
    "src/net/e.rs",
    "src/net/f.rs",
    "web/g.ts",
    "web/h.ts",
    "web/ui/i.tsx",
    "web/ui/j.tsx",
    "docs/k.md",
    "docs/l.md",
    "m.toml",
    "n.toml",
    "tests/o.rs",
    "tests/p.rs",
];

fn unit(i: usize, files: BTreeMap<usize, u64>) -> Unit {
    Unit {
        key: format!("#{i}"),
        number: Some(i as i64),
        title: format!("change {i}"),
        body: String::new(),
        url: None,
        card_id: None,
        commits: vec![format!("{i:040x}")],
        files: files
            .into_iter()
            .map(|(path, lines)| FileChange {
                path: PATHS[path].to_owned(),
                lines,
            })
            .collect(),
    }
}

fn units() -> impl Strategy<Value = Vec<Unit>> {
    // Mostly small diffs, sometimes one far over the caps.
    let lines = prop_oneof![8 => 0u64..400, 2 => 400u64..1_800, 1 => 1_800u64..4_000];
    prop::collection::vec(
        prop::collection::btree_map(0..PATHS.len(), lines, 1..5),
        0..30,
    )
    .prop_map(|units| {
        units
            .into_iter()
            .enumerate()
            .map(|(i, files)| unit(i, files))
            .collect()
    })
}

/// Connected components over files with counted lines, recomputed here.
fn components(units: &[Unit]) -> Vec<BTreeSet<usize>> {
    let mut out: Vec<BTreeSet<usize>> = Vec::new();
    for (i, unit) in units
        .iter()
        .enumerate()
        .filter(|(_, unit)| unit.lines() > 0)
    {
        let files: BTreeSet<&str> = unit
            .files
            .iter()
            .filter(|f| f.lines > 0)
            .map(|f| f.path.as_str())
            .collect();
        let (touching, rest): (Vec<_>, Vec<_>) = out.into_iter().partition(|group| {
            group.iter().any(|&j| {
                units[j]
                    .files
                    .iter()
                    .any(|f| f.lines > 0 && files.contains(f.path.as_str()))
            })
        });
        let mut merged: BTreeSet<usize> = touching.into_iter().flatten().collect();
        merged.insert(i);
        out = rest;
        out.push(merged);
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn plan_partitions_within_caps(units in units()) {
        let chunks = plan(&units);

        // Every counted changed line lands in exactly one chunk.
        let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
        for chunk in &chunks {
            prop_assert_eq!(chunk.lines, chunk.parts.iter().map(|p| p.lines).sum::<u64>());
            for part in &chunk.parts {
                for path in &part.files {
                    *seen.entry((part.unit.clone(), path.clone())).or_default() += 1;
                }
            }
        }
        for unit in &units {
            for file in unit.files.iter().filter(|f| f.lines > 0) {
                prop_assert_eq!(seen.get(&(unit.key.clone(), file.path.clone())), Some(&1));
            }
        }
        let total: u64 = units.iter().map(Unit::lines).sum();
        prop_assert_eq!(chunks.iter().map(|c| c.lines).sum::<u64>(), total);

        for chunk in &chunks {
            // No chunk over the cap except a lone oversized file.
            if chunk.lines > HARD_LINES {
                prop_assert!(chunk.oversized);
                prop_assert_eq!(chunk.parts.len(), 1);
                prop_assert_eq!(chunk.parts[0].files.len(), 1);
            }
            let keys: BTreeSet<&String> = chunk.parts.iter().map(|p| &p.unit).collect();
            prop_assert!(keys.len() <= MAX_UNITS);
        }

        // A PR under the cap is never split.
        for unit in units.iter().filter(|u| u.lines() > 0 && u.lines() <= HARD_LINES) {
            let parts = chunks.iter().flat_map(|c| &c.parts).filter(|p| p.unit == unit.key).count();
            prop_assert_eq!(parts, 1, "{} was split", unit.key);
        }

        // PRs sharing a file share a chunk whenever their group fits the cap.
        let chunk_of = |key: &str| chunks.iter().position(|c| c.parts.iter().any(|p| p.unit == key));
        for group in components(&units) {
            let lines: u64 = group.iter().map(|&i| units[i].lines()).sum();
            if lines <= HARD_LINES && group.len() <= MAX_UNITS {
                let homes: BTreeSet<_> = group.iter().map(|&i| chunk_of(&units[i].key)).collect();
                prop_assert_eq!(homes.len(), 1);
            }
        }

        // The same input gives the same plan.
        prop_assert_eq!(plan(&units), chunks);
    }
}

#[test]
fn oversized_unit_is_sliced_by_directory_and_seams_are_listed() {
    let big = Unit {
        files: vec![
            FileChange {
                path: "a/one.rs".into(),
                lines: 900,
            },
            FileChange {
                path: "a/two.rs".into(),
                lines: 500,
            },
            FileChange {
                path: "b/three.rs".into(),
                lines: 700,
            },
            FileChange {
                path: "c/huge.rs".into(),
                lines: 2_500,
            },
        ],
        ..unit(1, BTreeMap::new())
    };
    let chunks = plan(std::slice::from_ref(&big));
    let files: Vec<Vec<String>> = chunks.iter().map(|c| c.parts[0].files.clone()).collect();
    assert_eq!(
        files,
        vec![
            vec!["a/one.rs".to_owned(), "a/two.rs".to_owned()],
            vec!["b/three.rs".to_owned()],
            vec!["c/huge.rs".to_owned()],
        ]
    );
    assert_eq!(
        chunks.iter().map(|c| c.oversized).collect::<Vec<_>>(),
        [false, false, true]
    );

    // Two units over the cap that share a file: each packs whole, in
    // different chunks, and the shared file is a seam.
    let first = unit(1, BTreeMap::from([(0, 1_200), (1, 600)]));
    let second = unit(2, BTreeMap::from([(0, 300), (2, 1_000)]));
    let plan = Plan::new("b", "h", vec![first, second], Vec::new());
    assert_eq!(plan.chunks.len(), 2);
    assert_eq!(
        plan.seams,
        [Seam {
            path: "src/a.rs".into(),
            chunks: vec![1, 2]
        }]
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-11-02T10:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-11-02T10:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, files: &[(&str, &[u8])], message: &str) -> String {
    for (path, body) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Fixed(BTreeMap<String, PullInfo>);

impl PullLookup for Fixed {
    fn by_commit(&self, _: &str, base: &str, sha: &str) -> Result<Option<PullInfo>, LookupError> {
        assert_eq!(base, "main");
        Ok(self.0.get(sha).cloned())
    }

    fn by_number(&self, _: &str, number: i64) -> Result<Option<PullInfo>, LookupError> {
        Ok(self.0.values().find(|pull| pull.number == number).cloned())
    }
}

#[test]
fn collect_maps_commits_and_leaves_out_lockfiles_generated_and_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "dev@example.com"]);
    git(repo, &["config", "user.name", "Dev"]);
    let base = commit(
        repo,
        &[
            (".gitattributes", b"gen/** linguist-generated\n"),
            ("src/lib.rs", b"a\n"),
        ],
        "base",
    );
    let first = commit(
        repo,
        &[
            ("src/lib.rs", b"a\nb\nc\n"),
            ("Cargo.lock", b"x\n"),
            ("gen/api.ts", b"y\n"),
            ("board/routeTree.gen.ts", b"z\n"),
            ("logo.png", b"\x00\x01\x02\x00"),
        ],
        "first",
    );
    let second = commit(repo, &[("src/main.rs", b"m\n")], "second part one");
    let third = commit(repo, &[("src/main.rs", b"m\nn\n")], "second part two");
    let direct = commit(repo, &[("README.md", b"hi\n")], "direct commit");

    let pull = |number: i64, title: &str| PullInfo {
        number,
        title: title.into(),
        body: format!("body of {number}"),
        url: format!("https://example.com/pull/{number}"),
    };
    let lookup = Fixed(BTreeMap::from([
        (first.clone(), pull(1, "first PR")),
        (second.clone(), pull(2, "rebased PR")),
        (third.clone(), pull(2, "rebased PR")),
    ]));
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    tiller_core::migrate::up(&mut conn).unwrap();
    let project = test_project();
    let (units, excluded) = collect(&conn, &project, repo, &base, &direct, &lookup).unwrap();

    assert_eq!(
        units
            .iter()
            .map(|u| (u.key.as_str(), u.commits.len(), u.lines()))
            .collect::<Vec<_>>(),
        [("#1", 1, 2), ("#2", 2, 2), (&direct[..12], 1, 1)]
    );
    assert_eq!(units[0].body, "body of 1");
    assert_eq!(units[2].title, "direct commit");
    assert_eq!(
        excluded
            .iter()
            .map(|e| (e.path.as_str(), e.why.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Cargo.lock", "lockfile"),
            ("board/routeTree.gen.ts", "generated"),
            ("gen/api.ts", "linguist-generated"),
            ("logo.png", "binary"),
        ]
    );
}

fn test_project() -> Project {
    serde_json::from_value(serde_json::json!({
        "id": "alpha",
        "name": "Alpha",
        "repo": "example/alpha",
        "remote": null,
        "default_branch": "main",
        "delivery": "no-mistakes",
        "checks": [],
        "capacity": 1,
        "deploy_workflow": null,
        "deploy_environment": null,
        "deploy_timeout_minutes": 30,
        "merge_gate": "detect",
        "owner_host_id": "host",
        "added_at": "2026-11-02T10:00:00Z",
        "next_card_seq": 1,
        "retired_at": null,
        "rev": 1,
        "stage": "pre-launch",
        "reviewed_through_sha": null,
        "stage_changed_at": null,
    }))
    .unwrap()
}
