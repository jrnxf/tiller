//! The nonces of signed openings this peer took (`protocol.md` 10.2, 10.7):
//! a `hello`'s `nonce` and a pane request's `id`. A captured opening sent
//! again inside its skew window is refused as `replayed`.
//!
//! Each `peer serve` and `peer pane` is a process of its own, started by
//! sshd, so the set lives in a file under the home, read and written under
//! a lock. An entry is kept until its `ts` plus the skew, after which the
//! freshness check refuses the opening anyway, and the file holds at most
//! [`KEEP`] entries: past that the entry that expires first goes.

use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

use tiller_core::Timestamp;

use super::serve::{Denied, refused};

/// The most nonces the file holds.
pub(super) const KEEP: usize = 4096;

const FILE: &str = "peer-nonces";

/// Takes `nonce`, from an opening whose `ts` passed the skew check: `Ok`
/// the first time, `replayed` after that. A file that cannot be read or
/// written is `Failed`, so the link ends and the board host backs off.
pub(super) fn take(
    home: &Path,
    nonce: &str,
    ts: &str,
    skew: Duration,
    now: Timestamp,
) -> Result<(), Denied> {
    let skew = i64::try_from(skew.as_millis()).unwrap_or(i64::MAX);
    let sent = ts
        .parse::<Timestamp>()
        .map_or(now.as_millis(), Timestamp::as_millis);
    let until = sent.saturating_add(skew);
    match first_sight(home, nonce, until, now.as_millis()) {
        Ok(true) => Ok(()),
        Ok(false) => refused(
            "replayed",
            format!("the opening with nonce {nonce} was already taken"),
        ),
        Err(error) => Err(Denied::Failed(tiller_core::Error::Unavailable(format!(
            "the peer nonce file: {error}"
        )))),
    }
}

/// True when `nonce` was not in the file, which now holds it until
/// `until` (ms).
fn first_sight(home: &Path, nonce: &str, until: i64, now: i64) -> std::io::Result<bool> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join(format!("{FILE}.lock")))?;
    lock.lock()?;
    let path = home.join(FILE);
    let held = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    // A file that does not parse starts over: the skew check still bounds
    // what could be replayed, and only until the next opening.
    let mut seen: Vec<(i64, String)> = serde_json::from_str(&held).unwrap_or_default();
    seen.retain(|(kept_until, _)| *kept_until >= now);
    if seen.iter().any(|(_, taken)| taken == nonce) {
        return Ok(false);
    }
    seen.push((until, nonce.to_owned()));
    if seen.len() > KEEP {
        seen.sort_by_key(|(kept_until, _)| *kept_until);
        let over = seen.len() - KEEP;
        seen.drain(..over);
    }
    tiller_core::config_file::write_atomic(&path, &serde_json::to_string(&seen)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn a_nonce_is_taken_once_until_it_expires() {
        let home = tempfile::tempdir().unwrap();
        assert!(first_sight(home.path(), "n-1", 2 * MIN, 0).unwrap());
        assert!(!first_sight(home.path(), "n-1", 2 * MIN, MIN).unwrap());
        assert!(first_sight(home.path(), "n-2", 2 * MIN, MIN).unwrap());
        // Past its `ts` plus the skew the entry is pruned; the skew check
        // refuses such an opening before it gets here.
        assert!(first_sight(home.path(), "n-1", 4 * MIN, 3 * MIN).unwrap());
        let text = std::fs::read_to_string(home.path().join(FILE)).unwrap();
        let seen: Vec<(i64, String)> = serde_json::from_str(&text).unwrap();
        assert_eq!(seen, vec![(4 * MIN, "n-1".to_owned())]);
    }

    #[test]
    fn the_file_holds_at_most_its_cap() {
        let home = tempfile::tempdir().unwrap();
        let full: Vec<(i64, String)> = (0..KEEP)
            .map(|n| (MIN + i64::try_from(n).unwrap(), format!("n-{n}")))
            .collect();
        std::fs::write(
            home.path().join(FILE),
            serde_json::to_string(&full).unwrap(),
        )
        .unwrap();
        let until = MIN + i64::try_from(KEEP).unwrap();
        assert!(first_sight(home.path(), &format!("n-{KEEP}"), until, 0).unwrap());
        let text = std::fs::read_to_string(home.path().join(FILE)).unwrap();
        let seen: Vec<(i64, String)> = serde_json::from_str(&text).unwrap();
        assert_eq!(seen.len(), KEEP);
        // The entry that expires first went.
        assert!(!seen.iter().any(|(_, nonce)| nonce == "n-0"));
        assert!(!first_sight(home.path(), &format!("n-{KEEP}"), MIN, 0).unwrap());
    }

    #[test]
    fn a_torn_file_starts_over() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(FILE), "[[1, \"n-").unwrap();
        assert!(first_sight(home.path(), "n-1", MIN, 0).unwrap());
        assert!(!first_sight(home.path(), "n-1", MIN, 0).unwrap());
    }
}
