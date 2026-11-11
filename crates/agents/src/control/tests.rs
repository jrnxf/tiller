//! Interrupt, resume and relaunch against real git worktrees, the fake
//! herdr and a file-backed `state.db`, with a thread standing in for
//! Claude and its hooks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::thread::JoinHandle;

use tiller_core::domain::{CardKind, ConfigManifest, Delivery, RunOutcome, Size, Stage};
use tiller_core::state::find;
use tiller_core::state::projects::ProjectEdit;
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
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    }
}

/// Long enough for the stand-in Claude thread to answer.
fn patient() -> Waits {
    Waits {
        interrupt: Duration::from_secs(10),
        exit: Duration::from_secs(10),
        bind: Duration::from_secs(10),
        poll: Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
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
fn a_keyed_interrupt_sends_one_escape_however_often_it_is_retried() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let board = Ctx::new(Actor::Board).with_key("01KEYEDINTERRUPT0000000000");

    let done = interrupt_it(&mut rig, &board, &quick(), &spawned.run.id).unwrap();
    assert_eq!(done.cancel, Proof::Unconfirmed);
    // Still working, so only the key stops a second Escape.
    rig.herdr.set_screen(&spawned.pane_id, Screen::Working);
    let error = interrupt_it(&mut rig, &board, &quick(), &spawned.run.id).unwrap_err();
    assert!(matches!(error, SpawnError::Refused(_)), "{error}");
    assert!(error.to_string().contains("sent already"), "{error}");
    let escapes = rig
        .herdr
        .calls()
        .iter()
        .filter(|c| c.starts_with("send_escape"))
        .count();
    assert_eq!(escapes, 1, "{:?}", rig.herdr.calls());

    // A new key is a new press.
    let again = Ctx::new(Actor::Board).with_key("01KEYEDINTERRUPT0000000001");
    interrupt_it(&mut rig, &again, &quick(), &spawned.run.id).unwrap();
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

fn pause(rig: &mut Rig, run: &str) {
    let until = Timestamp::now().plus_minutes(60);
    rig.fleet
        .store
        .run_pause(&owner(), run, until, Some("waiting on a review"))
        .unwrap();
    assert!(rig.run(run).pause_until.is_some());
}

/// A manual resume clears the run's pause, whether the agent was still
/// running or Claude had to be resumed in its pane.
#[test]
fn a_manual_resume_clears_the_pause() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    pause(&mut rig, &spawned.run.id);
    let ctx = rig.orchestrator();
    let done = resume_it(&mut rig, &ctx, &quick(), &spawned.run.id).unwrap();
    assert!(done.left_alone);
    assert_eq!(done.run.pause_until, None);
    assert_eq!(rig.run(&spawned.run.id).pause_until, None);
    assert_eq!(rig.events("run.unpaused", &spawned.run.id), 1);
    // A second resume has no pause left to clear.
    resume_it(&mut rig, &ctx, &quick(), &spawned.run.id).unwrap();
    assert_eq!(rig.events("run.unpaused", &spawned.run.id), 1);

    pause(&mut rig, &spawned.run.id);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Shell);
    let claude = boot(&rig, "resume");
    let done = resume_it(&mut rig, &ctx, &patient(), &spawned.run.id).unwrap();
    claude.join().unwrap();
    assert!(!done.left_alone);
    assert_eq!(done.run.pause_until, None);
    assert_eq!(rig.events("run.unpaused", &spawned.run.id), 2);
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

/// The board's Resume session sends one idempotency key. It is
/// `session.resume`'s alone: the pane's bind is a second write and must not
/// reuse it.
#[test]
fn a_keyed_resume_binds_the_new_pane() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.herdr.remove_pane(&spawned.pane_id);
    let claude = boot(&rig, "resume");
    let board = Ctx {
        key: Some("board-resume-1".to_owned()),
        ..Ctx::new(Actor::Board)
    };

    let done = resume_it(&mut rig, &board, &patient(), &spawned.run.id).unwrap();
    claude.join().unwrap();
    let pane = done.pane_id.unwrap();
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
fn relaunch_renders_the_launch_files_again_and_keeps_the_brief() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let scratch = rig.scratch.join(&spawned.run.id);
    let old_tiller = rig.tiller.to_string_lossy().into_owned();
    let first_settings = std::fs::read_to_string(scratch.join("settings.json")).unwrap();
    assert!(first_settings.contains(&old_tiller), "{first_settings}");
    // The role prompt of the first launch was an older one.
    let first_prompt = std::fs::read_to_string(scratch.join("prompt.md")).unwrap();
    let brief = launch::brief_of(&first_prompt).unwrap().to_owned();
    assert!(brief.starts_with("Do the work on"), "{brief}");
    std::fs::write(
        scratch.join("prompt.md"),
        format!("an older role prompt\n\n# Your brief\n\n{brief}\n"),
    )
    .unwrap();
    // tiller is upgraded: its binary now lives somewhere else.
    rig.tiller = PathBuf::from("/home/user/upgraded/bin/tiller");
    let claude = boot(&rig, "startup");
    let ctx = rig.orchestrator();

    let done =
        relaunched(relaunch_it(&mut rig, &ctx, &patient(), &spawned.run.id, note("n")).unwrap());
    claude.join().unwrap().unwrap();
    assert_eq!(done.started, Proof::Confirmed, "{}", done.evidence);

    let settings = std::fs::read_to_string(scratch.join("settings.json")).unwrap();
    assert!(!settings.contains(&old_tiller), "{settings}");
    assert!(
        settings.contains("/home/user/upgraded/bin/tiller hook"),
        "{settings}"
    );
    let prompt = std::fs::read_to_string(scratch.join("prompt.md")).unwrap();
    assert!(!prompt.contains("an older role prompt"), "{prompt}");
    assert_eq!(launch::brief_of(&prompt), Some(brief.as_str()));
    assert_eq!(done.run.id, spawned.run.id);
    // The hash follows the rendered files: the run and the new session
    // carry the new one, its manifest resolves, the old session keeps its own.
    let hash = done.run.config_hash.clone().unwrap();
    assert_ne!(Some(&hash), spawned.run.config_hash.as_ref());
    assert_eq!(done.session.config_hash, Some(hash.clone()));
    let store = rig.fleet.open();
    let manifest: Option<ConfigManifest> =
        find(store.conn(), "config_hash = ?1", &[&hash]).unwrap();
    assert!(manifest.is_some());
    let old: Session = get(store.conn(), "session", &spawned.session.id).unwrap();
    assert_eq!(old.config_hash, spawned.run.config_hash);
    assert_eq!(rig.events("run.reconfigured", &spawned.run.id), 1);
}

