//! `tiller orchestrator start <project>` (`protocol.md` 6.4, T4.2): the
//! project's orchestrator, launched in its herdr workspace with the same
//! launch form as a worker (`roles/README.md`, The launch form).
//!
//! A start runs in this order:
//!
//! 1. a live orchestrator for the project is returned as it is (natural
//!    retry: never a second one);
//! 2. the launch files are written into `<scratch_root>/<session id>`, the
//!    id minted first because the script names it, and the manifest is
//!    recorded;
//! 3. herdr opens a new workspace for it in the checkout, labelled for the
//!    project. A pane an earlier orchestrator left is never typed into:
//!    herdr cannot tell a bare shell from a program running in it;
//! 4. `session.start` records the live orchestrator on that pane, which is
//!    what `tiller spawn` and the outbox look for; a start that lost a race
//!    to another one closes what it opened and returns the winner;
//! 5. the row tokens are reported, the layout pass runs and the launch line
//!    is typed into the pane.
//!
//! The orchestrator runs in the project checkout (`recovery.md`, Resuming a
//! session: an orchestrator resumes in "the project's checkout path from the
//! config file"), with no run, so `TILLER_RUN_ID` is empty. Its home, the
//! one directory besides its scratch it may edit, is `<home_root>/<project>`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tiller_core::domain::{Effort, OutboxMessage, Project, Session};
use tiller_core::state::get;
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::runs::NewOrchestrator;
use tiller_core::{Actor, Applied, Ctx, Store, ids};
use tiller_herdr::client::Herdr;
use tiller_herdr::layout;

use super::launch::{self, LaunchSpec, Roots};
use super::{Result, SpawnError, World, local_host, relayout};
use crate::roles::Role;

/// The session end reason of an orchestrator whose launch line never
/// reached its pane.
pub const LAUNCH_FAILED: &str = "launch_failed";

/// What a start returns: the live orchestrator and where it runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Orchestrator {
    pub session: Session,
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub label: String,
    /// The orchestrator's home; `None` for one this start did not launch.
    pub home: Option<String>,
    /// One note per unresolved ask, queued for the new session.
    pub notes: Vec<OutboxMessage>,
    /// Something the owner should know that did not stop the start: a
    /// layout repair that failed, or a returned orchestrator whose pane
    /// herdr no longer shows.
    pub warning: Option<String>,
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
    herdr: &'a dyn Herdr,
    scratch: &'a Path,
    home: Option<&'a Path>,
}

