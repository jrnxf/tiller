//! A pane's size leases (`terminal-holder.md` 2.6).
//!
//! A PTY has one size, and many clients may want different ones. Each
//! pane keeps a stack of leases: the top one sets the size. A lease
//! belongs to the connection that took it, under the client's own id, and
//! ends with a `release` or with that connection. When none is left, the
//! size stays as it is, so the app does not redraw for nothing.

use tiller_panes::Size;

/// The most leases one pane holds (section 2.8). One more is `busy`.
pub const LEASES_PER_PANE: usize = 64;

/// One lease: whose it is, its id and the size it asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub conn: u64,
    pub id: String,
    pub size: Size,
}

/// A pane's leases, the top one last.
#[derive(Clone, Debug, Default)]
pub struct Leases(Vec<Lease>);

/// The pane holds [`LEASES_PER_PANE`] leases already.
#[derive(Debug, PartialEq, Eq)]
pub struct Full;

impl Leases {
    /// Puts the lease `id` of `conn` on top with `size`: a new one, or one
    /// the connection holds already, moved up and changed.
    pub fn take(&mut self, conn: u64, id: &str, size: Size) -> Result<(), Full> {
        let held = self.position(conn, id);
        if held.is_none() && self.0.len() >= LEASES_PER_PANE {
            return Err(Full);
        }
        if let Some(at) = held {
            self.0.remove(at);
        }
        self.0.push(Lease {
            conn,
            id: id.to_owned(),
            size,
        });
        Ok(())
    }

    /// Drops the lease `id` of `conn`. False when it holds none by that id.
    pub fn release(&mut self, conn: u64, id: &str) -> bool {
        match self.position(conn, id) {
            Some(at) => {
                self.0.remove(at);
                true
            }
            None => false,
        }
    }

    /// Drops every lease of `conn`, whose connection ended. False when it
    /// held none.
    pub fn drop_conn(&mut self, conn: u64) -> bool {
        let before = self.0.len();
        self.0.retain(|lease| lease.conn != conn);
        self.0.len() != before
    }

    /// The lease that sets the size, if any is left.
    pub fn top(&self) -> Option<&Lease> {
        self.0.last()
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.0.len()
    }

    fn position(&self, conn: u64, id: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|lease| lease.conn == conn && lease.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16) -> Size {
        Size { cols, rows: 24 }
    }

    fn top(leases: &Leases) -> Option<(u64, &str, u16)> {
        leases
            .top()
            .map(|lease| (lease.conn, lease.id.as_str(), lease.size.cols))
    }

    #[test]
    fn the_top_lease_sets_the_size_and_a_release_gives_it_back() {
        let mut leases = Leases::default();
        assert_eq!(top(&leases), None);
        leases.take(1, "attach", size(80)).unwrap();
        leases.take(2, "fit", size(100)).unwrap();
        assert_eq!(top(&leases), Some((2, "fit", 100)));
        // The same id on another connection is another lease.
        leases.take(1, "fit", size(90)).unwrap();
        assert_eq!(top(&leases), Some((1, "fit", 90)));
        assert!(leases.release(1, "fit"));
        assert!(!leases.release(1, "fit"));
        assert_eq!(top(&leases), Some((2, "fit", 100)));
        // A held lease taken again moves to the top with its new size.
        leases.take(1, "attach", size(70)).unwrap();
        assert_eq!(top(&leases), Some((1, "attach", 70)));
        assert_eq!(leases.count(), 2);
        assert!(leases.drop_conn(1));
        assert!(!leases.drop_conn(1));
        assert_eq!(top(&leases), Some((2, "fit", 100)));
        assert!(leases.drop_conn(2));
        assert_eq!(top(&leases), None);
    }

    #[test]
    fn a_pane_holds_at_most_its_limit_of_leases() {
        let mut leases = Leases::default();
        for n in 0..LEASES_PER_PANE {
            leases.take(1, &n.to_string(), size(80)).unwrap();
        }
        assert_eq!(leases.take(1, "one more", size(80)), Err(Full));
        // One held already still moves.
        leases.take(1, "0", size(81)).unwrap();
        assert_eq!(top(&leases), Some((1, "0", 81)));
    }
}
