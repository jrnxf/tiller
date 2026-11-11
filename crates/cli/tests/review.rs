//! One pre-launch cycle end to end (`docs/design/pre-launch-validation.md`
//! section 8): a temp git remote and a fake forge, the switch to pre-launch,
//! two merged PRs, the due timer, a pass in two chunks with a fix card, the
//! next due date, then launch and the final pass.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::process::Command;

use common::World;
use serde_json::{Value, json};
use tiller_core::domain::{
    Checks, Effort, Mergeable, Obligation, PipelineState, PrState, ReviewDecision, RunOutcome,
    RunRole,
};
use tiller_core::state::approvals::PrFetch;
use tiller_core::state::review::open_weekly;
use tiller_core::state::runs::{EndRun, NewRun, SpawnChecks};
use tiller_core::time::{Timestamp, lisbon};
use tiller_core::{Actor, Ctx, NewEvent};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn lines(n: usize, word: &str) -> String {
    (0..n).map(|i| format!("{word} {i}\n")).collect()
}

/// Writes `files` in the dev clone and commits them on the current branch.
fn commit(dev: &Path, files: &[(&str, String)], message: &str) -> String {
    for (path, body) in files {
        let path = dev.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    git(dev, &["add", "-A"]);
    git(dev, &["commit", "-q", "-m", message]);
    git(dev, &["rev-parse", "HEAD"])
}

fn project(world: &World) -> Value {
    let out = world.must(&["project", "list"]);
    out.data()["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "alpha")
        .unwrap()
        .clone()
}

fn rev(world: &World) -> String {
    project(world)["rev"].as_i64().unwrap().to_string()
}

fn reviewer_run(world: &World, card: &str) -> (String, String) {
    let started = world
        .store()
        .run_start(
            &world.owner_ctx(),
            None,
            NewRun {
                card: card.into(),
                role: RunRole::Reviewer,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some("/home/user/work/review".into()),
                branch: None,
                base_branch: Some("main".into()),
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value;
    (started.run.id, started.session.id)
}

#[test]
fn prelaunch_cycle_end_to_end() {
    let world = World::initialized();
    let root = world.dir.path();
    let (origin, dev, checkout) = (
        root.join("origin.git"),
        root.join("dev"),
        root.join("checkout"),
    );
    git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            dev.to_str().unwrap(),
        ],
    );
    git(&dev, &["config", "user.email", "dev@example.com"]);
    git(&dev, &["config", "user.name", "Dev"]);
    git(&dev, &["checkout", "-q", "-b", "main"]);
    let base = commit(&dev, &[("README.md", "alpha\n".into())], "base");
    git(&dev, &["push", "-q", "origin", "main"]);
    git(
        root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    std::fs::write(
        world.home().join("config.toml"),
        format!("[checkouts]\nalpha = {:?}\n", checkout.to_str().unwrap()),
    )
    .unwrap();

    // The fake forge: `gh api <path>` prints `<path with / as _>.json`.
    let forge = root.join("forge");
    std::fs::create_dir_all(&forge).unwrap();
    let gh = root.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\nf=\"{}/$(printf '%s' \"$2\" | tr '/' '_').json\"\nif [ -f \"$f\" ]; then cat \"$f\"; else echo '[]'; fi\n",
            forge.display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&gh).status().unwrap();
    world.set_env("TILLER_GH", gh.to_str().unwrap());

    // The project validates every change today.
    let orchestrator = world.orchestrator();
    world.must(&[
        "project",
        "edit",
        "alpha",
        "--rev",
        &rev(&world),
        "--delivery",
        "no-mistakes",
    ]);

    // An agent may not change the stage; the owner may.
    let out = world.run_as(
        &orchestrator,
        &[
            "project",
            "stage",
            "alpha",
            "pre-launch",
            "--rev",
            &rev(&world),
        ],
    );
    assert_eq!(out.error_code(), "forbidden", "{}", out.json);
    let out = world.must(&[
        "project",
        "stage",
        "alpha",
        "pre-launch",
        "--rev",
        &rev(&world),
    ]);
    assert_eq!(out.data()["project"]["reviewed_through_sha"], base.as_str());
    let first_due = out.data()["obligation"]["id"].as_str().unwrap().to_owned();

    // Builders now open the PR directly.
    world.must_as(
        &orchestrator,
        &[
            "card", "create", "alpha", "--kind", "ship", "--title", "Parser",
        ],
    );
    let shown = world.must(&["card", "show", "alpha-1"]);
    assert_eq!(shown.data()["effective_delivery"], "direct-pr");

    // PR #1, Tiller's, merged with a merge commit.
    git(&dev, &["checkout", "-q", "-b", "parser"]);
    commit(&dev, &[("src/parser.rs", lines(1_200, "parse"))], "parser");
    git(&dev, &["checkout", "-q", "main"]);
    git(
        &dev,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "parser",
        ],
    );
    let first_merge = git(&dev, &["rev-parse", "HEAD"]);
    let (_, worker) = world.builder_run("alpha-1");
    world.must_as(
        &worker,
        &["pr", "link", "https://example.com/example/alpha/pull/1"],
    );
    let mut store = world.store();
    let pr: String = store
        .conn()
        .query_row("SELECT id FROM prs", [], |r| r.get(0))
        .unwrap();
    store
        .pr_refresh(
            &Ctx::new(Actor::Daemon),
            &pr,
            PrFetch {
                state: PrState::Merged,
                head_sha: Some(first_merge.clone()),
                base_ref: Some("main".into()),
                checks: Checks::Success,
                review_decision: ReviewDecision::Approved,
                mergeable: Mergeable::Clean,
                merged_at: Some(Timestamp::now()),
                merge_sha: Some(first_merge.clone()),
                pipeline_state: PipelineState::None,
                fetch_error: None,
            },
        )
        .unwrap();
    std::fs::write(
        forge.join("repos_example_alpha_pulls_1.json"),
        json!({"number": 1, "title": "Add the parser", "body": "Parses the config.", "html_url": "https://example.com/example/alpha/pull/1"}).to_string(),
    )
    .unwrap();

    // PR #2, merged outside Tiller as one squashed commit.
    let second = commit(
        &dev,
        &[("web/view.ts", lines(1_000, "view"))],
        "Add the view (#2)",
    );
    std::fs::write(
        forge.join(format!("repos_example_alpha_commits_{second}_pulls.json")),
        json!([{"number": 2, "title": "Add the view", "body": "Shows the config.", "html_url": "https://example.com/example/alpha/pull/2",
                "merged_at": "2026-11-04T10:00:00Z", "base": {"ref": "main"}}])
        .to_string(),
    )
    .unwrap();
    git(&dev, &["push", "-q", "origin", "main"]);

    // Nothing is due before the timer.
    let out = world.run_as(&orchestrator, &["review", "start", "alpha"]);
    assert_eq!(out.error_code(), "refused", "{}", out.json);

    // The due timer meets the weekly obligation and notes the orchestrator.
    let due: Obligation = tiller_core::state::get(store.conn(), "obligation", &first_due).unwrap();
    let slot = due.due_at.unwrap();
    store
        .obligation_meet(
            &Ctx::at(Actor::Daemon, slot),
            &first_due,
            NewEvent::new("obligation", &first_due, "obligation.due"),
        )
        .unwrap();
    let out = world.must_as(&orchestrator, &["review", "due"]);
    assert_eq!(
        out.data()["due"].as_array().unwrap().len(),
        1,
        "{}",
        out.json
    );

    // The pass: two chunks, one per PR.
    let out = world.must_as(&orchestrator, &["review", "start", "alpha"]);
    assert_eq!(out.data()["chunks"], 2, "{}", out.json);
    let pass = out.data()["pass"]["id"].as_str().unwrap().to_owned();
    let card = out.data()["card"].as_str().unwrap().to_owned();
    let head = out.data()["pass"]["head_sha"].as_str().unwrap().to_owned();
    assert_eq!(head, second);
    let replay = world.must_as(&orchestrator, &["review", "start", "alpha"]);
    assert!(
        replay.replayed(),
        "starting again is a no-op: {}",
        replay.json
    );

    for (chunk, title, other) in [
        (1, "Add the parser", "Shows the config."),
        (2, "Add the view", "Parses the config."),
    ] {
        let path = root.join(format!("chunk-{chunk}.md"));
        world.must_as(
            &orchestrator,
            &[
                "review",
                "brief",
                &pass,
                "--chunk",
                &chunk.to_string(),
                "--out",
                path.to_str().unwrap(),
            ],
        );
        let brief = std::fs::read_to_string(&path).unwrap();
        assert!(brief.contains(&format!("chunk {chunk} of 2")), "{brief}");
        assert!(brief.contains(title), "{brief}");
        assert!(!brief.contains(other), "only this chunk's PR body: {brief}");
        assert!(brief.contains("Not covered"), "{brief}");
    }

    // A chunk run records its review; finishing waits for both.
    let (run, session) = reviewer_run(&world, &card);
    world.must_as(
        &session,
        &[
            "review",
            "record",
            &run,
            "--reviewer",
            "code-review",
            "--coverage",
            "diff",
            "--verdict",
            "fail",
            "--sha",
            "0123456789abcdef0123456789abcdef01234567",
            "--high",
            "1",
        ],
    );
    world.must_as(
        &session,
        &[
            "artifact",
            "add",
            &card,
            "--kind",
            "report",
            "--title",
            "chunk 1 of 2",
            "--path",
            "/home/user/review.md",
        ],
    );
    world
        .store()
        .run_end(
            &world.owner_ctx(),
            &run,
            EndRun {
                outcome: RunOutcome::Reported,
                note: None,
            },
        )
        .unwrap();
    let out = world.run_as(&orchestrator, &["review", "finish", &pass]);
    assert_eq!(out.error_code(), "refused", "{}", out.json);
    let (run, session) = reviewer_run(&world, &card);
    world.must_as(
        &session,
        &[
            "review",
            "record",
            &run,
            "--reviewer",
            "code-review",
            "--coverage",
            "diff",
            "--verdict",
            "pass",
            "--sha",
            "0123456789abcdef0123456789abcdef01234567",
        ],
    );
    world.must_as(
        &session,
        &[
            "artifact",
            "add",
            &card,
            "--kind",
            "report",
            "--title",
            "chunk 2 of 2",
            "--path",
            "/home/user/review.md",
        ],
    );
    world
        .store()
        .run_end(
            &world.owner_ctx(),
            &run,
            EndRun {
                outcome: RunOutcome::Reported,
                note: None,
            },
        )
        .unwrap();

    // The summary's report, and the fix card filed from it.
    world.must_as(
        &orchestrator,
        &[
            "artifact",
            "add",
            &card,
            "--kind",
            "page",
            "--title",
            "pre-launch review",
            "--url",
            "https://example.com/report.html",
        ],
    );
    let fix = world.must_as(
        &orchestrator,
        &[
            "card",
            "create",
            "alpha",
            "--kind",
            "ship",
            "--title",
            "The parser drops the last line",
            "--priority",
            "1",
            "--category",
            "pre-launch-review",
            "--fixes",
            "alpha-1",
            "--key",
            &format!("review:{pass}:0123abcd"),
        ],
    );
    let fix = fix.data()["display_id"].as_str().unwrap().to_owned();
    let out = world.must_as(
        &orchestrator,
        &["review", "finish", &pass, "--fix-card", &fix],
    );
    assert_eq!(
        out.data()["project"]["reviewed_through_sha"],
        head.as_str(),
        "{}",
        out.json
    );
    let next = open_weekly(world.store().conn(), "alpha").unwrap().unwrap();
    assert_ne!(next.id, first_due);
    let at = lisbon::local(next.due_at.unwrap().as_jiff());
    assert_eq!(
        (at.weekday(), at.hour(), at.minute()),
        (jiff::civil::Weekday::Saturday, 6, 0)
    );

    // Launch: validation is back on and the final pass waits for its start.
    let out = world.must(&[
        "project",
        "stage",
        "alpha",
        "launched",
        "--rev",
        &rev(&world),
    ]);
    assert_eq!(out.data()["final_pass"]["state"], "pending", "{}", out.json);
    assert!(
        open_weekly(world.store().conn(), "alpha")
            .unwrap()
            .is_none()
    );
    let shown = world.must(&["card", "show", "alpha-1"]);
    assert_eq!(shown.data()["effective_delivery"], "no-mistakes");

    // A commit merged after the switch is still in the final pass.
    let late = commit(&dev, &[("src/late.rs", lines(10, "late"))], "late fix");
    git(&dev, &["push", "-q", "origin", "main"]);
    let out = world.must_as(&orchestrator, &["review", "start", "alpha"]);
    assert_eq!(out.data()["pass"]["kind"], "final", "{}", out.json);
    assert_eq!(out.data()["pass"]["base_sha"], head.as_str());
    assert_eq!(out.data()["pass"]["head_sha"], late.as_str());
    assert_eq!(out.data()["chunks"], 1);
}