#[test]
fn relaunch_follows_the_projects_delivery_in_the_brief_and_the_gh_login_rule() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let scratch = rig.scratch.join(&spawned.run.id);
    let first = std::fs::read_to_string(scratch.join("settings.json")).unwrap();
    assert!(first.contains(crate::roles::GH_LOGIN_RULE), "{first}");
    let first_prompt = std::fs::read_to_string(scratch.join("prompt.md")).unwrap();
    assert!(
        first_prompt.contains("Delivery: direct-pr."),
        "{first_prompt}"
    );
    // The project moves to no-mistakes, which reads the gh login.
    {
        let mut store = rig.fleet.open();
        let project: Project = get(store.conn(), "project", "alpha").unwrap();
        let project = store
            .project_edit(
                &owner(),
                "alpha",
                project.rev,
                ProjectEdit {
                    delivery: Some(Delivery::NoMistakes),
                    ..ProjectEdit::default()
                },
            )
            .unwrap()
            .value;
        store
            .project_stage(&owner(), "alpha", project.rev, Stage::Launched, None)
            .unwrap();
    }
    let claude = boot(&rig, "startup");
    let ctx = rig.orchestrator();
    let done =
        relaunched(relaunch_it(&mut rig, &ctx, &patient(), &spawned.run.id, note("n")).unwrap());
    claude.join().unwrap().unwrap();
    assert_eq!(done.started, Proof::Confirmed, "{}", done.evidence);

    let settings = std::fs::read_to_string(scratch.join("settings.json")).unwrap();
    assert!(
        !settings.contains(crate::roles::GH_LOGIN_RULE),
        "{settings}"
    );
    let prompt = std::fs::read_to_string(scratch.join("prompt.md")).unwrap();
    assert!(prompt.contains("Delivery: no-mistakes."), "{prompt}");
    assert!(!prompt.contains("Delivery: direct-pr."), "{prompt}");
    assert_eq!(prompt.matches("Delivery: ").count(), 1, "{prompt}");
}

