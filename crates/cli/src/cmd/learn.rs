//! `tiller retro ...` and `tiller learn config`: retros and the curator
//! (`docs/spec/memory.md` section 5, `docs/design/learning-loop.md`). The
//! record work is in `tiller_core::state::learning`; this module adds the
//! curator launch.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_agents::curator;
use tiller_core::domain::{
    Attention, Card, ConfigManifest, Event, Project, Retro, RetroOutcome, Review, Run, Session,
};
use tiller_core::state::learning::{self, CURATOR_MODEL, CURATOR_TIMEOUT_MINUTES, Finish};
use tiller_core::state::memory::NewNote;
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Store};
use tiller_panes::process::{self, Interrupt};

use super::KeyArg;
use super::spawn::Tools;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum RetroCmd {
    /// Schedule a retro for every closed card without one
    Sweep,
    /// List retros
    List {
        /// Only retros that still wait for a result
        #[arg(long)]
        pending: bool,
        /// Only this project's retros (its id)
        #[arg(long)]
        project: Option<String>,
    },
    /// A retro with the record it was decided from: the curator's brief
    Show {
        /// A retro id, or a card reference for its newest retro
        retro: String,
    },
    /// Launch the curator on a pending full retro
    Run(RetroRun),
    /// Finish a full retro: nothing, or one note for the pending queue
    Finish(RetroFinish),
}

