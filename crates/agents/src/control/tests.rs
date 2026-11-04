//! Interrupt, resume and relaunch against real git worktrees, the fake
//! herdr and a file-backed `state.db`, with a thread standing in for
//! Claude and its hooks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::thread::JoinHandle;

use tiller_core::domain::{CardKind, Size};
use tiller_herdr::fake::{FakeHerdr, Screen};
use tiller_herdr::testing::owner;

use super::*;
use crate::spawn::tests::{Rig, git, world};
use crate::spawn::{Spawned, World};

fn quick() -> Waits {
    Waits {
        interrupt: Duration::from_millis(300),
        exit: Duration::from_millis(300),
        bind: Duration::from_millis(300),
        poll: Duration::from_millis(10),
    }
}

/// Long enough for the stand-in Claude thread to answer.
fn patient() -> Waits {
    Waits {
        interrupt: Duration::from_secs(10),
        exit: Duration::from_secs(10),
        bind: Duration::from_secs(10),
        poll: Duration::from_millis(10),
    }
}

/// A builder run whose Claude is up in its pane: spawned, then started,
/// its `SessionStart` hook bound.
fn running(rig: &mut Rig) -> Spawned {
    let card = rig.card(CardKind::Ship, Some(Size::M));
    let spawned = rig.spawn(&card, RunRole::Builder);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Idle);
    rig.fleet.bind(&spawned.session.id, &spawned.pane_id);
    spawned
}

fn hook(store: &mut Store, session: &str, event: &str) {
    store
        .observation_record(
            &Ctx::new(Actor::Hook),
            NewObservation {
                session_id: Some(session.into()),
                run_id: None,
                source: ObservationSource::Hook,
                event: event.into(),
                verdict: None,
                observed_at: Timestamp::now(),
                payload: json!({}),
            },
        )
        .unwrap();
}

/// Claude, as a launch line run at a shell brings it up: the pane comes up
/// at an empty composer and `SessionStart:<source>` records against the
/// live session bound to it. Returns that session, or `None` when no
/// launch line ran within 10 s.
fn boot(rig: &Rig, source: &'static str) -> JoinHandle<Option<String>> {
    let herdr = FakeHerdr::open(rig.herdr.path());
    let mut store = rig.fleet.open();
    let before: Vec<(String, usize)> = herdr
        .snapshot()
        .unwrap()
        .panes
        .iter()
        .map(|p| {
            (
                p.pane_id.clone(),
                herdr.fake_pane(&p.pane_id).submitted.len(),
            )
        })
        .collect();
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for pane in herdr.snapshot().unwrap().panes {
                let fake = herdr.fake_pane(&pane.pane_id);
                let seen = before
                    .iter()
                    .find(|(id, _)| *id == pane.pane_id)
                    .map_or(0, |(_, n)| *n);
                let launched = fake.screen == Screen::Shell
                    && fake.submitted.len() > seen
                    && fake.submitted.last().is_some_and(|l| l.starts_with("sh '"));
                if !launched {
                    continue;
                }
                let session: Option<String> = store
                    .conn()
                    .query_row(
                        "SELECT id FROM sessions WHERE pane_id = ?1 AND ended_at IS NULL",
                        [&pane.pane_id],
                        |row| row.get(0),
                    )
                    .ok();
                let Some(session) = session else { continue };
                herdr.set_screen(&pane.pane_id, Screen::Idle);
                hook(&mut store, &session, &format!("SessionStart:{source}"));
                return Some(session);
            }
            thread::sleep(Duration::from_millis(10));
        }
        None
    })
}

fn session(rig: &Rig, id: &str) -> Session {
    get(rig.fleet.store.conn(), "session", id).unwrap()
}

fn interrupt_it(rig: &mut Rig, ctx: &Ctx, waits: &Waits, run: &str) -> Result<Interrupted> {
    let run = rig.run(run);
    interrupt(&mut rig.fleet.store, ctx, &rig.herdr, waits, &run).map(|a| a.value)
}

fn resume_it(rig: &mut Rig, ctx: &Ctx, waits: &Waits, run: &str) -> Result<Resumed> {
    let run = rig.run(run);
    let world = world!(rig);
    resume(&mut rig.fleet.store, ctx, &world, waits, &run).map(|a| a.value)
}

