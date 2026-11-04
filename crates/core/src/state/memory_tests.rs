use super::*;
use crate::state::learning::EvalResult;
use crate::state::tests::{memory_store, owner};

/// The document list's texts, which these tests do not read.
fn no_list(paths: &[String]) -> Result<Vec<String>> {
    Ok(vec![String::new(); paths.len()])
}

fn file(text: &str) -> FileState {
    let blob = if text.is_empty() {
        EMPTY_BLOB.to_owned()
    } else {
        doc::sha256_hex(text.as_bytes())
    };
    FileState {
        head_blob: blob.clone(),
        work_blob: blob,
        text: text.to_owned(),
        clean: true,
    }
}

fn daemon() -> Ctx {
    Ctx::new(Actor::Daemon)
}

/// Runs `mem.apply` the way the daemon does, with the new text standing in
/// for the commit.
fn apply(store: &mut Store, approval: &str, before: &str, after: &str) -> Approval {
    match store
        .mem_apply_begin(&daemon(), approval, &file(before))
        .unwrap()
        .value
    {
        Begin::Go { .. } => {}
        other => panic!("expected to apply, got {other:?}"),
    }
    let outcome = Outcome::Applied {
        blob: file(after).head_blob,
        commit: format!("commit-of-{approval}"),
    };
    store
        .mem_apply_finish(&daemon(), approval, &outcome)
        .unwrap()
        .value
}

fn add(store: &mut Store, project: &str, text: &str, now: &str, section: &str) -> Pending {
    let edit = EditOp::Add {
        section: section.into(),
        text: text.into(),
    };
    store
        .mem_edit(&owner(), project, &edit, &file(now).head_blob, &file(now))
        .unwrap()
        .value
}

/// An agent proposal against `base`, as the curator would leave it.
fn agent_proposal(
    store: &mut Store,
    project: &str,
    base: &str,
    text: &str,
    budget: Option<Value>,
) -> String {
    let ctx = Ctx::new(Actor::Daemon);
    store
        .write(&ctx, "test.propose", None, |tx| {
            let source = ensure_source(tx, project)?;
            let revision = ensure_revision(tx, &source.id, &file(base))?;
            let path = document_path(project);
            let diff = doc::unified_diff(&path, base, &doc::add(base, doc::NOTE_SECTION, text));
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO proposals (id, project_id, boundary, target_source_id, op, base_revision_id, base_blob,
                                        diff, diff_sha256, memory_fields, rationale, expected_effect,
                                        token_budget_after, created_at, created_by)
                 VALUES (?1, ?2, 'personal', ?3, 'add', ?4, ?5, ?6, ?7, ?8, 'Seen twice this week', 'fewer retries',
                         ?9, ?10, 'session:curator')",
                params![
                    id,
                    project,
                    source.id,
                    revision.id,
                    revision.blob_sha,
                    diff,
                    doc::sha256_hex(diff.as_bytes()),
                    json!({"section": doc::NOTE_SECTION, "class": "note"}),
                    budget,
                    tx.now(),
                ],
            )?;
            tx.event(NewEvent::new("proposal", &id, "proposal.created").rev(1))?;
            Ok(id)
        })
        .unwrap()
        .value
}

/// Bundles `proposals` as one change set with a passing holdout run, so
/// the gate lets their Keep through.
fn measured(store: &mut Store, name: &str, proposals: &[&str]) {
    let members: Vec<String> = proposals.iter().map(|p| (*p).to_owned()).collect();
    store
        .change_set_create(&owner(), name, &members, 10)
        .unwrap();
    let passing = EvalResult {
        cases_total: 4,
        cases_passed: 4,
        ..EvalResult::default()
    };
    store.eval_run_record(&owner(), name, passing).unwrap();
}

fn notes(view: &MemoryView) -> &DocSection {
    view.doc
        .sections
        .iter()
        .find(|s| s.title == doc::NOTE_SECTION)
        .unwrap()
}

fn proposal(store: &Store, id: &str) -> Proposal {
    get(store.conn(), "proposal", id).unwrap()
}