#[derive(Args, Debug)]
pub struct RetroRun {
    /// A retro id, or a card reference for its newest retro
    pub retro: String,
    /// Write the launch files in <retro>.dry-run beside the run's scratch directory and print
    /// the command without running it
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct RetroFinish {
    /// A retro id, or a card reference for its newest retro
    pub retro: String,
    /// Nothing worth saving: the normal result
    #[arg(long, group = "verdict")]
    pub nothing: bool,
    /// The one note's kind: `convention`, `decision`, `gotcha` or `how-to`
    #[arg(long, group = "verdict", requires_all = ["title", "body"])]
    pub kind: Option<String>,
    /// The one note's title: one line that says the fact
    #[arg(long, requires = "kind")]
    pub title: Option<String>,
    /// The one note's detail
    #[arg(long, requires = "kind")]
    pub body: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum LearnCmd {
    /// The manifest behind a config hash, or the parts in which two differ
    Config {
        /// A config hash, as `tiller config hash` or a run's record shows it
        hash: String,
        /// A second hash; shows the parts in which the two differ
        other: Option<String>,
    },
}

/// A database error as the core reports it.
fn db(error: rusqlite::Error) -> Failure {
    Failure::from(tiller_core::Error::from(error))
}

fn count(conn: &rusqlite::Connection, sql: &str, param: &str) -> Result<i64, Failure> {
    conn.query_row(sql, [param], |row| row.get(0)).map_err(db)
}

/// `retro show` and the curator's brief carry the owner's
/// exact answers, so a fleet session never gets them (learning-loop.md,
/// section 10).
fn refuse_fleet(app: &App, what: &str) -> Result<(), Failure> {
    match app.actor() {
        Actor::Session(id) => Err(Failure::forbidden(format!(
            "{what} is for the owner and the curator; session {id} may not"
        ))),
        _ => Ok(()),
    }
}

pub fn retro(app: &App, cmd: RetroCmd) -> Done {
    match cmd {
        RetroCmd::Sweep => sweep(app),
        RetroCmd::List { pending, project } => {
            refuse_fleet(app, "tiller retro list")?;
            list_retros(app, pending, project)
        }
        RetroCmd::Show { retro } => {
            refuse_fleet(app, "tiller retro show")?;
            let store = app.read()?;
            let retro = learning::resolve_retro(store.conn(), &retro)?;
            Ok(Outcome::read(brief(&store, &retro)?))
        }
        RetroCmd::Run(args) => run_curator(app, args),
        RetroCmd::Finish(args) => finish(app, args),
    }
}

fn sweep(app: &App) -> Done {
    let mut store = app.write()?;
    let applied = store.learning_sweep(&app.ctx())?;
    Ok(Outcome::applied(applied))
}

fn list_retros(app: &App, pending: bool, project: Option<String>) -> Done {
    let store = app.read()?;
    let retros: Vec<Retro> = list(
        store.conn(),
        "(?1 = 0 OR state = 'pending') AND (?2 IS NULL OR project_id = ?2) ORDER BY created_at DESC, id DESC LIMIT 200",
        &[&(pending as i64), &project],
    )?;
    Ok(Outcome::read(json!({"retros": retros})))
}

/// The record the curator reads (learning-loop.md, section 4): the card,
/// the counts, every run with its transcript path, the attention items with
/// the owner's exact answers, the reviews, the events in order, the evidence
/// already on the card and the earlier retros.
fn brief(store: &Store, retro: &Retro) -> Result<Value, Failure> {
    let conn = store.conn();
    let card: Card = get(conn, "card", &retro.card_id)?;
    let project: Project = get(conn, "project", &card.project_id)?;
    let runs: Vec<Run> = list(conn, "card_id = ?1 ORDER BY started_at, id", &[&card.id])?;
    let mut run_rows = Vec::new();
    for run in &runs {
        let sessions: Vec<Session> = list(conn, "run_id = ?1 ORDER BY started_at, id", &[&run.id])?;
        run_rows.push(json!({
            "id": run.id,
            "role": run.role,
            "attempt": run.attempt,
            "model": run.model,
            "effort": run.effort,
            "started_at": run.started_at,
            "ended_at": run.ended_at,
            "outcome": run.outcome,
            "outcome_note": run.outcome_note,
            "last_status_line": run.last_status_line,
            "config_hash": run.config_hash,
            "transcripts": sessions.iter().filter_map(|s| s.transcript_path.clone()).collect::<Vec<_>>(),
        }));
    }
    let attention: Vec<Attention> = list(conn, "card_id = ?1 ORDER BY opened_at", &[&card.id])?;
    let reviews: Vec<Review> = list(conn, "card_id = ?1 ORDER BY recorded_at", &[&card.id])?;
    // The newest 300 events, oldest first: the tail holds the close and
    // any fix card, which is what a retro is about.
    let mut events: Vec<Event> = list(
        conn,
        "card_id = ?1 ORDER BY seq DESC LIMIT 300",
        &[&card.id],
    )?;
    events.reverse();
    let events_total = count(
        conn,
        "SELECT COUNT(*) FROM events WHERE card_id = ?1",
        &card.id,
    )?;
    let earlier: Vec<Retro> = list(
        conn,
        "card_id = ?1 AND id <> ?2 ORDER BY created_at",
        &[&card.id, &retro.id],
    )?;
    let fix_cards: Vec<Card> = list(conn, "fixes_card_id = ?1 ORDER BY created_at", &[&card.id])?;
    Ok(json!({
        "retro": retro,
        "card": {
            "id": card.id,
            "tag": super::tag(&card.display_id),
            "title": card.title,
            "kind": card.kind,
            "size": card.size,
            "category": card.category,
            "body": card.body,
            "acceptance": card.acceptance,
            "outcome": card.outcome,
            "created_at": card.created_at,
            "closed_at": card.closed_at,
            "fixes_card_id": card.fixes_card_id,
            "project": project.id,
        },
        "fix_cards": fix_cards.iter().map(|c| json!({"id": c.id, "tag": super::tag(&c.display_id), "title": c.title})).collect::<Vec<_>>(),
        "runs": run_rows,
        "attention": attention,
        "reviews": reviews,
        "events": events.iter().map(|e| json!({"seq": e.seq, "at": e.at, "kind": e.kind, "entity": e.entity, "entity_id": e.entity_id, "actor": e.actor})).collect::<Vec<_>>(),
        "events_truncated": events_total > events.len() as i64,
        "earlier_retros": earlier,
        "finish": {
            "nothing": format!("tiller retro finish {} --nothing", retro.id),
            "noted": format!("tiller retro finish {} --kind <kind> --title <one line> --body <detail>", retro.id),
        },
    }))
}

/// A fresh UUID v4 for `claude --session-id`, so the curator's harness
/// session id is on the retro before the run starts.
fn new_session_id() -> Result<String, Failure> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        Failure::new(
            "internal",
            1,
            format!("no randomness for a session id: {error}"),
        )
    })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