fn relaunch_it(
    rig: &mut Rig,
    ctx: &Ctx,
    waits: &Waits,
    run: &str,
    request: RelaunchRequest,
) -> Result<RelaunchOutcome> {
    let run = rig.run(run);
    keyed_relaunch(rig, ctx, None, waits, &run, request)
}

fn keyed_relaunch(
    rig: &mut Rig,
    ctx: &Ctx,
    key: Option<&str>,
    waits: &Waits,
    run: &Run,
    request: RelaunchRequest,
) -> Result<RelaunchOutcome> {
    let world = world!(rig);
    relaunch(&mut rig.fleet.store, ctx, key, &world, waits, run, request).map(|a| a.value)
}

fn relaunched(outcome: RelaunchOutcome) -> Relaunched {
    match outcome {
        RelaunchOutcome::Relaunched(done) => *done,
        other => panic!("expected a relaunch, got {other:?}"),
    }
}

fn note(text: &str) -> RelaunchRequest {
    RelaunchRequest {
        note: text.into(),
        ..RelaunchRequest::default()
    }
}

#[test]
fn interrupt_confirms_on_the_turn_end_hook() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let herdr = FakeHerdr::open(rig.herdr.path());
    let mut store = rig.fleet.open();
    let (pane, session) = (spawned.pane_id.clone(), spawned.session.id.clone());
    let claude = thread::spawn(move || {
        let sent = format!("send_escape {pane}");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if herdr.calls().contains(&sent) {
                hook(&mut store, &session, "Stop");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    let ctx = rig.orchestrator();
    let done = interrupt_it(&mut rig, &ctx, &patient(), &spawned.run.id).unwrap();
    claude.join().unwrap();
    assert_eq!(done.cancel, Proof::Confirmed, "{}", done.evidence);
    assert!(done.evidence.contains("Stop"), "{}", done.evidence);
    assert_eq!(done.pane_id, spawned.pane_id);
    // Escape only: nothing typed.
    let calls = rig.herdr.calls();
    assert!(
        !calls.iter().any(
            |c| c.starts_with(&format!("send_text {}", spawned.pane_id)) && c.contains("/exit")
        )
    );
    assert_eq!(rig.herdr.fake_pane(&spawned.pane_id).screen, Screen::Idle);
}

#[test]
fn interrupt_confirms_on_the_transcript_marker() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let transcript = rig.fleet.dir.path().join("t.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"old\"}}\n",
    )
    .unwrap();
    rig.sql(&format!(
        "UPDATE sessions SET transcript_path = '{}' WHERE id = '{}'",
        transcript.display(),
        spawned.session.id
    ));
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let herdr = FakeHerdr::open(rig.herdr.path());
    let pane = spawned.pane_id.clone();
    let path = transcript.clone();
    let claude = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if herdr.calls().contains(&format!("send_escape {pane}")) {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                writeln!(
                    file,
                    "{}",
                    json!({"type": "user", "message": {"role": "user",
                        "content": [{"type": "text", "text": "[Request interrupted by user]"}]}})
                )
                .unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    });

    let done = interrupt_it(&mut rig, &owner(), &patient(), &spawned.run.id).unwrap();
    claude.join().unwrap();
    assert_eq!(done.cancel, Proof::Confirmed, "{}", done.evidence);
    assert!(done.evidence.contains("transcript"), "{}", done.evidence);
}

#[test]
fn interrupt_without_proof_is_unconfirmed() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);

    let done = interrupt_it(&mut rig, &owner(), &quick(), &spawned.run.id).unwrap();
    // The fake went idle, but busy state is never proof.
    assert_eq!(done.cancel, Proof::Unconfirmed);
    assert!(done.evidence.contains("claude (idle)"), "{}", done.evidence);
    let sent: i64 = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM observations WHERE session_id = ?1 AND event = 'interrupt_sent'",
            [&spawned.session.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sent, 1);
}

#[test]
fn interrupt_refuses_a_session_not_working() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);

    let error = interrupt_it(&mut rig, &owner(), &quick(), &spawned.run.id).unwrap_err();
    assert!(matches!(error, SpawnError::Refused(_)), "{error}");
    assert!(error.to_string().contains("is not working"), "{error}");
    assert!(
        !rig.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("send_escape")),
        "{:?}",
        rig.herdr.calls()
    );

    // A pane at a shell has no agent to interrupt.
    rig.herdr.set_screen(&spawned.pane_id, Screen::Shell);
    let error = interrupt_it(&mut rig, &owner(), &quick(), &spawned.run.id).unwrap_err();
    assert!(error.to_string().contains("shows no agent"), "{error}");
}

