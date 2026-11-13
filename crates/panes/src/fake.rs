//! An in-memory holder for tests (feature `testing`): [`FakePanes`] keeps
//! panes the way the holder does (one running pane per key, ids never
//! reused, at most [`EXITED_LIMIT`] exited panes kept, at most
//! [`PANE_LIMIT`] in all, a stack of size leases per pane) and logs every
//! call. It implements [`Panes`] and [`Live`]: subscriptions and watches
//! are channels, and a test drives them with [`FakePanes::output`],
//! [`FakePanes::exit`], [`FakePanes::emit`] and [`FakePanes::lag`].

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;

use crate::error::{PanesError, Result};
use crate::keys::Key;
use crate::live::{Control, EventFeed, Events, Feed, Subscribe, Subscription, Update};
use crate::message::{
    Cursor, DEFAULT_SIZE, ErrorCode, Event, Exit, Format, Listing, MAX_RECENT, Meta, Pane, PaneId,
    PaneState, Read, Screen, Signal, Spawn,
};
use crate::panes::{Live, Panes};

/// The most panes, running and exited, one holder keeps (spec 2.8).
pub const PANE_LIMIT: usize = 128;

/// The most exited panes one holder keeps; the oldest exit goes first
/// (spec 2.8). The fake does not drop exits by age.
pub const EXITED_LIMIT: usize = 64;

/// The first fake pid.
const FIRST_PID: u32 = 1 << 31;

/// The signal number `close` reports, as a child that `hup` ended.
const SIGHUP: i32 = 1;
/// The signal number `signal(kill)` reports.
const SIGKILL: i32 = 9;

/// What a subscription's or a watch's channel carries.
enum Msg<T> {
    Item(T),
    /// The fake went down: the stream fails as a dead socket would.
    Fail,
    /// This side ended the stream.
    End,
}

struct Subscriber {
    pane: PaneId,
    stream: u32,
    tx: Sender<Msg<Update>>,
}

#[derive(Debug)]
struct World {
    instance: String,
    panes: Vec<Pane>,
    next_num: u32,
    next_pid: u32,
    next_stream: u32,
    calls: Vec<String>,
    unreachable: bool,
    /// What each pane's screen shows, top row first.
    screens: HashMap<PaneId, Vec<String>>,
    /// The exited panes, oldest exit first, whatever their clock says.
    exits: VecDeque<PaneId>,
    /// Each pane's size leases, the top one last.
    leases: HashMap<PaneId, Vec<(String, u16, u16)>>,
    attach_clients: usize,
    subscribers: Vec<Subscriber>,
    watchers: Vec<(u32, Sender<Msg<Event>>)>,
}

impl std::fmt::Debug for Subscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscriber")
            .field("pane", &self.pane)
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

impl World {
    fn pane(&self, pane: &PaneId) -> Result<&Pane> {
        self.panes
            .iter()
            .find(|p| &p.pane == pane)
            .ok_or_else(|| PanesError::NotFound(pane.to_string()))
    }

    fn pane_mut(&mut self, pane: &PaneId) -> Result<&mut Pane> {
        self.panes
            .iter_mut()
            .find(|p| &p.pane == pane)
            .ok_or_else(|| PanesError::NotFound(pane.to_string()))
    }

    /// The pane, which must be running.
    fn running(&self, pane: &PaneId) -> Result<&Pane> {
        let record = self.pane(pane)?;
        match record.state {
            PaneState::Running => Ok(record),
            PaneState::Exited => Err(PanesError::Exited(pane.to_string())),
        }
    }

    /// Sends `update` to every subscriber of `pane`, and drops the ones
    /// that are gone.
    fn tell(&mut self, pane: &PaneId, update: &Update) {
        self.subscribers
            .retain(|slot| slot.pane != *pane || slot.tx.send(Msg::Item(update.clone())).is_ok());
    }

