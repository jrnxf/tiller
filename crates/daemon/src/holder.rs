//! The daemon's link to the terminal holder (`terminal-holder.md` 4.1, 4.2):
//! the holder's [`Panes`] calls, and its [`tiller_panes::Live`] streams for
//! the board's terminal view.
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
    HolderClient, Key, Listing, Pane, PaneId, PaneState, Panes, PanesError, Result, Spawn,
    Subscribe, Subscription,
};

/// The name this daemon gives the holder in its handshake.
const CLIENT: &str = "daemon";

/// What the daemon asks of the holder: its calls and its streams. Each
/// subscription is a connection of its own, so the size lease it holds
/// ends with it, and with a daemon that dies (spec 2.6).
pub trait Holder: Panes + tiller_panes::Live {}

impl<T: Panes + tiller_panes::Live + ?Sized> Holder for T {}

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
