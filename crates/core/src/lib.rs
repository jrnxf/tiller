//! Tiller's state core (T2.1): the `state.db` schema and migrations, ids,
//! domain types, and every mutation in `docs/spec/state.md` section 8 as one
//! function. The CLI, the hooks, the daemon and the board API all call these
//! functions; none of them writes SQL of its own (`protocol.md` section 1).

pub mod authority;
pub mod backup;
pub mod config_file;
pub mod domain;
pub mod error;
#[cfg(test)]
mod events;
pub mod health;
pub mod ids;
pub mod import;
pub mod limits;
pub mod memory;
pub mod migrate;
pub mod quota_rule;
pub mod state;
pub mod store;
pub mod time;

pub use error::{Error, Result};
pub use store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
pub use time::Timestamp;