    /// Sends `event` to every watch, and drops the ones that are gone.
    fn emit(&mut self, event: &Event) {
        self.watchers
            .retain(|(_, tx)| tx.send(Msg::Item(event.clone())).is_ok());
    }

    /// Sets `pane` to the top lease's size, and tells the watches when it
    /// changed, as the holder does: a subscription hears of a size only in
    /// the answer to its own resize or release. With no lease left the
    /// size stays (spec 2.6).
    fn apply_leases(&mut self, pane: &PaneId) -> Result<(u16, u16)> {
        let top = self
            .leases
            .get(pane)
            .and_then(|leases| leases.last())
            .map(|(_, cols, rows)| (*cols, *rows));
        let record = self.pane_mut(pane)?;
        let before = (record.cols, record.rows);
        if let Some((cols, rows)) = top {
            record.cols = cols;
            record.rows = rows;
        }
        let (cols, rows) = (record.cols, record.rows);
        if (cols, rows) != before {
            self.emit(&Event::Resized {
                pane: pane.clone(),
                cols,
                rows,
            });
        }
        Ok((cols, rows))
    }

    /// Records the exit of a running pane and tells everyone, then drops
    /// the oldest exit past [`EXITED_LIMIT`].
    fn exit(&mut self, pane: &PaneId, exit: Exit) -> bool {
        let Some(record) = self
            .panes
            .iter_mut()
            .find(|p| &p.pane == pane && p.state == PaneState::Running)
        else {
            return false;
        };
        record.state = PaneState::Exited;
        record.exit = Some(exit);
        self.tell(pane, &Update::Exited(exit));
        // The holder closes a subscription after its `exited`.
        self.subscribers.retain(|slot| slot.pane != *pane);
        self.leases.remove(pane);
        self.emit(&Event::Exited {
            pane: pane.clone(),
            exit,
        });
        self.exits.push_back(pane.clone());
        if self.exits.len() > EXITED_LIMIT
            && let Some(gone) = self.exits.pop_front()
        {
            self.panes.retain(|p| p.pane != gone);
            self.screens.remove(&gone);
            self.emit(&Event::Forgotten {
                pane: gone,
                reason: "limit".into(),
            });
        }
        true
    }

