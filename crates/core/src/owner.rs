//! The owner marker (`state.md` Host, `authority.md` G26, `recovery.md`
//! R20): `<home>/owner` names the system that owns the host. While it names
//! another system, every mutation is refused and reads keep working.
//!
//! The marker is read again on every check, because the owner can flip it
//! while a daemon runs. A missing or empty marker names no other system. A
//! marker that cannot be read is refused, since it may name one.

use std::io::ErrorKind;
use std::path::Path;

use crate::{Error, Result};

/// `<home>/owner`, beside `state.db`.
pub const MARKER: &str = "owner";

/// The name the marker holds when Tiller owns the host.
pub const TILLER: &str = "tiller";

/// The command that hands the host back to Tiller.
pub const FLIP_BACK: &str = flip_back!();

/// What the marker in a Tiller home says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    /// Missing, empty or `tiller`: Tiller's writes go ahead.
    Tiller,
    /// The other system the marker names.
    Other(String),
    /// The marker could not be read, so it may name another system.
    Unreadable(String),
}

/// Reads the marker in `home`. [`check`] is the gate for a write; a caller
/// that must treat an unreadable marker apart from a foreign one, such as a
/// hook that spools it, reads it here.
pub fn read(home: &Path) -> Marker {
    match std::fs::read_to_string(home.join(MARKER)) {
        Ok(text) => match text.trim() {
            "" | TILLER => Marker::Tiller,
            other => Marker::Other(other.to_owned()),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => Marker::Tiller,
        Err(error) => Marker::Unreadable(error.to_string()),
    }
}

/// [`Error::OwnerMarker`] while the marker in `home` names another system,
/// or cannot be read.
pub fn check(home: &Path) -> Result<()> {
    match read(home) {
        Marker::Tiller => Ok(()),
        Marker::Other(other) => Err(Error::OwnerMarker(format!(
            "the owner marker names {other:?}, not \"{TILLER}\""
        ))),
        Marker::Unreadable(error) => Err(Error::OwnerMarker(format!(
            "the owner marker could not be read ({error}), so it may name another system"
        ))),
    }
}

/// The name the marker in `home` holds, trimmed; `None` when it is
/// missing, empty or cannot be read. For showing the marker: [`check`]
/// decides whether a write may go ahead.
pub fn name(home: &Path) -> Option<String> {
    std::fs::read_to_string(home.join(MARKER))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::domain::{Boundary, CardKind, Delivery};
    use crate::state::cards::NewCard;
    use crate::state::projects::{Init, NewProject};
    use crate::store::{Actor, Ctx, Store};

    fn owner() -> Ctx {
        Ctx::new(Actor::Owner)
    }

    fn card(store: &mut Store, actor: Actor) -> Result<()> {
        store
            .card_create(
                &Ctx::new(actor),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: "under the marker".into(),
                    ..NewCard::default()
                },
                None,
            )
            .map(|_| ())
    }

    fn count(store: &Store) -> i64 {
        store
            .conn()
            .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }

    /// G26, R20: while `<home>/owner` names another system, no actor's
    /// mutation through the store writes anything, a rehearsal is refused
    /// too, and reads keep working. The marker is read on every write, so a
    /// flip back while the store stays open lets the next write through.
    #[test]
    fn mutations_refused_when_marker_names_other_system() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut store = Store::open(home.join("state.db")).unwrap();
        store
            .init(
                &owner(),
                Init {
                    host_label: "test-host".into(),
                    lane_capacity: 2,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap();
        store
            .project_add(
                &owner(),
                NewProject {
                    id: "alpha".into(),
                    name: "Alpha".into(),
                    repo: Some("example/alpha".into()),
                    remote: Some("https://example.com/alpha.git".into()),
                    default_branch: None,
                    delivery: Delivery::DirectPr,
                    checks: None,
                    capacity: Some(2),
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
            )
            .unwrap();
        let events = count(&store);

        std::fs::write(home.join(MARKER), "firstmate\n").unwrap();
        for actor in [Actor::Owner, Actor::Board, Actor::Daemon] {
            let refused = card(&mut store, actor.clone()).unwrap_err();
            assert!(
                matches!(refused, Error::OwnerMarker(_)),
                "{actor}: {refused:?}"
            );
            assert_eq!(refused.code(), "owner_marker");
            assert_eq!(refused.exit_code(), 6);
            assert_eq!(
                refused.to_string(),
                "read only: the owner marker names \"firstmate\", not \"tiller\""
            );
        }
        let rehearsed = store.rehearse(&owner(), "card.create", |_| Ok(()));
        assert!(
            matches!(rehearsed, Err(Error::OwnerMarker(_))),
            "{rehearsed:?}"
        );
        assert_eq!(count(&store), events, "a refused mutation writes nothing");
        assert!(
            store.projects().unwrap().iter().any(|p| p.id == "alpha"),
            "reads keep working"
        );

        // A fresh store sees the same marker.
        let mut other = Store::open(home.join("state.db")).unwrap();
        assert!(matches!(
            card(&mut other, Actor::Owner),
            Err(Error::OwnerMarker(_))
        ));

        std::fs::write(home.join(MARKER), "tiller\n").unwrap();
        card(&mut store, Actor::Owner).unwrap();
        std::fs::write(home.join(MARKER), "").unwrap();
        card(&mut store, Actor::Owner).unwrap();
        std::fs::remove_file(home.join(MARKER)).unwrap();
        card(&mut store, Actor::Owner).unwrap();
        assert!(count(&store) > events);
    }

    /// A marker that cannot be read may name another system, so it refuses
    /// (as a peer's forwarded mutation does, G26).
    #[test]
    fn an_unreadable_marker_refuses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(MARKER)).unwrap();
        let refused = check(dir.path()).unwrap_err();
        assert!(matches!(refused, Error::OwnerMarker(_)), "{refused:?}");
        assert!(
            refused.to_string().contains("could not be read"),
            "{refused}"
        );
    }

    /// An in-memory store has no home, so no marker applies to it.
    #[test]
    fn an_in_memory_store_has_no_marker() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(store.rehearse(&owner(), "card.create", |_| Ok(())).is_ok());
    }

    #[test]
    fn the_init_hints_name_the_flip_back_command() {
        assert_eq!(crate::INIT_HINT.lines().next(), Some(FLIP_BACK));
        assert_eq!(
            crate::RUN_INIT,
            format!("run {FLIP_BACK}, then tiller init")
        );
    }
}
