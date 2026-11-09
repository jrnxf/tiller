//! Intake end to end: each test drives `tiller hook` the way Claude Code
//! does (JSON on stdin, a file `state.db` under a Tiller home) and checks
//! what the hook printed and what it wrote.

use serde_json::{Value, json};
use tiller_core::domain::{
    Ask, AskCard, Boundary, Card, CardKind, Delivery, Disposition, Effort, Observation,
    OutboxMessage, OutboxState, RunRole,
};
use tiller_core::state::asks::{NewAsk, Resolution};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store};
use tiller_observe::hooks::{Env, Outcome, run_with};

use super::*;

const CLAUDE_ORCH: &str = "claude-orch";

/// Project `alpha` with a live orchestrator and a builder run on `alpha-1`,
/// in a `state.db` under a temporary Tiller home.
struct World {
    home: tempfile::TempDir,
    store: Store,
    orchestrator: String,
    worker: String,
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
                    lane_capacity: 2,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap();
        store
            .project_add(
                &owner,
                NewProject {
                    id: "alpha".into(),
                    name: "Alpha".into(),
                    repo: Some("example/alpha".into()),
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
        let orchestrator = store
            .session_start(
                &owner,
                NewOrchestrator {
                    project_id: "alpha".into(),
                    claude_session_id: Some(CLAUDE_ORCH.into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id;
        let card = new_card(&mut store, "the login page");
        let worker = store
            .run_start(
                &Ctx::new(Actor::Session(orchestrator.clone())),
                None,
                NewRun {
                    card: card.id,
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: None,
                    branch: None,
                    base_branch: None,
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value
            .session
            .id;
        Self {
            home,
            store,
            orchestrator,
            worker,
        }
    }

    fn env(&self, tiller_session_id: Option<&str>) -> Env {
        Env {
            tiller_home: self.home.path().to_owned(),
            user_home: None,
            tiller_session_id: tiller_session_id.map(str::to_owned),
        }
    }

    /// Runs one hook in the orchestrator's session, as `tiller hook` does.
    fn hook(&self, intake: &Hooks, event: &str, input: Value) -> (Outcome, Option<Value>) {
        self.hook_in(None, intake, event, input)
    }

    fn hook_in(
        &self,
        session: Option<&str>,
        intake: &Hooks,
        event: &str,
        mut input: Value,
    ) -> (Outcome, Option<Value>) {
        input["hook_event_name"] = json!(event);
        if input.get("session_id").is_none() {
            input["session_id"] = json!(CLAUDE_ORCH);
        }
        let bytes = serde_json::to_vec(&input).unwrap();
        run_with(event, &bytes, &self.env(session), Some(intake))
    }

    /// The owner types `text` in the orchestrator pane. Returns the ask id
    /// the hook's context line names.
    fn prompt(&self, prompt_id: &str, text: &str) -> String {
        let (_, output) = self.hook(
            &Hooks::default(),
            "UserPromptSubmit",
            json!({"prompt": text, "prompt_id": prompt_id}),
        );
        let context = context(output.as_ref());
        let id = context
            .strip_prefix("tiller: ask ")
            .and_then(|rest| rest.split_once(' '))
            .map(|(id, _)| id.to_owned())
            .unwrap_or_else(|| panic!("no ask in {context:?}"));
        let ask: Ask = get(self.store.conn(), "ask", &id).unwrap();
        assert_eq!(ask.text, text);
        id
    }

    /// The orchestrator's turn ends with `reply`. Returns the Stop
    /// observation's payload and what the hook printed.
    fn stop(&self, intake: &Hooks, reply: &str, active: bool) -> (Value, Option<Value>) {
        let (outcome, output) = self.hook(
            intake,
            "Stop",
            json!({"last_assistant_message": reply, "stop_hook_active": active, "nonce": ids::new_ulid()}),
        );
        let Outcome::Recorded { observation_id } = outcome else {
            panic!("Stop not recorded: {outcome:?}");
        };
        let observation: Observation =
            get(self.store.conn(), "observation", &observation_id).unwrap();
        (observation.payload, output)
    }

    fn resolve(&mut self, ask: &str, resolution: Resolution) -> Option<Card> {
        let ctx = Ctx::new(Actor::Session(self.orchestrator.clone()));
        self.store
            .ask_resolve(&ctx, ask, resolution)
            .unwrap()
            .value
            .card
    }

    fn count(&self, sql: &str) -> i64 {
        self.store
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }
}

/// Filed as alpha's own orchestrator, so no note reaches the outbox.
fn new_card(store: &mut Store, title: &str) -> Card {
    let orchestrator: String = store
        .conn()
        .query_row(
            "SELECT id FROM sessions WHERE role = 'orchestrator' AND project_id = 'alpha' AND ended_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .card_create(
            &Ctx::new(Actor::Session(orchestrator)),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: title.into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value
}

fn new_resolution(title: &str) -> Resolution {
    Resolution::New(NewCard {
        kind: CardKind::Ship,
        title: title.into(),
        ..NewCard::default()
    })
}

fn context(output: Option<&Value>) -> String {
    output
        .and_then(|output| output.pointer("/hookSpecificOutput/additionalContext"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no additionalContext in {output:?}"))
        .to_owned()
}

fn strings(payload: &Value, key: &str) -> Vec<String> {
    serde_json::from_value(payload[key].clone()).unwrap()
}

#[test]
fn three_interleaved_asks_map_to_the_right_cards() {
    let mut world = World::new();
    // Three asks arrive before the orchestrator resolves any of them.
    let login = world.prompt("p1", "the login page still 500s on a bad password");
    let export = world.prompt("p2", "add a CSV export to the report");
    let theme = world.prompt("p3", "and a dark theme please");
    assert_eq!(world.count("SELECT count(*) FROM asks"), 3);

    // The turn ends with all three open: each one is named.
    let (payload, output) = world.stop(&Hooks::default(), "Looking at all three.", false);
    assert_eq!(
        strings(&payload, "unresolved_ask_ids"),
        [&login, &export, &theme].map(String::as_str)
    );
    assert_eq!(payload["would_block"], true);
    assert_eq!(output, None, "observe mode never blocks");

    // Resolved out of order, by short id as the context line says.
    let export_card = world
        .resolve(ids::short(&export), new_resolution("CSV export"))
        .unwrap();
    let login_card = world
        .resolve(
            ids::short(&login),
            Resolution::Followup {
                card: "alpha-1".into(),
            },
        )
        .unwrap();
    let theme_card = world
        .resolve(ids::short(&theme), new_resolution("Dark theme"))
        .unwrap();
    assert_eq!(
        (
            login_card.display_id.as_str(),
            export_card.display_id.as_str(),
            theme_card.display_id.as_str()
        ),
        ("alpha-1", "alpha-2", "alpha-3")
    );

    let links: Vec<(String, String)> =
        list::<AskCard>(world.store.conn(), "1 ORDER BY linked_at, ask_id", &[])
            .unwrap()
            .into_iter()
            .map(|link| (link.ask_id, link.card_id))
            .collect();
    let mut want = vec![
        (login.clone(), login_card.id.clone()),
        (export.clone(), export_card.id.clone()),
        (theme.clone(), theme_card.id.clone()),
    ];
    let mut got = links;
    want.sort();
    got.sort();
    assert_eq!(got, want, "each ask links to its own card and no other");
    let created: Card = find(world.store.conn(), "created_by_ask_id = ?1", &[&export])
        .unwrap()
        .unwrap();
    assert_eq!(created.id, export_card.id);

    // The reply interleaves the three cards line by line; the tags map each
    // line back to its card, and nothing is left to block on.
    let reply = "[alpha-2] export is queued.\n[alpha-1] the 500 is a missing null check.\n[chat] also,\n  [alpha-3] dark theme next.\nSee [alpha-9] inline, which is not a tag.";
    let (payload, _) = world.stop(&Hooks::default(), reply, false);
    assert_eq!(strings(&payload, "tags"), ["alpha-2", "alpha-1", "alpha-3"]);
    assert!(strings(&payload, "unknown_tags").is_empty());
    assert!(strings(&payload, "unresolved_ask_ids").is_empty());
    assert_eq!(payload["would_block"], false);

    // Each prompt's observation names the ask it recorded.
    let prompts: Vec<Observation> = list(
        world.store.conn(),
        "event = 'UserPromptSubmit' ORDER BY observed_at, id",
        &[],
    )
    .unwrap();
    let asks: Vec<&str> = prompts
        .iter()
        .filter_map(|o| o.payload["ask_id"].as_str())
        .collect();
    assert_eq!(asks, [&login, &export, &theme].map(String::as_str));
}

#[test]
fn redelivered_prompt_is_one_ask_and_a_new_prompt_is_another() {
    let world = World::new();
    let first = world.prompt("p1", "continue");
    assert_eq!(
        world.prompt("p1", "continue"),
        first,
        "same prompt_id replays"
    );
    let second = world.prompt("p2", "continue");
    assert_ne!(second, first, "the same words typed again are a new ask");

    // Without a prompt_id the key is the text at the transcript's length.
    let transcript = world.home.path().join("t.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let input = json!({"prompt": "go on", "transcript_path": transcript});
    let ask_of = |input: &Value| {
        let (_, output) = world.hook(&Hooks::default(), "UserPromptSubmit", input.clone());
        context(output.as_ref())
    };
    let first = ask_of(&input);
    assert_eq!(
        ask_of(&input),
        first,
        "a redelivery at the same length replays"
    );
    std::fs::write(&transcript, "{}\n{}\n").unwrap();
    assert_ne!(ask_of(&input), first, "the transcript grew: a new prompt");
    assert_eq!(
        world.count("SELECT count(*) FROM asks WHERE text = 'go on'"),
        2
    );
}

#[test]
fn context_line_tells_the_orchestrator_how_to_resolve() {
    let world = World::new();
    let (_, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": "ship it", "prompt_id": "p1"}),
    );
    let ask: Ask = find(world.store.conn(), "text = 'ship it'", &[])
        .unwrap()
        .unwrap();
    let short = ids::short(&ask.id);
    assert_eq!(
        context(output.as_ref()),
        format!(
            "tiller: ask {} (short {short}) recorded. Before this turn ends run: tiller ask resolve {short} new|followup:<card>|chat|cancelled. Start each line about a card with its tag, like [alpha-42]. Project alpha: 1 free run slot: spawn at most one run, leave other cards queued and say they are queued, not started.",
            ask.id
        )
    );
    assert_eq!(ask.disposition, Disposition::Unresolved);
    assert_eq!(ask.session_id.as_deref(), Some(world.orchestrator.as_str()));
}

/// The T5.2 replay (a23.6): a full project's orchestrator was never told so,
/// spawned past the limit and reported the refused card as started.
#[test]
fn context_line_says_when_the_project_has_no_free_run_slot() {
    let mut world = World::new();
    let card = new_card(&mut world.store, "the sign-out link");
    world
        .store
        .run_start(
            &Ctx::new(Actor::Session(world.orchestrator.clone())),
            None,
            NewRun {
                card: card.id,
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: None,
                branch: None,
                base_branch: None,
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap();
    assert_eq!(world.store.project_free_runs("alpha").unwrap(), 0);
    let (_, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": "drop sign-out", "prompt_id": "p9"}),
    );
    let text = context(output.as_ref());
    assert!(
        text.contains("Project alpha: no free run slots: a spawn would be refused, so leave the card queued and say it is queued, not started."),
        "{text}"
    );
}

#[test]
fn wake_line_is_acked_not_recorded_as_ask() {
    let mut world = World::new();
    // A script's note wakes the orchestrator through the outbox.
    let noted = world
        .store
        .ask_record(
            &Ctx::new(Actor::Owner),
            Some("note-1"),
            NewAsk {
                project_id: "alpha".into(),
                session_id: None,
                source: tiller_core::domain::AskSource::Script,
                text: "weekly digest is ready".into(),
            },
        )
        .unwrap()
        .value;
    let message = noted.message.unwrap();
    let asks = world.count("SELECT count(*) FROM asks");

    let line = format!("tiller inbox {} # note", message.id);
    let (outcome, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": line, "prompt_id": "w1"}),
    );
    assert!(matches!(outcome, Outcome::Recorded { .. }));
    assert!(context(output.as_ref()).contains("weekly digest is ready"));
    assert_eq!(world.count("SELECT count(*) FROM asks"), asks, "no new ask");
    let acked: OutboxMessage = get(world.store.conn(), "outbox message", &message.id).unwrap();
    assert_eq!(acked.state, OutboxState::Acked);
    assert_eq!(
        world.count("SELECT count(*) FROM observations WHERE event = 'wake'"),
        1
    );

    // The line again returns the body and acks nothing twice.
    let (_, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": line, "prompt_id": "w2"}),
    );
    assert!(context(output.as_ref()).contains("weekly digest is ready"));
    assert_eq!(
        world.count("SELECT count(*) FROM events WHERE kind = 'outbox.acked'"),
        1
    );

    // A line for another session is never acked.
    let steer = world
        .store
        .outbox_enqueue(
            &Ctx::new(Actor::Session(world.orchestrator.clone())),
            Some("steer-1"),
            tiller_core::state::outbox::NewMessage {
                target_session_id: world.worker.clone(),
                kind: tiller_core::domain::OutboxKind::Steer,
                body: "use the new helper".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap()
        .value;
    let (_, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": format!("tiller inbox {}", steer.id), "prompt_id": "w3"}),
    );
    assert!(context(output.as_ref()).contains("not for this session"));
    let steer: OutboxMessage = get(world.store.conn(), "outbox message", &steer.id).unwrap();
    assert_ne!(steer.state, OutboxState::Acked);
    assert_eq!(world.count("SELECT count(*) FROM asks"), asks);
}

#[test]
fn board_card_wake_does_not_create_second_card() {
    let mut world = World::new();
    let added = world
        .store
        .board_add(
            &Ctx::new(Actor::Board),
            Some("click-1"),
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: "rate limit the API".into(),
                ..NewCard::default()
            },
        )
        .unwrap()
        .value;
    let message = added.message.unwrap();
    let (asks, cards) = (
        world.count("SELECT count(*) FROM asks"),
        world.count("SELECT count(*) FROM cards"),
    );

    // The daemon types the wake-up line; the orchestrator reads the note.
    let line = format!(
        "tiller inbox {} # note {}",
        message.id, added.card.display_id
    );
    let (_, output) = world.hook(
        &Hooks::default(),
        "UserPromptSubmit",
        json!({"prompt": line, "prompt_id": "w1"}),
    );
    let context = context(output.as_ref());
    assert!(
        context.contains("[alpha-2] added from the board"),
        "{context}"
    );
    assert_eq!(world.count("SELECT count(*) FROM asks"), asks);
    assert_eq!(world.count("SELECT count(*) FROM cards"), cards);

    // The orchestrator's reply about it has nothing to resolve or create.
    let (payload, _) = world.stop(&Hooks::default(), "[alpha-2] queued behind alpha-1.", false);
    assert_eq!(payload["would_block"], false);
    assert_eq!(strings(&payload, "tags"), ["alpha-2"]);
}

#[test]
fn board_created_card_is_not_created_twice() {
    let mut world = World::new();
    let add = |world: &mut World| {
        world
            .store
            .board_add(
                &Ctx::new(Actor::Board),
                Some("click-1"),
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: "rate limit the API".into(),
                    ..NewCard::default()
                },
            )
            .unwrap()
    };
    let first = add(&mut world);
    let again = add(&mut world);
    assert!(again.replayed, "a double click replays");
    assert_eq!(again.value.card.id, first.value.card.id);
    assert_eq!(
        world.count("SELECT count(*) FROM cards WHERE title = 'rate limit the API'"),
        1
    );
    assert_eq!(
        world.count("SELECT count(*) FROM asks WHERE source = 'board'"),
        1
    );
    assert_eq!(
        world.count("SELECT count(*) FROM outbox WHERE body LIKE '%added from the board%'"),
        1
    );
    // The board's ask is already resolved: it never waits in Asked.
    assert!(world.store.asked(Some("alpha")).unwrap().is_empty());
}

#[test]
fn unresolved_asks_stay_visible() {
    let mut world = World::new();
    let ask = world.prompt("p1", "look into the flaky test");
    for turn in ["Looking.", "Still looking."] {
        let (payload, _) = world.stop(&Hooks::default(), turn, false);
        assert_eq!(strings(&payload, "unresolved_ask_ids"), [ask.as_str()]);
    }
    // Nothing resolves it on its own: it sits in Asked, with its age.
    let asked = world.store.asked(Some("alpha")).unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].ask.id, ask);
    assert!(asked[0].age_seconds >= 0.0);

    // A successor orchestrator is told about it when it starts.
    world
        .store
        .session_end(&Ctx::new(Actor::Owner), &world.orchestrator, "replaced")
        .unwrap();
    let started = world
        .store
        .session_start(
            &Ctx::new(Actor::Owner),
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-orch-2".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value;
    let notes: Vec<OutboxMessage> = list(
        world.store.conn(),
        "target_session_id = ?1",
        &[&started.session.id],
    )
    .unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].body.contains(ids::short(&ask)));
}

#[test]
fn stop_hook_blocks_once_only_in_block_once_mode() {
    let world = World::new();
    world.prompt("p1", "what is left on alpha-1?");
    let reply = "[alpha-77] is new work.";

    let (payload, output) = world.stop(&Hooks::default(), reply, false);
    assert_eq!(payload["would_block"], true);
    assert_eq!(strings(&payload, "unknown_tags"), ["alpha-77"]);
    assert_eq!(output, None, "observe never blocks");

    let block_once = Hooks {
        config: HostConfig {
            stop_hook_mode: StopHookMode::BlockOnce,
            ..HostConfig::default()
        },
    };
    let (_, output) = world.stop(&block_once, reply, false);
    let output = output.unwrap();
    assert_eq!(output["decision"], "block");
    let reason = output["reason"].as_str().unwrap();
    assert!(
        reason.contains("resolve ask") && reason.contains("[alpha-77]"),
        "{reason}"
    );
    let (_, output) = world.stop(&block_once, reply, true);
    assert_eq!(
        output, None,
        "a turn already blocked once is never blocked again"
    );

    let off = Hooks {
        config: HostConfig {
            stop_hook_mode: StopHookMode::Off,
            ..HostConfig::default()
        },
    };
    let (payload, output) = world.stop(&off, reply, false);
    assert!(
        payload.get("would_block").is_none(),
        "off records only the turn end"
    );
    assert_eq!(output, None);
}

#[test]
fn workers_are_never_checked_and_their_prompts_follow_up_on_the_card() {
    let world = World::new();
    let block_once = Hooks {
        config: HostConfig {
            stop_hook_mode: StopHookMode::BlockOnce,
            ..HostConfig::default()
        },
    };
    let worker = Some(world.worker.as_str());
    let (_, output) = world.hook_in(
        worker,
        &block_once,
        "UserPromptSubmit",
        json!({"session_id": "claude-worker", "prompt": "use the v2 endpoint", "prompt_id": "p1"}),
    );
    assert!(context(output.as_ref()).contains("follow-up on [alpha-1]"));
    let ask: Ask = find(world.store.conn(), "text = 'use the v2 endpoint'", &[])
        .unwrap()
        .unwrap();
    assert_eq!(ask.disposition, Disposition::Followup);
    assert_eq!(
        world.count("SELECT count(*) FROM outbox WHERE body LIKE '%use the v2 endpoint%'"),
        1,
        "the orchestrator is told"
    );

    let (_, output) = world.hook_in(
        worker,
        &block_once,
        "Stop",
        json!({"session_id": "claude-worker", "last_assistant_message": "[alpha-99] done", "stop_hook_active": false}),
    );
    assert_eq!(output, None);
}

#[test]
fn transcript_host_hook_records_no_ask() {
    let world = World::new();
    let transcript = Hooks {
        config: HostConfig {
            intake_source: IntakeSource::Transcript,
            ..HostConfig::default()
        },
    };
    let (outcome, output) = world.hook(
        &transcript,
        "UserPromptSubmit",
        json!({"prompt": "hello", "prompt_id": "p1"}),
    );
    assert!(matches!(outcome, Outcome::Recorded { .. }));
    assert_eq!(output, None);
    assert_eq!(world.count("SELECT count(*) FROM asks"), 0);
}

#[test]
fn host_config_reads_config_toml_and_defaults_safely() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(HostConfig::load(home.path()), HostConfig::default());
    std::fs::write(
        home.path().join("config.toml"),
        "stop_hook_mode = \"block-once\"\nintake_source = \"transcript\"\n",
    )
    .unwrap();
    assert_eq!(
        HostConfig::load(home.path()),
        HostConfig {
            stop_hook_mode: StopHookMode::BlockOnce,
            intake_source: IntakeSource::Transcript,
            ..HostConfig::default()
        }
    );
    std::fs::write(home.path().join("config.toml"), "stop_hook_mode = [").unwrap();
    assert_eq!(HostConfig::load(home.path()), HostConfig::default());
}

#[test]
fn line_tags_take_only_card_tags_that_start_a_line() {
    let reply = "[alpha-1] one\n  [beta-22] two\n[chat] three\n[alpha-01] padded\n[Alpha-3] upper\nsee [alpha-4]\n[a-5] short\n- [alpha-6] bullet\n```\n[alpha-999] an example\n```\n[alpha-7]";
    assert_eq!(line_tags(reply), ["alpha-1", "beta-22", "alpha-7"]);
}

const ASKING: &str = "The login page is built. Do you want me to start the release now?";

fn guard_hooks(endpoint: Option<String>, key: Option<&str>) -> Hooks {
    Hooks {
        config: HostConfig {
            owner_guard: OwnerGuardConfig {
                decider_key: key.map(str::to_owned),
                endpoint,
                ..OwnerGuardConfig::default()
            },
            ..HostConfig::default()
        },
    }
}

fn open_decision(world: &mut World) {
    world
        .store
        .attention_open(
            &Ctx::new(Actor::Session(world.orchestrator.clone())),
            None,
            NewAttention {
                card: "alpha-1".into(),
                question: "Start the release now?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap();
}

#[test]
fn a_turn_that_asks_the_owner_without_an_item_is_blocked_once() {
    let mut world = World::new();
    let hooks = Hooks::default();
    world.prompt("p1", "how is the login page?");

    let (payload, output) = world.stop(&hooks, ASKING, false);
    assert_eq!(payload["asks_owner"], true);
    assert_eq!(payload["opened_attention"], false);
    let output = output.unwrap();
    assert_eq!(output["decision"], "block");
    let reason = output["reason"].as_str().unwrap();
    assert!(reason.contains("tiller attention open"), "{reason}");
    assert!(reason.contains("end the turn again"), "{reason}");

    let (_, output) = world.stop(&hooks, ASKING, true);
    assert_eq!(
        output, None,
        "a turn already blocked once is never blocked again"
    );

    let (payload, output) = world.stop(&hooks, "The login page is built.", false);
    assert_eq!(payload["asks_owner"], false);
    assert_eq!(output, None);

    open_decision(&mut world);
    let (payload, output) = world.stop(&hooks, ASKING, false);
    assert_eq!(payload["opened_attention"], true);
    assert_eq!(output, None, "an item opened this turn satisfies the guard");

    world.prompt("p2", "and the signup page?");
    let (_, output) = world.stop(&hooks, ASKING, false);
    assert!(output.is_some(), "an item from an earlier turn does not");
}

#[test]
fn the_owner_guard_can_be_turned_off_and_skips_workers() {
    let world = World::new();
    world.prompt("p1", "how is the login page?");
    let disabled = Hooks {
        config: HostConfig {
            owner_guard: OwnerGuardConfig {
                enabled: false,
                ..OwnerGuardConfig::default()
            },
            ..HostConfig::default()
        },
    };
    assert_eq!(world.stop(&disabled, ASKING, false).1, None);
    let off = Hooks {
        config: HostConfig {
            stop_hook_mode: StopHookMode::Off,
            ..HostConfig::default()
        },
    };
    assert_eq!(world.stop(&off, ASKING, false).1, None);
    let (_, output) = world.hook_in(
        Some(world.worker.as_str()),
        &Hooks::default(),
        "Stop",
        json!({"session_id": "claude-worker", "last_assistant_message": ASKING, "stop_hook_active": false}),
    );
    assert_eq!(output, None);
}

/// Serves one canned decision reply on loopback; returns the endpoint.
fn decider_endpoint(probability: f64) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            let body = json!({"answers": {"needs_owner": {"type": "noul", "noul": probability}}})
                .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    format!("http://127.0.0.1:{port}/v1/decisions")
}

#[test]
fn the_decision_model_catches_a_soft_offer_the_rule_misses() {
    let world = World::new();
    world.prompt("p1", "how is the login page?");
    let soft = "The login page is built. The release can go out any time.";

    let rule_only = guard_hooks(None, None);
    assert_eq!(world.stop(&rule_only, soft, false).1, None);

    let yes = guard_hooks(Some(decider_endpoint(0.4)), Some("test-key"));
    let output = world.stop(&yes, soft, false).1.unwrap();
    assert_eq!(output["decision"], "block");

    let no = guard_hooks(Some(decider_endpoint(0.1)), Some("test-key"));
    assert_eq!(world.stop(&no, soft, false).1, None);

    let down = guard_hooks(
        Some("http://127.0.0.1:1/v1/decisions".into()),
        Some("test-key"),
    );
    assert_eq!(
        world.stop(&down, soft, false).1,
        None,
        "a model error keeps the rule's answer"
    );
    assert!(world.stop(&down, ASKING, false).1.is_some());
}

#[test]
fn the_owner_guard_reads_its_settings_and_hides_the_key() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[owner_guard]\nenabled = false\ndecider_key = \"test-key\"\nthreshold = 0.5\nendpoint = \"http://127.0.0.1:9/x\"\n",
    )
    .unwrap();
    let guard = HostConfig::load(home.path()).owner_guard;
    assert!(!guard.enabled);
    assert_eq!(guard.decider_key.as_deref(), Some("test-key"));
    assert_eq!(guard.threshold, 0.5);
    assert_eq!(guard.endpoint.as_deref(), Some("http://127.0.0.1:9/x"));
    assert!(!format!("{guard:?}").contains("test-key"));
    std::fs::write(
        home.path().join("config.toml"),
        "[owner_guard]\nthreshold = 7\n",
    )
    .unwrap();
    assert_eq!(
        HostConfig::load(home.path()).owner_guard.threshold,
        owner_guard::DEFAULT_THRESHOLD
    );
}