#[test]
fn control_is_the_supervisor_or_the_owner() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let worker = Ctx::new(Actor::Session(spawned.session.id.clone()));
    let calls = rig.herdr.calls().len();

    let error = interrupt_it(&mut rig, &worker, &quick(), &spawned.run.id).unwrap_err();
    assert!(error.to_string().contains("may not interrupt"), "{error}");
    let error = resume_it(&mut rig, &worker, &quick(), &spawned.run.id).unwrap_err();
    assert!(error.to_string().contains("may not resume"), "{error}");
    let error = relaunch_it(&mut rig, &worker, &quick(), &spawned.run.id, note("n")).unwrap_err();
    assert!(error.to_string().contains("may not relaunch"), "{error}");
    assert_eq!(rig.herdr.calls().len(), calls);
}

#[test]
fn resume_leaves_a_running_session_alone() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let calls = rig.herdr.calls().len();

    let ctx = rig.orchestrator();
    let done = resume_it(&mut rig, &ctx, &quick(), &spawned.run.id).unwrap();
    assert!(done.left_alone);
    assert_eq!(done.started, None);
    assert_eq!(
        rig.events("session.resume_attempted", &spawned.session.id),
        0
    );
    // A look, no keys.
    assert!(
        rig.herdr.calls()[calls..]
            .iter()
            .all(|c| !c.starts_with("send_")),
        "{:?}",
        rig.herdr.calls()
    );
}

#[test]
fn resume_relaunches_claude_resume_in_its_pane() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    // Claude died; the shell is left in the pane.
    rig.herdr.set_screen(&spawned.pane_id, Screen::Shell);
    let claude = boot(&rig, "resume");
    // A resume records trust again, in case the entry went.
    std::fs::remove_file(rig.claude.store().unwrap()).unwrap();

    let ctx = rig.orchestrator();
    let done = resume_it(&mut rig, &ctx, &patient(), &spawned.run.id).unwrap();
    assert!(rig.trusted(std::path::Path::new(&spawned.worktree)));
    assert_eq!(
        claude.join().unwrap().as_deref(),
        Some(spawned.session.id.as_str())
    );
    assert!(!done.left_alone);
    assert_eq!(done.started, Some(Proof::Confirmed), "{}", done.evidence);
    assert_eq!(done.pane_id.as_deref(), Some(spawned.pane_id.as_str()));
    assert_eq!(done.session.id, spawned.session.id);
    assert!(done.session.ended_at.is_none());

    // The line runs `claude --resume` from the run's worktree.
    let line = rig
        .herdr
        .fake_pane(&spawned.pane_id)
        .submitted
        .last()
        .cloned()
        .unwrap();
    let script = line.trim_start_matches("sh '").trim_end_matches('\'');
    let text = std::fs::read_to_string(script).unwrap();
    assert!(
        text.contains(&format!("'--resume' 'claude-{}'", spawned.session.id)),
        "{text}"
    );
    assert!(text.contains(&spawned.worktree), "{text}");
    let attempted: Value = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'session.resume_attempted' AND entity_id = ?1",
            [&spawned.session.id],
            |row| row.get::<_, String>(0),
        )
        .map(|text| serde_json::from_str(&text).unwrap())
        .unwrap();
    assert_eq!(attempted["checks"]["reason"], "manual");
    assert_eq!(attempted["checks"]["cwd"], spawned.worktree.as_str());
}

#[test]
fn resume_opens_a_pane_when_its_own_is_gone() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.remove_pane(&spawned.pane_id);
    let claude = boot(&rig, "resume");

    let done = resume_it(&mut rig, &owner(), &patient(), &spawned.run.id).unwrap();
    claude.join().unwrap();
    assert_eq!(done.started, Some(Proof::Confirmed), "{}", done.evidence);
    let pane = done.pane_id.unwrap();
    assert_ne!(pane, spawned.pane_id);
    assert_eq!(done.session.pane_id.as_deref(), Some(pane.as_str()));
}

