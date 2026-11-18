//! The pending queue (`memory.md` section 4): notes agents suggest wait in
//! `memory_pending` until the owner keeps or discards them.
//!
//! Keep runs `kb add` before its transaction ([`Store::mem_keep_with`]) and
//! passes the note id it got to [`Store::mem_keep`], so a row goes only
//! after kb accepted the note.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::projects::require_owner;
use super::{check_rev, get, list};
use crate::domain::{PendingNote, Project, Retro, RetroState, Run, Session};
use crate::error::{Error, Result};
use crate::ids;
use crate::kb;
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};

/// The most notes one run adds.
pub const RUN_LIMIT: i64 = 3;
/// The most pending notes one project holds.
pub const PROJECT_LIMIT: i64 = 50;
pub const TITLE_MAX: usize = 120;
pub const BODY_MAX: usize = 2000;

/// One note to add with `mem.note`. A fleet session and the curator note for
/// their own project, so `project` is for the migration alone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewNote {
    pub project: Option<String>,
    pub kind: String,
    pub title: String,
    pub body: String,
}

/// An owner's edit of a pending note: the fields it changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NoteEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub kind: Option<String>,
}

/// What Keep wrote to kb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub note_id: String,
    pub scope: String,
}

/// Where a note comes from: the caller's project, card, run and source.
struct Origin {
    project: Option<String>,
    source: &'static str,
    role: Option<String>,
    card: Option<String>,
    run: Option<String>,
    session: Option<String>,
    retro: Option<String>,
}

/// The caller of `mem.note`: a live fleet session, the curator on a retro
/// still open, or the migration.
fn origin(conn: &rusqlite::Connection, actor: &Actor) -> Result<Origin> {
    match actor {
        Actor::Session(id) => {
            let id = super::resolve_id(conn, "sessions", "session", id)?;
            let session: Session = get(conn, "session", &id)?;
            if session.ended_at.is_some() {
                return Err(Error::refused(
                    "session",
                    &session.id,
                    "it has ended; an ended session adds no note",
                ));
            }
            let card = match &session.run_id {
                Some(run) => Some(get::<Run>(conn, "run", run)?.card_id),
                None => None,
            };
            Ok(Origin {
                project: Some(session.project_id),
                source: "fleet",
                role: Some(session.role.as_str().to_owned()),
                card,
                run: session.run_id,
                session: Some(session.id),
                retro: None,
            })
        }
        Actor::Curator(retro) => {
            let retro: Retro = get(conn, "retro", retro)?;
            if retro.state == RetroState::Done {
                return Err(Error::refused(
                    "retro",
                    &retro.id,
                    "it is done; its curator writes nothing more",
                ));
            }
            Ok(curator(retro))
        }
        Actor::Import => Ok(Origin {
            project: None,
            source: "migration",
            role: None,
            card: None,
            run: None,
            session: None,
            retro: None,
        }),
        other => Err(Error::Forbidden(format!(
            "mem.note is for a fleet session, the curator or the migration; {other} may not"
        ))),
    }
}

/// A note of the curator of `retro`.
fn curator(retro: Retro) -> Origin {
    Origin {
        project: Some(retro.project_id),
        source: "curator",
        role: None,
        card: Some(retro.card_id),
        run: None,
        session: None,
        retro: Some(retro.id),
    }
}

/// A pending kind: never `open-work`, which kb hides from fleet roles.
fn check_kind(kind: &str) -> Result<()> {
    if kb::KINDS.contains(&kind) {
        return Ok(());
    }
    let why = if kind == kb::OPEN_WORK {
        "open-work is not a pending kind".to_owned()
    } else {
        format!("not a note kind: {kind:?}")
    };
    Err(Error::Usage(format!("{why} ({})", kb::KINDS.join(", "))))
}

fn check_title(title: &str) -> Result<()> {
    super::required("title", title)?;
    crate::limits::within("title", title, TITLE_MAX)
}

fn check_body(body: &str) -> Result<()> {
    super::required("body", body)?;
    crate::limits::within("body", body, BODY_MAX)
}

fn count(tx: &Tx<'_>, column: &str, value: &str) -> Result<i64> {
    Ok(tx.conn().query_row(
        &format!("SELECT count(*) FROM memory_pending WHERE {column} = ?1"),
        [value],
        |row| row.get(0),
    )?)
}

/// The kb scope Keep writes a project's notes to: its repo's scope.
pub fn repo_scope_of(project: &Project) -> Option<String> {
    project
        .remote
        .as_deref()
        .and_then(kb::repo_scope)
        .or_else(|| project.repo.as_deref().and_then(kb::repo_scope))
}

/// The JSON Keep passes to `kb add` on stdin (`memory.md` section 4): the
/// note in `scope`, its body ending in the line that names where it came
/// from.
pub fn keep_input(note: &PendingNote, scope: &str) -> Value {
    let mut source = format!("Source: tiller {}", note.source);
    if let Some(card) = &note.card_id {
        source.push_str(&format!(", card {card}"));
    }
    if let Some(run) = &note.run_id {
        source.push_str(&format!(", run {run}"));
    }
    json!({
        "title": note.title,
        "kind": note.kind,
        "body": format!("{}\n\n{source}", note.body.trim_end()),
        "scope": scope,
        "confidence": "medium",
    })
}

