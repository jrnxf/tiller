//! `tiller retro ...` and `tiller learn ...`: the learning loop
//! (`docs/design/learning-loop.md`, section 10). The record work is in
//! `tiller_core::state::learning`; this module adds the curator launch, the
//! candidate rendering and the holdout run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiller_agents::curator;
use tiller_core::domain::{
    Attention, Card, ChangeSet, ChangeSetMember, ConfigManifest, EvalRun, Event, Project, Retro,
    RetroOutcome, Review, Run, Session,
};
use tiller_core::state::learning::{self, EvalResult, Finish, Learning};
use tiller_core::state::{find, get, list, resolve_id};
use tiller_core::{Actor, Store};

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

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
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
        let note = match (ran.cut, exit) {
            (true, _) => format!(
                "curator cut after {} minutes without finishing the retro",
                config.curator_timeout_minutes
            ),
            (false, Some(code)) => format!("curator exited {code} without finishing the retro"),
            (false, None) => {
                "curator was killed by a signal without finishing the retro".to_owned()
            }
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
    /// The exit code; `None` when cut or killed by a signal.
    exit: Option<i32>,
    /// Whether the timeout cut it.
    cut: bool,
    stdout: String,
}

/// Runs the curator, cutting it at `timeout`. After a cut the stdout reader
/// is not waited for: a grandchild holding the pipe would otherwise block
/// past the timeout.
fn run_with_timeout(
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    timeout: Duration,
) -> Result<Ran, Failure> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| Failure::new("internal", 1, "empty curator command"))?;
    let mut child = Command::new(program)
        .args(rest)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            Failure::unavailable(format!("cannot start {program}: {error}"))
                .hint("set spawn.claude in config.toml")
        })?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = std::io::Read::read_to_string(out, &mut text);
        }
        text
    });
    let started = Instant::now();
    let (exit, cut) = loop {
        match child.try_wait()? {
            Some(status) => break (status.code(), false),
            None if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(200));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
        }
    };
    let stdout = if cut {
        String::new()
    } else {
        reader.join().unwrap_or_default()
    };
    Ok(Ran { exit, cut, stdout })
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

/// `pins.env` as `KEY=value` lines, for the versions a run depends on.
fn pins(evals_dir: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(evals_dir.join("evals/pins.env"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            ))
        })
        .collect()
}

/// What a `claude plugin eval --json` result says (`evals/run.sh`).
fn parse_result(path: &Path) -> Result<EvalResult, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| Failure::usage(format!("{}: {error}", path.display())))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| Failure::usage(format!("{} is not JSON: {error}", path.display())))?;
    let aggregates = &value["aggregates"];
    let cases_total = aggregates["casesTotal"].as_i64().ok_or_else(|| {
        Failure::usage(format!("{} has no aggregates.casesTotal", path.display()))
    })?;
    Ok(EvalResult {
        members_sha256: None,
        candidate_sha256: None,
        cases_total,
        cases_passed: aggregates["casesPassed"].as_i64().unwrap_or(0),
        overall_score: aggregates["overallScore"].as_f64(),
        cost_usd: value["costUsd"].as_f64(),
        partial: value["partial"].as_bool().unwrap_or(false),
        claude_version: None,
        judge_model: value["judgeModel"].as_str().map(str::to_owned),
        result_path: Some(path.to_string_lossy().into_owned()),
    })
}

