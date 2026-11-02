//! `tiller import <bundle> [--dry-run]` (T2.4; `protocol.md` section 6.4).
//! Owner only. The writes carry `actor = import`.

use std::path::PathBuf;

use clap::Args;
use tiller_core::import::Bundle;
use tiller_core::{Actor, Ctx, Timestamp};

use crate::app::{App, Done, Outcome};

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// A tiller-import/v1 bundle directory, as Firstmate's export writes it
    pub bundle: PathBuf,
    /// Print the full plan and change nothing
    #[arg(long)]
    pub dry_run: bool,
}

pub fn import(app: &App, args: ImportArgs) -> Done {
    app.require_owner("tiller import")?;
    let bundle = Bundle::read(&args.bundle)?;
    if args.dry_run {
        let store = app.read()?;
        let plan = store.import_plan(&bundle, Timestamp::now())?;
        let warning =
            (!plan.ok()).then(|| "a count check fails; a real run refuses this bundle".to_owned());
        let help = if plan.already_imported {
            "this manifest is already imported; a real run changes nothing"
        } else {
            "run it again without --dry-run to import"
        };
        return Ok(Outcome::read(plan).with_warning(warning).with_help(help));
    }
    let mut store = app.write()?;
    let applied = store.import_apply(&Ctx::new(Actor::Import), &bundle)?;
    Ok(Outcome::applied(applied).with_help(
        "the memory view lists the placement plan for approval; tiller board shows the cards",
    ))
}
