//! Handoff and resume: the record the orchestrator writes is enough for a
//! fresh orchestrator to pick the project up, and the breakpoint note is
//! sent once, to the right session, only when it still helps.

use serde_json::{Value, json};
use tiller_core::domain::{
    Activity, AnsweredVia, Boundary, Card, CardKind, CardOutcome, Delivery, Effort, OutboxKind,
    OutboxMessage, RunOutcome, RunRole, StatusState,
};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::caches::QuotaReading;
use tiller_core::state::cards::NewCard;
use tiller_core::state::list;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::records::NewHandoff;
use tiller_core::state::runs::{EndRun, NewObservation, NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx, Store, Timestamp};

use super::*;

struct World {
    _home: tempfile::TempDir,
    store: Store,
    orchestrator: String,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(home.path().join("state.db")).unwrap();
        let owner = Ctx::new(Actor::Owner);
        store
            .init(
                &owner,
                Init {
                    host_label: "test-host".into(),
                    lane_capacity: 4,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap();
        let mut world = Self {
            _home: home,
            store,
            orchestrator: String::new(),
        };
        world.project("alpha");
        world.orchestrator = world.orchestrate("alpha", "claude-orch-1");
        world
    }

    fn project(&mut self, id: &str) {
        self.store
            .project_add(
                &Ctx::new(Actor::Owner),
                NewProject {
                    id: id.into(),
                    name: id.into(),
                    repo: Some(format!("example/{id}")),
                    remote: None,
                    default_branch: None,
                    delivery: Delivery::DirectPr,
                    checks: None,
                    capacity: Some(2),
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
            )
            .unwrap();
    }

    fn orchestrate(&mut self, project: &str, claude_id: &str) -> String {
        self.store
            .session_start(
                &Ctx::new(Actor::Owner),
                NewOrchestrator {
                    project_id: project.into(),
                    claude_session_id: Some(claude_id.into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id
    }

    fn as_orchestrator(&self) -> Ctx {
        Ctx::new(Actor::Session(self.orchestrator.clone()))
    }

    fn card(&mut self, project: &str, title: &str) -> Card {
        self.store
            .card_create(
                &Ctx::new(Actor::Owner),
                None,
                NewCard {
                    project_id: project.into(),
                    kind: CardKind::Ship,
                    title: title.into(),
                    ..NewCard::default()
                },
                None,
            )
            .unwrap()
            .value
    }

    /// Starts a builder run on `card`; returns the run id and its session.
    fn builder(&mut self, card: &Card) -> (String, String) {
        self.try_builder(card, SpawnChecks::default()).unwrap()
    }

    fn try_builder(
        &mut self,
        card: &Card,
        checks: SpawnChecks,
    ) -> tiller_core::Result<(String, String)> {
        let started = self
            .store
            .run_start(
                &self.as_orchestrator(),
                None,
                NewRun {
                    card: card.id.clone(),
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: None,
                    branch: None,
                    base_branch: None,
                    base_sha: None,
                    config_hash: None,
                    checks,
                },
            )?
            .value;
        Ok((started.run.id, started.session.id))
    }

    fn end_run(&mut self, run: &str, outcome: RunOutcome) {
        self.store
            .run_end(
                &Ctx::new(Actor::Owner),
                run,
                EndRun {
                    outcome,
                    note: None,
                },
            )
            .unwrap();
    }

    fn away_start(&mut self, scope: Value) -> tiller_core::Result<()> {
        self.store
            .away_start(&Ctx::new(Actor::Owner), scope)
            .map(|_| ())
    }

    fn away_scope(&self, spend: Value) -> Value {
        json!({
            "until_at": Timestamp::now().plus_minutes(24 * 60),
            "projects": ["alpha"],
            "orders": "keep going on the export; hold anything that changes the schema",
            "spend": spend,
            "merge": "none",
        })
    }

    fn quota(&mut self, percent: f64) {
        self.store
            .cache_quota(
                &Ctx::new(Actor::Daemon),
                vec![QuotaReading {
                    label: "claude-weekly".into(),
                    percent_remaining: Some(percent),
                    resets_at: None,
                    pace: None,
                    reserve: None,
                    burn_multiple: None,
                    elapsed_percent: None,
                    projected_exhaustion_at: None,
                    title: None,
                    kind: Some("weekly".into()),
                    fetch_error: None,
                }],
            )
            .unwrap();
    }

    fn status(&mut self, run: &str, line: &str) {
        self.store
            .run_status(&Ctx::new(Actor::Owner), run, StatusState::Working, line)
            .unwrap();
    }

    fn decision(&mut self, card: &Card, question: &str) -> String {
        self.store
            .attention_open(
                &self.as_orchestrator(),
                None,
                NewAttention {
                    card: card.display_id.clone(),
                    question: question.into(),
                    recommendation: Some("yes, behind a flag".into()),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value
            .id
    }

    fn handoff(&mut self, conclusions: &str, next: &str) {
        self.store
            .handoff_write(
                &self.as_orchestrator(),
                NewHandoff {
                    session_id: self.orchestrator.clone(),
                    conclusions: conclusions.into(),
                    next_action: next.into(),
                },
            )
            .unwrap();
    }

    fn brief(&self, project: &str) -> Option<Brief> {
        Brief::load(self.store.conn(), project).unwrap()
    }

    fn pre_compact(&mut self, session: &str, at: Timestamp) {
        self.store
            .observation_record(
                &Ctx::new(Actor::Hook),
                NewObservation {
                    session_id: Some(session.into()),
                    run_id: None,
                    source: tiller_core::domain::ObservationSource::Hook,
                    event: "PreCompact".into(),
                    verdict: None::<Activity>,
                    observed_at: at,
                    payload: json!({"trigger": "auto"}),
                },
            )
            .unwrap();
    }

    fn notes(&self) -> Vec<OutboxMessage> {
        list(
            self.store.conn(),
            "kind = 'note' ORDER BY created_at, id",
            &[],
        )
        .unwrap()
    }
}

/// The three questions a fresh orchestrator is asked, answered from the
/// record's JSON and nothing else: no store, no card show.
struct Resumed(Value);

impl Resumed {
    /// Which cards are active, and which worker is on each?
    fn active_cards_and_workers(&self) -> Vec<(String, Option<String>)> {
        let workers = self.0["workers"].as_array().unwrap();
        self.0["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|card| {
                let tag = card["card"].as_str().unwrap().to_owned();
                let worker = workers
                    .iter()
                    .find(|w| w["card"] == card["card"])
                    .map(|w| w["role"].as_str().unwrap().to_owned());
                (tag, worker)
            })
            .collect()
    }

    /// Which decision is waiting on the owner, and for which card?
    fn pending_decision(&self) -> Vec<(String, String)> {
        self.0["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["standing"] == "pending")
            .map(|d| {
                (
                    d["card"].as_str().unwrap().to_owned(),
                    d["question"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// What was decided, and what happens next?
    fn conclusion_and_next(&self) -> (String, String) {
        (
            self.0["conclusions"].as_str().unwrap().to_owned(),
            self.0["next_action"].as_str().unwrap().to_owned(),
        )
    }
}

fn at(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

#[test]
fn fresh_orchestrator_resumes_from_handoff_record() {
    let mut w = World::new();
    let login = w.card("alpha", "the login page");
    let export = w.card("alpha", "the billing export");
    let (run, _) = w.builder(&login);
    w.status(&run, "PR open, waiting on checks");
    w.decision(&export, "ship the export behind a flag?");
    w.handoff(
        "login is a builder run; export waits on the flag decision",
        "ask the owner about the flag, then start a builder on the export",
    );

    // The orchestrator is cleared (or crashes); a fresh one takes over. The
    // old session wrote the record; the new one has only the record.
    let old = w.orchestrator.clone();
    w.store
        .session_end(&Ctx::new(Actor::Owner), &old, "clear")
        .unwrap();
    w.orchestrate("alpha", "claude-orch-2");

    let brief = w.brief("alpha").expect("a handoff exists");
    let resumed = Resumed(serde_json::to_value(&brief).unwrap());

    assert_eq!(
        resumed.active_cards_and_workers(),
        vec![
            (login.display_id.clone(), Some("builder".into())),
            (export.display_id.clone(), None),
        ],
    );
    assert_eq!(
        resumed.pending_decision(),
        vec![(
            export.display_id.clone(),
            "ship the export behind a flag?".into()
        )],
    );
    assert_eq!(
        resumed.conclusion_and_next(),
        (
            "login is a builder run; export waits on the flag decision".into(),
            "ask the owner about the flag, then start a builder on the export".into(),
        ),
    );
    // The worker's line travels with it, and nothing moved yet.
    assert_eq!(brief.workers[0].line, "working: PR open, waiting on checks");
    assert_eq!(brief.workers[0].status_now, None);
    assert_eq!(brief.changed_since, Changed::default());
    assert!(brief.unresolved.is_empty());
}

#[test]
fn brief_marks_what_moved_since_the_handoff() {
    let mut w = World::new();
    let login = w.card("alpha", "the login page");
    let export = w.card("alpha", "the billing export");
    let (run, _) = w.builder(&login);
    w.status(&run, "PR open");
    let item = w.decision(&export, "ship behind a flag?");
    w.handoff("two cards", "wait for the owner");

    // After the handoff: the run reports on, the owner answers, the login
    // card closes with its run, and a new card and question appear.
    w.status(&run, "checks green");
    w.store
        .attention_answer(
            &Ctx::new(Actor::Owner),
            None,
            &item,
            "yes",
            AnsweredVia::Cli,
        )
        .unwrap();
    w.store
        .run_end(
            &Ctx::new(Actor::Owner),
            &run,
            EndRun {
                outcome: RunOutcome::Failed,
                note: Some("gave up".into()),
            },
        )
        .unwrap();
    w.store
        .card_close(
            &Ctx::new(Actor::Owner),
            &login.id,
            CardOutcome::Cancelled,
            Some("dropped"),
            None,
        )
        .unwrap();
    let invoices = w.card("alpha", "the invoice email");
    let new_item = w.decision(&invoices, "send from which address?");

    let brief = w.brief("alpha").unwrap();
    let login_line = &brief.cards[0];
    assert_eq!(login_line.card, login.display_id);
    assert_eq!(login_line.state, tiller_core::domain::CardState::Closed);
    assert_eq!(login_line.outcome, Some(CardOutcome::Cancelled));
    assert_eq!(brief.cards[1].state, tiller_core::domain::CardState::Open);

    assert_eq!(brief.decisions[0].standing, Standing::Answered);
    assert_eq!(brief.decisions[0].answer.as_deref(), Some("yes"));

    let worker = &brief.workers[0];
    assert!(worker.ended);
    assert_eq!(worker.outcome, Some(RunOutcome::Failed));
    assert_eq!(worker.line, "working: PR open");
    assert_eq!(worker.status_now.as_deref(), Some("working: checks green"));

    assert_eq!(
        brief
            .changed_since
            .cards
            .iter()
            .map(|c| c.card.as_str())
            .collect::<Vec<_>>(),
        vec![invoices.display_id.as_str()],
    );
    assert_eq!(brief.changed_since.decisions.len(), 1);
    assert_eq!(brief.changed_since.decisions[0].attention_id, new_item);
    assert_eq!(brief.changed_since.decisions[0].standing, Standing::Pending);
    assert!(brief.changed_since.workers.is_empty());
}

#[test]
fn the_newest_handoff_of_this_project_is_the_record() {
    let mut w = World::new();
    w.card("alpha", "the login page");
    w.handoff("first view", "first next");
    w.handoff("second view", "second next");

    // Another project's later handoff must not leak in.
    w.project("beta");
    let beta = w.orchestrate("beta", "claude-orch-beta");
    w.store
        .handoff_write(
            &Ctx::new(Actor::Session(beta.clone())),
            NewHandoff {
                session_id: beta,
                conclusions: "beta view".into(),
                next_action: "beta next".into(),
            },
        )
        .unwrap();

    let alpha = w.brief("alpha").unwrap();
    assert_eq!(alpha.conclusions, "second view");
    assert_eq!(alpha.next_action, "second next");
    assert_eq!(w.brief("beta").unwrap().conclusions, "beta view");
}

#[test]
fn a_project_with_no_handoff_has_no_brief() {
    let mut w = World::new();
    w.card("alpha", "the login page");
    assert_eq!(w.brief("alpha"), None);
}

#[test]
fn an_id_the_record_names_but_cannot_resolve_is_listed_not_dropped() {
    let mut w = World::new();
    let login = w.card("alpha", "the login page");
    w.handoff("one card", "go");
    w.store
        .conn()
        .execute(
            "UPDATE handoffs SET active_cards = ?1",
            [json!([login.id, "01H00000000000000000000000"]).to_string()],
        )
        .unwrap();

    let brief = w.brief("alpha").unwrap();
    assert_eq!(brief.cards.len(), 1);
    assert_eq!(brief.unresolved, vec!["01H00000000000000000000000"]);
}

#[test]
fn a_malformed_entry_in_the_record_is_listed_not_read_as_an_empty_fleet() {
    let mut w = World::new();
    let login = w.card("alpha", "the login page");
    let (_, _) = w.builder(&login);
    w.handoff("one card", "go");
    w.store
        .conn()
        .execute(
            "UPDATE handoffs SET active_cards = '[7]', workers = '[{\"run\": 1}]'",
            [],
        )
        .unwrap();

    let brief = w.brief("alpha").unwrap();
    assert!(brief.cards.is_empty() && brief.workers.is_empty());
    assert_eq!(brief.unresolved, vec!["7", r#"{"run":1}"#]);
}

#[test]
fn compaction_notes_the_orchestrator_once() {
    let mut w = World::new();
    let orchestrator = w.orchestrator.clone();
    let now = Timestamp::now();
    w.pre_compact(&orchestrator, now);

    let sent = signal_compactions(&mut w.store, now).unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].kind, OutboxKind::Note);
    assert_eq!(sent[0].target_session_id, orchestrator);
    assert_eq!(sent[0].body, BREAKPOINT_NOTE);
    assert!(sent[0].body.contains("tiller handoff write"));
    assert!(sent[0].body.contains("Only the owner clears a session"));

    // The hook and the daemon's pass may both call it.
    assert!(signal_compactions(&mut w.store, now).unwrap().is_empty());
    assert_eq!(w.notes().len(), 1);

    // The next compaction is a new breakpoint.
    w.pre_compact(&orchestrator, at(now.as_millis() + 60_000));
    let later = at(now.as_millis() + 60_000);
    assert_eq!(signal_compactions(&mut w.store, later).unwrap().len(), 1);
}

#[test]
fn compaction_note_is_for_a_live_orchestrator_that_still_needs_it() {
    let mut w = World::new();
    let orchestrator = w.orchestrator.clone();
    let now = Timestamp::now();

    // A worker compacting is its own business.
    let card = w.card("alpha", "the login page");
    let (_, worker) = w.builder(&card);
    w.pre_compact(&worker, now);
    assert!(signal_compactions(&mut w.store, now).unwrap().is_empty());

    // An observation older than the window is over.
    w.pre_compact(&orchestrator, at(now.as_millis() - 31 * 60_000));
    assert!(signal_compactions(&mut w.store, now).unwrap().is_empty());

    // A handoff written before the PreCompact does not answer it.
    w.handoff("earlier", "earlier next");
    let after_it = Timestamp::now();
    w.pre_compact(&orchestrator, at(after_it.as_millis() + 1_000));
    let sent = signal_compactions(&mut w.store, at(after_it.as_millis() + 1_000)).unwrap();
    assert_eq!(sent.len(), 1);

    // A handoff written after the PreCompact answers it already.
    let seen = Timestamp::now();
    w.pre_compact(&orchestrator, seen);
    w.handoff("done", "next");
    assert!(signal_compactions(&mut w.store, seen).unwrap().is_empty());

    // A session that has ended gets nothing, and nothing errors.
    let later = at(Timestamp::now().as_millis() + 120_000);
    w.pre_compact(&orchestrator, later);
    w.store
        .session_end(&Ctx::new(Actor::Owner), &orchestrator, "clear")
        .unwrap();
    assert!(signal_compactions(&mut w.store, later).unwrap().is_empty());
    assert_eq!(w.notes().len(), 1, "only the earlier-handoff note exists");
}

// -- P34: away mode ------------------------------------------------------------

#[test]
fn away_contract_caps_spend_and_writes_return_brief() {
    let mut w = World::new();
    let before = w.card("alpha", "started before the owner left");
    let export = w.card("alpha", "the billing export");
    let login = w.card("alpha", "the login page");
    let report = w.card("alpha", "the usage report");

    // A run from before the owner left does not count against the cap.
    let (early, _) = w.builder(&before);
    w.end_run(&early, RunOutcome::Abandoned);

    let scope = w.away_scope(json!({"max_new_runs": 2}));
    w.away_start(scope).unwrap();

    // The orchestrator reads the orders and what is left to spend.
    let line = {
        w.handoff("export and login are next", "start both builders");
        w.brief("alpha").unwrap().away.unwrap()
    };
    assert_eq!(
        line.orders.as_deref(),
        Some("keep going on the export; hold anything that changes the schema")
    );
    assert_eq!((line.max_new_runs, line.new_runs), (Some(2), 0));
    assert!(line.covers_project && line.no_merges);

    let (export_run, _) = w.builder(&export);
    w.status(&export_run, "PR open, waiting on checks");
    w.end_run(&export_run, RunOutcome::Abandoned);
    let (login_run, _) = w.builder(&login);
    w.decision(&login, "ship the login behind a flag?");

    // The third new run is over the cap, and says why.
    let refused = w.try_builder(&report, SpawnChecks::default()).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("away mode allows 2 new runs and 2 have started"),
        "{refused}"
    );
    assert_eq!(w.brief("alpha").unwrap().away.unwrap().new_runs, 2);

    // The owner returns. The brief says what the contract allowed, what it
    // spent, what ran and what waits.
    w.store
        .away_end(&Ctx::new(Actor::Owner), "ended by the owner")
        .unwrap();
    let brief = ReturnBrief::load(w.store.conn(), None, Timestamp::now())
        .unwrap()
        .unwrap();
    assert!(!brief.away);
    assert_eq!(brief.ended_reason.as_deref(), Some("ended by the owner"));
    assert_eq!(brief.projects, ["alpha"]);
    assert_eq!(
        brief.orders.as_deref(),
        Some("keep going on the export; hold anything that changes the schema")
    );
    assert_eq!(
        (
            brief.spend.max_new_runs,
            brief.spend.new_runs,
            brief.spend.cap_reached
        ),
        (Some(2), 2, true)
    );
    let ran: Vec<_> = brief
        .runs
        .iter()
        .map(|run| (run.card.as_str(), run.outcome))
        .collect();
    assert_eq!(
        ran,
        [
            (export.display_id.as_str(), Some(RunOutcome::Abandoned)),
            (login.display_id.as_str(), None),
        ]
    );
    assert_eq!(
        brief.runs[0].status.as_deref(),
        Some("working: PR open, waiting on checks")
    );
    assert_eq!(brief.waiting_decisions.len(), 1);
    assert_eq!(
        brief.waiting_decisions[0].question,
        "ship the login behind a flag?"
    );
    assert!(brief.landed.is_empty() && brief.merged_by_grant.is_empty());

    // The cap ended with the contract, and the orchestrator no longer reads
    // orders.
    assert!(w.brief("alpha").unwrap().away.is_none());
    w.end_run(&login_run, RunOutcome::Abandoned);
    w.try_builder(&report, SpawnChecks::default()).unwrap();
}

#[test]
fn away_quota_floor_binds_when_the_standing_floor_is_looser() {
    let mut w = World::new();
    let export = w.card("alpha", "the billing export");
    let scope = w.away_scope(json!({"quota_floor_percent": 20}));
    w.away_start(scope).unwrap();
    let checks = || SpawnChecks {
        quota_floor_percent: Some(3.0),
        quota_max_age_minutes: 15,
        ..SpawnChecks::default()
    };

    // 12 percent left clears the standing 3 percent floor, not the away 20.
    w.quota(12.0);
    let refused = w.try_builder(&export, checks()).unwrap_err();
    assert!(
        refused.to_string().contains("under the 20 percent floor")
            || refused.to_string().contains("at or under the 20"),
        "{refused}"
    );

    w.quota(45.0);
    w.try_builder(&export, checks()).unwrap();
}

#[test]
fn away_scope_that_cannot_be_enforced_is_refused() {
    let mut w = World::new();
    for spend in [
        json!({"max_new_runs": -1}),
        json!({"max_new_runs": "six"}),
        json!({"quota_floor_percent": 140}),
    ] {
        let scope = w.away_scope(spend);
        assert!(w.away_start(scope).is_err());
    }
    let mut scope = w.away_scope(json!({}));
    scope["merge"] = json!("sometimes");
    assert!(w.away_start(scope).is_err());
    assert!(
        ReturnBrief::load(w.store.conn(), None, Timestamp::now())
            .unwrap()
            .is_none()
    );
}