fn run_curator(app: &App, args: RetroRun) -> Done {
    app.require_owner("tiller retro run")
        .or_else(|error| match app.actor() {
            Actor::Daemon => Ok(()),
            _ => Err(error),
        })?;
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let retro = learning::resolve_retro(store.conn(), &args.retro)?;
    if args.dry_run {
        // A dry run claims nothing but is refused by the same rule.
        learning::check_start(&retro, app.ctx().now, CURATOR_TIMEOUT_MINUTES)?;
        let session_id = new_session_id()?;
        // Its launch goes beside the real scratch directory, which may hold
        // an unfinished run's `output.json`: the warning sends the owner
        // there first.
        let scratch = scratch_of(&tools, &retro.id).with_file_name(format!("{}.dry-run", retro.id));
        let launch = prepare(&tools, &store, &retro, &session_id, &scratch)?;
        return Ok(Outcome::read(json!({
            "retro": retro.id,
            "argv": launch.argv,
            "env": launch.env,
            "prompt": launch.prompt,
            "settings": launch.settings,
            "dry_run": true,
        })));
    }
    // In its own group the curator no longer gets the terminal's Ctrl-C, so
    // tiller catches it and stops the group itself. Caught before the claim,
    // so no signal can end tiller and leave the curator running.
    let interrupt = Interrupt::catch().map_err(|error| {
        Failure::new(
            "internal",
            1,
            format!("cannot watch for interrupts: {error}"),
        )
    })?;
    let claimed = claim(&tools, &mut store, &app.ctx(), &retro.id)?;
    let ran = launch(&claimed, &interrupt);
    // The end is its own write, at the time the run ended: the claim's
    // context holds the claim's time and key.
    let ctx = app.ctx().unkeyed();
    let ran = match ran {
        Ok(ran) => ran,
        Err(failure) => {
            let id = &claimed.retro.value.id;
            let what = "the curator could not be started";
            return Err(end_unstarted(&mut store, &ctx, id, what, failure).0);
        }
    };
    let ended = end(
        &mut store,
        &ctx,
        &claimed,
        &ran,
        "tiller retro run was interrupted",
    )?;
    let retro = ended.retro;
    Ok(Outcome::read(json!({
        "retro": retro,
        "exit": ran.exit,
        "session_id": claimed.session_id,
        "output": ended.output,
    }))
    .with_warning((retro.outcome == Some(RetroOutcome::Unfinished)).then(|| {
        format!(
            "the curator did not finish; once the cause is fixed, run `tiller retro run {}` again",
            retro.id
        )
    })))
}

/// Writes the curator's launch files for `retro` in a fresh `scratch`
/// directory: a second run of the same retro gets its own.
fn prepare(
    tools: &Tools,
    store: &Store,
    retro: &Retro,
    session_id: &str,
    scratch: &Path,
) -> Result<curator::Launch, Failure> {
    let brief = brief(store, retro)?;
    let brief_text = format!(
        "Retro `{}` of card {}.\n\n```json\n{}\n```\n",
        retro.id,
        brief["card"]["tag"].as_str().unwrap_or(""),
        serde_json::to_string_pretty(&brief)?
    );
    if scratch.exists() {
        let _ = std::fs::remove_dir_all(scratch);
    }
    let mut launch = curator::write(&curator::LaunchSpec {
        retro_id: &retro.id,
        scratch,
        tiller: tools.tiller(),
        tiller_home: tools.tiller_home(),
        claude: &tools.config.spawn.claude,
        model: CURATOR_MODEL,
        brief: &brief_text,
        claude_config: &tiller_agents::spawn::trust::ClaudeConfig::from_env(),
    })
    .map_err(super::spawn::failure)?;
    launch
        .argv
        .extend(["--session-id".to_owned(), session_id.to_owned()]);
    Ok(launch)
}

