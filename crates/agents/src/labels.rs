//! The label a holder pane carries in `meta.label` (`terminal-holder.md`
//! 4.4): what the switcher, `tiller attach` and the board show for it. It
//! has no tree glyph, since the holder keeps no sidebar order.

use tiller_core::domain::RunRole;

/// An orchestrator's pane: the project id.
pub fn orchestrator_label(project: &str) -> String {
    project.to_owned()
}

/// A worker's pane: the card and the role, as `alpha-42 build`.
pub fn worker_label(display_id: &str, role: RunRole) -> String {
    format!("{display_id} {}", role.short_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_project_or_the_card_and_role() {
        assert_eq!(orchestrator_label("alpha"), "alpha");
        assert_eq!(worker_label("alpha-42", RunRole::Builder), "alpha-42 build");
    }
}