#[test]
fn an_owner_edit_is_approved_and_applied_with_its_rows() {
    let (mut store, _) = memory_store();
    let pending = add(
        &mut store,
        "alpha",
        "Run the slow suite before landing",
        "",
        doc::RULE_SECTION,
    );
    let approval = pending.approval_id.unwrap();
    let after = "## How we work here\n\n- Run the slow suite before landing\n";
    let done = apply(&mut store, &approval, "", after);
    assert_eq!(done.state, ApprovalState::Succeeded);

    let applied = proposal(&store, &pending.proposal_id);
    assert_eq!(applied.state, "applied");
    let revision: Revision = get(
        store.conn(),
        "memory revision",
        applied.applied_revision_id.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(
        (revision.review.as_str(), revision.provenance.as_str()),
        ("approved", "tiller")
    );
    assert_eq!(revision.approval_id.as_deref(), Some(approval.as_str()));

    let view = store
        .memory_view("alpha", |_| Ok(file(after)), no_list)
        .unwrap();
    let rules = &view.doc.sections[0];
    assert_eq!(rules.lines.len(), 1);
    let line = &rules.lines[0];
    assert!(line.memory_id.is_some());
    assert_eq!(
        line.provenance.as_ref().and_then(|p| p.note.as_deref()),
        Some("you")
    );
    assert_eq!(view.doc.changes[0].kind, "added");
}

#[test]
fn an_edit_against_an_old_view_is_refused() {
    let (mut store, _) = memory_store();
    let edit = EditOp::Add {
        section: doc::RULE_SECTION.into(),
        text: "x".into(),
    };
    let error = store
        .mem_edit(&owner(), "alpha", &edit, "an-older-blob", &file(""))
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
}

#[test]
fn memory_changes_are_owner_only_and_never_from_a_peer() {
    let (mut store, session) = memory_store();
    let edit = EditOp::Add {
        section: doc::RULE_SECTION.into(),
        text: "x".into(),
    };
    for actor in [Actor::Peer("laptop".into()), Actor::Session(session)] {
        let error = store
            .mem_edit(&Ctx::new(actor), "alpha", &edit, EMPTY_BLOB, &file(""))
            .unwrap_err();
        assert!(matches!(error, Error::Forbidden(_)), "{error:?}");
    }
    let error = store
        .mem_apply_begin(&owner(), "any", &file(""))
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error:?}");
}

#[test]
fn a_moved_base_stales_the_approval_and_its_proposal() {
    let (mut store, _) = memory_store();
    let pending = add(&mut store, "alpha", "one", "", doc::RULE_SECTION);
    let approval = pending.approval_id.unwrap();
    let moved = "## How we work here\n\n- someone else\n";
    let begin = store
        .mem_apply_begin(&daemon(), &approval, &file(moved))
        .unwrap()
        .value;
    assert!(matches!(begin, Begin::Stale { .. }), "{begin:?}");
    let row: Approval = get(store.conn(), "approval", &approval).unwrap();
    assert_eq!(row.state, ApprovalState::Stale);
    assert_eq!(proposal(&store, &pending.proposal_id).state, "stale");

    // A dirty working tree stales it the same way.
    let pending = add(&mut store, "alpha", "two", "", doc::RULE_SECTION);
    let mut dirty = file("");
    dirty.work_blob = "edited-by-hand".into();
    let begin = store
        .mem_apply_begin(&daemon(), pending.approval_id.as_deref().unwrap(), &dirty)
        .unwrap()
        .value;
    assert!(matches!(begin, Begin::Stale { .. }), "{begin:?}");
}

#[test]
fn keep_waits_for_a_passing_holdout_run() {
    let (mut store, _) = memory_store();
    let suggested = agent_proposal(
        &mut store,
        "alpha",
        "",
        "The staging DB resets at midnight",
        None,
    );
    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    let gate = &notes(&view).suggestions[0].gate;
    assert!(!gate.ok);
    assert_eq!(
        gate.reason.as_deref(),
        Some("the proposal is in no change set")
    );
    let hint = format!("tiller learn eval --proposal {suggested}");
    assert_eq!(gate.hint.as_deref(), Some(hint.as_str()));
    let error = store
        .mem_decide(&owner(), &suggested, true, EMPTY_BLOB, &file(""), &[])
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("the proposal is in no change set; tiller learn eval --proposal"),
        "{error}"
    );
    assert_eq!(proposal(&store, &suggested).state, "proposed");

    measured(&mut store, "staging notes", &[&suggested]);
    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    assert!(notes(&view).suggestions[0].gate.ok);
    let kept = store
        .mem_decide(&owner(), &suggested, true, EMPTY_BLOB, &file(""), &[])
        .unwrap()
        .value;
    assert_eq!(kept.proposal_id, suggested);
    assert!(kept.approval_id.is_some());
}