#[test]
fn resume_refuses_under_a_floor_and_without_a_live_session() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Shell);
    rig.sql(&format!(
        "UPDATE hosts SET disk_floor_mb = 100000 WHERE id = '{}'",
        spawned.run.host_id
    ));
    let error = resume_it(&mut rig, &owner(), &quick(), &spawned.run.id).unwrap_err();
    assert!(
        error.to_string().contains("under the 100000 MB floor"),
        "{error}"
    );
    assert_eq!(
        rig.events("session.resume_attempted", &spawned.session.id),
        0
    );

    rig.fleet
        .store
        .session_end(&Ctx::new(Actor::Hook), &spawned.session.id, "logout")
        .unwrap();
    let error = resume_it(&mut rig, &owner(), &quick(), &spawned.run.id).unwrap_err();
    assert!(
        error.to_string().contains("tiller agent relaunch"),
        "{error}"
    );
}

#[test]
fn relaunch_resumes_worker_in_own_worktree_with_note() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    // Work in progress, committed and not.
    let worktree = PathBuf::from(&spawned.worktree);
    std::fs::write(worktree.join("done.txt"), "half\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "--quiet", "-m", "half"]);
    std::fs::write(worktree.join("wip.txt"), "draft\n").unwrap();
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    let claude = boot(&rig, "startup");
    let ctx = rig.orchestrator();
    std::fs::remove_file(rig.claude.store().unwrap()).unwrap();

    let done = relaunched(
        relaunch_it(
            &mut rig,
            &ctx,
            &patient(),
            &spawned.run.id,
            note("tests pass; the docs are left"),
        )
        .unwrap(),
    );
    let new = claude.join().unwrap().unwrap();
    assert_eq!(done.started, Proof::Confirmed, "{}", done.evidence);

    // The same run, a new session, in the same pane and worktree.
    assert_eq!(done.run.id, spawned.run.id);
    assert!(done.run.ended_at.is_none());
    assert_eq!(done.session.id, new);
    assert_ne!(new, spawned.session.id);
    assert_eq!(
        done.ended_session.as_deref(),
        Some(spawned.session.id.as_str())
    );
    assert!(
        done.stop_proof.as_deref().unwrap().contains("no agent"),
        "{done:?}"
    );
    assert_eq!(done.pane_id, spawned.pane_id);
    let old = session(&rig, &spawned.session.id);
    assert!(old.ended_at.is_some());
    assert_eq!(old.run_id.as_deref(), Some(spawned.run.id.as_str()));
    let submitted = rig.herdr.fake_pane(&spawned.pane_id).submitted;
    assert_eq!(submitted[submitted.len() - 2], "/exit");

    // Nothing was lost.
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read_to_string(worktree.join("wip.txt")).unwrap(),
        "draft\n"
    );
    assert!(rig.pool.returned().is_empty());

    // The new session starts in the worktree from the note.
    let script = submitted
        .last()
        .unwrap()
        .trim_start_matches("sh '")
        .trim_end_matches('\'');
    let text = std::fs::read_to_string(script).unwrap();
    assert!(text.contains(&spawned.worktree), "{text}");
    assert!(text.contains(&new), "{text}");
    assert!(!text.contains("'--resume'"), "{text}");
    assert!(text.contains("tests pass; the docs are left"), "{text}");
    assert!(rig.trusted(&worktree));
    assert_eq!(rig.events("run.relaunched", &spawned.run.id), 1);
}

#[test]
fn relaunch_interrupts_a_working_turn_before_the_exit() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let claude = boot(&rig, "startup");

    let done = relaunched(
        relaunch_it(&mut rig, &owner(), &patient(), &spawned.run.id, note("n")).unwrap(),
    );
    claude.join().unwrap();
    assert_eq!(done.started, Proof::Confirmed);
    let calls = rig.herdr.calls();
    let escape = calls
        .iter()
        .position(|c| *c == format!("send_escape {}", spawned.pane_id))
        .unwrap();
    let exit = calls
        .iter()
        .position(|c| *c == format!("send_text {} /exit", spawned.pane_id))
        .unwrap();
    assert!(escape < exit, "{calls:?}");
}

#[test]
fn relaunch_refuses_when_the_stop_is_unproven() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);

    // A draft in the composer would be lost.
    rig.herdr.type_draft(&spawned.pane_id, "half a thought");
    let error = relaunch_it(&mut rig, &owner(), &quick(), &spawned.run.id, note("n")).unwrap_err();
    assert!(error.to_string().contains("holds a draft"), "{error}");
    assert_eq!(
        rig.herdr.fake_pane(&spawned.pane_id).composer,
        "half a thought"
    );

    // A dialog would take `/exit` as its answer.
    let other = running(&mut rig);
    rig.herdr.set_screen(&other.pane_id, Screen::Permission);
    let error = relaunch_it(&mut rig, &owner(), &quick(), &other.run.id, note("n")).unwrap_err();
    assert!(error.to_string().contains("dialog"), "{error}");

    for run in [&spawned.run.id, &other.run.id] {
        assert_eq!(rig.events("run.relaunched", run), 0);
        assert!(rig.run(run).ended_at.is_none());
    }
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
}

