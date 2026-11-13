//! The daemon's link to the terminal holder (`terminal-holder.md` 4.1, 4.2),
//! for the first slice (S1c): `spawn` and `list` for new agents, and a
//! pane's live output and raw input for the board's terminal view.
//!
//! The link connects when it is first used and again after the holder
//! stops answering, so a daemon that starts before the holder, or outlives
//! one, finds it when it comes. The panes are the holder's, never the
//! link's: a new daemon reads each session's pane id from `state.db` and
//! subscribes to it again, and it starts nothing that is still running.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tiller_panes::handshake::Welcome;
use tiller_panes::message::{Exit, Format, Meta, Read, Screen, Signal};
use tiller_panes::{
    HolderClient, Key, Listing, Live as _, Pane, PaneId, PaneState, Panes, PanesError, Result,
    Spawn, StreamHandle, Subscribe, Subscription, Update,
};

/// The name this daemon gives the holder in its handshake.
const CLIENT: &str = "daemon";

/// What the daemon asks of the holder beyond [`Panes`]. The `Live` trait of
/// H02 takes its place.
pub trait Holder: Panes {
    /// The pane's output: one repaint, then the PTY's bytes.
    fn subscribe(&self, pane: &PaneId) -> Result<Box<dyn Live>>;
    /// Writes `bytes` to the pane as they are.
    fn raw_input(&self, pane: &PaneId, bytes: &[u8]) -> Result<()>;
    /// Sets the pane's PTY to `cols` by `rows`, and returns once the holder
    /// took it. Only the board's Fit resizes a pane (`holder_mirror`). The
    /// holder reports a resize only to the subscription that asked for it,
    /// so the views open on the pane learn of it from the daemon.
    fn set_size(&self, pane: &PaneId, cols: u16, rows: u16) -> Result<()>;
}

/// One subscription to a pane's output. [`Live::recv`] blocks.
pub trait Live: Send {
    /// The next update; `None` once the stream has ended.
    fn recv(&mut self) -> Result<Option<Update>>;
    /// Ends this subscription from another thread.
    fn control(&self) -> Arc<dyn LiveControl>;
}

/// Ends a [`Live`] while another thread waits in its `recv`.
pub trait LiveControl: Send + Sync {
    /// Ends the subscription: a `recv` waiting now returns `None`.
    fn end(&self);
}

impl Live for Subscription {
    fn recv(&mut self) -> Result<Option<Update>> {
        Subscription::recv(self)
    }

    fn control(&self) -> Arc<dyn LiveControl> {
        Arc::new(self.handle())
    }
}

impl LiveControl for StreamHandle {
    fn end(&self) {
        StreamHandle::end(self);
    }
}

/// The holder at `<home>/run/holder.sock`, reached through one client that
/// is replaced once it stops answering.
#[derive(Debug)]
pub struct HolderLink {
    path: PathBuf,
    run_dir: PathBuf,
    client: Mutex<Option<Arc<HolderClient>>>,
}

impl HolderLink {
    /// The link to the holder of this Tiller home. It connects on first use.
    pub fn new(home: &Path) -> Self {
        Self {
            path: tiller_panes::socket_path(home),
            run_dir: tiller_panes::run_dir(home),
            client: Mutex::new(None),
        }
    }

    /// Whether a holder has made its socket in this home. A socket left by
    /// a holder that died is there too, so this is not proof it answers.
    pub fn present(home: &Path) -> bool {
        tiller_panes::socket_path(home).exists()
    }