    /// The bytes a repaint of `pane` sends: its rows, one per line.
    fn repaint(&self, pane: &PaneId) -> Vec<u8> {
        self.screens
            .get(pane)
            .map(|rows| rows.join("\r\n").into_bytes())
            .unwrap_or_default()
    }
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

fn lock(world: &Mutex<World>) -> MutexGuard<'_, World> {
    world.lock().unwrap_or_else(PoisonError::into_inner)
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
                next_stream: 1,
                calls: Vec::new(),
                unreachable: false,
                screens: HashMap::new(),
                exits: VecDeque::new(),
                leases: HashMap::new(),
                attach_clients: 0,
                subscribers: Vec::new(),
                watchers: Vec::new(),
            })),
        }
    }

    fn world(&self) -> MutexGuard<'_, World> {
        lock(&self.world)
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

    /// Makes every call fail as `Unreachable` (true) or answer again
    /// (false). Going down also fails every subscription and watch, as a
    /// holder that dies closes their connections.
    pub fn set_unreachable(&self, unreachable: bool) {
        let mut world = self.world();
        world.unreachable = unreachable;
        if unreachable {
            for slot in world.subscribers.drain(..) {
                let _ = slot.tx.send(Msg::Fail);
            }
            for (_, tx) in world.watchers.drain(..) {
                let _ = tx.send(Msg::Fail);
            }
            world.leases.clear();
        }
    }

    /// Sets what `screen` reads and a repaint draws for `pane`.
    pub fn set_screen(&self, pane: &PaneId, rows: &[&str]) {
        let rows = rows.iter().map(|row| (*row).to_owned()).collect();
        self.world().screens.insert(pane.clone(), rows);
    }

    /// Sets how many attach clients `switch` tells.
    pub fn set_attach_clients(&self, clients: usize) {
        self.world().attach_clients = clients;
    }

    /// The child writes `bytes`: every subscriber gets them. False when no
    /// running pane has that id.
    pub fn output(&self, pane: &PaneId, bytes: &[u8]) -> bool {
        let mut world = self.world();
        let Some(record) = world
            .panes
            .iter_mut()
            .find(|p| &p.pane == pane && p.state == PaneState::Running)
        else {
            return false;
        };
        record.last_output_at = Some(Timestamp::now());
        world.tell(pane, &Update::Output(bytes.to_vec()));
        true
    }

    /// Cuts every subscriber of `pane`, as the holder does one that falls
    /// behind its queue. Their leases end with their connections.
    pub fn lag(&self, pane: &PaneId) {
        let mut world = self.world();
        world.tell(pane, &Update::Lagged);
        let cut: Vec<String> = world
            .subscribers
            .iter()
            .filter(|slot| slot.pane == *pane)
            .map(|slot| lease_of(slot.stream))
            .collect();
        world.subscribers.retain(|slot| slot.pane != *pane);
        if let Some(leases) = world.leases.get_mut(pane) {
            leases.retain(|(lease, ..)| !cut.contains(lease));
        }
        let _ = world.apply_leases(pane);
    }

    /// Sends `event` to every watch.
    pub fn emit(&self, event: &Event) {
        self.world().emit(event);
    }

    /// The size leases on `pane`, the top one last.
    pub fn leases(&self, pane: &PaneId) -> Vec<(String, u16, u16)> {
        self.world().leases.get(pane).cloned().unwrap_or_default()
    }

    /// Ends a running pane's child, as the holder's reaper records it.
    /// Returns false when no running pane has that id.
    pub fn exit(&self, pane: &PaneId, code: Option<i32>, signal: Option<i32>) -> bool {
        self.world().exit(
            pane,
            Exit {
                code,
                signal,
                at: Timestamp::now(),
            },
        )
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
        world.emit(&Event::Spawned {
            pane: pane.pane.clone(),
        });
        Ok(pane)
    }

    fn list(&self) -> Result<Listing> {
        let world = self.call("list".into())?;
        Ok(Listing {
            instance: world.instance.clone(),
            panes: world.panes.clone(),
        })
    }

    fn get(&self, pane: &PaneId) -> Result<Option<Pane>> {
        let world = self.call(format!("get {pane}"))?;
        Ok(world.pane(pane).ok().cloned())
    }

    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen> {
        let world = self.call(format!("screen {pane} {read:?} {format:?}"))?;
        // Before the pane, as the client refuses it before it is sent.
        if !read.in_range() {
            return Err(PanesError::Refused {
                request: "screen",
                code: ErrorCode::BadRequest,
                message: format!("recent takes 1 to {MAX_RECENT} rows, not {read:?}"),
            });
        }
        let record = world.pane(pane)?;
        // The visible screen is the last `rows` rows, with no scrollback.
        let lines = match read {
            Read::Visible => usize::from(record.rows),
            Read::Recent(n) => usize::from(n),
        };
        let all = world
            .screens
            .get(pane)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let rows_text = all[all.len().saturating_sub(lines)..].to_vec();
        Ok(Screen {
            cols: record.cols,
            rows: record.rows,
            cursor: Cursor {
                col: 0,
                row: 0,
                visible: true,
            },
            modes: Vec::new(),
            rows_text,
        })
    }

    fn input(&self, pane: &PaneId, text: &str) -> Result<()> {
        let world = self.call(format!("input {pane} {text}"))?;
        world.running(pane).map(drop)
    }

    fn paste(&self, pane: &PaneId, text: &str) -> Result<()> {
        let world = self.call(format!("paste {pane} {text}"))?;
        world.running(pane).map(drop)
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        let names: Vec<String> = keys.iter().map(Key::to_string).collect();
        let world = self.call(format!("keys {pane} {}", names.join(" ")))?;
        world.running(pane).map(drop)
    }

    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane> {
        let mut world = self.call(format!("meta {pane} {set:?} {remove:?}"))?;
        let record = world.pane_mut(pane)?;
        record.meta.extend(set.clone());
        for key in remove {
            record.meta.remove(*key);
        }
        let record = record.clone();
        world.emit(&Event::Meta { pane: pane.clone() });
        Ok(record)
    }

    /// `kill` ends the child; any other signal is only logged.
    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()> {
        let mut world = self.call(format!("signal {pane} {signal:?}"))?;
        world.running(pane)?;
        if signal == Signal::Kill {
            world.exit(
                pane,
                Exit {
                    code: None,
                    signal: Some(SIGKILL),
                    at: Timestamp::now(),
                },
            );
        }
        Ok(())
    }

    /// The child ends at once, by `hup`. An exited pane answers with its
    /// exit.
    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit> {
        let mut world = self.call(format!("close {pane} {}", grace.as_millis()))?;
        if let Some(exit) = world.pane(pane)?.exit {
            return Ok(exit);
        }
        let exit = Exit {
            code: None,
            signal: Some(SIGHUP),
            at: Timestamp::now(),
        };
        world.exit(pane, exit);
        Ok(exit)
    }

    fn forget(&self, pane: &PaneId) -> Result<()> {
        let mut world = self.call(format!("forget {pane}"))?;
        if world.pane(pane)?.state == PaneState::Running {
            return Err(PanesError::Refused {
                request: "forget",
                code: ErrorCode::BadRequest,
                message: format!("{pane} is running"),
            });
        }
        world.panes.retain(|p| &p.pane != pane);
        world.screens.remove(pane);
        world.exits.retain(|p| p != pane);
        world.emit(&Event::Forgotten {
            pane: pane.clone(),
            reason: "forget".into(),
        });
        Ok(())
    }

    fn switch(&self, pane: &PaneId) -> Result<usize> {
        let world = self.call(format!("switch {pane}"))?;
        world.running(pane)?;
        Ok(world.attach_clients)
    }
}