#[test]
fn relaunch_refuses_a_run_whose_prompt_holds_no_brief() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    std::fs::write(
        rig.scratch.join(&spawned.run.id).join("prompt.md"),
        "a prompt with nothing after the role\n",
    )
    .unwrap();
    let error = relaunch_it(&mut rig, &owner(), &quick(), &spawned.run.id, note("n")).unwrap_err();
    assert!(error.to_string().contains("holds no brief"), "{error}");
    assert_eq!(rig.events("run.relaunched", &spawned.run.id), 0);
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

fn stop_it(rig: &mut Rig, ctx: &Ctx, waits: &Waits, id: &str) -> Result<Stopped> {
    let session = session(rig, id);
    let world = world!(rig);
    stop_agent(&mut rig.fleet.store, ctx, &world, waits, &session).map(|a| a.value)
}

fn sweep_it(rig: &mut Rig, waits: &Waits) -> Vec<Swept> {
    stop_finished(
        &mut rig.fleet.store,
        &Ctx::new(Actor::Daemon),
        &rig.herdr,
        waits,
    )
    .unwrap()
}

/// A scout run whose Claude is up in its pane, which ends `reported`.
fn scouting(rig: &mut Rig) -> Spawned {
    let card = rig.card(CardKind::Scout, None);
    let spawned = rig.spawn(&card, RunRole::Scout);
    rig.herdr.set_screen(&spawned.pane_id, Screen::Idle);
    rig.fleet.bind(&spawned.session.id, &spawned.pane_id);
    spawned
}

/// A builder run whose PR the land loop merged.
fn landed(rig: &mut Rig) -> Spawned {
    let spawned = running(rig);
    rig.sql(&format!(
        "UPDATE runs SET ended_at = started_at, outcome = 'landed' WHERE id = '{}'",
        spawned.run.id
    ));
    spawned
}

fn typed(rig: &Rig, pane: &str, line: &str) -> bool {
    rig.herdr
        .calls()
        .contains(&format!("send_text {pane} {line}"))
}

#[test]
fn stop_ends_an_ended_runs_agent_and_returns_its_worktree() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    // Cleanup alone never closes a pane that runs an agent.
    let error = rig.cleanup(&spawned.run.id).unwrap_err();
    assert!(error.to_string().contains("still runs"), "{error}");

    let ctx = rig.orchestrator();
    let stopped = stop_it(&mut rig, &ctx, &quick(), &spawned.session.id).unwrap();
    assert!(stopped.stop_proof.contains("no agent"), "{stopped:?}");
    assert!(typed(&rig, &spawned.pane_id, "/exit"));
    assert_eq!(stopped.session.end_reason.as_deref(), Some(STOPPED));
    assert!(stopped.session.ended_at.is_some());
    let cleaned = stopped.cleaned.as_ref().unwrap();
    assert_eq!(cleaned.returned.as_deref(), Some(spawned.worktree.as_str()));
    assert_eq!(cleaned.closed_panes, vec![spawned.pane_id.clone()]);
    assert_eq!(stopped.warning, None);
    assert_eq!(rig.events("run.cleaned", &spawned.run.id), 1);
    assert_eq!(
        rig.pool.returned(),
        vec![(
            PathBuf::from(&spawned.worktree),
            format!("tiller:{}", spawned.run.id)
        )]
    );
}

/// The board's Stop agent sends one idempotency key. It is `session.end`'s
/// alone: the cleanup after it writes too and must not reuse it.
#[test]
fn a_keyed_stop_still_cleans_up() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    let board = Ctx {
        key: Some("board-stop-1".to_owned()),
        ..Ctx::new(Actor::Board)
    };

    let stopped = stop_it(&mut rig, &board, &quick(), &spawned.session.id).unwrap();
    assert_eq!(stopped.warning, None);
    let cleaned = stopped.cleaned.as_ref().unwrap();
    assert_eq!(cleaned.returned.as_deref(), Some(spawned.worktree.as_str()));
    assert_eq!(rig.events("run.cleaned", &spawned.run.id), 1);
}

