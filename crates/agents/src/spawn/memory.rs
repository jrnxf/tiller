//! The kb notes a launch renders into the role prompt (`memory.md` sections
//! 2 and 3, L3): the notes of the host's kb scopes for the project, in load
//! order, within the role's startup budget. The prompt carries them after
//! the role body, under one `# Memory` heading. A missing or unreadable
//! store loads nothing and the launch goes on.

use std::path::Path;

use tiller_core::domain::Project;
use tiller_core::kb::{self, KbConfig, Rendered};
use tiller_core::state::memory::repo_scope_of;
use tiller_core::{Ctx, Store};

use super::SpawnError;
use crate::roles::Role;

/// What a launch renders and records.
pub type Memory = Rendered;

/// The startup budget a role file and its notes share, in tokens (P42).
pub const STARTUP_BUDGET: i64 = 7_500;

/// The kb scopes a launch in `project` loads, in load order.
pub fn scopes(config: &KbConfig, project: &Project) -> Vec<String> {
    config.scopes(repo_scope_of(project).as_deref())
}

/// The notes a launch of `role` in `project` loads now, from the kb store
/// at `home` (`None`: no store).
pub fn render(
    config: &KbConfig,
    home: Option<&Path>,
    project: &Project,
    role: Role,
) -> Result<Memory, SpawnError> {
    let role_tokens = kb::tokens(role.prompt_body()?);
    let read = kb::read(home);
    Ok(kb::render(
        &read,
        &scopes(config, project),
        role_tokens,
        STARTUP_BUDGET,
    ))
}

/// Records that `session` loaded `memory` (`mem.load`), once its launch
/// line went in, so a launch that failed first records no load. The session
/// runs on the memory either way, so a failed record is a warning, never
/// the launch's error. Returns the events and that warning.
pub fn record_load(
    store: &mut Store,
    ctx: &Ctx,
    session: &str,
    memory: &Memory,
) -> (Vec<i64>, Option<String>) {
    match store.mem_load(ctx, session, memory) {
        Ok(applied) => (applied.events, None),
        Err(e) => (
            Vec::new(),
            Some(format!(
                "recording the memory session {session} loaded failed: {e}"
            )),
        ),
    }
}