#[test]
fn keep_on_an_old_base_is_refused_as_stale() {
    let (mut store, _) = memory_store();
    let suggested = agent_proposal(
        &mut store,
        "alpha",
        "",
        "The staging DB resets at midnight",
        None,
    );
    measured(&mut store, "staging notes", &[&suggested]);
    let first = add(&mut store, "alpha", "Owner line", "", doc::RULE_SECTION);
    let now = "## How we work here\n\n- Owner line\n";
    apply(&mut store, first.approval_id.as_deref().unwrap(), "", now);

    let view = store
        .memory_view("alpha", |_| Ok(file(now)), no_list)
        .unwrap();
    let notes = view
        .doc
        .sections
        .iter()
        .find(|s| s.title == doc::NOTE_SECTION)
        .unwrap();
    let gate = &notes.suggestions[0].gate;
    assert!(!gate.ok);
    assert_eq!(
        gate.reason.as_deref(),
        Some("the text moved; re-base and run the holdout again")
    );

    let error = store
        .mem_decide(
            &owner(),
            &suggested,
            true,
            &file(now).head_blob,
            &file(now),
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("the text moved"), "{error}");
    assert_eq!(proposal(&store, &suggested).state, "proposed");
}

#[test]
fn a_bundle_kept_in_one_batch_applies_in_order_on_one_text() {
    let (mut store, _) = memory_store();
    let first = agent_proposal(
        &mut store,
        "alpha",
        "",
        "The staging DB resets at midnight",
        None,
    );
    let second = agent_proposal(&mut store, "alpha", "", "The parser test flakes", None);
    let other = agent_proposal(&mut store, "alpha", "", "Unrelated", None);
    measured(&mut store, "staging notes", &[&first, &second]);
    measured(&mut store, "other", &[&other]);

    let kept = store
        .mem_decide(&owner(), &first, true, EMPTY_BLOB, &file(""), &[])
        .unwrap()
        .value;
    let now = doc::add("", doc::NOTE_SECTION, "The staging DB resets at midnight");
    apply(&mut store, kept.approval_id.as_deref().unwrap(), "", &now);

    // Out of the batch, or after a proposal of another set, it is stale.
    let head = file(&now).head_blob;
    for after in [Vec::new(), vec![other.clone()]] {
        let error = store
            .mem_decide(&owner(), &second, true, &head, &file(&now), &after)
            .unwrap_err();
        assert!(error.to_string().contains("the text moved"), "{error}");
    }
    let rebased = store
        .mem_decide(
            &owner(),
            &second,
            true,
            &head,
            &file(&now),
            std::slice::from_ref(&first),
        )
        .unwrap()
        .value;
    assert_ne!(rebased.proposal_id, second);
    assert_eq!(proposal(&store, &second).state, "stale");
    let row = proposal(&store, &rebased.proposal_id);
    assert_eq!(row.base_blob, head);
    let after = doc::add(&now, doc::NOTE_SECTION, "The parser test flakes");
    assert_eq!(
        row.diff,
        doc::unified_diff(&document_path("alpha"), &now, &after)
    );
}

#[test]
fn keep_over_the_startup_budget_is_refused() {
    let (mut store, _) = memory_store();
    let suggested = agent_proposal(
        &mut store,
        "alpha",
        "",
        "Long",
        Some(json!({"builder": 7_600})),
    );
    let error = store
        .mem_decide(&owner(), &suggested, true, EMPTY_BLOB, &file(""), &[])
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    assert_eq!(proposal(&store, &suggested).state, "proposed");
}

