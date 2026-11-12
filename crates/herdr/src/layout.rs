//! Layout rules from `docs/design/herdr-layout.md`: nested workspaces, glyph repair (T2.5).
//!
//! Each project's orchestrator heads a block of rows: its own workspace,
//! labelled with the project id, then one workspace per worker, labelled
//! `├ <card> <role>` with `└` on the last. herdr's rows are flat, so the order
//! and the glyphs carry the nesting. [`plan`] compares herdr's current rows
//! with the sessions `state.db` binds to panes and returns the moves and
//! renames that restore the rule; applying a plan twice changes nothing.
//! Workspaces Tiller does not own keep their place.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;
use tiller_core::domain::{Card, Role, Run, RunRole, Session};
use tiller_core::state::{get, list};

use crate::client::{Herdr, HerdrError, Result, Snapshot, WorkspaceInfo};

/// Where a session's workspace belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Orchestrator {
        project: String,
    },
    Worker {
        project: String,
        display_id: String,
        card_seq: i64,
        role: RunRole,
        attempt: i64,
    },
}

impl Slot {
    pub fn project(&self) -> &str {
        match self {
            Slot::Orchestrator { project } | Slot::Worker { project, .. } => project,
        }
    }

    /// The order inside a block: by card, then scout, build, review, verify.
    fn rank(&self) -> (i64, u8, i64) {
        match self {
            Slot::Orchestrator { .. } => (i64::MIN, 0, 0),
            Slot::Worker {
                card_seq,
                role,
                attempt,
                ..
            } => {
                let role = match role {
                    RunRole::Scout => 0,
                    RunRole::Builder => 1,
                    RunRole::Reviewer => 2,
                    RunRole::Verifier => 3,
                };
                (*card_seq, role, *attempt)
            }
        }
    }
}

/// A live session bound to a pane on this host, as `state.db` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub session_id: String,
    pub run_id: Option<String>,
    pub pane_id: String,
    pub slot: Slot,
    pub model: Option<String>,
}

impl Binding {
    /// The row tokens herdr shows in the sidebar (`$project`, `$card`,
    /// `$role`, `$model`), plus the session and run ids the delivery target
    /// check and a restore's adoption read. Identity lives here, never in a
    /// label.
    pub fn tokens(&self) -> BTreeMap<String, String> {
        let mut tokens = BTreeMap::from([
            ("project".to_owned(), self.slot.project().to_owned()),
            ("session".to_owned(), self.session_id.clone()),
        ]);
        match &self.slot {
            Slot::Orchestrator { .. } => {
                tokens.insert("role".into(), "orchestrator".into());
            }
            Slot::Worker {
                display_id, role, ..
            } => {
                tokens.insert("card".into(), display_id.clone());
                tokens.insert("role".into(), role.short_name().into());
            }
        }
        if let Some(run) = &self.run_id {
            tokens.insert("run".into(), run.clone());
        }
        if let Some(model) = &self.model {
            tokens.insert("model".into(), model.clone());
        }
        tokens
    }
}

/// Every live session on `host_id` that has a herdr pane. A session whose
/// pane is in the terminal holder is not herdr's to lay out, poll or read.
pub fn bindings(conn: &Connection, host_id: &str) -> tiller_core::Result<Vec<Binding>> {
    let sessions: Vec<Session> = list(
        conn,
        "host_id = ?1 AND ended_at IS NULL AND pane_id IS NOT NULL ORDER BY started_at, id",
        &[&host_id],
    )?;
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions {
        let Some(pane_id) = session.pane_id.clone() else {
            continue;
        };
        if tiller_core::ids::is_holder_pane(&pane_id) {
            continue;
        }
        let (slot, model) = match (&session.role, &session.run_id) {
            (Role::Orchestrator, _) | (_, None) => (
                Slot::Orchestrator {
                    project: session.project_id.clone(),
                },
                None,
            ),
            (_, Some(run_id)) => {
                let run: Run = get(conn, "run", run_id)?;
                let card: Card = get(conn, "card", &run.card_id)?;
                let card_seq =
                    tiller_core::ids::parse_display_id(&card.display_id).map_or(0, |(_, seq)| seq);
                (
                    Slot::Worker {
                        project: card.project_id.clone(),
                        display_id: card.display_id.clone(),
                        card_seq,
                        role: run.role,
                        attempt: run.attempt,
                    },
                    Some(run.model),
                )
            }
        };
        out.push(Binding {
            session_id: session.id,
            run_id: session.run_id,
            pane_id,
            slot,
            model,
        });
    }
    Ok(out)
}

