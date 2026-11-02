//! Identity (`state.md` section 2): ULID primary keys, card display ids and
//! project slugs.

use std::sync::Mutex;

use crate::error::{Error, Result};

/// The length of a ULID's short form: its last 7 characters, from the random
/// part, since a prefix is shared by every id minted in the same minutes
/// (`protocol.md` section 6.1).
pub const SHORT_LEN: usize = 7;

/// The project id reserved on every host for host-level work.
pub const FLEET_PROJECT: &str = "fleet";

/// The display id of the host health card `tiller init` creates.
pub const HOST_HEALTH_CARD: &str = "fleet-1";

static GENERATOR: Mutex<Option<ulid::Generator>> = Mutex::new(None);

/// Mints a new ULID. Ids minted by one process in the same millisecond still
/// sort in minting order.
pub fn new_ulid() -> String {
    let mut guard = GENERATOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generator = guard.get_or_insert_with(ulid::Generator::new);
    // The generator fails only when the random part overflows within one
    // millisecond (2^80 ids); a fresh non-monotonic id is still unique.
    generator
        .generate()
        .unwrap_or_else(|_| ulid::Ulid::generate())
        .to_string()
}

/// True when `text` is a full ULID in canonical form.
pub fn is_ulid(text: &str) -> bool {
    text.len() == ulid::ULID_LEN && ulid::Ulid::from_string(text).is_ok()
}

/// The short form shown beside every ULID.
pub fn short(id: &str) -> &str {
    id.get(id.len().saturating_sub(SHORT_LEN)..).unwrap_or(id)
}

/// True when `text` could be a ULID short form.
pub fn is_short(text: &str) -> bool {
    text.len() == SHORT_LEN && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Checks a project id: `^[a-z][a-z0-9]{1,15}$` (`state.md` section 2).
pub fn validate_project_id(id: &str) -> Result<()> {
    let mut bytes = id.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase());
    let rest_ok = bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if first_ok && rest_ok && (2..=16).contains(&id.len()) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "project id {id:?} must be 2 to 16 characters: a lowercase letter, then lowercase letters or digits"
        )))
    }
}

/// Formats a card display id, `<project>-<n>`.
pub fn display_id(project: &str, seq: i64) -> String {
    format!("{project}-{seq}")
}

/// Splits a display id into its project and sequence number, when it is one.
pub fn parse_display_id(text: &str) -> Option<(&str, i64)> {
    let (project, seq) = text.rsplit_once('-')?;
    validate_project_id(project).ok()?;
    let n: i64 = seq.parse().ok()?;
    (n >= 1 && !seq.starts_with('0')).then_some((project, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulids_sort_in_minting_order() {
        let ids: Vec<String> = (0..1000).map(|_| new_ulid()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        assert!(ids.iter().all(|id| is_ulid(id)));
    }

    #[test]
    fn display_ids_parse_only_canonical_forms() {
        assert_eq!(parse_display_id("alpha-42"), Some(("alpha", 42)));
        assert_eq!(
            parse_display_id(&display_id("fleet", 1)),
            Some(("fleet", 1))
        );
        for bad in [
            "alpha-0",
            "alpha-042",
            "Alpha-1",
            "a-1",
            "alpha-",
            "alpha",
            "alpha-x",
        ] {
            assert_eq!(parse_display_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn short_form_is_the_random_tail() {
        let id = new_ulid();
        assert_eq!(short(&id), &id[19..]);
        assert!(is_short(short(&id)));
    }
}