/// The members' target files with their diffs applied, under a heading per
/// source, the way the launcher renders approved memory (learning-loop.md,
/// section 7).
fn render_candidate(store: &Store, set: &ChangeSet) -> Result<String, Failure> {
    let conn = store.conn();
    let members: Vec<ChangeSetMember> =
        list(conn, "change_set_id = ?1 ORDER BY added_at", &[&set.id])?;
    if members.is_empty() {
        return Err(Failure::refused(format!(
            "change set {} has no members to render",
            set.name
        )));
    }
    // Per target: (repo, path, base blob, diffs in order).
    let mut targets: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for member in &members {
        let (repo, path, base_blob, diff): (Option<String>, Option<String>, String, String) =
            conn.query_row(
                "SELECT s.canonical_repo, s.canonical_path, p.base_blob, p.diff
                 FROM proposals p JOIN memory_sources s ON s.id = p.target_source_id WHERE p.id = ?1",
                [&member.proposal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(db)?;
        let (Some(repo), Some(path)) = (repo, path) else {
            return Err(Failure::refused(format!(
                "proposal {} targets a foreign source; it cannot be rendered",
                member.proposal_id
            )));
        };
        match targets.iter_mut().find(|t| t.0 == repo && t.1 == path) {
            Some(target) => target.3.push(diff),
            None => targets.push((repo, path, base_blob, vec![diff])),
        }
    }
    let work = tempfile::tempdir()?;
    let mut out = String::new();
    for (repo, path, base_blob, diffs) in targets {
        let base = if base_blob == tiller_core::import::EMPTY_BLOB {
            String::new()
        } else {
            let output = Command::new("git")
                .args(["-C", &repo, "cat-file", "-p", &base_blob])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Failure::refused(format!(
                    "git cat-file {base_blob} in {repo} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let file = work.path().join(&path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, base)?;
        for (n, diff) in diffs.iter().enumerate() {
            let patch = work.path().join(format!(".patch-{n}"));
            std::fs::write(&patch, diff)?;
            let output = Command::new("git")
                .args(["apply", "--whitespace=nowarn"])
                .arg(&patch)
                .current_dir(work.path())
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(Failure::refused(format!(
                    "a member's diff does not apply to {path}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        let rendered = std::fs::read_to_string(&file)?;
        out.push_str(&format!("## {path}\n\n{}\n\n", rendered.trim_end()));
    }
    Ok(out)
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
        Some(path) => parse_result(path)?,
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
                None => render_candidate(&store, &set)?,
            };
            let out = match &args.out {
                Some(dir) => dir.clone(),
                None => tempfile::Builder::new()
                    .prefix("tiller-learn-eval.")
                    .tempdir()?
                    .keep(),
            };
            std::fs::create_dir_all(&out)?;
            // An earlier run's sealed result must not be read as this one's.
            let sealed = out.join("sealed");
            if sealed.exists() {
                std::fs::remove_dir_all(&sealed)?;
            }
            let candidate_path = out.join("candidate.md");
            std::fs::write(&candidate_path, &candidate)?;
            let runner = evals_dir.join("evals/run.sh");
            let status = Command::new(&runner)
                .args(["--split", "holdout", "--candidate"])
                .arg(&candidate_path)
                .arg("--out")
                .arg(&out)
                .current_dir(&evals_dir)
                .stdin(Stdio::null())
                .status()
                .map_err(|error| {
                    Failure::unavailable(format!("cannot run {}: {error}", runner.display()))
                })?;
            let result_path = out.join("sealed/result.json");
            if !status.success() || !result_path.exists() {
                // run.sh exits non-zero when a case failed to load even
                // when the rest scored, so a result is recorded only from
                // a clean run.
                return Err(Failure::unavailable(format!(
                    "{} exited {:?}{}; nothing recorded",
                    runner.display(),
                    status.code(),
                    if result_path.exists() {
                        format!(" (its result is at {})", result_path.display())
                    } else {
                        " and left no sealed/result.json".to_owned()
                    }
                )));
            }
            let mut result = parse_result(&result_path)?;
            result.members_sha256 = Some(members);
            result.candidate_sha256 = Some(sha256_hex(candidate.as_bytes()));
            let pins = pins(&evals_dir);
            result.claude_version = pins.get("CLAUDE_CODE_VERSION").cloned();
            if result.judge_model.is_none() {
                result.judge_model = pins.get("JUDGE_MODEL").cloned();
            }
            result
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