#[test]
fn relaunch_refuses_when_claude_does_not_exit() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.ignore_exit(&spawned.pane_id);

    let error = relaunch_it(&mut rig, &owner(), &quick(), &spawned.run.id, note("n")).unwrap_err();
    assert!(error.to_string().contains("not proven stopped"), "{error}");
    assert!(error.to_string().contains("claude (idle)"), "{error}");
    assert_eq!(
        rig.herdr
            .fake_pane(&spawned.pane_id)
            .submitted
            .last()
            .unwrap(),
        "/exit"
    );
    assert_eq!(rig.events("run.relaunched", &spawned.run.id), 0);
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
}

#[test]
fn relaunch_refuses_a_worktree_off_its_branch() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    git(
        &PathBuf::from(&spawned.worktree),
        &["checkout", "--quiet", "-b", "elsewhere"],
    );

    let error = relaunch_it(&mut rig, &owner(), &quick(), &spawned.run.id, note("n")).unwrap_err();
    assert!(
        error.to_string().contains("not the run's branch"),
        "{error}"
    );
    assert!(!rig.herdr.calls().iter().any(|c| c.contains("/exit")));
    assert_eq!(rig.events("run.relaunched", &spawned.run.id), 0);
}

#[test]
fn relaunch_new_attempt_on_another_model() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let claude = boot(&rig, "startup");
    let request = RelaunchRequest {
        note: "start over on the parser".into(),
        new_attempt: true,
        brief: Some("Do the parser again.".into()),
        asked: Some(("fable".into(), Effort::Max)),
    };

    let ctx = rig.orchestrator();
    let done =
        relaunched(relaunch_it(&mut rig, &ctx, &patient(), &spawned.run.id, request).unwrap());
    claude.join().unwrap();
    assert_eq!(done.started, Proof::Confirmed, "{}", done.evidence);

    // A new run in the same worktree and pane on the asked model; the old
    // run ends `relaunched` and keeps its row.
    assert_ne!(done.run.id, spawned.run.id);
    assert_eq!(
        (done.run.model.as_str(), done.run.effort),
        ("fable", Effort::Max)
    );
    assert_eq!(
        done.run.worktree_path.as_deref(),
        Some(spawned.worktree.as_str())
    );
    assert_eq!(done.run.branch, spawned.run.branch);
    assert!(done.run.config_hash.is_some());
    assert_eq!(
        done.session.pane_id.as_deref(),
        Some(spawned.pane_id.as_str())
    );
    let old = rig.run(&spawned.run.id);
    assert!(old.ended_at.is_some());
    assert_eq!(
        old.outcome.map(|o| o.to_string()).as_deref(),
        Some("relaunched")
    );
    // Its own launch files: the brief in its instructions, the note in its
    // first prompt.
    let prompt = std::fs::read_to_string(rig.scratch.join(&done.run.id).join("prompt.md")).unwrap();
    assert!(prompt.contains("Do the parser again."), "{prompt}");
    let line = rig
        .herdr
        .fake_pane(&spawned.pane_id)
        .submitted
        .last()
        .cloned()
        .unwrap();
    let script =
        std::fs::read_to_string(line.trim_start_matches("sh '").trim_end_matches('\'')).unwrap();
    assert!(script.contains("start over on the parser"), "{script}");
    assert!(script.contains(&done.run.id), "{script}");
    assert!(rig.pool.returned().is_empty());
}

#[test]
fn relaunch_model_needs_a_new_attempt_and_the_rule() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let mut request = note("n");
    request.asked = Some(("fable".into(), Effort::Max));
    let error = relaunch_it(
        &mut rig,
        &owner(),
        &quick(),
        &spawned.run.id,
        request.clone(),
    )
    .unwrap_err();
    assert!(matches!(error, SpawnError::Usage(_)), "{error}");

    // The orchestrator may not lower the rule on a new attempt either.
    request.new_attempt = true;
    request.brief = Some("b".into());
    request.asked = Some(("haiku".into(), Effort::Low));
    let ctx = rig.orchestrator();
    let error = relaunch_it(&mut rig, &ctx, &quick(), &spawned.run.id, request).unwrap_err();
    assert!(matches!(error, SpawnError::Downgrade(_)), "{error}");
    assert!(!rig.herdr.calls().iter().any(|c| c.contains("/exit")));
}

