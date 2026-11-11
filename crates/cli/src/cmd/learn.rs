//! `tiller retro ...` and `tiller learn ...`: the learning loop
//! (`docs/design/learning-loop.md`, section 10). The record work is in
//! `tiller_core::state::learning`; this module adds the curator launch, the
//! candidate rendering and the holdout run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_agents::curator;
use tiller_agents::holdout::{self, HoldoutError};
use tiller_core::domain::{
    Attention, Card, ChangeSet, ConfigManifest, EvalRun, Event, Project, Retro, RetroOutcome,
    Review, Run, Session,
};
use tiller_core::state::learning::{self, Finish, Learning};
use tiller_core::state::{find, get, list, resolve_id};
use tiller_core::{Actor, Store};
use tiller_herdr::process::{self, Ended, Interrupt};

use super::KeyArg;
use super::admin::load_config;
use super::spawn::Tools;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum RetroCmd {
    /// Schedule a retro for every closed card without one, and move change sets along
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
    /// Finish a full retro: nothing, one learning, or a proposal
    Finish(RetroFinish),
}

#[derive(Args, Debug)]
pub struct RetroRun {
    /// A retro id, or a card reference for its newest retro
    pub retro: String,
    /// Write the launch files and print the command without running it
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
    /// The one learning kept, as evidence
    #[arg(long, group = "verdict")]
    pub learning: Option<String>,
    /// JSON pointers into the record that back the learning
    #[arg(long, requires = "learning")]
    pub evidence: Option<String>,
    /// The proposal filed for it
    #[arg(long, group = "verdict")]
    pub proposed: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum LearnCmd {
    /// Open a change set: one change, or a named bundle of proposals
    Bundle {
        /// The change set's name
        name: String,
        /// The memory proposals in the set, by id; one proposal makes a change of one
        proposals: Vec<String>,
        /// Cards the set is measured over; default `learning.window_cards`
        #[arg(long)]
        window: Option<i64>,
    },
    /// Add a proposal to an open change set
    Add {
        /// The change set, by name or id
        set: String,
        /// The memory proposal to add, by id
        proposal: String,
    },
    /// Close a change set without measuring it
    Abandon {
        /// The change set, by name or id
        set: String,
    },
    /// Run the holdout on a change set's candidate and record the result
    Eval(LearnEval),
    /// The gate's verdict on a proposal: what mem.decide checks
    Gate {
        /// The memory proposal, by id
        proposal: String,
    },
    /// Retros, pending work, change sets and the measuring window
    Status,
    /// The manifest behind a config hash, or the parts in which two differ
    Config {
        /// A config hash, as `tiller config hash` or a run's record shows it
        hash: String,
        /// A second hash; shows the parts in which the two differ
        other: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct LearnEval {
    /// The change set, by name or id
    #[arg(required_unless_present = "proposal")]
    pub set: Option<String>,
    /// A lone proposal: opens a set of one named after it
    #[arg(long, conflicts_with = "set")]
    pub proposal: Option<String>,
    /// Record a holdout result.json made elsewhere instead of running one
    #[arg(long)]
    pub result: Option<PathBuf>,
    /// The rendered candidate text to append, instead of rendering the set's members
    #[arg(long)]
    pub candidate_file: Option<PathBuf>,
    /// Where the run's results go; default a new temp dir
    #[arg(long)]
    pub out: Option<PathBuf>,
}

fn learning_config(app: &App) -> Result<Learning, Failure> {
    Ok(Learning::from_table(&load_config(app)?)?)
}

/// A database error as the core reports it.
fn db(error: rusqlite::Error) -> Failure {
    Failure::from(tiller_core::Error::from(error))
}

fn count(conn: &rusqlite::Connection, sql: &str, param: &str) -> Result<i64, Failure> {
    conn.query_row(sql, [param], |row| row.get(0)).map_err(db)
}

/// `retro show` and the curator's brief carry evidence rows and the owner's
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
    let config = learning_config(app)?;
    let mut store = app.write()?;
    let applied = store.learning_sweep(&app.ctx(), &config)?;
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
    let evidence: Vec<Value> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, ref, excerpt, captured_at, captured_by FROM evidence
                 WHERE card_id = ?1 ORDER BY captured_at",
            )
            .map_err(db)?;
        let rows = stmt
            .query_map([&card.id], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "ref": row.get::<_, Value>(2)?,
                    "excerpt": row.get::<_, Option<String>>(3)?,
                    "captured_at": row.get::<_, String>(4)?,
                    "captured_by": row.get::<_, String>(5)?,
                }))
            })
            .map_err(db)?;
        rows.collect::<rusqlite::Result<_>>().map_err(db)?
    };
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
        "evidence": evidence,
        "earlier_retros": earlier,
        "finish": {
            "nothing": format!("tiller retro finish {} --nothing", retro.id),
            "learning": format!("tiller retro finish {} --learning <one sentence> --evidence '<json>'", retro.id),
            "proposed": format!("tiller retro finish {} --proposed <proposal>", retro.id),
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
    let config = learning_config(app)?;
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let retro = learning::resolve_retro(store.conn(), &args.retro)?;
    let timeout_minutes = i64::try_from(config.curator_timeout_minutes).unwrap_or(i64::MAX);
    let session_id = new_session_id()?;
    // The retro is claimed before its scratch directory is touched, so a
    // run refused here (a curator is still running in that directory)
    // changes nothing. A dry run claims nothing but is refused the same way.
    let retro = if args.dry_run {
        learning::check_start(&retro, app.ctx().now, timeout_minutes)?;
        retro
    } else {
        store
            .retro_start(&app.ctx(), &retro.id, Some(&session_id), timeout_minutes)?
            .value
    };
    let scratch = tools.scratch_root().join("retros").join(&retro.id);
    let prepared = brief(&store, &retro).and_then(|brief| {
        let brief_text = format!(
            "Retro `{}` of card {}.\n\n```json\n{}\n```\n",
            retro.id,
            brief["card"]["tag"].as_str().unwrap_or(""),
            serde_json::to_string_pretty(&brief)?
        );
        if scratch.exists() {
            // A second run of the same retro gets its own directory.
            let _ = std::fs::remove_dir_all(&scratch);
        }
        curator::write(&curator::LaunchSpec {
            retro_id: &retro.id,
            scratch: &scratch,
            tiller: tools.tiller(),
            tiller_home: tools.tiller_home(),
            claude: &tools.config.spawn.claude,
            model: &config.curator_model,
            brief: &brief_text,
        })
        .map_err(super::spawn::failure)
    });
    let launch = match prepared {
        Ok(launch) => launch,
        Err(failure) if args.dry_run => return Err(failure),
        Err(failure) => {
            // The retro is claimed: end the run so it can start again at once.
            unfinished(
                app,
                &mut store,
                &retro.id,
                "the curator could not be prepared",
            )?;
            return Err(failure);
        }
    };
    let mut argv = launch.argv.clone();
    argv.extend(["--session-id".to_owned(), session_id.clone()]);
    if args.dry_run {
        return Ok(Outcome::read(json!({
            "retro": retro.id,
            "argv": argv,
            "env": launch.env,
            "prompt": launch.prompt,
            "settings": launch.settings,
            "dry_run": true,
        })));
    }
    let ran = match run_with_timeout(
        &argv,
        &launch.env,
        &scratch,
        Duration::from_secs(config.curator_timeout_minutes.max(1) * 60),
    ) {
        Ok(ran) => ran,
        Err(failure) => {
            unfinished(
                app,
                &mut store,
                &retro.id,
                "the curator could not be started",
            )?;
            return Err(failure);
        }
    };
    let output_path = scratch.join("output.json");
    let _ = std::fs::write(&output_path, &ran.stdout);
    let after = learning::resolve_retro(store.conn(), &retro.id)?;
    let exit = ran.exit;
    let retro = if after.state == tiller_core::domain::RetroState::Pending {
        let note = match (&ran.stopped, exit) {
            (Some(Stopped::Cut), _) => format!(
                "curator cut after {} minutes without finishing the retro",
                config.curator_timeout_minutes
            ),
            (Some(Stopped::Interrupted), _) => {
                "curator stopped: tiller retro run was interrupted before the retro was finished"
                    .to_owned()
            }
            (Some(Stopped::Lost(error)), _) => {
                format!(
                    "curator stopped: waiting for it failed ({error}) before the retro was finished"
                )
            }
            (None, Some(code)) => format!("curator exited {code} without finishing the retro"),
            (None, None) => "curator was killed by a signal without finishing the retro".to_owned(),
        };
        unfinished(app, &mut store, &retro.id, &note)?
    } else {
        after
    };
    Ok(Outcome::read(json!({
        "retro": retro,
        "exit": exit,
        "session_id": session_id,
        "output": output_path,
    }))
    .with_warning((retro.outcome == Some(RetroOutcome::Unfinished)).then(|| {
        format!(
            "the curator did not finish; once the cause is fixed, run `tiller retro run {}` again",
            retro.id
        )
    })))
}

