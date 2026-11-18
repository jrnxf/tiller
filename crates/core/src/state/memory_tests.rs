use super::*;
use crate::domain::CardKind;
use crate::state::tests::{builder_run, card, memory_store, owner, snapshot};

/// A live builder session on a new card of `alpha`, and its run.
fn builder(store: &mut Store, orchestrator: &str, title: &str) -> (Ctx, String) {
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, title), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &Ctx::new(Actor::Session(orchestrator.to_owned())),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value;
    (Ctx::new(Actor::Session(started.session.id)), started.run.id)
}

fn new(kind: &str, title: &str) -> NewNote {
    NewNote {
        project: None,
        kind: kind.into(),
        title: title.into(),
        body: format!("{title}, in a sentence."),
    }
}

/// A fleet session notes for its own project; the row names its role,
/// card and run; the event names the row.
#[test]
fn a_fleet_session_notes_for_its_own_project() {
    let (mut store, orchestrator) = memory_store();
    let (ctx, run) = builder(&mut store, &orchestrator, "one");
    let noted = store
        .mem_note(&ctx, None, &new("gotcha", "Run the gate first"))
        .unwrap();
    let row = noted.value;
    assert_eq!(row.project_id, "alpha");
    assert_eq!(row.source, "fleet");
    assert_eq!(row.role.as_deref(), Some("builder"));
    assert_eq!(row.run_id.as_deref(), Some(run.as_str()));
    assert!(row.card_id.is_some());
    assert_eq!(
        store.pending_notes(Some("alpha")).unwrap(),
        std::slice::from_ref(&row)
    );
    let (kind, payload): (String, String) = store
        .conn()
        .query_row(
            "SELECT kind, payload FROM events WHERE seq = ?1",
            [noted.events[0]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "memory.noted");
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(
        payload,
        json!({"pending_id": row.id, "project_id": "alpha", "source": "fleet"})
    );

    // Another project is refused, and so is a kind kb hides from fleet roles.
    let other = NewNote {
        project: Some("fleet".into()),
        ..new("gotcha", "Elsewhere")
    };
    let before = snapshot(store.conn(), &[]);
    assert!(matches!(
        store.mem_note(&ctx, None, &other),
        Err(Error::Forbidden(_))
    ));
    let error = store
        .mem_note(&ctx, None, &new("open-work", "Half done"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("open-work is not a pending kind")
    );
    assert!(store.mem_note(&ctx, None, &new("rumour", "x")).is_err());
    let long = "x".repeat(TITLE_MAX + 1);
    assert!(store.mem_note(&ctx, None, &new("gotcha", &long)).is_err());
    assert_eq!(snapshot(store.conn(), &[]), before);
}

/// A run adds at most three notes and a project holds at most fifty; the
/// refusal gives the numbers.
#[test]
fn mem_note_keeps_to_the_limits() {
    let (mut store, orchestrator) = memory_store();
    let (ctx, _) = builder(&mut store, &orchestrator, "one");
    for n in 0..RUN_LIMIT {
        store
            .mem_note(&ctx, None, &new("convention", &format!("Note {n}")))
            .unwrap();
    }
    let error = store
        .mem_note(&ctx, None, &new("convention", "One more"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("it added 3 notes; a run adds at most 3"),
        "{error}"
    );

    let import = Ctx::new(Actor::Import);
    for n in RUN_LIMIT..PROJECT_LIMIT {
        let note = NewNote {
            project: Some("alpha".into()),
            ..new("decision", &format!("Imported {n}"))
        };
        store.mem_note(&import, None, &note).unwrap();
    }
    let (other, _) = builder(&mut store, &orchestrator, "two");
    let error = store
        .mem_note(&other, None, &new("gotcha", "Fifty-first"))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("it holds 50 pending notes; a project holds at most 50"),
        "{error}"
    );
}

/// The owner and the orchestrator's peers do not add notes, and an ended
/// session adds none.
#[test]
fn only_the_fleet_the_curator_and_the_migration_note() {
    let (mut store, orchestrator) = memory_store();
    let note = NewNote {
        project: Some("alpha".into()),
        ..new("gotcha", "x")
    };
    assert!(matches!(
        store.mem_note(&owner(), None, &note),
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        store.mem_note(&Ctx::new(Actor::Daemon), None, &note),
        Err(Error::Forbidden(_))
    ));
    store
        .conn()
        .execute(
            "UPDATE sessions SET ended_at = '2026-11-02T10:00:00.000Z' WHERE id = ?1",
            [&orchestrator],
        )
        .unwrap();
    let error = store
        .mem_note(
            &Ctx::new(Actor::Session(orchestrator)),
            None,
            &new("gotcha", "x"),
        )
        .unwrap_err();
    assert!(error.to_string().contains("it has ended"), "{error}");
}

/// The owner edits, keeps and discards against the row's `rev`; a stale
/// `rev` changes nothing; each act writes its event.
#[test]
fn the_owner_edits_keeps_and_discards() {
    let (mut store, orchestrator) = memory_store();
    let (ctx, _) = builder(&mut store, &orchestrator, "one");
    let a = store
        .mem_note(&ctx, None, &new("gotcha", "First"))
        .unwrap()
        .value;
    let b = store
        .mem_note(&ctx, None, &new("gotcha", "Second"))
        .unwrap()
        .value;

    // A fleet session cannot act on the queue.
    let edit = NoteEdit {
        title: Some("First, better".into()),
        kind: Some("convention".into()),
        ..NoteEdit::default()
    };
    assert!(matches!(
        store.mem_edit(&ctx, None, &a.id, 1, &edit),
        Err(Error::Forbidden(_))
    ));
    let edited = store
        .mem_edit(&owner(), None, &a.id, 1, &edit)
        .unwrap()
        .value;
    assert_eq!(edited.title, "First, better");
    assert_eq!(edited.kind, "convention");
    assert_eq!(edited.rev, 2);
    assert!(edited.edited_at.is_some());
    assert!(matches!(
        store.mem_edit(&owner(), None, &a.id, 1, &edit),
        Err(Error::Conflict { .. })
    ));

    let kept = Kept {
        note_id: "first-better".into(),
        scope: "github.com/example/alpha".into(),
    };
    assert!(matches!(
        store.mem_keep(&owner(), None, &a.id, 1, &kept),
        Err(Error::Conflict { .. })
    ));
    let done = store
        .mem_keep(&owner(), Some("k1"), &a.id, 2, &kept)
        .unwrap();
    assert_eq!(done.value, kept);
    // A retry with the key replays; the row is gone.
    assert!(
        store
            .mem_keep(&owner(), Some("k1"), &a.id, 2, &kept)
            .unwrap()
            .replayed
    );
    assert!(matches!(
        store.pending_note(&a.id),
        Err(Error::NotFound { .. })
    ));

    store.mem_discard(&owner(), None, &b.id, 1).unwrap();
    assert!(store.pending_notes(None).unwrap().is_empty());
    let kinds: Vec<String> = store
        .conn()
        .prepare("SELECT kind FROM events WHERE entity = 'memory_pending' ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        kinds,
        [
            "memory.noted",
            "memory.noted",
            "memory.edited",
            "memory.kept",
            "memory.discarded"
        ]
    );
}

/// Keep writes the note to the project's repo scope, with the line that
/// names where it came from.
#[test]
fn keep_input_names_the_scope_and_the_source() {
    let (mut store, orchestrator) = memory_store();
    let (ctx, run) = builder(&mut store, &orchestrator, "one");
    let row = store
        .mem_note(&ctx, None, &new("how-to", "Reset the bench"))
        .unwrap()
        .value;
    let card = row.card_id.clone().unwrap();
    assert_eq!(
        keep_input(&row, "github.com/example/alpha"),
        json!({
            "title": "Reset the bench",
            "kind": "how-to",
            "body": format!("Reset the bench, in a sentence.\n\nSource: tiller fleet, card {card}, run {run}"),
            "scope": "github.com/example/alpha",
            "confidence": "medium",
        })
    );
    let mut project: Project = get(store.conn(), "project", "alpha").unwrap();
    project.remote = Some("https://example.com/example/alpha.git".into());
    assert_eq!(
        repo_scope_of(&project).as_deref(),
        Some("example.com/example/alpha")
    );
    project.remote = None;
    project.repo = Some("/home/user/alpha".into());
    assert_eq!(repo_scope_of(&project), None);
}

/// Keep deletes the row only after kb accepted the note: a refused `add`
/// leaves it, a project with no repo scope runs no `add`, and a retry with
/// the same key replays without running `add` again.
#[test]
fn keep_runs_add_before_it_deletes_the_row() {
    let (mut store, orchestrator) = memory_store();
    let (ctx, _) = builder(&mut store, &orchestrator, "one");
    let row = store
        .mem_note(&ctx, None, &new("gotcha", "Keep me"))
        .unwrap()
        .value;
    let never = |_: &Value| -> std::result::Result<kb::Added, String> {
        panic!("add runs only for a project with a repo scope")
    };

    // No repo scope: refused before kb runs.
    store
        .conn()
        .execute(
            "UPDATE projects SET repo = NULL, remote = NULL WHERE id = 'alpha'",
            [],
        )
        .unwrap();
    let error = store
        .mem_keep_with(&owner(), &row.id, 1, never)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    store
        .conn()
        .execute(
            "UPDATE projects SET remote = 'git@example.com:example/alpha.git' WHERE id = 'alpha'",
            [],
        )
        .unwrap();

    // A refused add keeps the row and says why.
    let error = store
        .mem_keep_with(&owner(), &row.id, 1, |_| {
            Err("kb refused the note: dup".into())
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("kb refused the note: dup"),
        "{error}"
    );
    assert_eq!(store.pending_note(&row.id).unwrap().id, row.id);

    // A fleet session may not keep.
    assert!(matches!(
        store.mem_keep_with(&ctx, &row.id, 1, never),
        Err(Error::Forbidden(_))
    ));

    // Kept: the input names the repo scope, and the row goes.
    let keyed = owner().with_key("k1".to_owned());
    let mut seen = None;
    let done = store
        .mem_keep_with(&keyed, &row.id, 1, |input| {
            seen = Some(input.clone());
            Ok(kb::Added {
                id: "keep-me".into(),
                existed: false,
            })
        })
        .unwrap();
    assert_eq!(
        done.value,
        Kept {
            note_id: "keep-me".into(),
            scope: "example.com/example/alpha".into(),
        }
    );
    assert_eq!(seen.unwrap()["scope"], "example.com/example/alpha");
    assert!(matches!(
        store.pending_note(&row.id),
        Err(Error::NotFound { .. })
    ));
    let again = store.mem_keep_with(&keyed, &row.id, 1, never).unwrap();
    assert!(again.replayed);
    assert_eq!(again.value, done.value);
}
