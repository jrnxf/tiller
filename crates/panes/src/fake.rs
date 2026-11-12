//! An in-memory holder for tests (feature `testing`): [`FakePanes`] keeps
//! panes the way the holder does (one running pane per key, ids never
//! reused, at most [`EXITED_LIMIT`] exited panes kept, at most
//! [`PANE_LIMIT`] in all) and logs every call.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::Timestamp;

use crate::error::{PanesError, Result};
use crate::message::{DEFAULT_SIZE, ErrorCode, Exit, Listing, Pane, PaneId, PaneState, Spawn};
use crate::panes::Panes;

/// The most panes, running and exited, one holder keeps (spec 2.8).
pub const PANE_LIMIT: usize = 128;

/// The most exited panes one holder keeps; the oldest exit goes first
/// (spec 2.8). The fake does not drop exits by age.
pub const EXITED_LIMIT: usize = 64;

/// The first fake pid.
const FIRST_PID: u32 = 1 << 31;

#[derive(Debug)]
struct World {
    instance: String,
    panes: Vec<Pane>,
    next_num: u32,
    next_pid: u32,
    calls: Vec<String>,
    unreachable: bool,
}

/// A stand-in for the holder. Clones share one holder, so a test can drop
/// a "daemon" and give its successor the same panes.
#[derive(Clone, Debug)]
pub struct FakePanes {
    world: Arc<Mutex<World>>,
}

impl Default for FakePanes {
    fn default() -> Self {
        Self::new()
    }
}

impl FakePanes {
    /// An empty holder with a new `instance`.
    pub fn new() -> Self {
        Self {
            world: Arc::new(Mutex::new(World {
                instance: ulid::Ulid::generate().to_string(),
                panes: Vec::new(),
                next_num: 1,
                // Above any real pid (and `i32::MAX`), so code that signals
                // a fake pane's process group can never reach a real one.
                next_pid: FIRST_PID,
                calls: Vec::new(),
                unreachable: false,
            })),
        }
    }

    fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Logs a call, or fails it as a holder that does not answer would.
    fn call(&self, detail: String) -> Result<MutexGuard<'_, World>> {
        let mut world = self.world();
        if world.unreachable {
            return Err(PanesError::Unreachable("the fake holder is down".into()));
        }
        world.calls.push(detail);
        Ok(world)
    }

    /// The holder `instance` every listing names.
    pub fn instance(&self) -> String {
        self.world().instance.clone()
    }

    /// Every call that reached the holder, oldest first, as `<verb> <args>`.
    pub fn calls(&self) -> Vec<String> {
        self.world().calls.clone()
    }

    /// Makes every call fail as `Unreachable` (true) or answer again (false).
    pub fn set_unreachable(&self, unreachable: bool) {
        self.world().unreachable = unreachable;
    }

    /// Ends a running pane's child, as the holder's reaper records it.
    /// Returns false when no running pane has that id.
    pub fn exit(&self, pane: &PaneId, code: Option<i32>, signal: Option<i32>) -> bool {
        let mut world = self.world();
        let Some(record) = world
            .panes
            .iter_mut()
            .find(|p| &p.pane == pane && p.state == PaneState::Running)
        else {
            return false;
        };
        record.state = PaneState::Exited;
        record.exit = Some(Exit {
            code,
            signal,
            at: Timestamp::now(),
        });
        let exited = |p: &Pane| p.state == PaneState::Exited;
        if world.panes.iter().filter(|p| exited(p)).count() > EXITED_LIMIT {
            let oldest = world
                .panes
                .iter()
                .enumerate()
                .filter(|(_, p)| exited(p))
                .min_by_key(|(_, p)| p.exit.map(|exit| exit.at))
                .map(|(at, _)| at);
            if let Some(at) = oldest {
                world.panes.remove(at);
            }
        }
        true
    }
}