fn scratch_of(tools: &Tools, retro: &str) -> PathBuf {
    tools.scratch_root().join("retros").join(retro)
}

/// A retro claimed for a curator run, with the curator's launch written.
pub(crate) struct Claimed {
    /// The claim: the retro reset if it ended `unfinished`, then started.
    pub retro: tiller_core::Applied<Retro>,
    session_id: String,
    scratch: PathBuf,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    timeout_minutes: u64,
}

/// Claims `retro` for a curator run and writes the curator's launch, as
/// `tiller retro run` does. The retro is claimed before its scratch
/// directory is touched, so a run refused here (a curator is still running
/// in that directory) changes nothing. A claimed retro whose curator cannot
/// be prepared ends `unfinished`, so it can start again at once.
pub(crate) fn claim(
    tools: &Tools,
    store: &mut Store,
    ctx: &tiller_core::Ctx,
    retro: &str,
) -> Result<Claimed, ClaimError> {
    let session_id = new_session_id()?;
    let claimed = store
        .retro_start(ctx, retro, Some(&session_id), CURATOR_TIMEOUT_MINUTES)
        .map_err(ClaimError::State)?;
    if claimed.replayed {
        // A replayed key claimed this retro before. Its run was launched
        // then, so nothing is prepared or launched again: preparing would
        // clear the scratch directory a curator may still be using.
        return Err(Failure::refused(format!(
            "retro {} was already claimed under this key; it is not run again",
            claimed.value.id
        ))
        .into());
    }
    let scratch = scratch_of(tools, &claimed.value.id);
    let launch = match prepare(tools, store, &claimed.value, &session_id, &scratch) {
        Ok(launch) => launch,
        Err(failure) => {
            // The end is its own write, at the time it ended. The claim
            // stored the caller's key, so the end is unkeyed: the same key
            // on another write would be refused.
            let end = tiller_core::Ctx {
                now: tiller_core::Timestamp::now(),
                ..ctx.unkeyed()
            };
            let what = "the curator could not be prepared";
            let id = &claimed.value.id;
            let notes = format!(
                "{}; {id} stays claimed until its timeout passes",
                note(what, &failure)
            );
            return Err(match end_unstarted(store, &end, id, what, failure) {
                (failure, None) => failure.into(),
                (failure, Some(error)) => ClaimError::Unended {
                    error: Box::new(error),
                    failure,
                    notes,
                },
            });
        }
    };
    Ok(Claimed {
        scratch,
        retro: claimed,
        session_id,
        argv: launch.argv,
        env: launch.env,
        // The curator is cut after at least a minute.
        timeout_minutes: u64::try_from(CURATOR_TIMEOUT_MINUTES.max(1)).unwrap_or(1),
    })
}

/// Why a claim failed: the store's own error, which the daemon checks for
/// damage, or any other refusal.
pub(crate) enum ClaimError {
    State(tiller_core::Error),
    Refused(Failure),
    /// The run could not be prepared and its retro could not be ended
    /// `unfinished` either: `failure` names both, for the CLI. `error` is the
    /// store's own, which the daemon checks for damage, and `notes` the
    /// cause and hint it shows after that error.
    Unended {
        error: Box<tiller_core::Error>,
        failure: Failure,
        notes: String,
    },
}

impl From<Failure> for ClaimError {
    fn from(failure: Failure) -> Self {
        Self::Refused(failure)
    }
}

impl From<ClaimError> for Failure {
    fn from(error: ClaimError) -> Self {
        match error {
            ClaimError::State(error) => error.into(),
            ClaimError::Refused(failure) | ClaimError::Unended { failure, .. } => failure,
        }
    }
}

/// Runs a claimed retro's curator until it exits, its timeout passes or
/// `interrupt` is caught. A curator that cannot be started is an error;
/// the caller ends the retro `unfinished`.
pub(crate) fn launch(claimed: &Claimed, interrupt: &Interrupt) -> Result<Ran, Failure> {
    run_with_timeout(
        &claimed.argv,
        &claimed.env,
        &claimed.scratch,
        Duration::from_secs(claimed.timeout_minutes * 60),
        interrupt,
    )
}

