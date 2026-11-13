//! `tiller orchestrator start <project>` (`protocol.md` 6.4, T4.2): the
//! project's orchestrator, launched in a terminal holder pane with the
//! same launch form as a worker (`roles/README.md`, The launch form).
//!
//! A start runs in this order:
//!
//! 1. a live orchestrator for the project is returned as it is (natural
//!    retry: never a second one), unless the start is a restart: then the
//!    owner's live orchestrator is stopped first, its end proven as a
//!    relaunch's is, and its pane closed, so the new one starts from launch
//!    files rendered by the current binary and config;
//! 2. the launch files are written into `<scratch_root>/<session id>`, the
//!    id minted first because the script names it, and the manifest is
//!    recorded;
//! 3. the terminal holder spawns a new pane for it in the checkout, which
//!    runs the launch script (`terminal-holder.md` 4.4). A pane an earlier
//!    orchestrator left is never reused: a holder pane runs one program;
//! 4. `session.start` records the live orchestrator on that pane, which is
//!    what `tiller spawn` and the outbox look for; a start that lost a race
//!    to another one closes what it opened and returns the winner.
//!
//! The orchestrator runs in the project checkout (`recovery.md`, Resuming a
//! session: an orchestrator resumes in "the project's checkout path from the
//! config file"), with no run, so `TILLER_RUN_ID` is empty. Its home, the
//! one directory besides its scratch it may edit, is `<home_root>/<project>`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tiller_core::domain::{Effort, OutboxMessage, Project, Session};
use tiller_core::state::get;
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::runs::{NewOrchestrator, OrchestratorStarted};
use tiller_core::{Actor, Applied, Ctx, Store, ids};

use super::launch::{self, LaunchSpec, Roots};
use super::memory;
use super::settings;
use super::{Result, SpawnError, World, close_pane, holder, local_host};
use crate::control::{self, Waits};
use crate::roles::Role;

/// The session end reason of an orchestrator stopped by a restart.
pub const RESTARTED: &str = "restarted";

/// What a start returns: the live orchestrator and where it runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Orchestrator {
    pub session: Session,
    pub pane_id: Option<String>,
    pub label: String,
    /// The orchestrator's home; `None` for one this start did not launch.
    pub home: Option<String>,
    /// One note per unresolved ask, queued for the new session.
    pub notes: Vec<OutboxMessage>,
    /// Something the owner should know that did not stop the start: a
    /// record that failed, or a returned orchestrator whose pane the
    /// holder no longer runs.
    pub warning: Option<String>,
}

fn refuse_retired(project: &Project) -> Result<()> {
    if project.retired_at.is_some() {
        return Err(tiller_core::Error::Refused {
            entity: "project",
            id: project.id.clone(),
            fact: "project is retired".into(),
        }
        .into());
    }
    Ok(())
}

/// The directories a start reads from the config file.
struct Places {
    checkout: PathBuf,
    home: PathBuf,
    pool: PathBuf,
    checkouts: PathBuf,
}

/// What a failed start undoes: the pane it opened, its scratch directory and
/// the home when this start made it.
struct Opened<'a> {
    world: &'a World<'a>,
    scratch: &'a Path,
    home: Option<&'a Path>,
}