#[test]
fn discard_then_undo_proposes_it_again() {
    let (mut store, _) = memory_store();
    let suggested = agent_proposal(&mut store, "alpha", "", "Flaky on Mondays", None);
    let out = store
        .mem_decide(&owner(), &suggested, false, EMPTY_BLOB, &file(""), &[])
        .unwrap()
        .value;
    assert_eq!(out.approval_id, None);
    assert_eq!(proposal(&store, &suggested).state, "discarded");
    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    assert_eq!(view.doc.changes[0].kind, "discard");

    let again = store
        .mem_undo(&owner(), &suggested, EMPTY_BLOB, &file(""), None)
        .unwrap()
        .value;
    assert_eq!(proposal(&store, &again.proposal_id).state, "proposed");
    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    assert!(view.doc.changes.is_empty());
    assert!(
        store
            .mem_undo(&owner(), &suggested, EMPTY_BLOB, &file(""), None)
            .is_err()
    );
}

#[test]
fn undo_of_an_added_line_retires_it_and_keeps_its_history() {
    let (mut store, _) = memory_store();
    let pending = add(&mut store, "alpha", "Temporary", "", doc::RULE_SECTION);
    let after = "## How we work here\n\n- Temporary\n";
    apply(
        &mut store,
        pending.approval_id.as_deref().unwrap(),
        "",
        after,
    );
    let memory: Memory = find(store.conn(), "proposal_id = ?1", &[&pending.proposal_id])
        .unwrap()
        .unwrap();

    let path = document_path("alpha");
    let reverse = doc::unified_diff(&path, after, "");
    let undo = store
        .mem_undo(
            &owner(),
            &pending.proposal_id,
            &file(after).head_blob,
            &file(after),
            Some(&reverse),
        )
        .unwrap()
        .value;
    let inverse = proposal(&store, &undo.proposal_id);
    assert_eq!(inverse.op, "retire");
    apply(&mut store, undo.approval_id.as_deref().unwrap(), after, "");

    let memory: Memory = get(store.conn(), "memory", &memory.id).unwrap();
    assert!(memory.retired_at.is_some());
    assert_eq!(memory.review_status, "retired");
    // Back at the empty blob: the baseline revision is recorded again.
    let revisions: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM memory_revisions WHERE blob_sha = ?1",
            [EMPTY_BLOB],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revisions, 1);
    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    assert!(view.doc.changes.is_empty(), "{:?}", view.doc.changes);
}

#[test]
fn a_rewrite_and_its_undo_move_the_row_hash_back() {
    let (mut store, _) = memory_store();
    let first = add(&mut store, "fleet", "old wording", "", doc::RULE_SECTION);
    let v1 = "## How we work here\n\n- old wording\n";
    apply(&mut store, first.approval_id.as_deref().unwrap(), "", v1);
    let edit = EditOp::Rewrite {
        section: doc::RULE_SECTION.into(),
        index: 0,
        text: "new   wording ".into(),
    };
    let pending = store
        .mem_edit(&owner(), "fleet", &edit, &file(v1).head_blob, &file(v1))
        .unwrap()
        .value;
    let v2 = "## How we work here\n\n- new wording\n";
    assert_eq!(
        proposal(&store, &pending.proposal_id).diff,
        doc::unified_diff("fleet.md", v1, v2)
    );
    apply(&mut store, pending.approval_id.as_deref().unwrap(), v1, v2);
    let memory: Memory = find(store.conn(), "proposal_id = ?1", &[&first.proposal_id])
        .unwrap()
        .unwrap();
    assert_eq!(memory.text_sha256, doc::text_sha256("new wording"));

    let reverse = doc::unified_diff("fleet.md", v2, v1);
    let undo = store
        .mem_undo(
            &owner(),
            &pending.proposal_id,
            &file(v2).head_blob,
            &file(v2),
            Some(&reverse),
        )
        .unwrap()
        .value;
    apply(&mut store, undo.approval_id.as_deref().unwrap(), v2, v1);
    let memory: Memory = get(store.conn(), "memory", &memory.id).unwrap();
    assert_eq!(memory.text_sha256, doc::text_sha256("old wording"));
}

#[test]
fn a_line_written_outside_tiller_is_adopted_when_edited() {
    let (mut store, _) = memory_store();
    let text = "## Watch out for\n\n- written by hand\n";
    let view = store
        .memory_view("alpha", |_| Ok(file(text)), no_list)
        .unwrap();
    let notes = view
        .doc
        .sections
        .iter()
        .find(|s| s.title == doc::NOTE_SECTION)
        .unwrap();
    assert_eq!(notes.lines[0].memory_id, None);

    let edit = EditOp::Remove {
        section: doc::NOTE_SECTION.into(),
        index: 0,
    };
    let pending = store
        .mem_edit(&owner(), "alpha", &edit, &file(text).head_blob, &file(text))
        .unwrap()
        .value;
    let removed = "## Watch out for\n";
    apply(
        &mut store,
        pending.approval_id.as_deref().unwrap(),
        text,
        removed,
    );
    let rows: Vec<Memory> = list(store.conn(), "provenance = 'external' ORDER BY id", &[]).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].retired_at.is_some());
}