/// Loads a pending note by its id or short id.
fn pending(conn: &rusqlite::Connection, input: &str) -> Result<PendingNote> {
    let id = super::resolve_id(conn, "memory_pending", "pending note", input)?;
    get(conn, "pending note", &id)
}

impl Store {
    /// `mem.note`: one pending note from a fleet session, the curator or the
    /// migration. A fleet session and the curator note for their own
    /// project. A run adds at most [`RUN_LIMIT`] notes and a project holds
    /// at most [`PROJECT_LIMIT`].
    pub fn mem_note(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        note: &NewNote,
    ) -> Result<Applied<PendingNote>> {
        self.write(ctx, "mem.note", key, |tx| insert_note(tx, note, None))
    }

    /// `mem.edit` (the owner): changes a pending note's title, body or kind.
    pub fn mem_edit(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        id: &str,
        rev: i64,
        edit: &NoteEdit,
    ) -> Result<Applied<PendingNote>> {
        require_owner(&ctx.actor, "mem.edit")?;
        self.write(ctx, "mem.edit", key, |tx| {
            let mut note = pending(tx.conn(), id)?;
            let field = edit
                .title
                .as_ref()
                .map(|_| ("title", json!(note.title)))
                .or_else(|| edit.body.as_ref().map(|_| ("body", json!(note.body))))
                .or_else(|| edit.kind.as_ref().map(|_| ("kind", json!(note.kind))));
            check_rev(
                "pending note",
                &note.id,
                note.rev,
                rev,
                field,
                note.edited_at,
            )?;
            let mut changed = Vec::new();
            if let Some(title) = &edit.title {
                let title = title.trim();
                check_title(title)?;
                if title != note.title {
                    note.title = title.to_owned();
                    changed.push("title");
                }
            }
            if let Some(body) = &edit.body {
                let body = body.trim();
                check_body(body)?;
                if body != note.body {
                    note.body = body.to_owned();
                    changed.push("body");
                }
            }
            if let Some(kind) = &edit.kind {
                check_kind(kind)?;
                if *kind != note.kind {
                    note.kind = kind.clone();
                    changed.push("kind");
                }
            }
            if changed.is_empty() {
                return Ok(note);
            }
            note.edited_at = Some(tx.now());
            note.rev += 1;
            tx.conn().execute(
                "UPDATE memory_pending SET title = ?2, body = ?3, kind = ?4, edited_at = ?5,
                     rev = ?6 WHERE id = ?1",
                rusqlite::params![
                    note.id,
                    note.title,
                    note.body,
                    note.kind,
                    note.edited_at,
                    note.rev
                ],
            )?;
            tx.event(
                NewEvent::new("memory_pending", &note.id, "memory.edited")
                    .rev(note.rev)
                    .card_opt(note.card_id.clone())
                    .payload(json!({"pending_id": note.id, "fields": changed})),
            )?;
            Ok(note)
        })
    }

    /// `mem.keep` (the owner), after `kb add` accepted the note as
    /// `kept.note_id`: deletes the row. The caller runs `add` first, so a
    /// refused or failed `add` leaves the row in place.
    pub fn mem_keep(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        id: &str,
        rev: i64,
        kept: &Kept,
    ) -> Result<Applied<Kept>> {
        require_owner(&ctx.actor, "mem.keep")?;
        self.write(ctx, "mem.keep", key, |tx| {
            let note = pending(tx.conn(), id)?;
            check_rev(
                "pending note",
                &note.id,
                note.rev,
                rev,
                None,
                note.edited_at,
            )?;
            super::required("note id", &kept.note_id)?;
            tx.conn()
                .execute("DELETE FROM memory_pending WHERE id = ?1", [&note.id])?;
            tx.event(
                NewEvent::new("memory_pending", &note.id, "memory.kept")
                    .rev(note.rev)
                    .card_opt(note.card_id.clone())
                    .payload(json!({
                        "pending_id": note.id,
                        "note_id": kept.note_id,
                        "scope": kept.scope,
                    })),
            )?;
            Ok(kept.clone())
        })
    }

    /// Keep, whole (`memory.md` section 4, How Keep writes to kb): checks the
    /// row, runs `add` with the note in the project's repo scope, then
    /// `mem.keep` with the id kb gave. A refused or failed `add` leaves the
    /// row in place. A request whose key is already recorded replays and
    /// runs no `add`. The key comes from `ctx`.
    pub fn mem_keep_with(
        &mut self,
        ctx: &Ctx,
        id: &str,
        rev: i64,
        add: impl FnOnce(&Value) -> std::result::Result<kb::Added, String>,
    ) -> Result<Applied<Kept>> {
        require_owner(&ctx.actor, "mem.keep")?;
        if self.key_recorded(ctx)? {
            let replay = Kept {
                note_id: String::new(),
                scope: String::new(),
            };
            return self.mem_keep(ctx, None, id, rev, &replay);
        }
        let note = pending(self.conn(), id)?;
        check_rev(
            "pending note",
            &note.id,
            note.rev,
            rev,
            None,
            note.edited_at,
        )?;
        let project: Project = get(self.conn(), "project", &note.project_id)?;
        let scope = repo_scope_of(&project).ok_or_else(|| {
            Error::refused(
                "project",
                &project.id,
                "it has no repo, so its notes have no kb scope to keep to",
            )
        })?;
        let added = add(&keep_input(&note, &scope))
            .map_err(|message| Error::refused("pending note", &note.id, message))?;
        let kept = Kept {
            note_id: added.id,
            scope,
        };
        self.mem_keep(ctx, None, &note.id, rev, &kept)
    }