/// Starts the project's orchestrator, or returns the live one.
/// `home_root` is where orchestrator homes live, already resolved from
/// `spawn.orchestrator_home` or its default.
///
/// `restart` is how long to wait for the live orchestrator to exit; with it
/// set, a live orchestrator is stopped and replaced instead of returned.
///
/// `ctx.key` keys the `session.start` alone, not the manifest or a stop: a
/// retry with a key that already started replays that start without
/// touching the holder, and one that lost a race to it closes what it opened.
pub fn start(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    home_root: &Path,
    project: &str,
    restart: Option<&Waits>,
) -> Result<Applied<Orchestrator>> {
    // The board and a verified peer board are the owner too: their Start
    // orchestrator comes here (`authority.md` section 4, G41).
    if !ctx.actor.is_owner() && ctx.actor != Actor::Daemon {
        return Err(tiller_core::Error::Forbidden(format!(
            "{} may not start an orchestrator; the owner or the daemon does",
            ctx.actor
        ))
        .into());
    }
    let keyed = ctx.clone();
    let ctx = &ctx.unkeyed();
    let project: Project = get(store.conn(), "project", project)?;
    let label = crate::labels::orchestrator_label(&project.id);
    // A start this key made already replays while its orchestrator lives.
    // One whose launch failed has ended, so a retry starts afresh, unkeyed.
    let keyed = if store.key_recorded(&keyed)? {
        let replay = store.session_start(&keyed, new_orchestrator(&project.id))?;
        let session = &replay.value.session;
        if session.project_id != project.id {
            return Err(SpawnError::Refused(format!(
                "this key started the orchestrator of project {}, not {}; start with a new key",
                session.project_id, project.id
            )));
        }
        // The stored response holds the session as it started: read it now.
        let now: Session = get(store.conn(), "session", &session.id)?;
        if now.ended_at.is_none() {
            return Ok(replayed(world, replay, label));
        }
        ctx.clone()
    } else {
        keyed
    };
    let mut events = Vec::new();
    let mut leftover = None;
    let restarting = match (live_orchestrator(store.conn(), &project.id)?, restart) {
        (Some(live), None) => return Ok(existing(world, live, label)),
        (Some(live), Some(waits)) => {
            // The board is the owner signed in on this host, so its Restart
            // and its stale restart record the board all the way through
            // (T6). A peer's board, the daemon and a session may not.
            refuse_retired(&project)?;
            if !matches!(ctx.actor, Actor::Owner | Actor::Board) {
                return Err(tiller_core::Error::Forbidden(format!(
                    "{} may not restart an orchestrator; the owner does",
                    ctx.actor
                ))
                .into());
            }
            Some((live, waits))
        }
        (None, _) => None,
    };
    refuse_retired(&project)?;
    local_host(store)?;
    let places = places(world, home_root, &project.id)?;
    let role = Role::Orchestrator;
    let front = role.frontmatter()?;
    let effort: Effort = front.effort.parse().map_err(|_| {
        SpawnError::Usage(format!(
            "the orchestrator role names effort {}",
            front.effort
        ))
    })?;
    let memory = memory::render(store, world.tiller_home, &project.id, role)?;
    // Every check comes before the stop: a restart refused by a check
    // leaves the live one running. The writes after the stop (the home, the
    // launch files, the manifest, the holder pane) can still fail and
    // leave the project with no orchestrator.
    if let Some((live, waits)) = restarting {
        let stopped =
            control::stop_orchestrator(store, ctx, world.holder(), waits, &live, RESTARTED)?;
        events.extend(stopped.events);
        leftover = stopped.warning;
    }
    let fresh_home = !places.home.exists();
    fs::create_dir_all(&places.home)?;
    fs::set_permissions(&places.home, fs::Permissions::from_mode(0o700))?;

    let session_id = ids::new_ulid();
    let scratch = world.scratch_root.join(&session_id);
    let opened = Opened {
        world,
        scratch: &scratch,
        home: fresh_home.then_some(places.home.as_path()),
    };
    let unwind = |pane: Option<&str>, error: SpawnError| opened.undo(pane, error);
    let claude_version = launch::claude_version(&world.config.spawn.claude);
    let launched = launch::write(&LaunchSpec {
        role,
        session_id: &session_id,
        run_id: "",
        worktree: &places.checkout,
        scratch: &scratch,
        tiller_home: world.tiller_home,
        model: &front.model,
        effort,
        brief: None,
        roots: Some(Roots {
            home: &places.home,
            pool: &places.pool,
            checkouts: &places.checkouts,
        }),
        tiller: world.tiller,
        default_branch: &project.default_branch,
        network_allow: &world.config.spawn.network_allow,
        review_hosts: &world.config.spawn.review_allowed_hosts,
        no_mistakes_socket: world.config.spawn.no_mistakes_socket.as_deref(),
        gh_login_readable: false,
        claude: &world.config.spawn.claude,
        claude_config: world.claude_config,
        claude_version: claude_version.as_deref(),
        memory: &memory,
        note: None,
    });
    let launched = match launched {
        Ok(launched) => launched,
        Err(e) => return Err(unwind(None, e)),
    };
    let recorded =
        match store.config_manifest_record(ctx, &launched.config_hash, &launched.manifest) {
            Ok(recorded) => recorded,
            Err(e) => return Err(unwind(None, e.into())),
        };
    events.extend(recorded.events);

    // The holder runs the launch as it spawns, so the session row below is
    // written while `claude` starts; its `SessionStart` hook finds the row
    // by `TILLER_SESSION_ID` only once that write is in. There is no write
    // that puts a pane on an orchestrator's session after its start, so the
    // order stays until one is added.
    let owner = holder::Owner {
        session: &session_id,
        run: None,
        project: &project.id,
        role: "orchestrator",
        label: &label,
    };
    let opened_pane = world
        .holder()
        .require()
        .map_err(SpawnError::from)
        .and_then(|panes| {
            holder::open(panes, &owner, &launched.script, &places.checkout)
                .map(|pane| pane.pane.to_string())
        });
    let pane_id = match opened_pane {
        Ok(pane_id) => pane_id,
        Err(e) => return Err(unwind(None, e)),
    };
    let started = store.session_start(
        &keyed,
        NewOrchestrator {
            id: Some(session_id.clone()),
            project_id: project.id.clone(),
            pane_id: Some(pane_id.clone()),
            config_hash: Some(launched.config_hash.clone()),
            ..NewOrchestrator::default()
        },
    );
    let started = match started {
        // The key started one already, in a request that won the race.
        Ok(started) if started.replayed => {
            let notes = opened.clean(Some(&pane_id));
            let mut found = replayed(world, started, label);
            found.events.splice(0..0, events);
            found.value.warning = [found.value.warning.take(), leftover]
                .into_iter()
                .chain(notes.into_iter().map(Some))
                .flatten()
                .reduce(|all, next| format!("{all}; {next}"));
            return Ok(found);
        }
        Ok(started) => started,
        Err(e) => {
            // Another start won the race: return its orchestrator.
            let live = live_orchestrator(store.conn(), &project.id);
            let error = unwind(Some(&pane_id), e.into());
            return match live {
                Ok(Some(live)) => {
                    // The manifest this start recorded is still written.
                    let mut found = existing(world, live, label);
                    found.events = events;
                    Ok(found)
                }
                _ => Err(error),
            };
        }
    };
    events.extend(started.events);
    // What the session was launched with, for the stale-session check. Its
    // event is not the start's, so a keyed replay of the start stays equal to
    // the first answer. A failed record leaves the session counted stale,
    // which only offers a restart.
    let stamped = match crate::stale::record(store, ctx, &session_id, role) {
        Ok(_) => None,
        Err(e) => Some(format!(
            "recording the launch of session {session_id} failed: {e}"
        )),
    };
    // The memory it was given, which the revision's "Loaded by" lists. Its
    // event is not the start's, as with the stamp.
    let (_, loaded) = memory::record_load(store, ctx, &session_id, &memory);
    let warning = [launched.mode_note, leftover, stamped, loaded]
        .into_iter()
        .flatten()
        .reduce(|all, next| format!("{all}; {next}"));
    let session = started.value.session;
    Ok(Applied {
        value: Orchestrator {
            session,
            pane_id: Some(pane_id),
            label,
            home: Some(places.home.to_string_lossy().into_owned()),
            notes: started.value.notes,
            warning,
        },
        events,
        replayed: false,
    })
}

