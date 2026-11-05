//! G33 (P09): a `no-mistakes` ask-user finding goes to the developer as an
//! attention item, read from the pipeline record; a worker never answers it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{AttentionKind, Coverage, Reviewer, Verdict};
use tiller_core::state::records::NewReview;

fn record(w: &mut World, run_id: &str, reviewer: Reviewer, verdict: Verdict) {
    w.store_mut()
        .review_record(
            &owner(),
            NewReview {
                run_id: run_id.into(),
                reviewer,
                coverage: Coverage::Diff,
                findings_high: 0,
                findings_medium: 1,
                findings_low: 0,
                verdict,
                source_url: Some("https://example.com/runs/1".into()),
            },
        )
        .unwrap();
}

#[test]
fn nm_ask_user_finding_becomes_attention_item() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);

    // A passing pipeline and another reviewer's ask-user raise nothing.
    record(&mut w, &run.id, Reviewer::NoMistakes, Verdict::Pass);
    record(&mut w, &run.id, Reviewer::CodeReview, Verdict::AskUser);
    w.pass(now());
    assert!(w.open_items(&run.card_id).is_empty());

    record(&mut w, &run.id, Reviewer::NoMistakes, Verdict::AskUser);
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("G33"), "{report:?}");
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Decision);
    assert_eq!(items[0].opened_by_run_id.as_deref(), Some(run.id.as_str()));
    assert!(items[0].question.contains("no-mistakes"));

    // Once, however many passes follow.
    w.pass(now().plus_minutes(2));
    w.pass(now().plus_minutes(3));
    assert_eq!(w.open_items(&run.card_id).len(), 1);
}