/// Claude reads an Enter right after `/exit` as a newline, as a Claude
/// relaunched in the same pane does: the line sat typed and the stop was
/// refused as unproven.
#[test]
fn stop_presses_enter_again_while_exit_stays_in_the_composer() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    rig.herdr.enter_as_newline(&spawned.pane_id, 2);

    let stopped = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap();
    assert!(stopped.stop_proof.contains("no agent"), "{stopped:?}");
    assert!(stopped.session.ended_at.is_some());
    let presses = enters_after_exit(&rig, &spawned.pane_id);
    assert_eq!(presses, 3);
    assert!(typed(&rig, &spawned.pane_id, "/exit"));
}

#[test]
fn stop_that_exit_never_leaves_is_refused_after_a_bounded_number_of_presses() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    rig.herdr.enter_as_newline(&spawned.pane_id, 100);

    let error = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap_err();
    assert!(error.to_string().contains("not proven stopped"), "{error}");
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
    let presses = enters_after_exit(&rig, &spawned.pane_id);
    assert_eq!(presses, tiller_herdr::submit::PRESSES as usize);
}

/// The Enters pressed in `pane` after `/exit` was typed.
fn enters_after_exit(rig: &Rig, pane: &str) -> usize {
    let calls = rig.herdr.calls();
    let exit = calls
        .iter()
        .position(|c| *c == format!("send_text {pane} /exit"))
        .unwrap();
    calls[exit..]
        .iter()
        .filter(|c| **c == format!("send_enter {pane}"))
        .count()
}

#[test]
fn stop_keeps_a_worktree_that_holds_work() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let worktree = PathBuf::from(&spawned.worktree);
    std::fs::write(worktree.join("wip.txt"), "draft\n").unwrap();
    rig.done(&spawned.run.id, RunOutcome::Failed).unwrap();

    let stopped = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap();
    assert!(stopped.session.ended_at.is_some());
    assert_eq!(stopped.cleaned, None);
    let warning = stopped.warning.unwrap();
    assert!(warning.contains("uncommitted"), "{warning}");
    assert!(worktree.join("wip.txt").exists());
    assert!(rig.pool.returned().is_empty());
}

#[test]
fn stop_refuses_an_active_run_and_an_outsider() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let error = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("is active"), "{text}");
    assert!(text.contains("tiller done"), "{text}");
    assert!(!typed(&rig, &spawned.pane_id, "/exit"));

    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    let worker = Ctx::new(Actor::Session(spawned.session.id.clone()));
    let error = stop_it(&mut rig, &worker, &quick(), &spawned.session.id).unwrap_err();
    assert!(error.to_string().contains("may not stop"), "{error}");
    assert!(!typed(&rig, &spawned.pane_id, "/exit"));
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
}

#[test]
fn stop_refuses_over_a_draft() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    rig.herdr.type_draft(&spawned.pane_id, "half a thought");

    let error = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap_err();
    assert!(error.to_string().contains("holds a draft"), "{error}");
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
    assert_eq!(rig.events("run.cleaned", &spawned.run.id), 0);
}

#[test]
fn the_daemon_stops_the_idle_agent_of_a_finished_run() {
    let mut rig = Rig::new();
    let reported = scouting(&mut rig);
    rig.done(&reported.run.id, RunOutcome::Reported).unwrap();
    let landed = landed(&mut rig);
    let active = running(&mut rig);
    let failed = running(&mut rig);
    rig.done(&failed.run.id, RunOutcome::Failed).unwrap();

    let swept = sweep_it(&mut rig, &quick());
    assert_eq!(swept.len(), 2, "{swept:?}");
    for finished in [&reported, &landed] {
        let one = swept
            .iter()
            .find(|s| s.session == finished.session.id)
            .unwrap();
        assert!(one.stopped, "{swept:?}");
        assert_eq!(one.run, finished.run.id);
        assert!(typed(&rig, &finished.pane_id, "/exit"));
        let ended = session(&rig, &finished.session.id);
        assert_eq!(ended.end_reason.as_deref(), Some(RUN_ENDED));
        // Its pane is a bare shell now, for reconcile to close (R15).
        assert_eq!(rig.herdr.fake_pane(&finished.pane_id).screen, Screen::Shell);
    }

    // An active run, and a failed one the orchestrator decides about, are
    // left alone.
    for other in [&active, &failed] {
        assert!(!typed(&rig, &other.pane_id, "/exit"));
        assert!(session(&rig, &other.session.id).ended_at.is_none());
    }
    // A second pass finds nothing to do.
    assert!(sweep_it(&mut rig, &quick()).is_empty());
}