/// How a curator run ended.
pub(crate) struct Ended {
    /// The retro as the run left it.
    pub retro: Retro,
    /// Where the curator's output is kept.
    output: PathBuf,
}

/// Keeps the curator's output and ends the retro `unfinished` when the
/// curator did not finish it, with why in its learning. `interrupted` says
/// what stopped the run when it was interrupted.
pub(crate) fn end(
    store: &mut Store,
    ctx: &tiller_core::Ctx,
    claimed: &Claimed,
    ran: &Ran,
    interrupted: &str,
) -> Result<Ended, Failure> {
    let id = &claimed.retro.value.id;
    let output = claimed.scratch.join("output.json");
    let _ = std::fs::write(&output, &ran.stdout);
    let after = learning::resolve_retro(store.conn(), id)?;
    if after.state != tiller_core::domain::RetroState::Pending {
        return Ok(Ended {
            retro: after,
            output,
        });
    }
    let note = match (&ran.stopped, ran.exit) {
        (Some(Stopped::Cut), _) => format!(
            "curator cut after {} without finishing the retro",
            match claimed.timeout_minutes {
                1 => "1 minute".to_owned(),
                minutes => format!("{minutes} minutes"),
            }
        ),
        (Some(Stopped::Interrupted), _) => {
            format!("curator stopped: {interrupted} before the retro was finished")
        }
        (Some(Stopped::Lost(error)), _) => {
            format!(
                "curator stopped: waiting for it failed ({error}) before the retro was finished"
            )
        }
        (None, Some(code)) => format!("curator exited {code} without finishing the retro"),
        (None, None) => "curator was killed by a signal without finishing the retro".to_owned(),
    };
    Ok(Ended {
        retro: unfinished(store, ctx, id, &note)?,
        output,
    })
}

/// The learning of a run that never reached its curator: `what` happened,
/// with the cause and its hint, so the owner sees them where the retro is
/// shown, not only in a log.
fn note(what: &str, failure: &Failure) -> String {
    match &failure.hint {
        Some(hint) => format!("{what}: {}; {hint}", failure.message),
        None => format!("{what}: {}", failure.message),
    }
}

/// Ends `retro` `unfinished` after `failure` kept its run from reaching the
/// curator (`what` says how), and returns the failure to report. When that
/// end fails too, the failure keeps its cause and hint, and its message adds
/// the store's error and that the retro stays claimed until its timeout
/// passes; the store's error comes back beside it, for the daemon to check
/// for damage.
fn end_unstarted(
    store: &mut Store,
    ctx: &tiller_core::Ctx,
    retro: &str,
    what: &str,
    mut failure: Failure,
) -> (Failure, Option<tiller_core::Error>) {
    match unfinished(store, ctx, retro, &note(what, &failure)) {
        Ok(_) => (failure, None),
        Err(error) => {
            failure.message = format!(
                "{}; {retro} also could not be ended unfinished ({error}), so it stays claimed until its timeout passes",
                failure.message
            );
            (failure, Some(error))
        }
    }
}

/// Ends a claimed retro whose curator did not finish it as `unfinished`,
/// with `note` as its learning, so the owner can run it again.
pub(crate) fn unfinished(
    store: &mut Store,
    ctx: &tiller_core::Ctx,
    retro: &str,
    note: &str,
) -> tiller_core::Result<Retro> {
    let finished = store.retro_finish(
        ctx,
        None,
        retro,
        Finish {
            outcome: RetroOutcome::Unfinished,
            learning: Some(note.to_owned()),
            note: None,
        },
    )?;
    Ok(finished.value)
}

