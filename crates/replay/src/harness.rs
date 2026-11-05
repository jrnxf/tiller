//! The replay loop: one ask at a time through the real hooks, the
//! orchestrator and the core and spawn functions the CLI calls.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};
use tiller_agents::intake::Hooks;
use tiller_agents::spawn::{self, Outcome as SpawnOutcome, Request};
use tiller_core::domain::{AttentionKind, Card, CardState, Run, RunOutcome};
use tiller_core::import::Bundle;
use tiller_core::state::asks::Resolution;
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::{get, list, resolve_card};
use tiller_core::{Actor, Ctx, Timestamp};
use tiller_observe::hooks::{self, Env, Outcome as HookOutcome};

use crate::Error;
use crate::audit::{Audit, fingerprint};
use crate::baseline::{self, ReplayAsk};
use crate::board::{self, BoardCard, History};
use crate::orchestrator::{Decision, Orchestrator, Recorded, Turn};
use crate::sandbox::{SESSION_PREFIX, Sandbox};

/// What one ask did, with no words from the bundle: the ask's text, card
/// titles and hold reasons stay in the sandbox.
#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub index: usize,
    pub ts: String,
    pub project: String,
    pub session: String,
    pub ask_id: String,
    /// The `UserPromptSubmit` hook's outcome.
    pub intake: String,
    pub disposition: String,
    pub cards: Vec<Linked>,
    pub decisions: Vec<Step>,
    pub runs: Vec<RunRecord>,
    /// The `Stop` hook's outcome and what it found.
    pub stop: Value,
    /// What Firstmate did, for T5.2 to compare against.
    pub baseline: Value,
    /// Why the orchestrator gave no decision list, when it gave none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unanswered: Option<String>,
}

/// A card the ask ended up linked to.
#[derive(Clone, Debug, Serialize)]
pub struct Linked {
    pub display_id: String,
    pub legacy_id: Option<String>,
    pub kind: String,
    pub relation: String,
}

/// One decision and what applying it did.
#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub decision: Value,
    pub ok: bool,
    pub result: Value,
    /// Imported cards the decision reopened to match the board, by display
    /// id. A reopen stays when the decision itself is then refused.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reopened: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunRecord {
    pub run_id: String,
    pub card: String,
    pub role: String,
    pub model: String,
    pub effort: String,
    pub dispatch: Value,
    pub outcome: Option<String>,
    pub cleaned: Value,
}

pub struct Replay<'a> {
    pub sandbox: &'a mut Sandbox,
    /// Where an ask with no card goes.
    pub default_project: String,
    /// Each label a `new` decision of the current turn gave, to the card it
    /// created. A label names a card within one turn only: a later turn
    /// may give the same label to another card.
    labels: HashMap<String, String>,
    /// Every card a replayed ask filed, in order, for the boards after it.
    filed: Vec<String>,
    /// The display ids [`Self::reference`] reopened for the current step.
    reopened: Vec<String>,
    /// Each Firstmate task id, to the card the import made of it.
    imported: HashMap<String, Card>,
    /// The Firstmate task id of each imported card, by card id.
    keys: HashMap<String, String>,
    /// When each bundle card was open, for the board a turn shows.
    history: History,
    /// The imported cards the current turn's board shows open, by card id:
    /// the sandbox reopens one the moment a decision names it.
    shown: HashSet<String>,
    /// The Firstmate task ids of filed cards a replay card took the place of.
    replaced: HashSet<String>,
    /// How far the earliest ask lies in the past, in milliseconds: a hold
    /// the orchestrator dates from an ask's day is moved forward by it, the
    /// same for every ask, so one date is one instant all through.
    shift_ms: i64,
}

impl<'a> Replay<'a> {
    pub fn new(
        sandbox: &'a mut Sandbox,
        default_project: String,
        imported: HashMap<String, Card>,
        history: History,
    ) -> Self {
        let keys = imported
            .iter()
            .map(|(key, card)| (card.id.clone(), key.clone()))
            .collect();
        let shift_ms = history
            .earliest()
            .map_or(0, |at| Timestamp::now().as_millis() - at.as_millis());
        Self {
            sandbox,
            default_project,
            labels: HashMap::new(),
            filed: Vec::new(),
            reopened: Vec::new(),
            imported,
            keys,
            history,
            shown: HashSet::new(),
            replaced: HashSet::new(),
            shift_ms,
        }
    }

