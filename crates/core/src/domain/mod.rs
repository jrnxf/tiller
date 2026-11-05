//! Domain types: the enumerated values, the lifecycle transitions and one
//! record per table.

pub mod checks;
pub mod enums;
pub mod forge;
pub mod lifecycle;
pub mod records;

pub use checks::CheckPlan;
pub use enums::*;
pub use forge::Forge;
pub use lifecycle::{AttentionStatus, CardStatus, Finish, Illegal, RunStatus};
pub use records::*;
