//! `tiller agent interrupt`, `resume`, `relaunch` and `stop` (T2.5, T4.5): the
//! pane half lives in `tiller_agents::control`; this parses the call and
//! picks the tools. The board's Resume, Relaunch and Stop run the same
//! `*_run` and `stop_session` the commands do, through the daemon.

use std::path::PathBuf;

use clap::Args;
use serde_json::json;
use tiller_agents::control::{self, RelaunchOutcome, RelaunchRequest, Resumed, Stopped, Waits};
use tiller_agents::spawn::{SpawnError, launch, readings};
use tiller_core::domain::{Effort, Run, Session};
use tiller_core::{Applied, Ctx, Store};

use super::spawn::{Tools, failure, read_brief};
use super::{KeyArg, resolve_run, resolve_session};
use crate::app::{App, Done, Outcome};

#[derive(Args, Debug)]
pub struct Relaunch {
    /// The run: a ULID or short form, `<display id> <role>`, or a run name like
    /// alpha-42-build
    pub run: String,
    /// The progress the new session starts from
    #[arg(long)]
    pub note: String,
    /// End the run (`relaunched`) and start attempt n+1 in the same worktree
    #[arg(long, requires = "brief_file")]
    pub new_attempt: bool,
    /// The new attempt's brief; needed with --new-attempt
    #[arg(long)]
    pub brief_file: Option<PathBuf>,
    /// The new attempt's model; only raises the dispatch rule unless the
    /// owner asks
    #[arg(long, requires_all = ["effort", "new_attempt"])]
    pub model: Option<String>,
    /// low, medium, high, xhigh or max; needs --model and --new-attempt
    #[arg(long, requires_all = ["model", "new_attempt"])]
    pub effort: Option<Effort>,
    #[command(flatten)]
    pub key: KeyArg,
}

/// The run's worktree is the disk a launch fills.
pub(super) fn disk_free_mb(run: &Run) -> Option<i64> {
    run.worktree_path
        .as_deref()
        .and_then(|path| readings::disk_free_mb(std::path::Path::new(path)))
}

/// A warning when the command could not see its effect.
fn unseen(what: &str, proof: control::Proof, evidence: &str) -> Option<String> {
    (proof == control::Proof::Unconfirmed).then(|| format!("{what} is unconfirmed: {evidence}"))
}

/// `tiller agent interrupt <run>`. Not idempotent: a second Escape reaches
/// whatever the agent does next, so it refuses unless the session works.
pub fn interrupt(app: &App, run: &str) -> Done {
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let run = resolve_run(&store, run)?;
    let world = tools.world(None, None);
    let applied = control::interrupt(
        &mut store,
        &app.ctx(),
        world.holder(),
        &Waits::default(),
        &run,
    )
    .map_err(failure)?;
    let warning = unseen("the cancel", applied.value.cancel, &applied.value.evidence);
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// What a control did, and what it warns of.
pub(super) type Warned<T> = Result<(Applied<T>, Option<String>), SpawnError>;

/// `tiller agent resume <run>`: `claude --resume` in the run's worktree; a
/// live session the holder shows running is left alone.
pub fn resume(app: &App, run: &str) -> Done {
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let run = resolve_run(&store, run)?;
    let (applied, warning) = resume_run(&tools, &mut store, &app.ctx(), &run).map_err(failure)?;
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// The resume `tiller agent resume` and the board's Resume both run.
pub(super) fn resume_run(
    tools: &Tools,
    store: &mut Store,
    ctx: &Ctx,
    run: &Run,
) -> Warned<Resumed> {
    let world = tools.world(readings::ram_free_mb(), disk_free_mb(run));
    let applied = control::resume(store, ctx, &world, &Waits::default(), run)?;
    let warning = match applied.value.started {
        Some(proof) => unseen("the start", proof, &applied.value.evidence),
        None => None,
    }
    .or_else(|| applied.value.warning.clone());
    Ok((applied, warning))
}

/// `tiller agent relaunch <run> --note <t>`: stops the live session, proven,
/// and starts a new one in the same worktree, on the same run or as a new
/// attempt.
pub fn relaunch(app: &App, args: Relaunch) -> Done {
    let brief = args.brief_file.as_deref().map(read_brief).transpose()?;
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let run = resolve_run(&store, &args.run)?;
    let normalized = json!({
        "run": run.id,
        "note": args.note,
        "new_attempt": args.new_attempt,
        "brief": brief,
        "model": args.model,
        "effort": args.effort,
    });
    let keyed = app.key(&store, "run.relaunch", &normalized, &args.key.key)?;
    let (applied, warning) = relaunch_run(
        &tools,
        &mut store,
        &app.ctx(),
        Some(&keyed.key),
        &run,
        RelaunchRequest {
            note: args.note,
            new_attempt: args.new_attempt,
            brief,
            asked: args.model.zip(args.effort),
        },
    )
    .map_err(failure)?;
    Ok(Outcome::applied(applied)
        .with_key(&keyed)
        .with_warning(warning))
}

/// The relaunch `tiller agent relaunch` and the board's Relaunch both run.
pub(super) fn relaunch_run(
    tools: &Tools,
    store: &mut Store,
    ctx: &Ctx,
    key: Option<&str>,
    run: &Run,
    request: RelaunchRequest,
) -> Warned<RelaunchOutcome> {
    let world = tools.world(readings::ram_free_mb(), disk_free_mb(run));
    let applied = control::relaunch(store, ctx, key, &world, &Waits::default(), run, request)?;
    let warning = match &applied.value {
        RelaunchOutcome::Relaunched(done) => {
            unseen("the start", done.started, &done.evidence).or_else(|| done.warning.clone())
        }
        RelaunchOutcome::Replayed { .. } => None,
    };
    Ok((applied, warning))
}

/// The brief a run was launched on, from the run scratch: the board's new
/// attempt starts on it, since the board takes no brief file.
pub(super) fn run_brief(tools: &Tools, run: &Run) -> Result<String, SpawnError> {
    let scratch = tools.scratch_root().join(&run.id);
    launch::brief_in(&scratch)
        .filter(|brief| !brief.is_empty())
        .ok_or_else(|| {
            SpawnError::Refused(format!(
                "{} holds no brief, so a new attempt has nothing to start on; run tiller agent relaunch {} --new-attempt --brief-file <f> from a terminal; nothing was changed",
                scratch.display(),
                run.id
            ))
        })
}

/// `tiller agent stop <run|session>`: stops the agent of an ended run, or
/// an orchestrator, proven, then cleans up after it. A cleanup that refuses
/// is a warning: the agent is stopped either way.
pub fn stop(app: &App, target: &str) -> Done {
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let session = resolve_session(&store, target)?;
    let (applied, warning) =
        stop_session(&tools, &mut store, &app.ctx(), &session).map_err(failure)?;
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// The stop `tiller agent stop` and the board's Stop agent both run.
pub(super) fn stop_session(
    tools: &Tools,
    store: &mut Store,
    ctx: &Ctx,
    session: &Session,
) -> Warned<Stopped> {
    let world = tools.world(None, None);
    let applied = control::stop_agent(store, ctx, &world, &Waits::default(), session)?;
    let warning = applied.value.warning.clone();
    Ok((applied, warning))
}