impl Panes for FakePanes {
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        let mut world = self.call(format!("spawn {} {}", req.key, req.argv.join(" ")))?;
        let absolute = req
            .argv
            .first()
            .is_some_and(|arg| Path::new(arg).is_absolute());
        if !absolute {
            return Err(PanesError::Refused {
                request: "spawn",
                code: ErrorCode::BadRequest,
                message: "argv[0] must be an absolute path".into(),
            });
        }
        if let Some(running) = world
            .panes
            .iter()
            .find(|p| p.key == req.key && p.state == PaneState::Running)
        {
            return Ok(running.clone());
        }
        if world.panes.len() >= PANE_LIMIT {
            return Err(PanesError::Refused {
                request: "spawn",
                code: ErrorCode::Busy,
                message: format!("the holder keeps at most {PANE_LIMIT} panes"),
            });
        }
        let size = req.size.unwrap_or(DEFAULT_SIZE);
        let pane = Pane {
            pane: PaneId::generate(),
            num: world.next_num,
            key: req.key.clone(),
            pid: world.next_pid,
            argv: req.argv.clone(),
            cwd: req.cwd.clone(),
            started_at: Timestamp::now(),
            state: PaneState::Running,
            exit: None,
            cols: size.cols,
            rows: size.rows,
            meta: req.meta.clone(),
            title: String::new(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 0,
        };
        world.next_num += 1;
        world.next_pid += 1;
        world.panes.push(pane.clone());
        Ok(pane)
    }

    fn list(&self) -> Result<Listing> {
        let world = self.call("list".into())?;
        Ok(Listing {
            instance: world.instance.clone(),
            panes: world.panes.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Meta;
    use std::collections::BTreeMap;

    fn spawn(key: &str) -> Spawn {
        Spawn {
            key: key.into(),
            argv: vec!["/bin/sh".into()],
            cwd: "/home/user".into(),
            env: BTreeMap::new(),
            size: None,
            meta: Meta::from([("role".into(), "worker".into())]),
        }
    }

    #[test]
    fn a_running_key_returns_its_pane_and_an_exited_one_starts_another() {
        let fake = FakePanes::new();
        let first = fake.spawn(&spawn("s1")).unwrap();
        assert_eq!((first.cols, first.rows), (120, 40));
        assert_eq!(fake.spawn(&spawn("s1")).unwrap(), first);
        assert!(fake.exit(&first.pane, Some(0), None));
        let second = fake.spawn(&spawn("s1")).unwrap();
        assert_ne!(second.pane, first.pane);
        assert_ne!(second.num, first.num);
        let listing = fake.list().unwrap();
        assert_eq!(listing.instance, fake.instance());
        assert_eq!(listing.panes.len(), 2);
        assert_eq!(listing.panes[0].state, PaneState::Exited);
        assert_eq!(listing.panes[0].exit.map(|e| e.code), Some(Some(0)));
        assert_eq!(listing.panes[1].state, PaneState::Running);
    }

    #[test]
    fn a_clone_is_the_same_holder() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap();
        let after_restart = fake.clone();
        let listing = after_restart.list().unwrap();
        assert_eq!(listing.instance, fake.instance());
        assert_eq!(listing.panes, vec![pane]);
        assert_eq!(fake.calls(), ["spawn s1 /bin/sh", "list"]);
    }

    #[test]
    fn a_relative_argv0_a_full_holder_and_a_down_holder_are_refused() {
        let fake = FakePanes::new();
        let mut req = spawn("s1");
        req.argv = vec!["sh".into()];
        assert!(matches!(
            fake.spawn(&req),
            Err(PanesError::Refused {
                code: ErrorCode::BadRequest,
                ..
            })
        ));
        fake.set_unreachable(true);
        assert!(matches!(fake.list(), Err(PanesError::Unreachable(_))));
        fake.set_unreachable(false);
        assert!(fake.list().unwrap().panes.is_empty());
        for n in 0..PANE_LIMIT {
            fake.spawn(&spawn(&format!("k{n}"))).unwrap();
        }
        assert!(matches!(
            fake.spawn(&spawn("one-more")),
            Err(PanesError::Refused {
                code: ErrorCode::Busy,
                ..
            })
        ));
        // A running key still returns its pane at the limit.
        assert!(fake.spawn(&spawn("k0")).is_ok());
        assert_eq!(fake.list().unwrap().panes.len(), PANE_LIMIT);
    }

    #[test]
    fn past_the_exited_limit_the_oldest_exit_is_dropped() {
        let fake = FakePanes::new();
        let panes: Vec<Pane> = (0..=EXITED_LIMIT)
            .map(|n| fake.spawn(&spawn(&format!("k{n}"))).unwrap())
            .collect();
        for pane in &panes {
            assert!(fake.exit(&pane.pane, Some(0), None));
        }
        let kept = fake.list().unwrap().panes;
        assert_eq!(kept.len(), EXITED_LIMIT);
        assert!(kept.iter().all(|p| p.pane != panes[0].pane));
        assert!(fake.spawn(&spawn("again")).is_ok());
    }
}