#[test]
fn the_daemon_waits_for_a_finished_agent_to_be_idle() {
    let mut rig = Rig::new();
    let working = scouting(&mut rig);
    rig.done(&working.run.id, RunOutcome::Reported).unwrap();
    rig.herdr.set_screen(&working.pane_id, Screen::Working);
    let drafting = scouting(&mut rig);
    rig.done(&drafting.run.id, RunOutcome::Reported).unwrap();
    rig.herdr.type_draft(&drafting.pane_id, "half a thought");
    let asking = scouting(&mut rig);
    rig.done(&asking.run.id, RunOutcome::Reported).unwrap();
    rig.herdr.set_screen(&asking.pane_id, Screen::Permission);

    let swept = sweep_it(&mut rig, &quick());
    assert_eq!(swept.len(), 3, "{swept:?}");
    assert!(swept.iter().all(|s| !s.stopped), "{swept:?}");
    for spawned in [&working, &drafting, &asking] {
        assert!(!typed(&rig, &spawned.pane_id, "/exit"));
        let calls = rig.herdr.calls();
        assert!(
            !calls.contains(&format!("send_escape {}", spawned.pane_id)),
            "{calls:?}"
        );
        assert!(session(&rig, &spawned.session.id).ended_at.is_none());
    }
    assert_eq!(
        rig.herdr.fake_pane(&drafting.pane_id).composer,
        "half a thought"
    );

    // Once the turn ends, the next pass stops it.
    rig.herdr.set_screen(&working.pane_id, Screen::Idle);
    let swept = sweep_it(&mut rig, &quick());
    let done = swept
        .iter()
        .find(|s| s.session == working.session.id)
        .unwrap();
    assert!(done.stopped, "{swept:?}");
    assert!(session(&rig, &working.session.id).ended_at.is_some());
}

#[test]
fn the_daemon_types_exit_once_into_an_agent_that_ignores_it() {
    let mut rig = Rig::new();
    let stubborn = scouting(&mut rig);
    rig.done(&stubborn.run.id, RunOutcome::Reported).unwrap();
    rig.herdr.ignore_exit(&stubborn.pane_id);

    let swept = sweep_it(&mut rig, &quick());
    let [first] = swept.as_slice() else {
        panic!("{swept:?}")
    };
    assert!(!first.stopped && first.exit_sent, "{first:?}");
    assert!(first.evidence.contains("not proven stopped"), "{first:?}");
    assert!(session(&rig, &stubborn.session.id).ended_at.is_none());

    // The `exit_sent` observation outlives the daemon, so no later pass,
    // after a restart too, types `/exit` again.
    assert!(sweep_it(&mut rig, &quick()).is_empty());
    let exits = rig
        .herdr
        .calls()
        .iter()
        .filter(|c| **c == format!("send_text {} /exit", stubborn.pane_id))
        .count();
    assert_eq!(exits, 1);
}

