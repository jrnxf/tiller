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

/// True when `pane` is a terminal holder pane id: `p` and a ULID
/// (`terminal-holder.md` 2.4). Any other pane id was stored before the
/// cutover and names no pane the holder has.
pub fn is_holder_pane(pane: &str) -> bool {
    pane.strip_prefix('p').is_some_and(is_ulid)
}

/// The short form shown beside every ULID.
pub fn short(id: &str) -> &str {
    id.get(id.len().saturating_sub(SHORT_LEN)..).unwrap_or(id)
}

/// True when `text` could be a ULID short form.
pub fn is_short(text: &str) -> bool {
    text.len() == SHORT_LEN && text.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The longest project id: room for a repo-style name such as
/// `acme-web2-staging` (`state.md` section 2).
pub const PROJECT_ID_MAX: usize = 32;

/// Checks a project id (`state.md` section 2): 2 to [`PROJECT_ID_MAX`]
/// characters, one or more segments joined by single hyphens, each a
/// lowercase letter followed by lowercase letters or digits, so
/// `^[a-z][a-z0-9]*(-[a-z][a-z0-9]*)*$`. No segment starts with a digit, so in
/// a card id (`alpha-42`) or a run's agent name (`alpha-42-build-2`) the first
/// segment that starts with a digit is always the card number.
pub fn validate_project_id(id: &str) -> Result<()> {
    let segments_ok = id.split('-').all(|segment| {
        let mut bytes = segment.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    if segments_ok && (2..=PROJECT_ID_MAX).contains(&id.len()) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "project id {id:?} must be 2 to {PROJECT_ID_MAX} characters: words of lowercase letters and digits joined by single hyphens, each word starting with a letter (e.g. alpha or my-app2)"
        )))
    }
}

/// Formats a card display id, `<project>-<n>`.
pub fn display_id(project: &str, seq: i64) -> String {
    format!("{project}-{seq}")
}

/// Splits a display id into its project and sequence number, when it is one.
/// The split is at the last hyphen: a project id may hold hyphens, but never
/// a segment that starts with a digit.
pub fn parse_display_id(text: &str) -> Option<(&str, i64)> {
    let (project, seq) = text.rsplit_once('-')?;
    validate_project_id(project).ok()?;
    if seq.starts_with('0') || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = seq.parse().ok()?;
    (n >= 1).then_some((project, n))
}

#[cfg(test)]
mod tests {
    use proptest::strategy::Strategy as _;

    use super::*;

    #[test]
    fn a_holder_pane_id_is_p_and_a_ulid() {
        assert!(is_holder_pane(&format!("p{}", new_ulid())));
        for other in ["w2:p1", "p1", "p", "", "w1:p01ARZ3NDEKTSV4RRFFQ69G5FAV"] {
            assert!(!is_holder_pane(other), "{other}");
        }
        assert!(!is_holder_pane(&new_ulid()));
    }

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
            parse_display_id("acme-web2-staging-1"),
            Some(("acme-web2-staging", 1))
        );
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
            "alpha--1",
            "alpha-2-3",
            "alpha-42-build",
            "-alpha-1",
            "alpha-+1",
        ] {
            assert_eq!(parse_display_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn project_ids_allow_single_hyphens_between_letter_led_words() {
        for good in [
            "ab",
            "web2staging",
            "web2-staging",
            "acme-web2-staging",
            "a-b",
            "fleet",
            "abcdefghijklmnopqrstuvwxyz012345",
        ] {
            assert!(validate_project_id(good).is_ok(), "{good}");
        }
        for bad in [
            "a",
            "",
            "-ab",
            "ab-",
            "a--b",
            "ab-12",
            "ab-1c",
            "5ab",
            "Ab",
            "a_b",
            "a.b",
            "a b",
            "abcdefghijklmnopqrstuvwxyz0123456",
        ] {
            let error = validate_project_id(bad).unwrap_err().to_string();
            assert!(error.contains("joined by single hyphens"), "{bad}: {error}");
        }
    }

    fn project_id() -> impl proptest::strategy::Strategy<Value = String> {
        proptest::string::string_regex("[a-z][a-z0-9]{0,7}(-[a-z][a-z0-9]{0,7}){0,3}")
            .unwrap()
            .prop_filter("2 to 32 characters", |id| {
                (2..=PROJECT_ID_MAX).contains(&id.len())
            })
    }

    proptest::proptest! {
        #[test]
        fn display_ids_round_trip(project in project_id(), seq in 1..=i64::MAX) {
            proptest::prop_assert!(validate_project_id(&project).is_ok());
            let display = display_id(&project, seq);
            proptest::prop_assert_eq!(parse_display_id(&display), Some((project.as_str(), seq)));
            // the card id itself is never a project id
            proptest::prop_assert!(validate_project_id(&display).is_err());
        }
    }

    #[test]
    fn short_form_is_the_random_tail() {
        let id = new_ulid();
        assert_eq!(short(&id), &id[19..]);
        assert!(is_short(short(&id)));
    }
}