/// Starts the project's orchestrator, or returns the live one.
/// `home_root` is where orchestrator homes live, already resolved from
/// `spawn.orchestrator_home` or its default.
pub fn start(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    home_root: &Path,
    project: &str,
) -> Result<Applied<Orchestrator>> {
    if !matches!(ctx.actor, Actor::Owner | Actor::Daemon) {
        return Err(tiller_core::Error::Forbidden(format!(
            "{} may not start an orchestrator; the owner or the daemon does",
            ctx.actor
        ))
        .into());
    }
    let project: Project = get(store.conn(), "project", project)?;
    let label = layout::orchestrator_label(&project.id);
    if let Some(live) = live_orchestrator(store.conn(), &project.id)? {
        return Ok(existing(world.herdr, live, label));
    }
    if project.retired_at.is_some() {
        return Err(tiller_core::Error::Refused {
            entity: "project",
            id: project.id.clone(),
            fact: "project is retired".into(),
        }
        .into());
    }
    let host = local_host(store)?;
    let places = places(world, home_root, &project.id)?;
    let fresh_home = !places.home.exists();
    fs::create_dir_all(&places.home)?;
    fs::set_permissions(&places.home, fs::Permissions::from_mode(0o700))?;

    let role = Role::Orchestrator;
    let front = role.frontmatter()?;
    let effort: Effort = front.effort.parse().map_err(|_| {
        SpawnError::Usage(format!(
            "the orchestrator role names effort {}",
            front.effort
        ))
    })?;
    let session_id = ids::new_ulid();
    let scratch = world.scratch_root.join(&session_id);
    let claude_version = launch::claude_version(&world.config.spawn.claude);
    let launched = launch::write(&LaunchSpec {
        role,
        session_id: &session_id,
        run_id: "",
        worktree: &places.checkout,
        scratch: &scratch,
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
        no_mistakes_socket: world.config.spawn.no_mistakes_socket.as_deref(),
        claude: &world.config.spawn.claude,
        claude_version: claude_version.as_deref(),
        memory: &[],
        note: None,
    })?;
    let mut events = Vec::new();
    let opened = Opened {
        herdr: world.herdr,
        scratch: &scratch,
        home: fresh_home.then_some(places.home.as_path()),
    };
    let unwind = |pane: Option<&str>, error: SpawnError| opened.undo(pane, error);
    let recorded =
        match store.config_manifest_record(ctx, &launched.config_hash, &launched.manifest) {
            Ok(recorded) => recorded,
            Err(e) => return Err(unwind(None, e.into())),
        };
    events.extend(recorded.events);

    let pane = match world.herdr.create_workspace(&label, Some(&places.checkout)) {
        Ok(pane) => pane,
        Err(e) => return Err(unwind(None, e.into())),
    };
    let started = store.session_start(
        ctx,
        NewOrchestrator {
            id: Some(session_id.clone()),
            project_id: project.id.clone(),
            pane_id: Some(pane.pane_id.clone()),
            config_hash: Some(launched.config_hash.clone()),
            ..NewOrchestrator::default()
        },
    );
    let started = match started {
        Ok(started) => started,
        Err(e) => {
            // Another start won the race: return its orchestrator.
            let live = live_orchestrator(store.conn(), &project.id);
            let error = unwind(Some(&pane.pane_id), e.into());
            return match live {
                Ok(Some(live)) => {
                    // The manifest this start recorded is still written.
                    let mut found = existing(world.herdr, live, label);
                    found.events = events;
                    Ok(found)
                }
                _ => Err(error),
            };
        }
    };
    events.extend(started.events);

    let launch = || -> Result<Option<String>> {
        let bindings = layout::bindings(store.conn(), &host)?;
        // The tokens name the session before anything else reads the pane.
        if let Some(binding) = bindings.iter().find(|b| b.session_id == session_id) {
            world
                .herdr
                .report_tokens(&pane.pane_id, &binding.tokens())?;
        }
        let warning = relayout(world.herdr, &bindings).err();
        world.herdr.send_text(&pane.pane_id, &launched.line)?;
        world.herdr.send_enter(&pane.pane_id)?;
        Ok(warning)
    };
    let warning = match launch() {
        Ok(warning) => warning,
        Err(error) => {
            let mut notes = Vec::new();
            if let Err(e) = store.session_end(ctx, &session_id, LAUNCH_FAILED) {
                notes.push(format!("ending session {session_id} failed: {e}"));
            }
            let error = unwind(Some(&pane.pane_id), error);
            return Err(match (error, notes.is_empty()) {
                (error, true) => error,
                (SpawnError::Undone { error, notes: more }, false) => SpawnError::Undone {
                    error,
                    notes: format!("{more}; {}", notes.join("; ")),
                },
                (error, false) => SpawnError::Undone {
                    error: Box::new(error),
                    notes: notes.join("; "),
                },
            });
        }
    };
    let session = started.value.session;
    Ok(Applied {
        value: Orchestrator {
            session,
            pane_id: Some(pane.pane_id),
            workspace_id: Some(pane.workspace_id),
            label,
            home: Some(places.home.to_string_lossy().into_owned()),
            notes: started.value.notes,
            warning,
        },
        events,
        replayed: false,
    })
}