#[test]
fn the_daemon_leaves_an_agent_a_message_is_waiting_for() {
    let mut rig = Rig::new();
    let waiting = scouting(&mut rig);
    rig.done(&waiting.run.id, RunOutcome::Reported).unwrap();
    rig.fleet
        .store
        .outbox_enqueue(
            &owner(),
            None,
            tiller_core::state::outbox::NewMessage {
                target_session_id: waiting.session.id.clone(),
                kind: tiller_core::domain::OutboxKind::Steer,
                body: "one more thing".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap();

    // Delivery types `tiller inbox <id>` into the same composer, so the
    // agent is left alone until its message is delivered and acknowledged.
    let left = |rig: &mut Rig| {
        let swept = sweep_it(rig, &quick());
        assert_eq!(swept.len(), 1);
        assert!(!swept[0].stopped && !swept[0].exit_sent && !swept[0].failed);
        assert!(
            swept[0].evidence.contains("message waits"),
            "{}",
            swept[0].evidence
        );
        assert!(!typed(rig, &waiting.pane_id, "/exit"));
    };
    left(&mut rig);
    // Typed but not yet acknowledged, the message is still the agent's turn.
    rig.sql("UPDATE outbox SET state = 'delivered'");
    left(&mut rig);
}

#[test]
fn the_daemon_never_types_into_a_pane_another_session_now_holds() {
    let mut rig = Rig::new();
    let finished = scouting(&mut rig);
    rig.done(&finished.run.id, RunOutcome::Reported).unwrap();
    // The agent died without its hook, and herdr gave the closed pane's id
    // to another workspace, whose agent now sits idle there.
    rig.herdr
        .report_tokens(
            &finished.pane_id,
            &std::collections::BTreeMap::from([("session".into(), "01OTHERSESSION".into())]),
        )
        .unwrap();

    let swept = sweep_it(&mut rig, &quick());
    assert_eq!(swept.len(), 1);
    assert!(swept[0].stopped && !swept[0].exit_sent);
    assert!(
        swept[0].evidence.contains("01OTHERSESSION"),
        "{}",
        swept[0].evidence
    );
    assert!(!typed(&rig, &finished.pane_id, "/exit"));
}

#[test]
fn stop_never_types_into_a_pane_the_next_attempt_holds() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let claude = boot(&rig, "startup");
    let request = RelaunchRequest {
        note: "start over".into(),
        new_attempt: true,
        brief: Some("Do it again.".into()),
        asked: None,
    };
    let ctx = rig.orchestrator();
    let done =
        relaunched(relaunch_it(&mut rig, &ctx, &patient(), &spawned.run.id, request).unwrap());
    claude.join().unwrap();
    assert_eq!(
        done.session.pane_id.as_deref(),
        Some(spawned.pane_id.as_str())
    );
    let exits = |rig: &Rig| {
        rig.herdr
            .calls()
            .iter()
            .filter(|c| **c == format!("send_text {} /exit", spawned.pane_id))
            .count()
    };
    let before = exits(&rig);

    // The old session's pane now holds the new attempt's agent.
    let stopped = stop_it(&mut rig, &owner(), &quick(), &spawned.session.id).unwrap();
    assert!(
        stopped.stop_proof.contains(&done.session.id),
        "{}",
        stopped.stop_proof
    );
    assert_eq!(exits(&rig), before);
    assert!(session(&rig, &done.session.id).ended_at.is_none());
}

#[test]
fn the_live_orchestrator_stops_an_ended_run_it_inherited() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    // An ended run is never reowned: its supervisor stays a session that is
    // not the live orchestrator.
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE runs SET supervisor_session_id = ?1 WHERE id = ?2",
            [&spawned.session.id, &spawned.run.id],
        )
        .unwrap();

    let ctx = rig.orchestrator();
    let stopped = stop_it(&mut rig, &ctx, &quick(), &spawned.session.id).unwrap();
    assert!(session(&rig, &stopped.session.id).ended_at.is_some());
}

fn delete_it(rig: &mut Rig, waits: &Waits) -> Result<ProjectDeleted> {
    delete_project(&mut rig.fleet.store, &owner(), &rig.herdr, waits, "alpha").map(|a| a.value)
}

#[test]
fn delete_stops_the_projects_agents_then_deletes_it() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    let card: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT c.display_id FROM cards c JOIN runs r ON r.card_id = c.id WHERE r.id = ?1",
            [&spawned.run.id],
            |row| row.get(0),
        )
        .unwrap();

    let deleted = delete_it(&mut rig, &quick()).unwrap();
    assert!(typed(&rig, &spawned.pane_id, "/exit"));
    let builder = deleted
        .agents
        .stopped
        .iter()
        .find(|stopped| stopped.agent.session.as_deref() == Some(spawned.session.id.as_str()))
        .unwrap();
    assert!(builder.stop_proof.is_some(), "{deleted:?}");
    assert!(
        deleted
            .agents
            .stopped
            .iter()
            .any(|stopped| stopped.agent.session.as_deref()
                == Some(rig.fleet.orchestrator_session.as_str())),
        "{deleted:?}"
    );
    assert_eq!(deleted.agents.cancelled_cards, vec![card]);
    assert!(deleted.deletion.removed.contains_key("sessions"));
    assert!(matches!(
        get::<Project>(rig.fleet.store.conn(), "project", "alpha"),
        Err(tiller_core::Error::NotFound { .. })
    ));
}