/// One repair step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Rename { workspace_id: String, label: String },
    Move { workspace_id: String, index: usize },
}

pub fn orchestrator_label(project: &str) -> String {
    project.to_owned()
}

pub fn worker_label(display_id: &str, role: RunRole, last: bool) -> String {
    let glyph = if last { '└' } else { '├' };
    format!("{glyph} {display_id} {}", role.short_name())
}

/// The workspace each binding's pane lives in, per the snapshot. A binding
/// whose pane herdr does not show has no row to place.
pub fn members(snapshot: &Snapshot, bindings: &[Binding]) -> Vec<(String, Slot)> {
    let mut seen = HashMap::new();
    for binding in bindings {
        if let Some(pane) = snapshot.pane(&binding.pane_id) {
            seen.entry(pane.workspace_id.clone())
                .or_insert_with(|| binding.slot.clone());
        }
    }
    snapshot
        .workspaces
        .iter()
        .filter_map(|w| {
            seen.remove(&w.workspace_id)
                .map(|s| (w.workspace_id.clone(), s))
        })
        .collect()
}

/// The renames, then the moves, that make `workspaces` follow the layout rule.
pub fn plan(workspaces: &[WorkspaceInfo], members: &[(String, Slot)]) -> Vec<Action> {
    let slot_of: HashMap<&str, &Slot> = members.iter().map(|(w, s)| (w.as_str(), s)).collect();
    let heads: HashMap<&str, &str> = members
        .iter()
        .filter_map(|(w, s)| match s {
            Slot::Orchestrator { project } => Some((project.as_str(), w.as_str())),
            Slot::Worker { .. } => None,
        })
        .collect();
    let mut blocks: HashMap<&str, Vec<(&str, &Slot)>> = HashMap::new();
    for (workspace, slot) in members {
        if let Slot::Worker { project, .. } = slot
            && heads.contains_key(project.as_str())
        {
            blocks
                .entry(project.as_str())
                .or_default()
                .push((workspace.as_str(), slot));
        }
    }
    for block in blocks.values_mut() {
        block.sort_by_key(|(w, s)| (s.rank(), *w));
    }

    let mut actions = Vec::new();
    let mut desired: Vec<&str> = Vec::with_capacity(workspaces.len());
    for workspace in workspaces {
        let id = workspace.workspace_id.as_str();
        match slot_of.get(id) {
            Some(Slot::Orchestrator { project }) if heads.get(project.as_str()) == Some(&id) => {
                desired.push(id);
                for (worker, _) in blocks.get(project.as_str()).into_iter().flatten() {
                    desired.push(worker);
                }
            }
            // A worker under a present orchestrator is placed with its block.
            Some(Slot::Worker { project, .. }) if heads.contains_key(project.as_str()) => {}
            // Foreign rows, a second orchestrator row, and workers whose
            // orchestrator has no row keep their place.
            _ => desired.push(id),
        }
    }

    let labels: HashMap<&str, &str> = workspaces
        .iter()
        .map(|w| (w.workspace_id.as_str(), w.label.as_str()))
        .collect();
    let mut rename = |id: &str, label: String| {
        if labels.get(id) != Some(&label.as_str()) {
            actions.push(Action::Rename {
                workspace_id: id.to_owned(),
                label,
            });
        }
    };
    for (project, head) in &heads {
        rename(head, orchestrator_label(project));
    }
    for block in blocks.values() {
        for (i, (workspace, slot)) in block.iter().enumerate() {
            if let Slot::Worker {
                display_id, role, ..
            } = slot
            {
                rename(
                    workspace,
                    worker_label(display_id, *role, i + 1 == block.len()),
                );
            }
        }
    }
    actions.sort_by(|a, b| match (a, b) {
        (
            Action::Rename {
                workspace_id: a, ..
            },
            Action::Rename {
                workspace_id: b, ..
            },
        ) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    });

    // Each step moves one row up into place; the prefix above it already
    // matches, so an index counts the same before and after the removal.
    let mut current: Vec<&str> = workspaces.iter().map(|w| w.workspace_id.as_str()).collect();
    for (index, id) in desired.iter().enumerate() {
        if current.get(index) == Some(id) {
            continue;
        }
        if let Some(from) = current.iter().position(|w| w == id) {
            let moved = current.remove(from);
            current.insert(index, moved);
            actions.push(Action::Move {
                workspace_id: (*id).to_owned(),
                index,
            });
        }
    }
    actions
}