/// The live orchestrator, returned as a replay. A pane herdr no longer
/// shows is only a warning: the liveness ladder decides whether the session
/// ended (`recovery.md`, section 2), not a start.
fn existing(herdr: &dyn Herdr, live: Session, label: String) -> Applied<Orchestrator> {
    let (workspace_id, warning) = match &live.pane_id {
        None => (
            None,
            Some(format!("orchestrator session {} has no pane", live.id)),
        ),
        Some(pane) => match herdr.pane(pane) {
            Ok(Some(info)) => (Some(info.workspace_id), None),
            Ok(None) => (
                None,
                Some(format!(
                    "herdr no longer shows pane {pane} of orchestrator session {}",
                    live.id
                )),
            ),
            Err(e) => (None, Some(format!("herdr: {e}"))),
        },
    };
    Applied {
        value: Orchestrator {
            pane_id: live.pane_id.clone(),
            session: live,
            workspace_id,
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
    let config = world.config;
    let named = |key: &str, path: Option<&Path>| -> Result<PathBuf> {
        let path = path.ok_or_else(|| {
            SpawnError::Config(format!(
                "{key} is not set; tiller orchestrator start needs it"
            ))
        })?;
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err(SpawnError::Config(format!(
                "{key} must be an absolute path without `..`, not {}",
                path.display()
            )));
        }
        // No `.` parts or trailing slash, so `starts_with` compares what the
        // fragment will name.
        Ok(path.components().collect())
    };
    let checkout = named(
        &format!("checkouts.{project}"),
        config.checkouts.get(project).map(PathBuf::as_path),
    )?;
    let pool = named("spawn.pool_root", config.spawn.pool_root.as_deref())?;
    let checkouts = named(
        "spawn.checkouts_root",
        config.spawn.checkouts_root.as_deref(),
    )?;
    let home = named("spawn.orchestrator_home", Some(home_root))?.join(project);
    let scratch = named("spawn.scratch_root", Some(world.scratch_root))?;
    if !checkout.starts_with(&checkouts) {
        return Err(SpawnError::Config(format!(
            "checkouts.{project} ({}) is not under spawn.checkouts_root ({})",
            checkout.display(),
            checkouts.display()
        )));
    }
    for (what, path) in [
        ("the orchestrator home", &home),
        ("spawn.scratch_root", &scratch),
    ] {
        for (key, root) in [
            ("spawn.pool_root", &pool),
            ("spawn.checkouts_root", &checkouts),
        ] {
            if path.starts_with(root) || root.starts_with(path) {
                return Err(SpawnError::Config(format!(
                    "{what} {} overlaps {key} ({})",
                    path.display(),
                    root.display()
                )));
            }
        }
    }
    if !checkout.is_dir() {
        return Err(SpawnError::Config(format!(
            "checkouts.{project} ({}) is not a directory",
            checkout.display()
        )));
    }
    Ok(Places {
        checkout,
        home,
        pool,
        checkouts,
    })
}

impl Opened<'_> {
    /// Undoes a start that failed before its launch line ran: closes the
    /// pane it opened, removes its scratch directory and the home it made
    /// while that is still empty. Returns the original error, with what
    /// could not be undone appended.
    fn undo(&self, pane: Option<&str>, error: SpawnError) -> SpawnError {
        let mut notes = Vec::new();
        if let Some(pane) = pane
            && let Err(e) = self.herdr.close_pane(pane)
        {
            notes.push(format!("closing pane {pane} failed: {e}"));
        }
        if let Err(e) = fs::remove_dir_all(self.scratch) {
            notes.push(format!("removing {} failed: {e}", self.scratch.display()));
        }
        // `remove_dir` leaves a home that holds anything.
        if let Some(home) = self.home {
            let _ = fs::remove_dir(home);
        }
        if notes.is_empty() {
            return error;
        }
        SpawnError::Undone {
            error: Box::new(error),
            notes: notes.join("; "),
        }
    }
}