#[test]
fn delete_refuses_naming_the_agent_that_did_not_stop() {
    let mut rig = Rig::new();
    let spawned = running(&mut rig);
    // `/exit` never leaves the composer, so the stop is never proven.
    rig.herdr.enter_as_newline(&spawned.pane_id, 100);

    let error = delete_it(&mut rig, &quick()).unwrap_err().to_string();
    assert!(error.contains(&spawned.session.id), "{error}");
    assert!(error.contains("builder"), "{error}");
    assert!(error.contains("was kept"), "{error}");
    get::<Project>(rig.fleet.store.conn(), "project", "alpha").unwrap();
    assert!(session(&rig, &spawned.session.id).ended_at.is_none());
    assert!(rig.run(&spawned.run.id).ended_at.is_none());
}

#[test]
fn retire_stops_the_orchestrator_only_when_asked() {
    for stop_agents in [false, true] {
        let mut rig = Rig::new();
        let orchestrator = rig.fleet.orchestrator_session.clone();
        let retired = retire_project(
            &mut rig.fleet.store,
            &owner(),
            &rig.herdr,
            &quick(),
            "alpha",
            stop_agents,
        )
        .unwrap();
        assert!(retired.value.project.retired_at.is_some());
        let stopped = usize::from(stop_agents);
        assert_eq!(retired.value.agents.stopped.len(), stopped, "{retired:?}");
        assert_eq!(session(&rig, &orchestrator).ended_at.is_some(), stop_agents);
    }
}

#[test]
fn retire_of_a_retired_project_stops_nothing() {
    let mut rig = Rig::new();
    let orchestrator = rig.fleet.orchestrator_session.clone();
    let retire = |rig: &mut Rig, stop_agents| {
        retire_project(
            &mut rig.fleet.store,
            &owner(),
            &rig.herdr,
            &quick(),
            "alpha",
            stop_agents,
        )
        .unwrap()
    };
    retire(&mut rig, false);

    let again = retire(&mut rig, true);
    assert!(again.value.agents.stopped.is_empty(), "{again:?}");
    assert!(
        again.value.agents.warnings[0].contains("already retired"),
        "{again:?}"
    );
    assert!(session(&rig, &orchestrator).ended_at.is_none());
    assert!(!typed(&rig, &rig.fleet.orchestrator_pane, "/exit"));
}

/// The stops wait on herdr between short writes of their own, so another
/// request writes while a delete's agent is still exiting.
#[test]
fn delete_holds_no_store_write_while_an_agent_stops() {
    let mut rig = Rig::new();
    running(&mut rig);
    let db = rig.fleet.db.clone();
    let landed = std::cell::RefCell::new(Vec::new());
    let other_write = |call: &str, text: &str| {
        if call != "send_text" || text != "/exit" {
            return;
        }
        let mut other = Store::open_current(&db).unwrap();
        // Fails at once, rather than waiting, if the delete holds the write.
        other.conn().pragma_update(None, "busy_timeout", 0).unwrap();
        let rev = get::<Project>(other.conn(), "project", "fleet")
            .unwrap()
            .rev;
        let edit = ProjectEdit {
            name: Some("Fleet, edited".into()),
            ..Default::default()
        };
        landed
            .borrow_mut()
            .push(other.project_edit(&owner(), "fleet", rev, edit).map(|_| ()));
    };
    let hooked = crate::spawn::tests::Hooked {
        herdr: &rig.herdr,
        hook: &other_write,
    };

    delete_project(&mut rig.fleet.store, &owner(), &hooked, &quick(), "alpha").unwrap();
    let landed = landed.into_inner();
    assert!(!landed.is_empty(), "no agent was asked to exit");
    for write in landed {
        write.unwrap();
    }
}