/// The bare `session.start` a keyed replay looks itself up with.
fn new_orchestrator(project_id: &str) -> NewOrchestrator {
    NewOrchestrator {
        project_id: project_id.to_owned(),
        ..NewOrchestrator::default()
    }
}

/// A keyed start's stored result: its session as recorded, whether the
/// holder still runs it, and its notes.
fn replayed(
    world: &World<'_>,
    replay: Applied<OrchestratorStarted>,
    label: String,
) -> Applied<Orchestrator> {
    let mut found = existing(world, replay.value.session, label);
    found.value.notes = replay.value.notes;
    found.events = replay.events;
    found
}

/// The live orchestrator, returned as a replay. A pane the holder no longer
/// runs is only a warning: the liveness ladder decides whether the session
/// ended (`recovery.md`, section 2), not a start.
fn existing(world: &World<'_>, live: Session, label: String) -> Applied<Orchestrator> {
    let warning = match &live.pane_id {
        None => Some(format!("orchestrator session {} has no pane", live.id)),
        Some(pane) => match holder::running(world.panes, pane) {
            Ok(true) => None,
            Ok(false) => Some(format!(
                "the terminal holder no longer runs pane {pane} of orchestrator session {}",
                live.id
            )),
            Err(e) => Some(e.to_string()),
        },
    };
    Applied {
        value: Orchestrator {
            pane_id: live.pane_id.clone(),
            session: live,
            label,
            home: None,
            notes: Vec::new(),
            warning,
        },
        events: Vec::new(),
        replayed: true,
    }
}

/// The checkout, home, pool root and checkouts root, checked: every one
/// absolute with no `..`, the checkout under the checkouts root, and the
/// home and the scratch root outside both roots, since the fragment's deny
/// rules on them would win over its allow rules on the home and scratch.
fn places(world: &World<'_>, home_root: &Path, project: &str) -> Result<Places> {
    let (resolved, problems) =
        settings::resolve(world.config, home_root, world.scratch_root, Some(project));
    if let Some(first) = problems.into_iter().next() {
        return Err(SpawnError::Config(first.problem));
    }
    match resolved {
        settings::Resolved {
            checkout: Some(checkout),
            home: Some(home),
            pool: Some(pool),
            checkouts: Some(checkouts),
        } => Ok(Places {
            checkout,
            home,
            pool,
            checkouts,
        }),
        _ => Err(SpawnError::Config(
            "the start settings did not resolve".into(),
        )),
    }
}

impl Opened<'_> {
    /// Undoes a start that failed before its launch line ran: closes the
    /// pane it opened, removes its scratch directory and the home it made
    /// while that is still empty. Returns the original error, with what
    /// could not be undone appended.
    fn undo(&self, pane: Option<&str>, error: SpawnError) -> SpawnError {
        let notes = self.clean(pane);
        if notes.is_empty() {
            return error;
        }
        SpawnError::Undone {
            error: Box::new(error),
            notes: notes.join("; "),
        }
    }

    /// Closes the pane, removes the scratch directory and the home it made
    /// while that is still empty. Returns what could not be undone.
    fn clean(&self, pane: Option<&str>) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(pane) = pane
            && let Err(e) = close_pane(self.world, pane)
        {
            notes.push(format!("closing pane {pane} failed: {e}"));
        }
        // A launch refused before its files were written left no scratch.
        match fs::remove_dir_all(self.scratch) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                notes.push(format!("removing {} failed: {e}", self.scratch.display()));
            }
            _ => {}
        }
        // `remove_dir` leaves a home that holds anything.
        if let Some(home) = self.home {
            let _ = fs::remove_dir(home);
        }
        notes
    }
}
