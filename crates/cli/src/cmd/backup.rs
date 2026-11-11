//! Backup, restore, export and the schema migration tool (T2.3;
//! `protocol.md` section 6.4, `recovery.md` R10 and R11). All owner only.
//! Restore, the export's import and migrate never open `state.db` through
//! the store, which would migrate it; they take the daemon's lock instead.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use tiller_core::Timestamp;
use tiller_core::backup::{self, Paths};
use tiller_core::domain::Boundary;
use tiller_core::migrate::LATEST;

use crate::app::{App, Done, Failure, Outcome};

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Write one JSONL file per table, and a manifest, into this new or empty directory
    #[arg(long, value_name = "DIR")]
    pub jsonl: PathBuf,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// A backup the manifest lists; the newest one that verifies by default
    pub file: Option<PathBuf>,
    /// Rebuild a new state.db from a JSONL export instead of a backup
    #[arg(long, value_name = "DIR", conflicts_with = "file")]
    pub jsonl: Option<PathBuf>,
    /// This host's boundary, which the export must match (with --jsonl)
    #[arg(long, requires = "jsonl")]
    pub boundary: Option<Boundary>,
    /// Print what would be restored and change nothing
    #[arg(long, conflicts_with = "jsonl")]
    pub dry_run: bool,
}

#[derive(Subcommand, Debug)]
pub enum MigrateCmd {
    /// Migrate up to this tiller's schema
    Up,
    /// Roll the schema back, after a verified copy, with the newer binary
    Down {
        /// The schema version to go down to; one below this tiller's by default
        #[arg(long)]
        to: Option<usize>,
    },
}

fn paths(app: &App) -> Paths {
    Paths::for_db(&app.db)
}

/// `tiller backup`: a verified backup now, then retention.
pub fn backup(app: &App) -> Done {
    app.require_owner("tiller backup")?;
    let store = app.read()?;
    let done = backup::backup(store.conn(), &paths(app), Timestamp::now())?;
    Ok(Outcome::read(done))
}

/// `tiller export --jsonl <dir>`.
pub fn export(app: &App, args: ExportArgs) -> Done {
    app.require_owner("tiller export")?;
    let store = app.read()?;
    let manifest = backup::export_jsonl(store.conn(), &paths(app), &args.jsonl, Timestamp::now())?;
    Ok(Outcome::read(manifest)
        .with_help("the export holds work data: keep it on this host and out of git"))
}

/// `tiller restore [<file>] [--dry-run]` and `tiller restore --jsonl <dir>`.
pub fn restore(app: &App, args: RestoreArgs) -> Done {
    app.require_owner("tiller restore")?;
    // A dry run changes nothing, so the marker, which guards writes, does not apply.
    if !args.dry_run {
        app.check_owner_marker()?;
    }
    let paths = paths(app);
    if let Some(dir) = args.jsonl {
        let boundary = args.boundary.ok_or_else(|| {
            Failure::usage("restoring from an export needs this host's --boundary personal|work")
        })?;
        let imported = backup::import_jsonl(&dir, &paths, boundary)?;
        return Ok(Outcome::read(imported).with_help("tiller daemon"));
    }
    let restored = backup::restore(&paths, args.file.as_deref(), args.dry_run, Timestamp::now())?;
    let outcome = Outcome::read(&restored);
    Ok(if restored.dry_run {
        outcome.with_help("run it again without --dry-run to restore")
    } else {
        outcome.with_help("tiller daemon, which adopts live panes and asks about merged PRs")
    })
}

/// `tiller migrate up|down [--to <n>]`.
pub fn migrate(app: &App, cmd: MigrateCmd) -> Done {
    app.check_owner_marker()?;
    let target = match cmd {
        MigrateCmd::Up => LATEST,
        MigrateCmd::Down { to } => {
            app.require_owner("tiller migrate down")?;
            let target = to.unwrap_or(LATEST.saturating_sub(1));
            if target >= LATEST {
                return Err(Failure::usage(format!(
                    "migrate down goes below this tiller's schema {LATEST}; use --to <n> with n < {LATEST}"
                )));
            }
            target
        }
    };
    let migrated = backup::migrate(&paths(app), target, Timestamp::now())?;
    Ok(Outcome::read(migrated))
}