impl Live for FakePanes {
    /// One repaint of the pane's rows, then what [`FakePanes::output`]
    /// sends. A pane that has exited sends its `exited` after the repaint,
    /// as the holder does.
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription> {
        let mut world = self.call(format!("subscribe {pane} {:?}", opts.sync))?;
        let record = world.pane(pane)?;
        let (cols, rows, exit) = (record.cols, record.rows, record.exit);
        let stream = world.next_stream;
        world.next_stream += 1;
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(Msg::Item(Update::Repaint {
            cols,
            rows,
            bytes: world.repaint(pane),
        }));
        match exit {
            Some(exit) => {
                let _ = tx.send(Msg::Item(Update::Exited(exit)));
            }
            None => world.subscribers.push(Subscriber {
                pane: pane.clone(),
                stream,
                tx: tx.clone(),
            }),
        }
        drop(world);
        let lease = lease_of(stream);
        let ended = Arc::new(AtomicBool::new(false));
        let control = Arc::new(FakeControl {
            world: Arc::clone(&self.world),
            pane: pane.clone(),
            stream,
            lease: lease.clone(),
            tx,
            ended: Arc::clone(&ended),
        });
        let feed = FakeFeed {
            rx,
            done: false,
            ended,
        };
        Ok(Subscription::from_parts(
            pane.clone(),
            stream,
            (cols, rows),
            lease,
            Box::new(feed),
            control,
        ))
    }

    fn watch(&self) -> Result<Events> {
        let mut world = self.call("watch".into())?;
        let (tx, rx) = mpsc::channel();
        // A watch takes a stream number, so its end can find it.
        let id = world.next_stream;
        world.next_stream += 1;
        world.watchers.push((id, tx.clone()));
        drop(world);
        let shared = Arc::clone(&self.world);
        let ended = Arc::new(AtomicBool::new(false));
        let feed = FakeEvents {
            rx,
            done: false,
            ended: Arc::clone(&ended),
        };
        let end = move || {
            ended.store(true, Ordering::SeqCst);
            let _ = tx.send(Msg::End);
            lock(&shared).watchers.retain(|(watch, _)| *watch != id);
        };
        Ok(Events::from_parts(Box::new(feed), Arc::new(end)))
    }
}

