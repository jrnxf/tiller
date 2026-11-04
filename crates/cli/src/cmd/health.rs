//! `tiller health ...` (T7.1): the Health numbers as one shape, for the T7.4
//! report. `numbers` is Tiller's own; `baseline` is the same figures computed
//! the same way over a Firstmate export. The baseline never reaches the
//! Health page.

use std::path::PathBuf;

use clap::Subcommand;
use tiller_core::Timestamp;
use tiller_core::health::HealthNumbers;
use tiller_core::import::Bundle;
use tiller_core::state::signals::lisbon_weeks;

use crate::app::{App, Done, Outcome};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Tiller's own numbers over the last Lisbon weeks, with sample sizes
    Numbers {
        /// How many weeks, the one in progress included
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=52))]
        weeks: u8,
    },
    /// The same numbers over Firstmate's history, from an import bundle
    Baseline {
        /// A tiller-import/v1 bundle directory with a baseline/ folder
        bundle: PathBuf,
    },
}

pub fn run(app: &App, cmd: Cmd) -> Done {
    match cmd {
        Cmd::Numbers { weeks } => {
            let store = app.read()?;
            let now = Timestamp::now();
            let signals = store.signals(&lisbon_weeks(now, usize::from(weeks)), now)?;
            Ok(Outcome::read(HealthNumbers::from_signals(&signals)))
        }
        Cmd::Baseline { bundle } => {
            let bundle = Bundle::read(bundle)?;
            Ok(Outcome::read(HealthNumbers::from_baseline(&bundle)?)
                .with_help("for the T7.4 report only; the Health page shows Tiller's own numbers"))
        }
    }
}