#[test]
fn a_failed_commit_fails_the_approval_and_the_proposal() {
    let (mut store, _) = memory_store();
    let pending = add(&mut store, "alpha", "flagged", "", doc::RULE_SECTION);
    let approval = pending.approval_id.unwrap();
    store
        .mem_apply_begin(&daemon(), &approval, &file(""))
        .unwrap();
    let outcome = Outcome::Failed {
        reason: "the scrub flagged it".into(),
    };
    let row = store
        .mem_apply_finish(&daemon(), &approval, &outcome)
        .unwrap()
        .value;
    assert_eq!(row.state, ApprovalState::Failed);
    let failed = proposal(&store, &pending.proposal_id);
    assert_eq!(failed.state, "failed");
    assert_eq!(
        failed.failure_reason.as_deref(),
        Some("the scrub flagged it")
    );
    let memories: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(memories, 0);
}

#[test]
fn a_title_the_file_repeats_numbers_its_lines_across_both_parts() {
    let (mut store, _) = memory_store();
    let text = "## How we work here\n\n- first part\n\n## Watch out for\n\n- a note\n\n## How we work here\n\n- second part\n";
    let view = store
        .memory_view("alpha", |_| Ok(file(text)), no_list)
        .unwrap();
    let rules: Vec<&str> = view
        .doc
        .sections
        .iter()
        .filter(|s| s.title == doc::RULE_SECTION)
        .flat_map(|s| s.lines.iter().map(|l| l.text.as_str()))
        .collect();
    assert_eq!(rules, ["first part", "second part"]);

    let edit = EditOp::Rewrite {
        section: doc::RULE_SECTION.into(),
        index: 1,
        text: "second part, reworded".into(),
    };
    let pending = store
        .mem_edit(&owner(), "alpha", &edit, &file(text).head_blob, &file(text))
        .unwrap()
        .value;
    let diff = proposal(&store, &pending.proposal_id).diff;
    assert!(
        diff.contains("\n-- second part\n") && diff.contains("\n+- second part, reworded\n"),
        "{diff}"
    );
    assert!(!diff.contains("\n-- first part\n"), "{diff}");
}

#[test]
fn a_second_suggestion_on_a_line_is_listed_apart_naming_the_line() {
    let (mut store, _) = memory_store();
    let first = add(&mut store, "alpha", "old wording", "", doc::RULE_SECTION);
    let v1 = "## How we work here\n\n- old wording\n";
    apply(&mut store, first.approval_id.as_deref().unwrap(), "", v1);
    let memory: Memory = find(store.conn(), "proposal_id = ?1", &[&first.proposal_id])
        .unwrap()
        .unwrap();
    for wording in ["new wording", "other wording"] {
        let ctx = Ctx::new(Actor::Daemon);
        store
            .write(&ctx, "test.propose", None, |tx| {
                let source = ensure_source(tx, "alpha")?;
                let revision = ensure_revision(tx, &source.id, &file(v1))?;
                let after = format!("## How we work here\n\n- {wording}\n");
                let diff = doc::unified_diff("projects/alpha.md", v1, &after);
                let id = ids::new_ulid();
                tx.conn().execute(
                    "INSERT INTO proposals (id, project_id, boundary, target_source_id, op, memory_id, base_revision_id,
                                            base_blob, diff, diff_sha256, memory_fields, expected_effect, created_at, created_by)
                     VALUES (?1, 'alpha', 'personal', ?2, 'rewrite', ?3, ?4, ?5, ?6, ?7, ?8, 'clearer', ?9, 'session:curator')",
                    params![
                        id,
                        source.id,
                        memory.id,
                        revision.id,
                        revision.blob_sha,
                        diff,
                        doc::sha256_hex(diff.as_bytes()),
                        json!({"section": doc::RULE_SECTION, "text": wording}),
                        tx.now(),
                    ],
                )?;
                tx.event(NewEvent::new("proposal", &id, "proposal.created").rev(1))?;
                Ok(())
            })
            .unwrap();
    }

    let view = store
        .memory_view("alpha", |_| Ok(file(v1)), no_list)
        .unwrap();
    let rules = &view.doc.sections[0];
    let inline = rules.lines[0].suggestion.as_ref().unwrap();
    assert_eq!(inline.target.as_deref(), Some("old wording"));
    assert_eq!(rules.suggestions.len(), 1);
    let apart = &rules.suggestions[0];
    assert_eq!(apart.op, "rewrite");
    assert_eq!(apart.target.as_deref(), Some("old wording"));
    assert_ne!(apart.text, inline.text);
}