    /// `mem.discard` (the owner): deletes the row. Its event names only the id.
    pub fn mem_discard(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        id: &str,
        rev: i64,
    ) -> Result<Applied<Value>> {
        require_owner(&ctx.actor, "mem.discard")?;
        self.write(ctx, "mem.discard", key, |tx| {
            let note = pending(tx.conn(), id)?;
            check_rev(
                "pending note",
                &note.id,
                note.rev,
                rev,
                None,
                note.edited_at,
            )?;
            tx.conn()
                .execute("DELETE FROM memory_pending WHERE id = ?1", [&note.id])?;
            tx.event(
                NewEvent::new("memory_pending", &note.id, "memory.discarded")
                    .rev(note.rev)
                    .card_opt(note.card_id.clone())
                    // Only the id: a discarded note's text reaches no
                    // event a fleet session can read (`memory.md` section 7).
                    .payload(json!({"pending_id": note.id})),
            )?;
            Ok(json!({}))
        })
    }

    /// One pending note, by id or short id.
    pub fn pending_note(&self, id: &str) -> Result<PendingNote> {
        pending(self.conn(), id)
    }

    /// The pending notes of one project, or of every project, oldest first.
    pub fn pending_notes(&self, project: Option<&str>) -> Result<Vec<PendingNote>> {
        match project {
            Some(project) => list(
                self.conn(),
                "project_id = ?1 ORDER BY created_at, id",
                &[&project],
            ),
            None => list(self.conn(), "1 ORDER BY created_at, id", &[]),
        }
    }
}

/// The body of `mem.note`. `retro.finish` composes it into its own
/// transaction with the retro it finishes, so the note is the curator's
/// whoever finishes the retro.
pub(super) fn insert_note(
    tx: &mut Tx<'_>,
    note: &NewNote,
    retro: Option<&Retro>,
) -> Result<PendingNote> {
    let from = match retro {
        Some(retro) => curator(retro.clone()),
        None => origin(tx.conn(), tx.actor())?,
    };
    let project = match (&from.project, &note.project) {
        (Some(own), Some(asked)) if own != asked => {
            return Err(Error::Forbidden(format!(
                "{} notes for {own}, not {asked}",
                tx.actor()
            )));
        }
        (Some(own), _) => own.clone(),
        (None, Some(asked)) => asked.clone(),
        (None, None) => return Err(Error::Usage("name the project the note is for".into())),
    };
    get::<Project>(tx.conn(), "project", &project)?;
    check_kind(&note.kind)?;
    let title = note.title.trim();
    let body = note.body.trim();
    check_title(title)?;
    check_body(body)?;
    if let Some(run) = &from.run {
        let used = count(tx, "run_id", run)?;
        if used >= RUN_LIMIT {
            return Err(Error::refused(
                "run",
                run,
                format!("it added {used} notes; a run adds at most {RUN_LIMIT}"),
            ));
        }
    }
    let held = count(tx, "project_id", &project)?;
    if held >= PROJECT_LIMIT {
        return Err(Error::refused(
            "project",
            &project,
            format!(
                "it holds {held} pending notes; a project holds at most {PROJECT_LIMIT} until the owner keeps or discards some"
            ),
        ));
    }
    let row = PendingNote {
        id: ids::new_ulid(),
        project_id: project,
        kind: note.kind.clone(),
        title: title.to_owned(),
        body: body.to_owned(),
        source: from.source.to_owned(),
        role: from.role,
        card_id: from.card,
        run_id: from.run,
        session_id: from.session,
        retro_id: from.retro,
        created_at: tx.now(),
        edited_at: None,
        rev: 1,
    };
    tx.conn().execute(
        "INSERT INTO memory_pending (id, project_id, kind, title, body, source, role, card_id,
             run_id, session_id, retro_id, created_at, rev)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)",
        rusqlite::params![
            row.id,
            row.project_id,
            row.kind,
            row.title,
            row.body,
            row.source,
            row.role,
            row.card_id,
            row.run_id,
            row.session_id,
            row.retro_id,
            row.created_at,
        ],
    )?;
    tx.event(
        NewEvent::new("memory_pending", &row.id, "memory.noted")
            .rev(1)
            .card_opt(row.card_id.clone())
            .payload(json!({
                "pending_id": row.id,
                "project_id": row.project_id,
                "source": row.source,
            })),
    )?;
    Ok(row)
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