/// The board's Run again (`learning-loop.md` section 4): claims `retro` as
/// `tiller retro run` does, by the same rules, and returns the run for the
/// daemon to do on a thread of its own. The daemon ends the run, and the
/// curator with all it started, by setting the run's flag when it stops.
pub(crate) fn board_run(
    app: &App,
    store: &mut Store,
    ctx: &tiller_core::Ctx,
    retro: &str,
) -> Result<tiller_daemon::api::CuratorRun, ClaimError> {
    let tools = Tools::load(app)?;
    let claimed = claim(&tools, store, ctx, retro)?;
    let (db, home) = (app.db.clone(), app.home.clone());
    Ok(tiller_daemon::api::CuratorRun {
        claimed: claimed.retro.clone(),
        run: Box::new(move |stop| {
            let interrupt = Interrupt::flag(stop);
            let ran = launch(&claimed, &interrupt);
            let mut store = Store::open_current(&db)
                .map_err(|error| error.to_string())?
                .with_owner_home(home);
            let ctx = tiller_core::Ctx::new(Actor::Daemon);
            match ran {
                Ok(ran) => end(&mut store, &ctx, &claimed, &ran, "the daemon stopped")
                    .map(|ended| ended.retro)
                    .map_err(|failure| failure.message.clone()),
                Err(failure) => {
                    let id = &claimed.retro.value.id;
                    let what = "the curator could not be started";
                    let (failure, _) = end_unstarted(&mut store, &ctx, id, what, failure);
                    Err(failure.message.clone())
                }
            }
        }),
    })
}

/// What a curator run came to.
pub(crate) struct Ran {
    /// The exit code; `None` when stopped or killed by a signal.
    exit: Option<i32>,
    /// Why tiller stopped it, when it did.
    stopped: Option<Stopped>,
    stdout: String,
}

/// Why tiller stopped a curator, with every process it started.
#[derive(Debug, PartialEq, Eq)]
enum Stopped {
    /// The timeout passed.
    Cut,
    /// The interrupt was caught: the owner's Ctrl-C (SIGINT, SIGTERM or
    /// SIGHUP to `tiller retro run`), or the daemon stopping.
    Interrupted,
    /// Waiting for it failed, with this error.
    Lost(String),
}

/// How long a stopped curator's output is read for: its group is killed,
/// so only a process that left the group can keep the pipe open longer.
const STOPPED_OUTPUT_WAIT: Duration = Duration::from_secs(1);

/// Runs the curator in a process group of its own and stops the whole
/// group when the curator exits, at `timeout`, when `interrupt` is caught,
/// or when waiting for it fails, so nothing it started outlives the run
/// (`tiller_panes::process::wait_all`).
fn run_with_timeout(
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    timeout: Duration,
    interrupt: &Interrupt,
) -> Result<Ran, Failure> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Failure::new("internal", 1, "empty curator command"))?;
    let mut command = Command::new(program);
    command
        .args(rest)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    process::own_group(&mut command);
    let mut child = command.spawn().map_err(|error| {
        Failure::unavailable(format!("cannot start {program}: {error}"))
            .hint("set spawn.claude in config.toml")
    })?;
    let mut stdout = child.stdout.take();
    let (sender, reader) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = std::io::Read::read_to_string(out, &mut text);
        }
        let _ = sender.send(text);
    });
    let deadline = Instant::now() + timeout;
    let (exit, stopped) = match process::wait_all(
        &mut child,
        deadline,
        Duration::from_millis(200),
        Some(interrupt),
    ) {
        process::Ended::Exited(status) => (status.code(), None),
        process::Ended::TimedOut => (None, Some(Stopped::Cut)),
        process::Ended::Interrupted => (None, Some(Stopped::Interrupted)),
        process::Ended::Failed(error) => (None, Some(Stopped::Lost(error.to_string()))),
    };
    // A process that left the curator's group can still hold stdout open
    // past its exit, so the read shares the deadline. After a stop the group
    // is gone and the pipe ends at once, so what the curator wrote before it
    // is kept, waiting [`STOPPED_OUTPUT_WAIT`] at most.
    let wait = match stopped {
        None => deadline.saturating_duration_since(Instant::now()),
        Some(_) => STOPPED_OUTPUT_WAIT,
    };
    let stdout = reader.recv_timeout(wait).unwrap_or_default();
    Ok(Ran {
        exit,
        stopped,
        stdout,
    })
}