/// What one layout pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repair {
    pub renamed: usize,
    pub moved: usize,
    pub tokens_reported: usize,
    /// Moves left undone because the client has no API socket: the labels
    /// are right and the order is not (`spawn.herdr_socket` unset).
    pub unordered: usize,
}

impl Repair {
    /// Whether the pass wrote anything to herdr.
    pub fn changed(&self) -> bool {
        self.renamed + self.moved + self.tokens_reported > 0
    }
}

/// Re-reports row tokens herdr lost or never had, then applies the layout
/// plan: renames first, so a herdr that refuses `workspace.move` still ends
/// with correct labels in the wrong order (`herdr-layout.md`, Known limits).
/// A client without the API socket cannot move at all, which is a setup and
/// not a failure: the moves it skips are counted in `unordered`.
///
/// A pane whose `session` token names another session is left alone, row
/// and tokens both: the binding is stale, and reconcile settles it. A pane
/// with no tokens at all is trusted, because R18 re-reports them after a
/// herdr restart and nothing else can tell a recycled pane id apart; the
/// delivery pass still types only into an empty Claude composer.
pub fn repair(herdr: &dyn Herdr, snapshot: &Snapshot, bindings: &[Binding]) -> Result<Repair> {
    let mut done = Repair::default();
    let mut trusted = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let Some(pane) = snapshot.pane(&binding.pane_id) else {
            continue;
        };
        if let Some(other) = pane
            .tokens
            .get("session")
            .filter(|s| **s != binding.session_id)
        {
            tracing::warn!(
                pane = %binding.pane_id,
                bound = %binding.session_id,
                claimed = %other,
                "pane names another session; left for reconcile"
            );
            continue;
        }
        let want = binding.tokens();
        if want.iter().any(|(k, v)| pane.tokens.get(k) != Some(v)) {
            herdr.report_tokens(&binding.pane_id, &want)?;
            done.tokens_reported += 1;
        }
        trusted.push(binding.clone());
    }
    let actions = plan(&snapshot.workspaces, &members(snapshot, &trusted));
    for action in &actions {
        if let Action::Rename {
            workspace_id,
            label,
        } = action
        {
            herdr.rename_workspace(workspace_id, label)?;
            done.renamed += 1;
        }
    }
    let moves: Vec<(&str, usize)> = actions
        .iter()
        .filter_map(|action| match action {
            Action::Move {
                workspace_id,
                index,
            } => Some((workspace_id.as_str(), *index)),
            Action::Rename { .. } => None,
        })
        .collect();
    for (n, (workspace_id, index)) in moves.iter().enumerate() {
        match herdr.move_workspace(workspace_id, *index) {
            Ok(()) => done.moved += 1,
            Err(HerdrError::NoSocket(_)) => {
                done.unordered = moves.len() - n;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.into(),
            label: label.into(),
        }
    }

    fn orch(project: &str) -> Slot {
        Slot::Orchestrator {
            project: project.into(),
        }
    }

    fn worker(project: &str, seq: i64, role: RunRole) -> Slot {
        Slot::Worker {
            project: project.into(),
            display_id: format!("{project}-{seq}"),
            card_seq: seq,
            role,
            attempt: 1,
        }
    }

    /// Applies a plan the way herdr does: rename in place, move by removing
    /// and inserting at the index.
    fn apply(workspaces: &[WorkspaceInfo], actions: &[Action]) -> Vec<WorkspaceInfo> {
        let mut out = workspaces.to_vec();
        for action in actions {
            match action {
                Action::Rename {
                    workspace_id,
                    label,
                } => {
                    if let Some(w) = out.iter_mut().find(|w| &w.workspace_id == workspace_id) {
                        w.label = label.clone();
                    }
                }
                Action::Move {
                    workspace_id,
                    index,
                } => {
                    let from = out
                        .iter()
                        .position(|w| &w.workspace_id == workspace_id)
                        .unwrap();
                    let moved = out.remove(from);
                    out.insert(*index, moved);
                }
            }
        }
        out
    }

    fn labels(workspaces: &[WorkspaceInfo]) -> Vec<&str> {
        workspaces.iter().map(|w| w.label.as_str()).collect()
    }

    #[test]
    fn worker_workspace_nests_under_its_orchestrator() {
        // A new worker workspace opens at the bottom, after another project.
        let current = vec![
            ws("w1", "alpha"),
            ws("w2", "└ alpha-42 build"),
            ws("w3", "beta"),
            ws("w4", "notes"),
            ws("w5", "├ alpha-43 scout"),
        ];
        let members = vec![
            ("w1".into(), orch("alpha")),
            ("w2".into(), worker("alpha", 42, RunRole::Builder)),
            ("w3".into(), orch("beta")),
            ("w5".into(), worker("alpha", 43, RunRole::Scout)),
        ];
        let after = apply(&current, &plan(&current, &members));
        assert_eq!(
            labels(&after),
            [
                "alpha",
                "├ alpha-42 build",
                "└ alpha-43 scout",
                "beta",
                "notes"
            ]
        );
        assert!(
            plan(&after, &members).is_empty(),
            "a repaired layout needs no further step"
        );
    }

    #[test]
    fn block_orders_by_card_then_role_and_relabels_glyphs() {
        let current = vec![
            ws("w9", "gamma"),
            ws("w1", "alpha"),
            ws("w2", "├ alpha-43 scout"),
            ws("w3", "├ alpha-42 verify"),
            ws("w4", "└ alpha-42 build"),
        ];
        let members = vec![
            ("w1".into(), orch("alpha")),
            ("w2".into(), worker("alpha", 43, RunRole::Scout)),
            ("w3".into(), worker("alpha", 42, RunRole::Verifier)),
            ("w4".into(), worker("alpha", 42, RunRole::Builder)),
        ];
        let after = apply(&current, &plan(&current, &members));
        assert_eq!(
            labels(&after),
            [
                "gamma",
                "alpha",
                "├ alpha-42 build",
                "├ alpha-42 verify",
                "└ alpha-43 scout"
            ]
        );
    }

    #[test]
    fn worker_leaving_relabels_the_new_last_child() {
        // alpha-43's run ended and its workspace (`└ alpha-43 scout`) closed.
        let current = vec![ws("w1", "alpha"), ws("w2", "├ alpha-42 build")];
        let members = vec![
            ("w1".into(), orch("alpha")),
            ("w2".into(), worker("alpha", 42, RunRole::Builder)),
        ];
        let actions = plan(&current, &members);
        assert_eq!(
            actions,
            [Action::Rename {
                workspace_id: "w2".into(),
                label: "└ alpha-42 build".into()
            }]
        );
    }

    #[test]
    fn workers_without_an_orchestrator_row_and_foreign_rows_stay_put() {
        let current = vec![
            ws("w1", "scratch"),
            ws("w2", "├ beta-1 build"),
            ws("w3", "alpha"),
        ];
        let members = vec![
            ("w2".into(), worker("beta", 1, RunRole::Builder)),
            ("w3".into(), orch("alpha")),
        ];
        assert!(plan(&current, &members).is_empty());
    }

    #[test]
    fn tokens_name_the_session_and_never_the_label() {
        let binding = Binding {
            session_id: "01SESSION".into(),
            run_id: Some("01RUN".into()),
            pane_id: "w2:p1".into(),
            slot: worker("alpha", 42, RunRole::Reviewer),
            model: Some("opus".into()),
        };
        let tokens = binding.tokens();
        assert_eq!(tokens["session"], "01SESSION");
        assert_eq!(tokens["card"], "alpha-42");
        assert_eq!(tokens["role"], "review");
        assert_eq!(tokens["model"], "opus");
        assert_eq!(tokens["run"], "01RUN");
        assert!(!worker_label("alpha-42", RunRole::Reviewer, true).contains("01SESSION"));
    }
}