/// Ends a claimed retro whose curator did not finish it as `unfinished`,
/// with `note` as its learning, so the owner can run it again.
fn unfinished(app: &App, store: &mut Store, retro: &str, note: &str) -> Result<Retro, Failure> {
    let finished = store.retro_finish(
        &app.ctx(),
        None,
        retro,
        Finish {
            outcome: RetroOutcome::Unfinished,
            learning: Some(note.to_owned()),
            evidence: None,
            proposal: None,
        },
    )?;
    Ok(finished.value)
}

/// What a curator run came to.
struct Ran {
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
    /// tiller got SIGINT, SIGTERM or SIGHUP (the owner's Ctrl-C).
    Interrupted,
    /// Waiting for it failed, with this error.
    Lost(String),
}

/// Runs the curator in a process group of its own and stops the whole
/// group when the curator exits, at `timeout`, on an interrupt, or when
/// waiting for it fails, so nothing it started outlives the run
/// (`tiller_herdr::process::wait_all`).
fn run_with_timeout(
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    timeout: Duration,
) -> Result<Ran, Failure> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Failure::new("internal", 1, "empty curator command"))?;
    // In its own group the curator no longer gets the terminal's Ctrl-C,
    // so tiller catches it and stops the group itself. Caught before the
    // spawn, so no signal can end tiller and leave the curator running.
    let interrupt = Interrupt::catch().map_err(|error| {
        Failure::new(
            "internal",
            1,
            format!("cannot watch for interrupts: {error}"),
        )
    })?;
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
        Some(&interrupt),
    ) {
        Ended::Exited(status) => (status.code(), None),
        Ended::TimedOut => (None, Some(Stopped::Cut)),
        Ended::Interrupted => (None, Some(Stopped::Interrupted)),
        Ended::Failed(error) => (None, Some(Stopped::Lost(error.to_string()))),
    };
    // A process that left the curator's group can still hold stdout open
    // past its exit, so the read shares the deadline; after a stop it is
    // not waited for at all.
    let stdout = match stopped {
        None => reader
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default(),
        Some(_) => String::new(),
    };
    Ok(Ran {
        exit,
        stopped,
        stdout,
    })
}