#[test]
fn relaunch_key_replays_without_touching_herdr() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let claude = boot(&rig, "startup");
    let ctx = rig.orchestrator();
    let key = Some("relaunch-1");
    let first = relaunched(
        keyed_relaunch(&mut rig, &ctx, key, &patient(), &spawned.run, note("n")).unwrap(),
    );
    claude.join().unwrap();
    assert_eq!(first.started, Proof::Confirmed);
    let calls = rig.herdr.calls().len();

    // The run moved on, but the key answers with what it did.
    let again = keyed_relaunch(&mut rig, &ctx, key, &quick(), &spawned.run, note("n")).unwrap();
    let RelaunchOutcome::Replayed { session, .. } = again else {
        panic!("expected a replay, got {again:?}");
    };
    assert_eq!(session.id, first.session.id);
    assert_eq!(rig.herdr.calls().len(), calls);

    // A recorded key answers only a caller who may relaunch.
    let worker = Ctx::new(Actor::Session(first.session.id.clone()));
    let error =
        keyed_relaunch(&mut rig, &worker, key, &quick(), &spawned.run, note("n")).unwrap_err();
    assert!(error.to_string().contains("may not relaunch"), "{error}");
    assert_eq!(rig.herdr.calls().len(), calls);
}

#[test]
fn relaunch_the_record_refuses_leaves_the_agent_running() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.sql(&format!(
        "UPDATE hosts SET disk_floor_mb = 100000 WHERE id = '{}'",
        spawned.run.host_id
    ));
    let request = RelaunchRequest {
        note: "n".into(),
        new_attempt: true,
        brief: Some("b".into()),
        asked: None,
    };

    let ctx = rig.orchestrator();
    let error = relaunch_it(&mut rig, &ctx, &quick(), &spawned.run.id, request).unwrap_err();
    assert!(
        error.to_string().contains("under the 100000 MB floor"),
        "{error}"
    );
    // The rehearsal refused before `/exit`: Claude and every row stay.
    assert!(!rig.herdr.calls().iter().any(|c| c.contains("/exit")));
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
    assert!(rig.run(&spawned.run.id).ended_at.is_none());
    assert_eq!(rig.events("run.relaunched", &spawned.run.id), 0);
    let runs: i64 = rig
        .fleet
        .store
        .conn()
        .query_row("SELECT count(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 1);
}

#[test]
fn relaunch_after_session_end_with_claude_still_up_opens_a_pane() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    // Claude records `SessionEnd` but herdr still shows it in the pane.
    rig.herdr.ignore_exit(&spawned.pane_id);
    let hook_end = {
        let herdr = FakeHerdr::open(rig.herdr.path());
        let mut store = rig.fleet.open();
        let (session, pane) = (spawned.session.id.clone(), spawned.pane_id.clone());
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if herdr
                    .fake_pane(&pane)
                    .submitted
                    .iter()
                    .any(|l| l == "/exit")
                {
                    store
                        .session_end(&Ctx::new(Actor::Hook), &session, "logout")
                        .unwrap();
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let claude = boot(&rig, "startup");
    let waits = Waits {
        exit: Duration::from_secs(1),
        ..patient()
    };

    let done =
        relaunched(relaunch_it(&mut rig, &owner(), &waits, &spawned.run.id, note("n")).unwrap());
    hook_end.join().unwrap();
    claude.join().unwrap();
    assert_eq!(done.started, Proof::Confirmed, "{}", done.evidence);
    assert!(
        done.stop_proof.as_deref().unwrap().contains("SessionEnd"),
        "{done:?}"
    );
    // Nothing more is typed into the pane Claude still holds.
    assert_ne!(done.pane_id, spawned.pane_id);
    assert_eq!(
        rig.herdr
            .fake_pane(&spawned.pane_id)
            .submitted
            .last()
            .unwrap(),
        "/exit"
    );
    assert_eq!(
        done.ended_session.as_deref(),
        Some(spawned.session.id.as_str())
    );
}
