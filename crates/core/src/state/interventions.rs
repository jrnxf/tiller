//! Why the owner was needed (build-plan T7.1; `state.md`, Intervention).
//!
//! Each time the owner was needed is one of two rows: an attention item that
//! was not withdrawn, or a note the owner sent to a card's orchestrator
//! (`card comment`). A classifier labels it, and the Health page counts the
//! avoidable labels - corrections, unplanned escalations and rework - per
//! accepted outcome. The classifier is plain rules over rows that are already
//! in `state.db`, so it costs no tokens and gives the same label to the same
//! rows every time. The owner can relabel any item; the override is the only
//! thing stored (`intervention_labels`), so the rules can improve without a
//! migration and a relabel is never lost.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::projects::require_owner;
use crate::domain::{AttentionKind, InterventionKind};
use crate::error::{Error, Result};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store};
use crate::time::Timestamp;

/// How long a summary line is, in characters.
const SUMMARY_CHARS: usize = 140;

/// Answers that accept a recommendation without repeating it.
const AFFIRMATIONS: &[&str] = &[
    "yes",
    "y",
    "ok",
    "okay",
    "approve",
    "approved",
    "accept",
    "accepted",
    "go",
    "go ahead",
    "do it",
    "agreed",
    "sounds good",
    "proceed",
    "lgtm",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionSource {
    Attention,
    Note,
}

/// One time the owner was needed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intervention {
    /// The attention id, or the outbox id of the note.
    pub id: String,
    pub source: InterventionSource,
    pub card_id: String,
    pub display_id: String,
    pub project_id: String,
    pub at: Timestamp,
    /// The question, or the note without its card prefix, cut short.
    pub summary: String,
    /// What the rules said.
    pub classified: InterventionKind,
    /// What counts: the owner's label when there is one, else `classified`.
    pub kind: InterventionKind,
    pub relabeled: bool,
}

/// Interventions of each kind. `planned` is the routine share and is never
/// counted as avoidable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterventionCounts {
    pub correction: i64,
    pub unplanned_escalation: i64,
    pub rework: i64,
    pub planned: i64,
}

impl InterventionCounts {
    pub fn add(&mut self, kind: InterventionKind) {
        match kind {
            InterventionKind::Correction => self.correction += 1,
            InterventionKind::UnplannedEscalation => self.unplanned_escalation += 1,
            InterventionKind::Rework => self.rework += 1,
            InterventionKind::Planned => self.planned += 1,
        }
    }

    pub fn avoidable(&self) -> i64 {
        self.correction + self.unplanned_escalation + self.rework
    }

    pub fn total(&self) -> i64 {
        self.avoidable() + self.planned
    }
}

/// What a relabel left behind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relabeled {
    pub id: String,
    pub kind: InterventionKind,
    pub classified: InterventionKind,
    pub relabeled: bool,
}

/// The facts the rules read, so they can be tested without a database.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub source: InterventionSource,
    pub attention_kind: Option<AttentionKind>,
    pub recommendation: Option<String>,
    pub answer: Option<String>,
    /// The card is a fix for a landed card, a revert, or a builder attempt
    /// beyond the first had started when the owner was needed.
    pub rework: bool,
    /// Some run on the card had started when the owner was needed.
    pub run_started: bool,
}