fn finish(app: &App, args: RetroFinish) -> Done {
    let mut store = app.write()?;
    let retro = learning::resolve_retro(store.conn(), &args.retro)?;
    let evidence = args
        .evidence
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|error| Failure::usage(format!("--evidence is not JSON: {error}")))?;
    let finish = if args.nothing {
        Finish {
            outcome: RetroOutcome::Nothing,
            learning: None,
            evidence: None,
            proposal: None,
        }
    } else if let Some(learning) = args.learning.clone() {
        Finish {
            outcome: RetroOutcome::Evidence,
            learning: Some(learning),
            evidence,
            proposal: None,
        }
    } else if let Some(proposal) = args.proposed.clone() {
        Finish {
            outcome: RetroOutcome::Proposed,
            learning: None,
            evidence: None,
            proposal: Some(proposal),
        }
    } else {
        return Err(Failure::usage(
            "say how it ended: --nothing, --learning <sentence> or --proposed <proposal>",
        ));
    };
    let normalized = json!({"retro": retro.id, "finish": finish});
    let keyed = app.key(&store, "retro.finish", &normalized, &args.key.key)?;
    let applied = store.retro_finish(&app.ctx(), Some(&keyed.key), &retro.id, finish)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn learn(app: &App, cmd: LearnCmd) -> Done {
    match cmd {
        LearnCmd::Bundle {
            name,
            proposals,
            window,
        } => {
            let config = learning_config(app)?;
            let mut store = app.write()?;
            let applied = store.change_set_create(
                &app.ctx(),
                &name,
                &proposals,
                window.unwrap_or(config.window_cards),
            )?;
            Ok(Outcome::applied(applied))
        }
        LearnCmd::Add { set, proposal } => {
            let mut store = app.write()?;
            Ok(Outcome::applied(store.change_set_add(
                &app.ctx(),
                &set,
                &proposal,
            )?))
        }
        LearnCmd::Abandon { set } => {
            let mut store = app.write()?;
            Ok(Outcome::applied(
                store.change_set_abandon(&app.ctx(), &set)?,
            ))
        }
        LearnCmd::Eval(args) => eval(app, args),
        LearnCmd::Gate { proposal } => {
            let store = app.read()?;
            let gate = store.learning_gate(&proposal)?;
            let hint = gate.hint.clone();
            let mut outcome = Outcome::read(gate);
            if let Some(hint) = hint {
                outcome = outcome.with_help(hint);
            }
            Ok(outcome)
        }
        LearnCmd::Status => {
            refuse_fleet(app, "tiller learn status")?;
            let store = app.read()?;
            Ok(Outcome::read(
                store.learning_status(tiller_core::Timestamp::now())?,
            ))
        }
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

/// A holdout error as the command reports it.
fn holdout_failure(error: HoldoutError) -> Failure {
    match error {
        HoldoutError::Usage(message) => Failure::usage(message),
        HoldoutError::Refused(message) => Failure::refused(message),
        HoldoutError::Unavailable(message) => Failure::unavailable(message),
        HoldoutError::Core(error) => Failure::from(error),
        HoldoutError::Io(error) => Failure::from(error),
    }
}

fn eval(app: &App, args: LearnEval) -> Done {
    app.require_owner("tiller learn eval")?;
    let config = learning_config(app)?;
    let mut store = app.write()?;
    let mut events = Vec::new();
    let set: ChangeSet = match (&args.set, &args.proposal) {
        (Some(set), _) => {
            if let Some(found) = find::<ChangeSet>(
                store.conn(),
                "name = ?1 AND state <> 'closed' ORDER BY opened_at DESC",
                &[&set],
            )? {
                found
            } else {
                let id = resolve_id(store.conn(), "change_sets", "change set", set)?;
                get(store.conn(), "change set", &id)?
            }
        }
        (None, Some(proposal)) => {
            let id = resolve_id(store.conn(), "proposals", "proposal", proposal)?;
            match learning::open_set_of(store.conn(), &id)? {
                Some(set) => set,
                None => {
                    let created = store.change_set_create(
                        &app.ctx(),
                        tiller_core::ids::short(&id),
                        std::slice::from_ref(&id),
                        config.window_cards,
                    )?;
                    events.extend(created.events);
                    created.value
                }
            }
        }
        (None, None) => return Err(Failure::usage("name a change set or --proposal")),
    };

    if args.result.is_some() && args.candidate_file.is_some() {
        return Err(Failure::usage(
            "--result records a run made elsewhere; it cannot be bound to a --candidate-file it did not test",
        ));
    }
    let result = match &args.result {
        Some(path) => holdout::parse_result(path).map_err(holdout_failure)?,
        None => {
            let evals_dir = config.evals_dir.as_deref().map(PathBuf::from).ok_or_else(|| {
                Failure::usage("learning.evals_dir is not set; it names the checkout that holds evals/run.sh")
                    .hint("tiller config set learning.evals_dir <path>, or record a run with --result")
            })?;
            // The members the candidate stands for, taken before the run:
            // a member added meanwhile makes the record refuse.
            let members = learning::members_sha256_of(store.conn(), &set.id)?;
            let candidate = match &args.candidate_file {
                Some(path) => std::fs::read_to_string(path)
                    .map_err(|error| Failure::usage(format!("{}: {error}", path.display())))?,
                None => holdout::render_candidate(store.conn(), &set, &app.home)
                    .map_err(holdout_failure)?,
            };
            let out = match &args.out {
                Some(dir) => dir.clone(),
                None => tempfile::Builder::new()
                    .prefix("tiller-learn-eval.")
                    .tempdir()?
                    .keep(),
            };
            holdout::run(&evals_dir, &out, &candidate, members).map_err(holdout_failure)?
        }
    };
    let recorded = store.eval_run_record(&app.ctx(), &set.id, result)?;
    events.extend(recorded.events);
    let run: EvalRun = recorded.value;
    let passed = run.passed == 1;
    Ok(Outcome::applied(tiller_core::Applied {
        value: json!({"change_set": set, "eval_run": run}),
        events,
        replayed: false,
    })
    .with_help(if passed {
        format!(
            "the holdout passed; tiller learn gate <proposal> shows what else holds for {}",
            set.name
        )
    } else {
        "the holdout did not pass: write more tuning cases, then run it again".to_owned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `pid` has exited: no such process, or one only waiting to be
    /// reaped.
    fn gone(pid: &str) -> bool {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        stat.trim().is_empty() || stat.trim_start().starts_with('Z')
    }

    #[test]
    fn a_cut_stops_the_curator_and_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        // A curator that starts a child of its own and then hangs.
        let script = format!("sleep 300 & echo $! > '{}'; sleep 300", file.display());
        let argv = ["sh".to_owned(), "-c".to_owned(), script];
        let started = Instant::now();
        let ran = run_with_timeout(&argv, &[], dir.path(), Duration::from_millis(500)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let grandchild = std::fs::read_to_string(&file).unwrap().trim().to_owned();
        let mut stopped = gone(&grandchild);
        let waited = Instant::now();
        while !stopped && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
            stopped = gone(&grandchild);
        }
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
        let ran = run_with_timeout(&argv, &[], dir.path(), Duration::from_secs(60)).unwrap();
        assert_eq!((ran.exit, ran.stopped), (Some(2), None));
        assert_eq!(ran.stdout.trim(), "{\"done\":true}");
    }
}