    /// The client in use, or a new one.
    fn client(&self) -> Result<Arc<HolderClient>> {
        let mut slot = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = slot.as_ref() {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(HolderClient::connect(&self.path, CLIENT)?);
        *slot = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Stops using `used` after it failed, unless another call has
    /// replaced it already.
    fn drop_client(&self, used: &Arc<HolderClient>) {
        let mut slot = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if slot
            .as_ref()
            .is_some_and(|client| Arc::ptr_eq(client, used))
        {
            *slot = None;
        }
    }

    /// Runs `call` on the client, and once more on a new client when the
    /// first did not answer: a client whose holder restarted, or whose
    /// connection broke, fails every call from then on. Only for calls the
    /// holder may take twice.
    fn twice<T>(&self, call: impl Fn(&HolderClient) -> Result<T>) -> Result<T> {
        match self.once(&call) {
            Err(PanesError::Unreachable(_)) => self.once(call),
            result => result,
        }
    }

    /// Runs `call` on the client once, for a call the holder must not take
    /// twice, and stops using the client when it did not answer.
    fn once<T>(&self, call: impl FnOnce(&HolderClient) -> Result<T>) -> Result<T> {
        let client = self.client()?;
        let result = call(&client);
        if matches!(result, Err(PanesError::Unreachable(_))) {
            self.drop_client(&client);
        }
        result
    }
}

impl Panes for HolderLink {
    /// A spawn is keyed by its session, so a second try after a lost reply
    /// returns the pane the first one started.
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        self.twice(|client| client.spawn(req))
    }

    fn list(&self) -> Result<Listing> {
        self.twice(Panes::list)
    }

    fn get(&self, pane: &PaneId) -> Result<Option<Pane>> {
        self.twice(|client| client.get(pane))
    }

    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen> {
        self.twice(|client| client.screen(pane, read, format))
    }

    fn input(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.once(|client| client.input(pane, text))
    }

    fn paste(&self, pane: &PaneId, text: &str) -> Result<()> {
        self.once(|client| client.paste(pane, text))
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        self.once(|client| client.keys(pane, keys))
    }

    /// The same merge twice leaves the same metadata.
    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane> {
        self.twice(|client| client.set_meta(pane, set, remove))
    }

    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()> {
        self.once(|client| client.signal(pane, signal))
    }

    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit> {
        self.once(|client| client.close(pane, grace))
    }

    fn forget(&self, pane: &PaneId) -> Result<()> {
        self.once(|client| Panes::forget(client, pane))
    }

    fn switch(&self, pane: &PaneId) -> Result<usize> {
        self.once(|client| client.switch(pane))
    }
}

impl Holder for HolderLink {
    fn subscribe(&self, pane: &PaneId) -> Result<Box<dyn Live>> {
        self.twice(|client| client.subscribe(pane, Subscribe::default()))
            .map(|live| Box::new(live) as Box<dyn Live>)
    }

    /// The pane's `num` and the write go through one client: a `num` names
    /// a pane only in the holder instance that gave it. The write is never
    /// sent twice, since the holder sends no reply that would say whether
    /// the first one landed.
    fn raw_input(&self, pane: &PaneId, bytes: &[u8]) -> Result<()> {
        let client = self.client()?;
        let (client, listing) = match client.list() {
            Err(PanesError::Unreachable(_)) => {
                self.drop_client(&client);
                let client = self.client()?;
                let listing = client.list();
                (client, listing)
            }
            listing => (client, listing),
        };
        let result = listing.and_then(|listing| {
            let num = running(&listing, pane)?.num;
            client.raw_input(num, bytes)
        });
        if matches!(result, Err(PanesError::Unreachable(_))) {
            self.drop_client(&client);
        }
        result
    }

    /// Through a subscription of its own, which ends once the holder took
    /// the size: the slice's holder keeps no leases, so the size stays
    /// when it ends (H11 makes the Fit a lease on the view's own
    /// subscription). Only the subscribe is tried twice; a resize the
    /// holder does not answer fails the Fit.
    fn set_size(&self, pane: &PaneId, cols: u16, rows: u16) -> Result<()> {
        let mut sizer = self.twice(|client| client.subscribe(pane, Subscribe::default()))?;
        sizer.resize(cols, rows)?;
        loop {
            match sizer.recv()? {
                Some(Update::Resized { .. }) => return Ok(()),
                Some(Update::Exited(_)) => return Err(PanesError::Exited(pane.to_string())),
                Some(Update::Lagged) | None => {
                    return Err(PanesError::Unreachable(format!(
                        "the terminal holder ended the subscription before it resized pane {pane}"
                    )));
                }
                Some(Update::Repaint { .. } | Update::Output(_)) => {}
            }
        }
    }
}

impl tiller_panes::Live for HolderLink {
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription> {
        self.twice(|client| client.subscribe(pane, opts))
    }