fn normalize(text: &str) -> String {
    let kept: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether an answer accepts the recommendation it was given.
fn agrees(answer: &str, recommendation: &str) -> bool {
    let (answer, recommendation) = (normalize(answer), normalize(recommendation));
    if recommendation.is_empty() {
        return true;
    }
    answer == recommendation
        || AFFIRMATIONS.contains(&answer.as_str())
        || (!answer.is_empty()
            && (answer.contains(&recommendation) || recommendation.contains(&answer)))
}

/// The rules, in order; the first that applies wins.
///
/// 1. A merge, review or report is a routine approval: `planned`.
/// 2. Work that is itself rework - a fix for a landed card, a revert, or a
///    builder attempt beyond the first - is `rework`.
/// 3. A note sent before any run began is the brief: `planned`. After that it
///    is steering, a `correction`.
/// 4. A decision answered against the recommendation is a `correction`.
/// 5. Anything else the queue asked for (a decision, a credential, a
///    recovery) was not planned: an `unplanned_escalation`.
pub(crate) fn classify(facts: &Facts) -> InterventionKind {
    if matches!(
        facts.attention_kind,
        Some(AttentionKind::Merge | AttentionKind::Review | AttentionKind::Report)
    ) {
        return InterventionKind::Planned;
    }
    if facts.rework {
        return InterventionKind::Rework;
    }
    if facts.source == InterventionSource::Note {
        return if facts.run_started {
            InterventionKind::Correction
        } else {
            InterventionKind::Planned
        };
    }
    if facts.attention_kind == Some(AttentionKind::Decision)
        && let (Some(answer), Some(recommendation)) = (&facts.answer, &facts.recommendation)
        && !agrees(answer, recommendation)
    {
        return InterventionKind::Correction;
    }
    InterventionKind::UnplannedEscalation
}

fn summary(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SUMMARY_CHARS {
        return line;
    }
    let cut: String = line.chars().take(SUMMARY_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// A note's body starts with `[<display id>] `, which the row shows apart.
fn without_prefix(body: &str) -> &str {
    match body
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
    {
        Some((_, rest)) => rest,
        None => body,
    }
}

const ATTENTION_ROWS: &str =
    "SELECT a.id, a.card_id, c.display_id, c.project_id, a.opened_at, a.kind,
        a.question, a.recommendation, a.answer,
        (c.fixes_card_id IS NOT NULL OR COALESCE(c.category, '') = 'revert'
         OR EXISTS (SELECT 1 FROM runs r WHERE r.card_id = c.id AND r.role = 'builder'
                    AND r.attempt > 1 AND r.started_at <= a.opened_at)),
        EXISTS (SELECT 1 FROM runs r WHERE r.card_id = c.id AND r.started_at <= a.opened_at),
        l.label
     FROM attention a JOIN cards c ON c.id = a.card_id
     LEFT JOIN intervention_labels l ON l.ref_id = a.id
     WHERE a.withdrawn_at IS NULL AND a.opened_at >= ?1 AND a.opened_at < ?2
       AND (?3 IS NULL OR a.id = ?3)";

const NOTE_ROWS: &str = "SELECT o.id, o.card_id, c.display_id, c.project_id, o.created_at, o.body,
        (c.fixes_card_id IS NOT NULL OR COALESCE(c.category, '') = 'revert'
         OR EXISTS (SELECT 1 FROM runs r WHERE r.card_id = c.id AND r.role = 'builder'
                    AND r.attempt > 1 AND r.started_at <= o.created_at)),
        EXISTS (SELECT 1 FROM runs r WHERE r.card_id = c.id AND r.started_at <= o.created_at),
        l.label
     FROM outbox o JOIN cards c ON c.id = o.card_id
     LEFT JOIN intervention_labels l ON l.ref_id = o.id
     WHERE o.kind = 'note' AND o.reply_to IS NULL
       AND (o.created_by IN ('owner', 'board') OR o.created_by LIKE 'board@%')
       AND o.created_at >= ?1 AND o.created_at < ?2
       AND (?3 IS NULL OR o.id = ?3)";

/// The items opened from `start` up to `end` (text bounds, as stored), or only
/// the one with id `only`, oldest first.
fn load(
    conn: &Connection,
    start: &str,
    end: &str,
    only: Option<&str>,
) -> Result<Vec<Intervention>> {
    let mut found = Vec::new();

    {
        let mut stmt = conn.prepare(ATTENTION_ROWS)?;
        let rows = stmt.query_map(params![start, end, only], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Timestamp>(4)?,
                row.get::<_, AttentionKind>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, bool>(9)?,
                row.get::<_, bool>(10)?,
                row.get::<_, Option<InterventionKind>>(11)?,
            ))
        })?;
        for row in rows {
            let (
                id,
                card_id,
                display_id,
                project_id,
                at,
                kind,
                question,
                recommendation,
                answer,
                rework,
                run_started,
                label,
            ) = row?;
            let classified = classify(&Facts {
                source: InterventionSource::Attention,
                attention_kind: Some(kind),
                recommendation,
                answer,
                rework,
                run_started,
            });
            found.push(Intervention {
                id,
                source: InterventionSource::Attention,
                card_id,
                display_id,
                project_id,
                at,
                summary: summary(&question),
                classified,
                kind: label.unwrap_or(classified),
                relabeled: label.is_some(),
            });
        }
    }
    {
        let mut stmt = conn.prepare(NOTE_ROWS)?;
        let rows = stmt.query_map(params![start, end, only], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Timestamp>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, Option<InterventionKind>>(8)?,
            ))
        })?;
        for row in rows {
            let (id, card_id, display_id, project_id, at, body, rework, run_started, label) = row?;
            let classified = classify(&Facts {
                source: InterventionSource::Note,
                attention_kind: None,
                recommendation: None,
                answer: None,
                rework,
                run_started,
            });
            found.push(Intervention {
                id,
                source: InterventionSource::Note,
                card_id,
                display_id,
                project_id,
                at,
                summary: summary(without_prefix(&body)),
                classified,
                kind: label.unwrap_or(classified),
                relabeled: label.is_some(),
            });
        }
    }

    found.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
    Ok(found)
}