/// The lease a fake subscription's resizes name.
fn lease_of(stream: u32) -> String {
    format!("stream-{stream}")
}

/// A fake subscription's channel end.
struct FakeFeed {
    rx: Receiver<Msg<Update>>,
    done: bool,
    /// Set by the end, which yields nothing more, as a closed socket does.
    ended: Arc<AtomicBool>,
}

impl Feed for FakeFeed {
    fn recv(&mut self) -> Result<Option<Update>> {
        if self.done || self.ended.load(Ordering::SeqCst) {
            return Ok(None);
        }
        match self.rx.recv() {
            Ok(Msg::Item(update)) => {
                // After its exit or a lag the holder closes the stream.
                self.done = matches!(update, Update::Exited(_) | Update::Lagged);
                Ok(Some(update))
            }
            Ok(Msg::End) | Err(_) => {
                self.done = true;
                Ok(None)
            }
            Ok(Msg::Fail) => {
                self.done = true;
                Err(PanesError::Unreachable("the fake holder went down".into()))
            }
        }
    }
}

/// A fake subscription's lease and end.
struct FakeControl {
    world: Arc<Mutex<World>>,
    pane: PaneId,
    stream: u32,
    lease: String,
    tx: Sender<Msg<Update>>,
    ended: Arc<AtomicBool>,
}

impl FakeControl {
    fn check(&self) -> Result<MutexGuard<'_, World>> {
        if self.ended.load(Ordering::SeqCst) {
            return Err(PanesError::Unreachable(format!(
                "the subscription {} has ended",
                self.lease
            )));
        }
        let world = lock(&self.world);
        if world.unreachable {
            return Err(PanesError::Unreachable("the fake holder is down".into()));
        }
        // A lag, an exit or a down holder drops the subscriber, as the
        // holder closes its stream.
        if !world
            .subscribers
            .iter()
            .any(|slot| slot.stream == self.stream)
        {
            return Err(PanesError::Unreachable(format!(
                "the subscription {} has ended",
                self.lease
            )));
        }
        Ok(world)
    }

    /// The answer to a resize or a release: the size now, to this
    /// subscription even when it did not change.
    fn answer(&self, world: &mut World) -> Result<()> {
        let (cols, rows) = world.apply_leases(&self.pane)?;
        let _ = self.tx.send(Msg::Item(Update::Resized { cols, rows }));
        Ok(())
    }
}

impl Control for FakeControl {
    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let mut world = self.check()?;
        world
            .calls
            .push(format!("resize {} {cols}x{rows} {}", self.pane, self.lease));
        world.running(&self.pane)?;
        let leases = world.leases.entry(self.pane.clone()).or_default();
        leases.retain(|(lease, ..)| *lease != self.lease);
        leases.push((self.lease.clone(), cols, rows));
        self.answer(&mut world)
    }

    fn release(&self) -> Result<()> {
        let mut world = self.check()?;
        world
            .calls
            .push(format!("release {} {}", self.pane, self.lease));
        if let Some(leases) = world.leases.get_mut(&self.pane) {
            leases.retain(|(lease, ..)| *lease != self.lease);
        }
        self.answer(&mut world)
    }

    /// Drops the subscriber and its lease, as a closed connection does.
    fn end(&self) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.tx.send(Msg::End);
        let mut world = lock(&self.world);
        world.subscribers.retain(|slot| slot.stream != self.stream);
        let had = world.leases.get_mut(&self.pane).is_some_and(|leases| {
            let before = leases.len();
            leases.retain(|(lease, ..)| *lease != self.lease);
            leases.len() != before
        });
        if had {
            let _ = world.apply_leases(&self.pane);
        }
    }
}