    fn watch(&self) -> Result<tiller_panes::Events> {
        self.twice(tiller_panes::Live::watch)
    }
}

/// The holder as reconcile's observation source (`terminal-holder.md` 6.1).
impl tiller_reconcile::holder::Link for HolderLink {
    /// A ping first: a client kept from before a holder restart holds the
    /// old `welcome`, and only a call finds its connection gone.
    fn welcome(&self) -> Result<Welcome> {
        self.twice(|client| {
            client.ping()?;
            Ok(client.welcome().clone())
        })
    }

    fn running_binary(&self) -> Option<PathBuf> {
        tiller_holder::lock::running_binary(&self.run_dir)
    }

    /// Asked once: a second ask could land on the new image.
    fn upgrade(&self, binary: &Path) -> Result<tiller_panes::Upgrade> {
        let Some(binary) = binary.to_str() else {
            return Ok(tiller_panes::Upgrade::Refused(format!(
                "the binary path {} is not UTF-8",
                binary.display()
            )));
        };
        self.once(|client| client.upgrade(binary, false))
    }
}

/// After a start: reads which live sessions `state.db` puts in the
/// holder, and checks each pane is still there, so the log says what this
/// daemon picked up again. The terminal view subscribes to the recorded
/// pane on its own, so nothing is started here; a pane missing from the
/// holder is left for reconcile (H10).
pub fn reattach(db: &Path, holder: &dyn Holder) {
    let recorded = match recorded(db) {
        Ok(recorded) => recorded,
        Err(error) => {
            tracing::warn!(%error, "could not read which sessions run in the terminal holder");
            return;
        }
    };
    if recorded.is_empty() {
        return;
    }
    let listing = match holder.list() {
        Ok(listing) => listing,
        Err(error) => {
            tracing::warn!(%error, sessions = recorded.len(), "the terminal holder did not answer; its panes are picked up when it does");
            return;
        }
    };
    for (session, pane) in recorded {
        match running(&listing, &PaneId::from(pane.as_str())) {
            Ok(found) => tracing::info!(
                session,
                pane,
                pid = found.pid,
                "found the session's pane in the terminal holder"
            ),
            Err(PanesError::Exited(_)) => tracing::info!(
                session,
                pane,
                "the session's pane in the terminal holder has exited"
            ),
            Err(_) => tracing::warn!(
                session,
                pane,
                instance = listing.instance,
                "the terminal holder has no pane for the session"
            ),
        }
    }
}

/// Each live session whose pane is in the holder, with that pane.
fn recorded(db: &Path) -> tiller_core::Result<Vec<(String, String)>> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let store = tiller_core::Store::open_current(db)?;
    let sessions: Vec<tiller_core::domain::Session> = tiller_core::state::list(
        store.conn(),
        "ended_at IS NULL AND pane_id IS NOT NULL ORDER BY started_at, id",
        &[],
    )?;
    Ok(sessions
        .into_iter()
        .filter_map(|session| {
            let pane = session.pane_id?;
            tiller_core::ids::is_holder_pane(&pane).then_some((session.id, pane))
        })
        .collect())
}

/// The running pane `pane` in `listing`: `NotFound` when the holder has no
/// such pane, `Exited` when its child has exited.
pub fn running<'a>(listing: &'a Listing, pane: &PaneId) -> Result<&'a Pane> {
    match listing.panes.iter().find(|found| &found.pane == pane) {
        None => Err(PanesError::NotFound(pane.to_string())),
        Some(found) if found.state == PaneState::Exited => {
            Err(PanesError::Exited(pane.to_string()))
        }
        Some(found) => Ok(found),
    }
}

