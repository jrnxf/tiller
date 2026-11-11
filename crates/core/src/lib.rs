//! Tiller's state core (T2.1): the `state.db` schema and migrations, ids,
//! domain types, and every mutation in `docs/spec/state.md` section 8 as one
//! function. The CLI, the hooks, the daemon and the board API all call these
//! functions; none of them writes SQL of its own (`protocol.md` section 1).

/// The command that hands the host to Tiller, as a literal, so that
/// [`owner::FLIP_BACK`], [`INIT_HINT`] and [`RUN_INIT`] are built from one
/// text with `concat!`. Defined before the modules, so `owner` sees it.
macro_rules! flip_back {
    () => {
        "tiller owner set tiller"
    };
}

pub mod authority;
pub mod backup;
pub mod config_file;
pub mod domain;
pub mod error;
#[cfg(test)]
mod events;
pub mod health;
pub mod host_config;
pub mod ids;
pub mod import;
pub mod limits;
pub mod memory;
pub mod migrate;
pub mod owner;
pub mod quota_rule;
pub mod state;
pub mod store;
pub mod time;

/// The commands that make `state.db`, one per line, in the order to run them,
/// named in every answer before it exists. `tiller init` refuses while the
/// owner marker names another system, so the owner hands the host to Tiller
/// first (`tiller owner set tiller` is a no-op when the marker already says so).
/// The board's setup screen shows each line as its own command.
pub const INIT_HINT: &str = concat!(
    flip_back!(),
    "\ntiller init --host-label <label> --lane-capacity <n>"
);

/// The same two steps as one clause, for the end of a refusal before
/// `tiller init`: "state.db is not initialized; run ...".
pub const RUN_INIT: &str = concat!("run ", flip_back!(), ", then tiller init");

/// The refusal every read and write gives before `tiller init`.
pub fn not_initialized() -> String {
    format!("state.db is not initialized; {RUN_INIT}")
}

pub use error::{Error, Result};
pub use store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
pub use time::Timestamp;
