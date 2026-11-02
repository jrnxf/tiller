//! `tiller import <bundle>` for `tiller-import/v1` (T2.4).
//!
//! The bundle is what Firstmate's export writes (`docs/firstmate-export.md`).
//! [`Bundle::read`] verifies it against its manifest, [`plan`] works out every
//! row a real run would write, and [`Store::import_apply`](crate::Store::import_apply)
//! writes exactly that plan in one transaction (`state.md`, `import.apply`).
//! Memory becomes a placement plan for the owner to approve, never a write
//! to a canonical file (`memory.md` section 10).

mod apply;
pub mod bundle;
pub mod plan;

#[cfg(test)]
mod tests;

pub use bundle::Bundle;
pub use plan::{Action, Placement, Plan, Wait, plan};