    /// The project's cards as they stood when the ask at `index` arrived:
    /// the imported ones [`History`] says were open, then the open ones
    /// earlier replayed asks filed.
    fn board(&mut self, index: usize, project: &str, day: &str) -> Result<Vec<BoardCard>, Error> {
        let mut shown = Vec::new();
        self.shown.clear();
        for key in self.history.open(index, day, &self.replaced) {
            if let Some(card) = self.imported.get(key).filter(|c| c.project_id == project) {
                self.shown.insert(card.id.clone());
                shown.push(board::shown(card, self.history.since(key), false));
            }
        }
        for id in &self.filed {
            let card: Card = get(self.sandbox.store.conn(), "card", id)?;
            if card.project_id == project && card.state == CardState::Open {
                shown.push(board::shown(&card, None, true));
            }
        }
        Ok(shown)
    }

    /// Replays one ask. An error here is the harness failing, not a
    /// decision being refused: a refusal is recorded and the ask goes on.
    pub fn ask(
        &mut self,
        index: usize,
        ask: &ReplayAsk,
        orchestrator: &mut dyn Orchestrator,
    ) -> Result<Record, Error> {
        self.labels.clear();
        let project = self.route(ask)?;
        let session = self.sandbox.orchestrator(&project)?;
        let env = Env {
            tiller_home: self.sandbox.home.clone(),
            user_home: Some(self.sandbox.root.clone()),
            tiller_session_id: Some(session.clone()),
        };
        let intake = Hooks::load(&self.sandbox.home);
        let claude = format!("{SESSION_PREFIX}{project}");
        let input = json!({
            "session_id": claude,
            "transcript_path": self.transcript(&project, index, "user")?,
            "hook_event_name": "UserPromptSubmit",
            "cwd": self.sandbox.root,
            "prompt": ask.ask,
            "prompt_id": format!("replay-{index}"),
        });
        let (outcome, output) = hooks::run_with(
            "UserPromptSubmit",
            &serde_json::to_vec(&input)?,
            &env,
            Some(&intake),
        );
        let intake_outcome = outcome_name(&outcome);
        let ask_id = match &outcome {
            HookOutcome::Recorded { observation_id } => {
                let seen: tiller_core::domain::Observation =
                    get(self.sandbox.store.conn(), "observation", observation_id)?;
                seen.payload
                    .get("ask_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }
            _ => None,
        };
        let ask_id = ask_id.ok_or_else(|| {
            Error::Sandbox(format!(
                "intake recorded no ask for line {index}: {outcome:?}"
            ))
        })?;
        let context = output
            .as_ref()
            .and_then(|o| o.pointer("/hookSpecificOutput/additionalContext"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        let day = ask.ts.get(..10).unwrap_or_default();
        let board = self.board(index, &project, day)?;
        let turn = Turn {
            index,
            ask_id: ask_id.clone(),
            project: project.clone(),
            text: ask.ask.clone(),
            context,
            now: ask.ts.clone(),
            board,
            projects: self.sandbox.projects()?,
        };
        // A turn the orchestrator could not answer is one unanswered ask,
        // not the end of the replay: the ask stays unresolved.
        let (decisions, unanswered) = match orchestrator.decide(&turn) {
            Ok(decisions) => (decisions, None),
            Err(Error::Orchestrator(e)) => (Vec::new(), Some(e)),
            Err(e) => return Err(e),
        };
        let ctx = Ctx::new(Actor::Session(session.clone()));
        let before = self.filed.len();
        let mut steps = Vec::new();
        let mut runs = Vec::new();
        let mut reply = None;
        let mut resolved = false;
        for decision in &decisions {
            if let Decision::Reply { text } = decision {
                reply = Some(text.clone());
            }
            let applied = self.apply(&ctx, &ask_id, &project, &mut resolved, decision);
            if let Ok(Some(run)) = &applied.1 {
                runs.push((
                    run.clone(),
                    applied.0.get("dispatch").cloned().unwrap_or_default(),
                ));
            }
            steps.push(Step {
                decision: redacted(decision),
                ok: applied.1.is_ok(),
                result: match applied.1 {
                    Ok(_) => applied.0,
                    Err(e) => json!(e.to_string()),
                },
                reopened: std::mem::take(&mut self.reopened),
            });
        }
        // The replay's new cards take the place of Firstmate's, in order.
        let filed = self.filed.len() - before;
        self.replaced
            .extend(self.history.filed(index).iter().take(filed).cloned());
        let runs = runs
            .iter()
            .map(|(run, dispatch)| self.end(run, dispatch.clone()))
            .collect::<Result<Vec<_>, _>>()?;

        let cards = self.linked(&ask_id)?;
        let reply = reply.unwrap_or_else(|| default_reply(&cards));
        let input = json!({
            "session_id": claude,
            "transcript_path": self.transcript(&project, index, "assistant")?,
            "hook_event_name": "Stop",
            "cwd": self.sandbox.root,
            "last_assistant_message": reply,
            "stop_hook_active": false,
        });
        let (outcome, output) =
            hooks::run_with("Stop", &serde_json::to_vec(&input)?, &env, Some(&intake));
        let found = match &outcome {
            HookOutcome::Recorded { observation_id } => {
                let seen: tiller_core::domain::Observation =
                    get(self.sandbox.store.conn(), "observation", observation_id)?;
                seen.payload
            }
            _ => Value::Null,
        };
        let resolved: tiller_core::domain::Ask = get(self.sandbox.store.conn(), "ask", &ask_id)?;
        Ok(Record {
            index,
            ts: ask.ts.clone(),
            project,
            session,
            ask_id,
            intake: intake_outcome,
            disposition: resolved.disposition.to_string(),
            cards,
            decisions: steps,
            runs,
            stop: json!({"outcome": outcome_name(&outcome), "found": found, "output": output}),
            baseline: ask.summary(),
            unanswered,
        })
    }

    /// Grows the orchestrator session's transcript by one entry, as Claude
    /// Code does each turn: the hooks key a turn end by the transcript's
    /// length, so two turns with the same closing words stay two.
    fn transcript(&self, project: &str, index: usize, role: &str) -> Result<PathBuf, Error> {
        let dir = self.sandbox.root.join("transcripts");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{project}.jsonl"));
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{}", json!({"turn": index, "role": role}))?;
        Ok(path)
    }

    /// The project whose orchestrator gets the ask: its first card's, unless
    /// that project is retired and so has no orchestrator.
    fn route(&self, ask: &ReplayAsk) -> Result<String, Error> {
        let live = self.sandbox.projects()?;
        for card in &ask.cards {
            if let Some(found) = self.card(card)?
                && live.contains(&found.project_id)
            {
                return Ok(found.project_id);
            }
        }
        Ok(self.default_project.clone())
    }

    /// A card reference as the orchestrator wrote it: a label it gave, a
    /// Firstmate task id, or a Tiller reference.
    fn card(&self, reference: &str) -> Result<Option<Card>, Error> {
        let conn = self.sandbox.store.conn();
        if let Some(id) = self
            .labels
            .get(reference)
            .or_else(|| self.imported.get(reference).map(|card| &card.id))
        {
            return Ok(Some(get(conn, "card", id)?));
        }
        Ok(resolve_card(conn, reference).ok())
    }

    /// A card reference as a card id. A card the turn's board showed open
    /// but the import closed (it closed later) is reopened first, so the
    /// sandbox matches the board the orchestrator was shown.
    fn reference(&mut self, reference: &str) -> Result<String, Error> {
        let Some(card) = self.card(reference)? else {
            return Ok(reference.to_owned());
        };
        if card.state == CardState::Closed && self.shown.contains(&card.id) {
            let owner = Ctx::new(Actor::Owner);
            self.sandbox.store.card_reopen(
                &owner,
                &card.id,
                "replay: open when the ask arrived",
            )?;
            self.reopened.push(card.display_id);
        }
        Ok(card.id)
    }

    /// A card as a record names it: its display id and, for an imported
    /// card, the Firstmate task id the baseline names it by, which T5.2
    /// compares by.
    fn named(&self, id: &str) -> Result<Value, Error> {
        let card: Card = get(self.sandbox.store.conn(), "card", id)?;
        Ok(json!({"display_id": card.display_id, "legacy_id": self.keys.get(&card.id)}))
    }

    /// Applies one decision. Returns what it did, and the run it started.
    fn apply(
        &mut self,
        ctx: &Ctx,
        ask: &str,
        project: &str,
        resolved: &mut bool,
        decision: &Decision,
    ) -> (Value, Result<Option<Run>, Error>) {
        let mut out = Value::Null;
        let result = self.apply_inner(ctx, ask, project, resolved, decision, &mut out);
        (out, result)
    }

    fn apply_inner(
        &mut self,
        ctx: &Ctx,
        ask: &str,
        project: &str,
        resolved: &mut bool,
        decision: &Decision,
        out: &mut Value,
    ) -> Result<Option<Run>, Error> {
        let store = &mut self.sandbox.store;
        match decision {
            Decision::New {
                label,
                project: target,
                kind,
                title,
                size,
                acceptance,
            } => {
                let new = NewCard {
                    project_id: target.clone().unwrap_or_else(|| project.to_owned()),
                    kind: *kind,
                    title: title.clone(),
                    size: *size,
                    acceptance: acceptance.clone(),
                    ..NewCard::default()
                };
                let cross = (new.project_id != project).then(|| new.project_id.clone());
                let card = if cross.is_some() {
                    // Another project's card: filed there, not linked to an
                    // ask its orchestrator never saw.
                    store.card_create(ctx, None, new, None)?.value
                } else if *resolved {
                    store
                        .ask_link(ctx, None, ask, Resolution::New(new))?
                        .value
                        .card
                        .ok_or_else(|| Error::Sandbox("ask link made no card".into()))?
                } else {
                    *resolved = true;
                    store
                        .ask_resolve(ctx, ask, Resolution::New(new))?
                        .value
                        .card
                        .ok_or_else(|| Error::Sandbox("ask resolve made no card".into()))?
                };
                *out = json!({"card": card.display_id, "kind": card.kind, "cross_project": cross});
                self.filed.push(card.id.clone());
                if let Some(label) = label {
                    self.labels.insert(label.clone(), card.id);
                }
                Ok(None)
            }
            Decision::Followup { card } => {
                let card = self.reference(card)?;
                let store = &mut self.sandbox.store;
                let resolution = Resolution::Followup { card };
                let linked = if *resolved {
                    store.ask_link(ctx, None, ask, resolution)?.value
                } else {
                    *resolved = true;
                    store.ask_resolve(ctx, ask, resolution)?.value
                };
                *out = json!({"card": linked.card.map(|c| c.display_id)});
                Ok(None)
            }
            Decision::Reopen { card, reason } => {
                let card = self.reference(card)?;
                let card = self.sandbox.store.card_reopen(ctx, &card, reason)?.value;
                *out = json!({"card": card.display_id, "legacy": self.keys.get(&card.id), "state": card.state});
                Ok(None)
            }
            Decision::Chat { reason } => {
                *resolved = true;
                let reason = reason.clone();
                store.ask_resolve(ctx, ask, Resolution::Chat { reason })?;
                Ok(None)
            }
            Decision::Cancelled { reason } => {
                *resolved = true;
                let reason = reason.clone();
                store.ask_resolve(ctx, ask, Resolution::Cancelled { reason })?;
                Ok(None)
            }
            Decision::Spawn {
                card,
                role,
                model,
                effort,
            } => {
                let card = self.reference(card)?;
                let project = get::<Card>(self.sandbox.store.conn(), "card", &card)?.project_id;
                self.sandbox.checkout(&project)?;
                // As `tiller spawn`, which takes `--model` only with `--effort`.
                let asked = match (model, effort) {
                    (Some(model), Some(effort)) => Some((model.clone(), *effort)),
                    (None, None) => None,
                    _ => {
                        return Err(Error::Input(
                            "spawn takes a model only with an effort, and an effort only with a model"
                                .into(),
                        ));
                    }
                };
                let (store, world) = self.sandbox.world();
                let request = Request {
                    card,
                    role: *role,
                    asked,
                    brief: None,
                    harness: "claude".into(),
                };
                let spawned = spawn::spawn(store, ctx, None, &world, request)?;
                let run = match spawned.value {
                    SpawnOutcome::Spawned(s) => {
                        *out = json!({"run": s.run.id, "model": s.run.model, "effort": s.run.effort, "dispatch": s.dispatch});
                        s.run
                    }
                    SpawnOutcome::Replayed { run, .. } => *run,
                };
                Ok(Some(run))
            }
            Decision::Hold {
                card,
                until,
                reason,
            } => {
                let card = self.reference(card)?;
                // The orchestrator dates a hold from the ask's day; the
                // sandbox's clock is today's.
                let until =
                    Timestamp::from_millis(until.as_millis() + self.shift_ms).unwrap_or(*until);
                let hold = self
                    .sandbox
                    .store
                    .card_hold(ctx, &card, until, reason)?
                    .value;
                *out = json!({"hold": hold.id, "until": hold.until_at, "card": self.named(&card)?});
                Ok(None)
            }
            Decision::Attention { card, question } => {
                let card = self.reference(card)?;
                let opened = self.sandbox.store.attention_open(
                    ctx,
                    None,
                    NewAttention {
                        card: card.clone(),
                        kind: AttentionKind::Decision,
                        question: question.clone(),
                        ..NewAttention::default()
                    },
                )?;
                *out = json!({"attention": opened.value.id, "card": self.named(&card)?});
                Ok(None)
            }
            Decision::Dep { card, depends_on } => {
                let card = self.reference(card)?;
                let depends_on = self.reference(depends_on)?;
                self.sandbox.store.dep_add(ctx, &card, &depends_on)?;
                *out = json!({"card": self.named(&card)?, "depends_on": self.named(&depends_on)?});
                Ok(None)
            }
            Decision::Reply { .. } => Ok(None),
        }
    }

    /// Ends a stubbed run as the owner cancelling it, then cleans it up, so
    /// it frees its lane and its pool slot for the next ask.
    fn end(&mut self, run: &Run, dispatch: Value) -> Result<RunRecord, Error> {
        let owner = Ctx::new(Actor::Owner);
        let ended = spawn::done(
            &mut self.sandbox.store,
            &owner,
            &self.sandbox.scanner,
            run,
            RunOutcome::Cancelled,
        )?;
        let (store, world) = self.sandbox.world();
        let cleaned = match spawn::cleanup(store, &owner, &world, run) {
            Ok(applied) => json!({
                "closed_panes": applied.value.closed_panes,
                "returned": applied.value.returned,
                "warnings": applied.value.warnings,
            }),
            Err(e) => json!({"refused": e.to_string()}),
        };
        let card: Card = get(self.sandbox.store.conn(), "card", &run.card_id)?;
        Ok(RunRecord {
            run_id: run.id.clone(),
            card: card.display_id,
            role: run.role.to_string(),
            model: run.model.clone(),
            effort: run.effort.to_string(),
            dispatch,
            outcome: ended.value.run.outcome.map(|o| o.to_string()),
            cleaned,
        })
    }

    fn linked(&self, ask: &str) -> Result<Vec<Linked>, Error> {
        let conn = self.sandbox.store.conn();
        let links: Vec<tiller_core::domain::AskCard> =
            list(conn, "ask_id = ?1 ORDER BY linked_at", &[&ask])?;
        links
            .into_iter()
            .map(|link| {
                let card: Card = get(conn, "card", &link.card_id)?;
                Ok(Linked {
                    display_id: card.display_id,
                    legacy_id: self.keys.get(&card.id).cloned(),
                    kind: card.kind.to_string(),
                    relation: link.relation.to_string(),
                })
            })
            .collect()
    }
}

/// A turn's closing message when the orchestrator wrote none: one tagged
/// line per card, as the role asks.
fn default_reply(cards: &[Linked]) -> String {
    if cards.is_empty() {
        return "Answered.".to_owned();
    }
    cards
        .iter()
        .map(|c| format!("[{}] {}", c.display_id, c.relation))
        .collect::<Vec<_>>()
        .join("\n")
}

fn outcome_name(outcome: &HookOutcome) -> String {
    match outcome {
        HookOutcome::Recorded { .. } => "recorded".into(),
        other => format!("{other:?}"),
    }
}

/// A decision as a record keeps it: free text (titles, reasons, questions,
/// replies) as its length only, since it may quote the bundle.
pub fn redacted(decision: &Decision) -> Value {
    let mut value = serde_json::to_value(decision).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        for field in ["title", "reason", "question", "text"] {
            if let Some(text) = map.get(field).and_then(Value::as_str) {
                let len = text.chars().count();
                map.insert(field.into(), json!(format!("<{len} chars>")));
            }
        }
    }
    value
}

/// What one whole replay asks for.
pub struct Options {
    /// The export bundle, read in place and never written.
    pub bundle: PathBuf,
    /// An empty or missing directory the sandbox is built in.
    pub sandbox: PathBuf,
    /// Where `decisions.jsonl` and `audit.json` go.
    pub out: PathBuf,
    /// The `[dispatch]` table spawn resolves models with.
    pub dispatch: Option<toml::Value>,
    /// A real Tiller home to prove untouched, read-only.
    pub real_home: Option<PathBuf>,
}

/// The counts a replay prints: nothing from the bundle.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub asks: usize,
    pub dispositions: BTreeMap<String, usize>,
    pub decisions: usize,
    pub refused: usize,
    pub cards_linked: usize,
    pub runs: usize,
    pub stop_unresolved: usize,
    /// Asks the orchestrator gave no decision list for.
    pub unanswered: usize,
    pub audit: Audit,
}

/// Replays every ask in the bundle's `baseline/replay.jsonl`, writes one
/// record per ask and the audit to `out`, and returns the counts. With no
/// orchestrator given, [`Recorded`] decides.
pub fn run(
    options: &Options,
    orchestrator: Option<&mut dyn Orchestrator>,
) -> Result<Summary, Error> {
    let before = fingerprint(&options.bundle)?;
    let bundle = Bundle::read(&options.bundle)?;
    let asks = baseline::read(&options.bundle.join("baseline").join("replay.jsonl"))?;
    let mut sandbox = Sandbox::create(&options.sandbox, &bundle, options.dispatch.clone())?;
    let imported = sandbox.imported(&bundle)?;
    let projects = imported
        .iter()
        .map(|(k, c)| (k.clone(), c.project_id.clone()))
        .collect();
    let mut recorded = Recorded::new(&asks, &bundle, &projects);
    let orchestrator = match orchestrator {
        Some(given) => given,
        None => &mut recorded,
    };
    let projects = sandbox.projects()?;
    let default = projects
        .iter()
        .find(|p| p.as_str() == "fleet")
        .or_else(|| projects.first())
        .cloned()
        .ok_or_else(|| Error::Input("the bundle has no project".into()))?;

    fs::create_dir_all(&options.out)?;
    let mut log = BufWriter::new(File::create(options.out.join("decisions.jsonl"))?);
    let mut summary = Summary {
        asks: asks.len(),
        ..Summary::default()
    };
    let history = History::new(&bundle.cards, &asks);
    let mut replay = Replay::new(&mut sandbox, default, imported, history);
    for (index, ask) in asks.iter().enumerate() {
        let record = replay.ask(index, ask, orchestrator)?;
        serde_json::to_writer(&mut log, &record)?;
        log.write_all(b"\n")?;
        *summary
            .dispositions
            .entry(record.disposition.clone())
            .or_default() += 1;
        summary.decisions += record.decisions.len();
        summary.refused += record.decisions.iter().filter(|s| !s.ok).count();
        summary.cards_linked += record.cards.len();
        summary.runs += record.runs.len();
        summary.unanswered += usize::from(record.unanswered.is_some());
        let unresolved = record
            .stop
            .pointer("/found/unresolved_ask_ids")
            .and_then(Value::as_array);
        summary.stop_unresolved += unresolved.map_or(0, Vec::len);
    }
    log.flush()?;
    drop(replay);
    let audit = Audit::run(
        &sandbox,
        &options.bundle,
        &before,
        options.real_home.as_deref(),
    )?;
    fs::write(
        options.out.join("audit.json"),
        serde_json::to_vec_pretty(&audit)?,
    )?;
    summary.audit = audit;
    Ok(summary)
}