fn as_session(session: &str) -> Ctx {
    Ctx::new(Actor::Session(session.to_owned()))
}

fn note(store: &mut Store, ctx: &Ctx, project: Option<&str>, text: &str) -> Noted {
    let note = Note {
        project: project.map(str::to_owned),
        text: text.into(),
        ..Note::default()
    };
    store.mem_note(ctx, None, &note).unwrap().value
}

fn add_note(text: &str, evidence: &[&str]) -> Propose {
    Propose {
        project: "alpha".into(),
        op: ProposeOp::Add {
            section: doc::NOTE_SECTION.into(),
            text: text.into(),
        },
        evidence: evidence.iter().map(|e| (*e).to_owned()).collect(),
        expected_effect: Some("no more failed runs at midnight".into()),
        rationale: Some("Seen on two cards this week".into()),
    }
}

#[test]
fn a_proposal_is_filed_with_its_evidence_and_kept_only_through_the_gate() {
    let (mut store, session) = memory_store();
    let ctx = as_session(&session);
    let noted = note(&mut store, &ctx, None, "The staging DB reset mid-run");
    assert_eq!(noted.project_id, "alpha");
    assert_eq!(noted.independence_key, session);
    let filed = store
        .mem_propose(
            &ctx,
            None,
            &add_note("The staging DB resets at midnight", &[&noted.evidence_id]),
            &file(""),
        )
        .unwrap()
        .value;
    assert_eq!(filed.state, "proposed");
    assert_eq!(
        filed.authored_by_session_id.as_deref(),
        Some(session.as_str())
    );
    let cited: Vec<String> = store
        .conn()
        .prepare("SELECT evidence_id FROM proposal_evidence WHERE proposal_id = ?1")
        .unwrap()
        .query_map([&filed.id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(cited, vec![noted.evidence_id.clone()]);

    let view = store
        .memory_view("alpha", |_| Ok(file("")), no_list)
        .unwrap();
    let suggestion = &notes(&view).suggestions[0];
    assert_eq!(suggestion.proposal_id, filed.id);
    assert_eq!(suggestion.op, "add");
    assert_eq!(
        suggestion.text.as_deref(),
        Some("The staging DB resets at midnight")
    );
    assert_eq!(suggestion.by, "the alpha orchestrator");
    assert!(!suggestion.gate.ok);

    let error = store
        .mem_decide(&owner(), &filed.id, true, EMPTY_BLOB, &file(""), &[])
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    assert_eq!(proposal(&store, &filed.id).state, "proposed");
    measured(&mut store, "staging notes", &[&filed.id]);
    let kept = store
        .mem_decide(&owner(), &filed.id, true, EMPTY_BLOB, &file(""), &[])
        .unwrap()
        .value;
    assert!(kept.approval_id.is_some());
}

#[test]
fn a_contradicting_note_marks_the_memory_due_and_backs_a_rewrite() {
    let (mut store, session) = memory_store();
    let first = add(
        &mut store,
        "alpha",
        "Deploys go out on Fridays",
        "",
        doc::RULE_SECTION,
    );
    let v1 = "## How we work here\n\n- Deploys go out on Fridays\n";
    apply(&mut store, first.approval_id.as_deref().unwrap(), "", v1);
    let memory: Memory = find(store.conn(), "proposal_id = ?1", &[&first.proposal_id])
        .unwrap()
        .unwrap();

    let ctx = as_session(&session);
    let mut contradicting = Note {
        text: "The owner froze Friday deploys".into(),
        kind: Some("owner_message".into()),
        contradicts: Some(memory.id.clone()),
        ..Note::default()
    };
    // An agent reports what it saw; it does not speak for the owner.
    let error = store.mem_note(&ctx, None, &contradicting).unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error:?}");
    contradicting.kind = Some("review".into());
    let noted = store.mem_note(&ctx, None, &contradicting).unwrap().value;
    assert_eq!(noted.due_memory_id.as_deref(), Some(memory.id.as_str()));
    let (status, reason, due_at): (String, Option<String>, Option<String>) = store
        .conn()
        .query_row(
            "SELECT review_status, due_reason, review_due_at FROM memories WHERE id = ?1",
            [&memory.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), reason.as_deref()),
        ("due", Some("contradicting_evidence"))
    );
    assert!(due_at.is_some(), "a contradicted memory is due now");

    let rewrite = Propose {
        project: "alpha".into(),
        op: ProposeOp::Rewrite {
            memory: memory.id.clone(),
            text: "Deploys go out on Thursdays".into(),
        },
        evidence: vec![noted.evidence_id.clone()],
        expected_effect: Some("no Friday deploy attempts".into()),
        rationale: None,
    };
    let filed = store
        .mem_propose(&ctx, None, &rewrite, &file(v1))
        .unwrap()
        .value;
    assert_eq!(filed.memory_id.as_deref(), Some(memory.id.as_str()));
    let view = store
        .memory_view("alpha", |_| Ok(file(v1)), no_list)
        .unwrap();
    let inline = view.doc.sections[0].lines[0].suggestion.as_ref().unwrap();
    assert_eq!(inline.proposal_id, filed.id);
    assert_eq!(inline.text.as_deref(), Some("Deploys go out on Thursdays"));

    // Kept and applied, the rewritten memory stands on the new evidence too.
    measured(&mut store, "deploy days", &[&filed.id]);
    let head = file(v1).head_blob;
    let kept = store
        .mem_decide(&owner(), &filed.id, true, &head, &file(v1), &[])
        .unwrap()
        .value;
    let v2 = "## How we work here\n\n- Deploys go out on Thursdays\n";
    apply(&mut store, kept.approval_id.as_deref().unwrap(), v1, v2);
    let backing: Vec<String> = store
        .conn()
        .prepare("SELECT evidence_id FROM memory_evidence WHERE memory_id = ?1")
        .unwrap()
        .query_map([&memory.id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(backing.contains(&noted.evidence_id), "{backing:?}");
}

#[test]
fn a_proposal_without_backing_or_outside_its_project_is_refused() {
    let (mut store, session) = memory_store();
    let ctx = as_session(&session);
    let ours = note(&mut store, &ctx, None, "The staging DB reset mid-run");
    let before = store.last_seq().unwrap();

    let error = store
        .mem_propose(&ctx, None, &add_note("A line", &[]), &file(""))
        .unwrap_err();
    assert!(matches!(error, Error::Usage(_)), "{error:?}");
    let mut vague = add_note("A line", &[&ours.evidence_id]);
    vague.expected_effect = None;
    let error = store
        .mem_propose(&ctx, None, &vague, &file(""))
        .unwrap_err();
    assert!(matches!(error, Error::Usage(_)), "{error:?}");

    let mut elsewhere = add_note("A line", &[&ours.evidence_id]);
    elsewhere.project = "beta".into();
    let error = store
        .mem_propose(&ctx, None, &elsewhere, &file(""))
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error:?}");
    let error = store
        .mem_note(
            &ctx,
            None,
            &Note {
                project: Some("beta".into()),
                text: "A fact".into(),
                ..Note::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error:?}");

    let peer = Ctx::new(Actor::Peer("board-a".into()));
    let error = store
        .mem_propose(
            &peer,
            None,
            &add_note("A line", &[&ours.evidence_id]),
            &file(""),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error:?}");
    assert_eq!(
        store.last_seq().unwrap(),
        before,
        "a refusal writes nothing"
    );

    let fleet = note(&mut store, &owner(), Some(FLEET_PROJECT), "Fleet-wide fact");
    let error = store
        .mem_propose(
            &ctx,
            None,
            &add_note("A line", &[&fleet.evidence_id]),
            &file(""),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
}