fn finish(app: &App, args: RetroFinish) -> Done {
    let mut store = app.write()?;
    let retro = learning::resolve_retro(store.conn(), &args.retro)?;
    let finish = match (args.nothing, args.kind, args.title, args.body) {
        (true, ..) => Finish {
            outcome: RetroOutcome::Nothing,
            learning: None,
            note: None,
        },
        (false, Some(kind), Some(title), Some(body)) => Finish {
            outcome: RetroOutcome::Noted,
            learning: None,
            note: Some(NewNote {
                project: None,
                kind,
                title,
                body,
            }),
        },
        _ => {
            return Err(Failure::usage(
                "say how it ended: --nothing, or --kind, --title and --body for one note",
            ));
        }
    };
    let normalized = json!({"retro": retro.id, "finish": finish});
    let keyed = app.key(&store, "retro.finish", &normalized, &args.key.key)?;
    let applied = store.retro_finish(&app.ctx(), Some(&keyed.key), &retro.id, finish)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn learn(app: &App, cmd: LearnCmd) -> Done {
    match cmd {
        LearnCmd::Config { hash, other } => {
            let store = app.read()?;
            let manifest = |hash: &str| -> Result<ConfigManifest, Failure> {
                find(store.conn(), "config_hash = ?1", &[&hash])?.ok_or_else(|| {
                    Failure::from(tiller_core::Error::NotFound {
                        entity: "config manifest",
                        id: hash.to_owned(),
                        candidates: Vec::new(),
                    })
                })
            };
            let a = manifest(&hash)?;
            match other {
                None => Ok(Outcome::read(a)),
                Some(other) => {
                    let b = manifest(&other)?;
                    let diff = learning::manifest_diff(&a.parts, &b.parts);
                    Ok(Outcome::read(json!({
                        "a": a.config_hash,
                        "b": b.config_hash,
                        "differs": diff.keys().collect::<Vec<_>>(),
                        "parts": diff.iter().map(|(k, (x, y))| (k.clone(), json!({"a": x, "b": y}))).collect::<serde_json::Map<_, _>>(),
                    })))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use tiller_panes::testing::{eventually, gone};

    use super::*;

    /// An interrupt nothing raises.
    fn idle() -> Interrupt {
        Interrupt::flag(Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn a_cut_stops_the_curator_and_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        // A curator that starts a child of its own and then hangs.
        let script = format!("sleep 300 & echo $! > '{}'; sleep 300", file.display());
        let argv = ["sh".to_owned(), "-c".to_owned(), script];
        let started = Instant::now();
        let ran =
            run_with_timeout(&argv, &[], dir.path(), Duration::from_millis(500), &idle()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let grandchild = std::fs::read_to_string(&file).unwrap().trim().to_owned();
        let stopped = eventually(|| gone(&grandchild));
        let _ = Command::new("kill").args(["-KILL", &grandchild]).status();
        assert_eq!((ran.exit, ran.stopped), (None, Some(Stopped::Cut)));
        assert!(stopped, "the grandchild {grandchild} outlived the cut");
    }

    #[test]
    fn a_curator_that_exits_keeps_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let argv = [
            "sh".to_owned(),
            "-c".to_owned(),
            "echo '{\"done\":true}'; exit 2".to_owned(),
        ];
        let ran =
            run_with_timeout(&argv, &[], dir.path(), Duration::from_secs(60), &idle()).unwrap();
        assert_eq!((ran.exit, ran.stopped), (Some(2), None));
        assert_eq!(ran.stdout.trim(), "{\"done\":true}");
    }

    /// What a curator wrote before its cut is kept, for the owner to read
    /// before running it again.
    #[test]
    fn a_cut_curator_keeps_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let argv = [
            "sh".to_owned(),
            "-c".to_owned(),
            "echo 'half way'; exec sleep 300".to_owned(),
        ];
        let ran =
            run_with_timeout(&argv, &[], dir.path(), Duration::from_millis(500), &idle()).unwrap();
        assert_eq!(ran.stopped, Some(Stopped::Cut));
        assert_eq!(ran.stdout.trim(), "half way");
    }
}