/// A fake watch's channel end.
struct FakeEvents {
    rx: Receiver<Msg<Event>>,
    done: bool,
    /// Set by the end, as for [`FakeFeed`].
    ended: Arc<AtomicBool>,
}

impl EventFeed for FakeEvents {
    fn recv(&mut self) -> Result<Option<Event>> {
        if self.done || self.ended.load(Ordering::SeqCst) {
            return Ok(None);
        }
        match self.rx.recv() {
            Ok(Msg::Item(event)) => Ok(Some(event)),
            Ok(Msg::End) | Err(_) => {
                self.done = true;
                Ok(None)
            }
            Ok(Msg::Fail) => {
                self.done = true;
                Err(PanesError::Unreachable("the fake holder went down".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Meta, Size};
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
        // The exits run newest pane first, so the first exit is not the
        // first pane in the list, and their clocks may tie.
        for pane in panes.iter().rev() {
            assert!(fake.exit(&pane.pane, Some(0), None));
        }
        let kept = fake.list().unwrap().panes;
        assert_eq!(kept.len(), EXITED_LIMIT);
        assert!(kept.iter().all(|p| p.pane != panes[EXITED_LIMIT].pane));
        assert!(fake.spawn(&spawn("again")).is_ok());
    }

    /// The fake through the trait only, as the daemon will hold it.
    fn as_panes(fake: &FakePanes) -> &dyn Panes {
        fake
    }

    fn as_live(fake: &FakePanes) -> &dyn Live {
        fake
    }

    #[test]
    fn every_panes_method_answers_as_the_holder_does() {
        let fake = FakePanes::new();
        let panes = as_panes(&fake);
        let pane = panes.spawn(&spawn("s1")).unwrap().pane;
        assert_eq!(panes.get(&pane).unwrap().unwrap().key, "s1");
        assert_eq!(panes.get(&"p404".into()).unwrap(), None);

        fake.set_screen(&pane, &["one", "two", "three"]);
        let screen = panes.screen(&pane, Read::Recent(2), Format::Text).unwrap();
        assert_eq!(screen.rows_text, ["two", "three"]);
        assert_eq!((screen.cols, screen.rows), (120, 40));
        assert_eq!(
            panes
                .screen(&pane, Read::Visible, Format::Ansi)
                .unwrap()
                .text(),
            "one\ntwo\nthree"
        );

        panes.input(&pane, "hi").unwrap();
        panes.paste(&pane, "a\nb").unwrap();
        panes.keys(&pane, &[Key::ENTER, Key::ESC]).unwrap();
        let set = Meta::from([("label".into(), "alpha".into())]);
        let after = panes.set_meta(&pane, &set, &["role"]).unwrap();
        assert_eq!(after.meta, set);
        fake.set_attach_clients(2);
        assert_eq!(panes.switch(&pane).unwrap(), 2);
        panes.signal(&pane, Signal::Int).unwrap();

        assert!(matches!(
            panes.forget(&pane),
            Err(PanesError::Refused { .. })
        ));
        let exit = panes.close(&pane, Duration::from_secs(3)).unwrap();
        assert_eq!(exit.signal, Some(SIGHUP));
        assert_eq!(panes.close(&pane, Duration::ZERO).unwrap(), exit);
        assert!(matches!(
            panes.input(&pane, "late"),
            Err(PanesError::Exited(_))
        ));
        panes.forget(&pane).unwrap();
        assert_eq!(panes.get(&pane).unwrap(), None);
        assert!(matches!(
            panes.screen(&pane, Read::Visible, Format::Text),
            Err(PanesError::NotFound(_))
        ));
        assert_eq!(
            &fake.calls()[5..8],
            [
                format!("input {pane} hi"),
                format!("paste {pane} a\nb"),
                format!("keys {pane} enter esc"),
            ]
        );
    }

    #[test]
    fn a_subscription_repaints_then_streams_and_its_lease_ends_with_it() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        fake.set_screen(&pane, &["$ ls", "a b"]);
        let live = as_live(&fake);
        let mut first = live.subscribe(&pane, Subscribe::default()).unwrap();
        let mut second = live.subscribe(&pane, Subscribe::default()).unwrap();
        assert_ne!(first.stream(), second.stream());
        assert_eq!(
            first.recv().unwrap(),
            Some(Update::Repaint {
                cols: 120,
                rows: 40,
                bytes: b"$ ls\r\na b".to_vec(),
            })
        );
        assert!(matches!(second.recv(), Ok(Some(Update::Repaint { .. }))));

        assert!(fake.output(&pane, b"tick"));
        assert_eq!(
            first.recv().unwrap(),
            Some(Update::Output(b"tick".to_vec()))
        );
        assert_eq!(
            second.recv().unwrap(),
            Some(Update::Output(b"tick".to_vec()))
        );

        // The top lease sets the size. Only the asker hears of it in its
        // answer; the other subscriber learns of it from a watch.
        let mut events = live.watch().unwrap();
        first.resize(100, 30).unwrap();
        let resized = Some(Update::Resized {
            cols: 100,
            rows: 30,
        });
        assert_eq!(first.recv().unwrap(), resized);
        second.resize(80, 24).unwrap();
        second.release().unwrap();
        assert_eq!(fake.leases(&pane).len(), 1);
        // 80x24, then back to the first lease's 100x30.
        assert!(matches!(
            second.recv(),
            Ok(Some(Update::Resized { cols: 80, .. }))
        ));
        assert_eq!(second.recv().unwrap(), resized);
        for (cols, rows) in [(100, 30), (80, 24), (100, 30)] {
            assert_eq!(
                events.recv().unwrap(),
                Some(Event::Resized {
                    pane: pane.clone(),
                    cols,
                    rows
                })
            );
        }
        assert!(fake.output(&pane, b"tock"));
        assert_eq!(
            first.recv().unwrap(),
            Some(Update::Output(b"tock".to_vec()))
        );

        // Dropping a subscription ends its lease; the size stays.
        drop(first);
        assert!(fake.leases(&pane).is_empty());
        let handle = second.handle();
        handle.end();
        assert_eq!(second.recv().unwrap(), None);
        assert!(handle.resize(1, 1).is_err());
    }

    #[test]
    fn a_subscription_to_an_exited_pane_sees_its_exit_after_the_repaint() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        let mut before = fake.subscribe(&pane, Subscribe::default()).unwrap();
        assert!(fake.exit(&pane, Some(3), None));
        let mut after = fake.subscribe(&pane, Subscribe::default()).unwrap();
        for subscription in [&mut before, &mut after] {
            let mut updates = Vec::new();
            while let Some(update) = subscription.recv().unwrap() {
                updates.push(update);
            }
            assert!(matches!(updates[0], Update::Repaint { .. }), "{updates:?}");
            assert!(
                matches!(updates.last(), Some(Update::Exited(exit)) if exit.code == Some(3)),
                "{updates:?}"
            );
        }
    }

    #[test]
    fn a_watch_sees_events_and_a_down_holder_fails_every_stream() {
        let fake = FakePanes::new();
        let mut events = as_live(&fake).watch().unwrap();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        let mut subscription = fake.subscribe(&pane, Subscribe::default()).unwrap();
        fake.emit(&Event::Bell { pane: pane.clone() });
        fake.signal(&pane, Signal::Kill).unwrap();
        assert_eq!(
            events.recv().unwrap(),
            Some(Event::Spawned { pane: pane.clone() })
        );
        assert_eq!(
            events.recv().unwrap(),
            Some(Event::Bell { pane: pane.clone() })
        );
        assert!(matches!(
            events.recv().unwrap(),
            Some(Event::Exited { exit, .. }) if exit.signal == Some(SIGKILL)
        ));
        assert!(matches!(
            subscription.recv(),
            Ok(Some(Update::Repaint { .. }))
        ));
        assert!(matches!(subscription.recv(), Ok(Some(Update::Exited(_)))));
        assert_eq!(subscription.recv().unwrap(), None);

        let ender = events.ender();
        ender.end();
        assert_eq!(events.recv().unwrap(), None);
        // An ended watch is no longer one the fake sends to.
        assert!(fake.world().watchers.is_empty());

        let mut events = fake.watch().unwrap();
        let other = fake.spawn(&spawn("s2")).unwrap().pane;
        let mut subscription = fake.subscribe(&other, Subscribe::default()).unwrap();
        fake.set_unreachable(true);
        assert!(matches!(events.recv(), Ok(Some(Event::Spawned { .. }))));
        assert!(matches!(events.recv(), Err(PanesError::Unreachable(_))));
        assert_eq!(events.recv().unwrap(), None);
        assert!(matches!(
            subscription.recv(),
            Ok(Some(Update::Repaint { .. }))
        ));
        assert!(matches!(
            subscription.recv(),
            Err(PanesError::Unreachable(_))
        ));
        assert!(fake.subscribe(&other, Subscribe::default()).is_err());
    }

    #[test]
    fn a_lagged_subscriber_is_cut() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        let mut subscription = fake.subscribe(&pane, Subscribe::default()).unwrap();
        subscription.resize(100, 30).unwrap();
        fake.lag(&pane);
        // Its lease ended with it, before the subscription value is dropped.
        assert!(fake.leases(&pane).is_empty());
        assert!(matches!(
            subscription.recv(),
            Ok(Some(Update::Repaint { .. }))
        ));
        assert!(matches!(
            subscription.recv(),
            Ok(Some(Update::Resized { .. }))
        ));
        assert_eq!(subscription.recv().unwrap(), Some(Update::Lagged));
        assert_eq!(subscription.recv().unwrap(), None);
        assert!(fake.output(&pane, b"after"));
        // A cut stream takes no lease, as a closed connection takes none.
        assert!(matches!(
            subscription.resize(90, 20),
            Err(PanesError::Unreachable(_))
        ));
        assert!(fake.leases(&pane).is_empty());
    }

