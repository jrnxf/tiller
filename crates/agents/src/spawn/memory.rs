//! The approved memory a launch renders into the role prompt (`memory.md`
//! section 2, L3): the current approved revision of each Tiller document
//! that loads for the session's role and project, read from the memory repo
//! by its blob SHA, never from the working tree. The prompt carries it after
//! the role body, under one `# Memory` heading and a `## <path>` heading per
//! document, which is the form the holdout eval measures a candidate in
//! (`learning-loop.md` section 7).

use std::path::Path;
use std::process::{Command, Stdio};

use tiller_core::import::EMPTY_BLOB;
use tiller_core::memory::render_loaded;
use tiller_core::state::memory_load::{Loadable, Skipped};
use tiller_core::{Ctx, Store};

use super::SpawnError;
use crate::holdout::repo_dir;
use crate::roles::Role;

/// What a launch renders and records: the prompt text, the `(source id,
/// blob sha)` pairs of the config manifest, the revisions `mem.load`
/// writes for the session, and the documents it went on without.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Empty when nothing loads; else the `# Memory` part of the prompt.
    pub text: String,
    pub pairs: Vec<(String, String)>,
    pub revisions: Vec<String>,
    /// The documents that would load but have no approved revision: the
    /// launch goes on without them, and `mem.load` names them.
    pub skipped: Vec<Skipped>,
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
    match store.mem_load(ctx, session, &memory.revisions, &memory.skipped) {
        Ok(applied) => (applied.events, None),
        Err(e) => (
            Vec::new(),
            Some(format!(
                "recording the memory session {session} loaded failed: {e}"
            )),
        ),
    }
}

/// The approved memory for a launch of `role` in `project`, from the memory
/// repo under `home`. A blob the repo cannot give refuses the launch: the
/// session would otherwise start without text the record says it got. A
/// document with no approved revision does not refuse it: the launch goes
/// on without that document, and [`Memory::skipped`] names it.
pub fn render(store: &Store, home: &Path, project: &str, role: Role) -> Result<Memory, SpawnError> {
    let loadable = store.mem_loadable(project, role.name())?;
    let mut memory = Memory {
        skipped: store.mem_skipped(project, role.name())?,
        ..Memory::default()
    };
    let mut docs = Vec::new();
    for doc in &loadable {
        let text = blob(home, doc)?;
        // An emptied document gives the session nothing to load.
        if text.trim().is_empty() {
            continue;
        }
        memory
            .pairs
            .push((doc.source_id.clone(), doc.blob_sha.clone()));
        memory.revisions.push(doc.revision_id.clone());
        docs.push((doc.canonical_path.as_str(), text));
    }
    memory.text = render_loaded(docs.iter().map(|(path, text)| (*path, text.as_str())));
    Ok(memory)
}

/// The text of `doc`'s approved blob (`git cat-file blob <sha>`).
fn blob(home: &Path, doc: &Loadable) -> Result<String, SpawnError> {
    if doc.blob_sha == EMPTY_BLOB {
        return Ok(String::new());
    }
    let dir = repo_dir(&doc.canonical_repo, home);
    let output = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["cat-file", "blob", &doc.blob_sha])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(SpawnError::Refused(format!(
            "the approved revision {} of {} (blob {}) cannot be read from {}: {}",
            doc.revision_id,
            doc.canonical_path,
            doc.blob_sha,
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        SpawnError::Refused(format!(
            "the approved revision {} of {} is not UTF-8 text",
            doc.revision_id, doc.canonical_path
        ))
    })
}
