//! `tiller import <bundle>` for `tiller-import/v1` (T2.4).
//!
//! The bundle is what Firstmate's export writes (`docs/firstmate-export.md`).
//! [`Bundle::read`] verifies it against its manifest, [`plan`] works out every
//! row a real run would write, and [`Store::import_apply`](crate::Store::import_apply)
//! writes exactly that plan in one transaction (`state.md`, `import.apply`).
//! Memory becomes pending notes for the owner to keep in kb or discard
//! (`memory.md` section 4).

mod apply;
pub mod bundle;
pub mod plan;

#[cfg(test)]
mod tests;

pub use bundle::Bundle;
pub use plan::{Action, Placement, Plan, Wait, plan};