    #[test]
    fn a_visible_read_has_no_scrollback() {
        let fake = FakePanes::new();
        let mut req = spawn("s1");
        req.size = Some(Size { cols: 80, rows: 2 });
        let pane = fake.spawn(&req).unwrap().pane;
        fake.set_screen(&pane, &["old", "one", "two"]);
        let visible = fake.screen(&pane, Read::Visible, Format::Text).unwrap();
        assert_eq!(visible.rows_text, ["one", "two"]);
        let recent = fake.screen(&pane, Read::Recent(3), Format::Text).unwrap();
        assert_eq!(recent.rows_text, ["old", "one", "two"]);
    }

    #[test]
    fn an_ended_stream_yields_nothing_it_had_queued() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        let mut subscription = fake.subscribe(&pane, Subscribe::default()).unwrap();
        let mut events = fake.watch().unwrap();
        assert!(fake.output(&pane, b"queued"));
        fake.emit(&Event::Bell { pane: pane.clone() });
        subscription.handle().end();
        events.ender().end();
        assert_eq!(subscription.recv().unwrap(), None);
        assert_eq!(events.recv().unwrap(), None);
    }

    #[test]
    fn a_recent_read_outside_its_range_is_refused() {
        let fake = FakePanes::new();
        let pane = fake.spawn(&spawn("s1")).unwrap().pane;
        for lines in [0, MAX_RECENT + 1] {
            assert!(!Read::Recent(lines).in_range());
            assert!(matches!(
                fake.screen(&pane, Read::Recent(lines), Format::Text),
                Err(PanesError::Refused {
                    code: ErrorCode::BadRequest,
                    ..
                })
            ));
        }
        assert!(
            fake.screen(&pane, Read::Recent(MAX_RECENT), Format::Text)
                .is_ok()
        );
    }
}
