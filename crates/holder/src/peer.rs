//! The peer check (`terminal-holder.md` 2.3).
//!
//! On every accept the holder reads the peer's uid from the kernel
//! (`SO_PEERCRED` on Linux, `getpeereid` and `LOCAL_PEERPID` on macOS,
//! through tokio). A uid that is not the holder's own gets no reply, and the
//! socket is closed.

use std::fmt;

/// Who is on the other end of a connection, as the kernel says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    pub uid: u32,
    /// The peer's pid, where the system gives it.
    pub pid: Option<i32>,
}

impl PeerCred {
    /// The credential of the peer on `stream`.
    pub fn of(stream: &tokio::net::UnixStream) -> std::io::Result<Self> {
        let cred = stream.peer_cred()?;
        Ok(Self {
            uid: cred.uid(),
            pid: cred.pid(),
        })
    }
}

impl fmt::Display for PeerCred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pid {
            Some(pid) => write!(f, "uid {} (pid {pid})", self.uid),
            None => write!(f, "uid {} (pid unknown)", self.uid),
        }
    }
}

/// A connection the holder turns away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a connection from {peer}, not the holder's own uid {own}")]
pub struct Refusal {
    pub peer: PeerCred,
    pub own: u32,
}

/// Whether the holder, running as `own`, serves `peer`.
pub fn admit(peer: PeerCred, own: u32) -> Result<(), Refusal> {
    if peer.uid == own {
        Ok(())
    } else {
        Err(Refusal { peer, own })
    }
}

/// The uid the holder runs as.
pub fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_holders_own_uid_is_served() {
        let peer = PeerCred {
            uid: 1000,
            pid: Some(42),
        };
        assert_eq!(admit(peer, 1000), Ok(()));
    }

    // The test host has no second uid to connect from, so the check is
    // tested against crafted credentials.
    #[test]
    fn any_other_uid_is_refused_root_included() {
        for uid in [0, 999, 1001, u32::MAX] {
            let peer = PeerCred { uid, pid: Some(7) };
            let refusal = admit(peer, 1000).unwrap_err();
            assert_eq!(refusal, Refusal { peer, own: 1000 });
            assert_eq!(
                refusal.to_string(),
                format!("a connection from uid {uid} (pid 7), not the holder's own uid 1000")
            );
        }
        let refusal = admit(PeerCred { uid: 0, pid: None }, 1000).unwrap_err();
        assert!(refusal.to_string().contains("pid unknown"), "{refusal}");
    }

    #[tokio::test]
    async fn a_real_connection_carries_this_processs_uid_and_pid() {
        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let peer = PeerCred::of(&a).unwrap();
        assert_eq!(peer.uid, own_uid());
        assert_eq!(peer.pid, Some(std::process::id() as i32));
        assert_eq!(admit(peer, own_uid()), Ok(()));
    }
}