/// How a pane's child ended, for the board's `closed` line.
pub fn exit_reason(exit: Option<&tiller_panes::Exit>) -> String {
    match exit {
        Some(tiller_panes::Exit {
            code: Some(code), ..
        }) => format!("the agent exited with code {code}"),
        Some(tiller_panes::Exit {
            signal: Some(signal),
            ..
        }) => format!("the agent was killed by signal {signal}"),
        _ => "the agent exited".to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A holder for the daemon's tests: [`FakePanes`] for the panes, with a
    //! screen per pane that a subscription repaints and then follows, and
    //! every raw input kept. Clones share one holder, so a test can stop a
    //! "daemon" and start another on the same panes.

    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    use std::time::Duration;

    use tiller_panes::message::{Exit, Format, Meta, Read, Screen, Signal};
    use tiller_panes::{FakePanes, Key, Listing, Pane, PaneId, Panes, Result, Spawn, Update};

    use super::{Holder, Live, LiveControl, running};

    #[derive(Default)]
    struct Screens {
        /// Everything each pane has written, in order.
        written: HashMap<PaneId, Vec<u8>>,
        /// Each pane's open subscriptions, by a number of their own.
        live: HashMap<PaneId, Vec<(u64, Sender<Update>)>>,
        next: u64,
        /// Each raw input, with its pane.
        inputs: Vec<(PaneId, Vec<u8>)>,
        /// The size each resized pane has now.
        sizes: HashMap<PaneId, (u16, u16)>,
        /// Each size the daemon set, with its pane, in order.
        set: Vec<(PaneId, (u16, u16))>,
        /// Whether a size the daemon sets is refused.
        refuse_sizes: bool,
    }

    #[derive(Clone, Default)]
    pub(crate) struct FakeHolder {
        pub(crate) panes: FakePanes,
        screens: Arc<Mutex<Screens>>,
    }

    impl FakeHolder {
        fn screens(&self) -> MutexGuard<'_, Screens> {
            self.screens.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// The pane's child writes `bytes`.
        pub(crate) fn write(&self, pane: &PaneId, bytes: &[u8]) {
            let mut screens = self.screens();
            screens
                .written
                .entry(pane.clone())
                .or_default()
                .extend_from_slice(bytes);
            if let Some(live) = screens.live.get_mut(pane) {
                live.retain(|(_, tx)| tx.send(Update::Output(bytes.to_vec())).is_ok());
            }
        }

        /// The pane's child exits with `code`; its subscriptions see it and
        /// close.
        pub(crate) fn exit(&self, pane: &PaneId, code: i32) {
            assert!(self.panes.exit(pane, Some(code), None));
            let exit = self
                .panes
                .list()
                .ok()
                .and_then(|listing| listing.panes.into_iter().find(|p| &p.pane == pane))
                .and_then(|p| p.exit);
            let live = self.screens().live.remove(pane).unwrap_or_default();
            if let Some(exit) = exit {
                for (_, tx) in live {
                    let _ = tx.send(Update::Exited(exit));
                }
            }
        }

        pub(crate) fn inputs(&self) -> Vec<(PaneId, Vec<u8>)> {
            self.screens().inputs.clone()
        }

        /// Another client resizes the pane's PTY; each subscription sees it,
        /// as it will once `watch` events land.
        pub(crate) fn resize(&self, pane: &PaneId, cols: u16, rows: u16) {
            let mut screens = self.screens();
            screens.sizes.insert(pane.clone(), (cols, rows));
            if let Some(live) = screens.live.get_mut(pane) {
                live.retain(|(_, tx)| tx.send(Update::Resized { cols, rows }).is_ok());
            }
        }

        /// Another client resizes the pane's PTY, and no subscription hears
        /// of it, as with the slice's holder.
        pub(crate) fn resize_unseen(&self, pane: &PaneId, cols: u16, rows: u16) {
            self.screens().sizes.insert(pane.clone(), (cols, rows));
        }

        /// Each size the daemon set, with its pane, in order.
        pub(crate) fn sizes_set(&self) -> Vec<(PaneId, (u16, u16))> {
            self.screens().set.clone()
        }

        /// Refuses each size the daemon sets from now on, or stops refusing.
        pub(crate) fn refuse_sizes(&self, refuse: bool) {
            self.screens().refuse_sizes = refuse;
        }

        /// The pane's PTY size now.
        pub(crate) fn size(&self, pane: &PaneId) -> Option<(u16, u16)> {
            let listing = self.list().ok()?;
            let found = listing.panes.into_iter().find(|p| &p.pane == pane)?;
            Some((found.cols, found.rows))
        }

        /// How many subscriptions to the pane are open.
        pub(crate) fn subscribers(&self, pane: &PaneId) -> usize {
            self.screens().live.get(pane).map_or(0, Vec::len)
        }
    }

    impl Panes for FakeHolder {
        fn spawn(&self, req: &Spawn) -> Result<Pane> {
            self.panes.spawn(req)
        }

        /// As the holder's listing does, each pane has its PTY's size.
        fn list(&self) -> Result<Listing> {
            let mut listing = self.panes.list()?;
            let screens = self.screens();
            for pane in &mut listing.panes {
                if let Some((cols, rows)) = screens.sizes.get(&pane.pane) {
                    (pane.cols, pane.rows) = (*cols, *rows);
                }
            }
            Ok(listing)
        }

        /// With the PTY's size, as [`FakeHolder::list`] gives it.
        fn get(&self, pane: &PaneId) -> Result<Option<Pane>> {
            let listing = self.list()?;
            Ok(listing.panes.into_iter().find(|p| &p.pane == pane))
        }

        fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen> {
            self.panes.screen(pane, read, format)
        }

        fn input(&self, pane: &PaneId, text: &str) -> Result<()> {
            self.panes.input(pane, text)
        }

        fn paste(&self, pane: &PaneId, text: &str) -> Result<()> {
            self.panes.paste(pane, text)
        }

        fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
            self.panes.keys(pane, keys)
        }

        fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane> {
            self.panes.set_meta(pane, set, remove)
        }

        fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()> {
            self.panes.signal(pane, signal)
        }

        fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit> {
            self.panes.close(pane, grace)
        }

        fn forget(&self, pane: &PaneId) -> Result<()> {
            self.panes.forget(pane)
        }

        fn switch(&self, pane: &PaneId) -> Result<usize> {
            self.panes.switch(pane)
        }
    }

    impl Holder for FakeHolder {
        fn subscribe(&self, pane: &PaneId) -> Result<Box<dyn Live>> {
            let listing = self.list()?;
            let found = running(&listing, pane)?;
            let (cols, rows) = (found.cols, found.rows);
            let (tx, rx) = channel();
            let mut screens = self.screens();
            let bytes = screens.written.get(pane).cloned().unwrap_or_default();
            let _ = tx.send(Update::Repaint { cols, rows, bytes });
            let id = screens.next;
            screens.next += 1;
            screens.live.entry(pane.clone()).or_default().push((id, tx));
            Ok(Box::new(FakeLive {
                rx,
                control: Arc::new(FakeControl {
                    holder: self.clone(),
                    pane: pane.clone(),
                    id,
                }),
            }))
        }

        fn raw_input(&self, pane: &PaneId, bytes: &[u8]) -> Result<()> {
            let listing = self.panes.list()?;
            running(&listing, pane)?;
            self.screens().inputs.push((pane.clone(), bytes.to_vec()));
            Ok(())
        }

        /// As the slice's holder does, the size reaches no other
        /// subscription.
        fn set_size(&self, pane: &PaneId, cols: u16, rows: u16) -> Result<()> {
            let listing = self.panes.list()?;
            running(&listing, pane)?;
            let mut screens = self.screens();
            if screens.refuse_sizes {
                return Err(tiller_panes::PanesError::Unreachable(
                    "the holder refused the size".to_owned(),
                ));
            }
            screens.sizes.insert(pane.clone(), (cols, rows));
            screens.set.push((pane.clone(), (cols, rows)));
            Ok(())
        }
    }

    struct FakeLive {
        rx: Receiver<Update>,
        control: Arc<FakeControl>,
    }

    impl Live for FakeLive {
        /// The holder's subscription closes after `exited` and after an
        /// end; the fake's sender goes then too.
        fn recv(&mut self) -> Result<Option<Update>> {
            Ok(self.rx.recv().ok())
        }

        fn control(&self) -> Arc<dyn LiveControl> {
            self.control.clone()
        }
    }

    impl Drop for FakeLive {
        fn drop(&mut self) {
            self.control.end();
        }
    }

    struct FakeControl {
        holder: FakeHolder,
        pane: PaneId,
        id: u64,
    }

    impl LiveControl for FakeControl {
        fn end(&self) {
            let mut screens = self.holder.screens();
            if let Some(live) = screens.live.get_mut(&self.pane) {
                live.retain(|(id, _)| *id != self.id);
            }
        }
    }
}