impl Store {
    /// Every time the owner was needed from `start` up to `end`, oldest
    /// first, each with its label.
    pub fn interventions(&self, start: Timestamp, end: Timestamp) -> Result<Vec<Intervention>> {
        load(self.conn(), &start.to_string(), &end.to_string(), None)
    }

    /// The counts of one window, and per project.
    pub(crate) fn intervention_counts(
        &self,
        start: Timestamp,
        end: Timestamp,
    ) -> Result<(InterventionCounts, BTreeMap<String, InterventionCounts>)> {
        let mut all = InterventionCounts::default();
        let mut by_project: BTreeMap<String, InterventionCounts> = BTreeMap::new();
        for item in self.interventions(start, end)? {
            all.add(item.kind);
            by_project
                .entry(item.project_id)
                .or_default()
                .add(item.kind);
        }
        Ok((all, by_project))
    }

    /// `health.relabel`, owner only: sets the label the Health page counts an
    /// item under, or clears it (`None`) so the classifier's own label counts
    /// again. Setting the label an item already has writes nothing.
    pub fn intervention_relabel(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        id: &str,
        label: Option<InterventionKind>,
    ) -> Result<Applied<Relabeled>> {
        require_owner(&ctx.actor, "health.relabel")?;
        if matches!(ctx.actor, Actor::Peer(_)) {
            // The numbers are this host's own; a relabel is not forwarded.
            return Err(Error::Forbidden(format!(
                "health.relabel is local to this host; {} may not",
                ctx.actor
            )));
        }
        self.write(ctx, "health.relabel", key, |tx| {
            let id = id.trim();
            let item = load(tx.conn(), "", "~", Some(id))?
                .into_iter()
                .next()
                .ok_or_else(|| Error::not_found("intervention", id))?;
            let before = item.relabeled.then_some(item.kind);
            if before != label {
                match label {
                    Some(label) => {
                        tx.conn().execute(
                            "INSERT INTO intervention_labels (ref_id, label, labeled_at, labeled_by)
                             VALUES (?1, ?2, ?3, ?4)
                             ON CONFLICT (ref_id) DO UPDATE SET
                                 label = excluded.label, labeled_at = excluded.labeled_at,
                                 labeled_by = excluded.labeled_by",
                            params![item.id, label, tx.now(), tx.actor().to_string()],
                        )?;
                    }
                    None => {
                        tx.conn().execute(
                            "DELETE FROM intervention_labels WHERE ref_id = ?1",
                            [&item.id],
                        )?;
                    }
                }
                tx.event(
                    NewEvent::new("intervention", &item.id, "intervention.relabeled")
                        .card(item.card_id.clone())
                        .payload(json!({
                            "from": item.kind,
                            "to": label.unwrap_or(item.classified),
                            "classified": item.classified,
                            "cleared": label.is_none(),
                        })),
                )?;
            }
            let kind = label.unwrap_or(item.classified);
            Ok(Relabeled {
                id: item.id,
                kind,
                classified: item.classified,
                relabeled: label.is_some(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::signals::WeekWindow;
    use crate::state::tests::{memory_store, owner};

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn facts(source: InterventionSource) -> Facts {
        Facts {
            source,
            attention_kind: None,
            recommendation: None,
            answer: None,
            rework: false,
            run_started: false,
        }
    }

    fn decision(recommendation: &str, answer: &str) -> Facts {
        Facts {
            attention_kind: Some(AttentionKind::Decision),
            recommendation: Some(recommendation.into()),
            answer: Some(answer.into()),
            ..facts(InterventionSource::Attention)
        }
    }

    #[test]
    fn the_rules_label_each_kind_of_need() {
        use InterventionKind::*;
        let kind = |attention_kind| Facts {
            attention_kind: Some(attention_kind),
            ..facts(InterventionSource::Attention)
        };
        // Routine approvals are planned, even on work that is rework.
        for routine in [
            AttentionKind::Merge,
            AttentionKind::Review,
            AttentionKind::Report,
        ] {
            let mut facts = kind(routine);
            assert_eq!(classify(&facts), Planned);
            facts.rework = true;
            assert_eq!(classify(&facts), Planned);
        }
        // A question nobody planned for.
        assert_eq!(
            classify(&kind(AttentionKind::Credential)),
            UnplannedEscalation
        );
        assert_eq!(
            classify(&kind(AttentionKind::Recovery)),
            UnplannedEscalation
        );
        assert_eq!(
            classify(&kind(AttentionKind::Decision)),
            UnplannedEscalation
        );
        // An answer that accepts the recommendation is still an escalation; one
        // that goes against it is a correction.
        assert_eq!(
            classify(&decision("Ship it as is", "yes")),
            UnplannedEscalation
        );
        assert_eq!(
            classify(&decision("Ship it as is", "ship it as is.")),
            UnplannedEscalation
        );
        assert_eq!(
            classify(&decision("Ship it as is", "No, split the PR first")),
            Correction
        );
        // Rework outranks the rest.
        let mut again = kind(AttentionKind::Credential);
        again.rework = true;
        assert_eq!(classify(&again), Rework);
        // A note is the brief before any run began, steering after.
        let mut note = facts(InterventionSource::Note);
        assert_eq!(classify(&note), Planned);
        note.run_started = true;
        assert_eq!(classify(&note), Correction);
        note.rework = true;
        assert_eq!(classify(&note), Rework);
    }

    fn sql(store: &Store, text: &str) {
        store.conn().execute_batch(text).unwrap();
    }

    fn card(store: &Store, id: &str, extra: &str) {
        // `extra` is `category, fixes_card_id`.
        sql(
            store,
            &format!(
                "INSERT INTO cards (id, display_id, project_id, kind, title, category, fixes_card_id, created_at)
                 VALUES ('{id}', 'alpha-{id}', 'alpha', 'ship', 't', {extra}, '2026-10-20T09:00:00.000Z')"
            ),
        );
    }

    fn attention(
        store: &Store,
        id: &str,
        card: &str,
        kind: &str,
        opened: &str,
        recommendation: &str,
        answer: &str,
    ) {
        let answered = if answer.is_empty() {
            "NULL, NULL, NULL".to_owned()
        } else {
            format!("'{opened}', '{answer}', 'board'")
        };
        sql(
            store,
            &format!(
                "INSERT INTO attention (id, card_id, kind, question, recommendation, opened_at, answered_at, answer, answered_via)
                 VALUES ('{id}', '{card}', '{kind}', 'Question {id}?', '{recommendation}', '{opened}', {answered})"
            ),
        );
    }

    fn note(store: &Store, session: &str, id: &str, card: &str, at: &str, by: &str) {
        sql(
            store,
            &format!(
                "INSERT INTO outbox (id, target_session_id, kind, body, card_id, created_at, created_by)
                 VALUES ('{id}', '{session}', 'note', '[alpha-{card}] keep the old API', '{card}', '{at}', '{by}')"
            ),
        );
    }

    fn week() -> WeekWindow {
        WeekWindow {
            start: at("2026-10-26T00:00:00Z"),
            end: at("2026-11-02T00:00:00Z"),
        }
    }

    /// One of each: a planned merge, a correction (the developer disagreed),
    /// an unplanned credential, rework (a second builder attempt had begun),
    /// a note before any run, a note after one, and a withdrawn item.
    fn fixture() -> (Store, String) {
        let (store, session) = memory_store();
        let host: String = store
            .conn()
            .query_row("SELECT id FROM hosts", [], |row| row.get(0))
            .unwrap();
        card(&store, "c1", "NULL, NULL");
        card(&store, "c2", "NULL, NULL");
        for (id, attempt, started) in [
            ("r1", 1, "2026-10-26T09:00:00.000Z"),
            ("r2", 2, "2026-10-28T09:00:00.000Z"),
        ] {
            sql(
                &store,
                &format!(
                    "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, supervisor_session_id, started_at, ended_at, outcome)
                     VALUES ('{id}', 'c2', 'builder', {attempt}, 'm', 'high', '{host}', '{session}', '{started}', '{started}', 'failed')"
                ),
            );
        }
        attention(
            &store,
            "a-merge",
            "c1",
            "merge",
            "2026-10-27T10:00:00.000Z",
            "",
            "yes",
        );
        attention(
            &store,
            "a-fix",
            "c1",
            "decision",
            "2026-10-27T11:00:00.000Z",
            "Keep it",
            "No, drop it",
        );
        attention(
            &store,
            "a-cred",
            "c1",
            "credential",
            "2026-10-27T12:00:00.000Z",
            "",
            "token",
        );
        attention(
            &store,
            "a-redo",
            "c2",
            "decision",
            "2026-10-29T10:00:00.000Z",
            "Retry",
            "Retry",
        );
        attention(
            &store,
            "a-open",
            "c1",
            "recovery",
            "2026-10-30T10:00:00.000Z",
            "",
            "",
        );
        attention(
            &store,
            "a-gone",
            "c1",
            "recovery",
            "2026-10-30T11:00:00.000Z",
            "",
            "",
        );
        sql(
            &store,
            "UPDATE attention SET withdrawn_at = '2026-10-30T12:00:00.000Z' WHERE id = 'a-gone'",
        );
        note(
            &store,
            &session,
            "n-early",
            "c1",
            "2026-10-26T08:00:00.000Z",
            "owner",
        );
        note(
            &store,
            &session,
            "n-late",
            "c2",
            "2026-10-28T10:00:00.000Z",
            "board",
        );
        // Not the developer: an agent's note is not an intervention.
        note(
            &store,
            &session,
            "n-agent",
            "c2",
            "2026-10-28T11:00:00.000Z",
            "session:abc",
        );
        (store, session)
    }

    fn kinds(store: &Store) -> Vec<(String, InterventionKind)> {
        store
            .interventions(week().start, week().end)
            .unwrap()
            .into_iter()
            .map(|item| (item.id, item.kind))
            .collect()
    }

    #[test]
    fn items_come_from_attention_and_owner_notes_and_skip_withdrawn_ones() {
        use InterventionKind::*;
        let (store, _) = fixture();
        assert_eq!(
            kinds(&store),
            vec![
                ("n-early".into(), Planned),
                ("a-merge".into(), Planned),
                ("a-fix".into(), Correction),
                ("a-cred".into(), UnplannedEscalation),
                ("n-late".into(), Rework),
                ("a-redo".into(), Rework),
                ("a-open".into(), UnplannedEscalation),
            ]
        );
        let first = &store.interventions(week().start, week().end).unwrap()[0];
        assert_eq!(first.summary, "keep the old API");
        assert_eq!(first.display_id, "alpha-c1");
    }

    #[test]
    fn weeks_count_each_kind_and_per_project_avoidable() {
        let (store, _) = fixture();
        let signals = store
            .signals(&[week()], at("2026-11-01T12:00:00Z"))
            .unwrap();
        let now = &signals.weeks[0];
        assert_eq!(
            now.interventions,
            InterventionCounts {
                correction: 1,
                unplanned_escalation: 2,
                rework: 2,
                planned: 2
            }
        );
        assert_eq!(now.interventions.avoidable(), 5);
        // The sample behind the median: answered items only.
        assert_eq!(now.answered, 4);
        let alpha = signals
            .by_project
            .iter()
            .find(|p| p.project_id == "alpha")
            .unwrap();
        assert_eq!((alpha.avoidable, alpha.answered), (5, 4));
        assert_eq!(signals.interventions.len(), 7);
        assert_eq!(signals.interventions[0].id, "a-open", "newest first");
    }

    #[test]
    fn a_relabel_changes_the_counts_and_clearing_restores_the_rules() {
        use InterventionKind::*;
        let (mut store, _) = fixture();
        let applied = store
            .intervention_relabel(&owner(), Some("k1"), "a-cred", Some(Planned))
            .unwrap();
        assert!(!applied.replayed);
        assert_eq!(applied.events.len(), 1);
        assert_eq!(
            applied.value,
            Relabeled {
                id: "a-cred".into(),
                kind: Planned,
                classified: UnplannedEscalation,
                relabeled: true
            }
        );
        let signals = store
            .signals(&[week()], at("2026-11-01T12:00:00Z"))
            .unwrap();
        assert_eq!(signals.weeks[0].interventions.unplanned_escalation, 1);
        assert_eq!(signals.weeks[0].interventions.planned, 3);
        let listed = signals
            .interventions
            .iter()
            .find(|i| i.id == "a-cred")
            .unwrap();
        assert_eq!(
            (listed.kind, listed.classified, listed.relabeled),
            (Planned, UnplannedEscalation, true)
        );

        // The same label again writes nothing; a retry of the key replays.
        let again = store
            .intervention_relabel(&owner(), None, "a-cred", Some(Planned))
            .unwrap();
        assert!(again.replayed && again.events.is_empty());
        let retry = store
            .intervention_relabel(&owner(), Some("k1"), "a-cred", Some(Planned))
            .unwrap();
        assert!(retry.replayed);

        // A new label replaces the old one in place.
        store
            .intervention_relabel(&owner(), None, "a-cred", Some(Correction))
            .unwrap();
        let rows: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM intervention_labels", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);

        let cleared = store
            .intervention_relabel(&owner(), None, "a-cred", None)
            .unwrap();
        assert_eq!(cleared.value.kind, UnplannedEscalation);
        assert!(!cleared.value.relabeled);
        assert_eq!(
            kinds(&store)
                .into_iter()
                .find(|(id, _)| id == "a-cred")
                .unwrap()
                .1,
            UnplannedEscalation
        );
        let events: Vec<String> = store
            .conn()
            .prepare(
                "SELECT payload FROM events WHERE kind = 'intervention.relabeled' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(events[2].contains("\"cleared\":true"));
    }

    #[test]
    fn a_relabel_is_owner_only_local_and_needs_a_real_item() {
        let (mut store, session) = fixture();
        let denied = |store: &mut Store, actor: Actor| {
            store
                .intervention_relabel(
                    &Ctx::new(actor),
                    None,
                    "a-cred",
                    Some(InterventionKind::Planned),
                )
                .unwrap_err()
        };
        assert!(matches!(
            denied(&mut store, Actor::Session(session)),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            denied(&mut store, Actor::Daemon),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            denied(&mut store, Actor::Peer("laptop".into())),
            Error::Forbidden(_)
        ));
        let missing = store
            .intervention_relabel(&owner(), None, "nope", Some(InterventionKind::Planned))
            .unwrap_err();
        assert!(matches!(missing, Error::NotFound { .. }));
        // A withdrawn item is not an intervention, so it cannot be labeled.
        assert!(
            store
                .intervention_relabel(&owner(), None, "a-gone", Some(InterventionKind::Planned))
                .is_err()
        );
        let rows: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM intervention_labels", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
    }

    /// The board reads these field names (`SignalsIntervention`).
    #[test]
    fn an_intervention_serializes_to_the_fields_the_board_reads() {
        let (store, _) = fixture();
        let item = &store.interventions(week().start, week().end).unwrap()[0];
        let value = serde_json::to_value(item).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "at",
                "card_id",
                "classified",
                "display_id",
                "id",
                "kind",
                "project_id",
                "relabeled",
                "source",
                "summary"
            ]
        );
        assert_eq!(value["source"], "note");
        assert_eq!(value["kind"], "planned");
    }
}
