//! The live upgrade (`terminal-holder.md` 6.2), both halves: the old
//! program image checks the new binary, holds the panes still and
//! re-executes, and the new one checks a record and reads it back.
//!
//! Every way the upgrade can fail before `execve` answers
//! `upgrade_refused`, and the old holder runs on as it was (R22). Each
//! reason starts with the step that failed: `busy`, `preflight`,
//! `version`, `record`, `dry_run`, `requester` or `exec`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tiller_panes::handshake::{self, ProtoRange, SUPPORTED};
use tokio::io::AsyncWriteExt;

use crate::handover::codec::fnv1a;
use crate::handover::{OLDEST, RECORD_MAX, Record, RecordError, Requester, VERSION};
use crate::holder::{Conn, Handover, Holder};
use crate::model::Model;

/// How long the new binary has to say what it accepts.
pub const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the new binary has to read the record back.
pub const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the requester's connection has to finish the frame it writes.
const WRITE_TURN_WAIT: Duration = Duration::from_secs(1);
/// How long the new image has to answer the requester.
const ANSWER_WRITE: Duration = Duration::from_secs(1);
/// The most a child's output the holder reads into a reason.
const REASON_MAX: usize = 300;

/// What a binary accepts, as `tiller holder --check-handover` prints it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepts {
    pub handover: Versions,
    pub proto: ProtoRange,
    pub holder: String,
}

/// The handover record versions a build reads, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    pub oldest: u32,
    pub version: u32,
}

impl Accepts {
    /// This build's.
    pub fn this_build() -> Self {
        Self {
            handover: Versions {
                oldest: OLDEST,
                version: VERSION,
            },
            proto: SUPPORTED,
            holder: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Why a holder of this build cannot hand over to a binary that
    /// accepts `self`, if it cannot.
    fn refusal(&self) -> Option<String> {
        let Versions { oldest, version } = self.handover;
        if !(oldest..=version).contains(&VERSION) {
            return Some(format!(
                "version: the binary ({}) reads handover versions {oldest} to {version}, and this holder writes {VERSION}",
                self.holder
            ));
        }
        if handshake::negotiate(self.proto, SUPPORTED).is_none() {
            return Some(format!(
                "version: the binary ({}) speaks protocol {}, and this holder's clients speak {SUPPORTED}",
                self.holder, self.proto
            ));
        }
        None
    }
}

/// What `tiller holder --dry-run-handover` prints: the checksum of the
/// record it read, written again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DryRun {
    pub checksum: String,
}

/// `tiller holder --dry-run-handover`: reads a record, rebuilds every
/// pane's model from it and checks that each reads back as it was, and
/// writes the record again. Equal checksums mean the new binary reads
/// every field the old one wrote.
pub fn dry_run(bytes: &[u8]) -> Result<DryRun, RecordError> {
    let record = Record::decode(bytes)?;
    for pane in &record.panes {
        Model::import(&pane.model)
            .map_err(|err| RecordError::Unfit(format!("pane {}: {err}", pane.pane.pane)))?;
    }
    Ok(DryRun {
        checksum: checksum(&record.encode()?),
    })
}

fn checksum(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}

/// The old program image's half: runs the upgrade to `binary`, and with
/// `dry_run` stops before it re-executes. `Ok` is the `upgrading` reply,
/// which the new image sends when it re-executed. `socket` is a copy of
/// the requester's connection, for the new image to answer on.
pub async fn upgrade(
    holder: &Arc<Holder>,
    conn: &Conn,
    id: u64,
    binary: &Path,
    dry_run: bool,
    socket: Option<io::Result<OwnedFd>>,
) -> Result<(), String> {
    let _slot = holder
        .take_upgrade_slot()
        .ok_or_else(|| "busy: another upgrade is under way".to_owned())?;
    tracing::info!(binary = %binary.display(), dry_run, "an upgrade");

    // 1. Preflight.
    let out = run_child(binary, "--check-handover", None, PREFLIGHT_TIMEOUT)
        .await
        .map_err(|err| format!("preflight: {err}"))?;
    let accepts: Accepts = serde_json::from_slice(&out).map_err(|err| {
        format!("preflight: the binary's answer is not one this holder reads: {err}")
    })?;
    if let Some(refusal) = accepts.refusal() {
        return Err(refusal);
    }

    // 2. Dry run, on a record of this moment. The panes are held still
    // only while it is built.
    let turn = holder.spawn_turn().await;
    let held = holder.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        held.quiesced(turn, None, |handover| handover?.record.encode())
    })
    .await
    .map_err(|err| format!("record: {err}"))?
    .map_err(|err| format!("record: {err}"))?;
    let wrote = checksum(&bytes);
    let out = run_child(binary, "--dry-run-handover", Some(bytes), DRY_RUN_TIMEOUT)
        .await
        .map_err(|err| format!("dry_run: {err}"))?;
    let read: DryRun = serde_json::from_slice(&out).map_err(|err| {
        format!("dry_run: the binary's answer is not one this holder reads: {err}")
    })?;
    if read.checksum != wrote {
        return Err(format!(
            "dry_run: the binary read the record back as {}, and this holder wrote {wrote}",
            read.checksum
        ));
    }
    if dry_run {
        tracing::info!(binary = %binary.display(), "the upgrade's dry run passed");
        return Ok(());
    }

    // 3 to 5. Quiesce, hand over and re-execute.
    let socket = socket
        .unwrap_or_else(|| Err(io::Error::other("the connection has no socket")))
        .map_err(|err| format!("requester: {err}"))?;
    // No frame is half written on the requester's socket, so the new
    // image's answer starts on a frame of its own.
    let write_turn = tokio::time::timeout(WRITE_TURN_WAIT, conn.write_turn())
        .await
        .map_err(|_| "busy: the connection did not finish its write".to_owned())?;
    if conn.outbox.is_closed() || conn.outbox.cut_at().is_some() {
        return Err("requester: the connection is closed or cut".to_owned());
    }
    let turn = holder.spawn_turn().await;
    let held = holder.clone();
    let binary = binary.to_owned();
    let refusal = tokio::task::spawn_blocking(move || {
        let requester = Requester {
            fd: socket.as_raw_fd(),
            id,
        };
        held.quiesced(turn, Some(requester), |handover| {
            exec(&binary, handover, socket.as_fd())
        })
    })
    .await
    .unwrap_or_else(|err| format!("exec: {err}"));
    drop(write_turn);
    tracing::warn!(%refusal, "the upgrade did not re-execute; the holder runs on");
    Err(refusal)
}

/// Writes the record, lets the new image inherit its fds and
/// re-executes. Returns only when it did not, with the fds as they were.
fn exec(
    binary: &Path,
    handover: Result<Handover<'_>, RecordError>,
    socket: BorrowedFd<'_>,
) -> String {
    let handover = match handover {
        Ok(handover) => handover,
        Err(err) => return format!("record: {err}"),
    };
    let bytes = match handover.record.encode() {
        Ok(bytes) => bytes,
        Err(err) => return format!("record: {err}"),
    };
    let file = match write_record(handover.run, &bytes) {
        Ok(file) => file,
        Err(err) => return format!("record: the record could not be written: {err}"),
    };
    let mut fds = handover.fds;
    fds.push(socket);
    fds.push(file.as_fd());
    let err = match inherit(&fds, true) {
        Ok(()) => std::process::Command::new(binary)
            .arg0("tiller")
            .args(["holder", "--resume", &file.as_raw_fd().to_string()])
            .exec(),
        Err(err) => err,
    };
    if let Err(again) = inherit(&fds, false) {
        tracing::error!(%again, "an fd kept its close-on-exec flag off");
    }
    format!("exec: {err}")
}

/// Clears `FD_CLOEXEC` on each fd, or sets it again.
fn inherit(fds: &[BorrowedFd<'_>], on: bool) -> io::Result<()> {
    let flags = if on {
        rustix::io::FdFlags::empty()
    } else {
        rustix::io::FdFlags::CLOEXEC
    };
    for fd in fds {
        rustix::io::fcntl_setfd(fd, flags)?;
    }
    Ok(())
}

/// The record in a sealed `memfd`: nothing can change it after this.
#[cfg(target_os = "linux")]
fn write_record(_run: &Path, bytes: &[u8]) -> io::Result<File> {
    use rustix::fs::{MemfdFlags, SealFlags};
    let fd = rustix::fs::memfd_create(
        "tiller-handover",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    rustix::fs::fcntl_add_seals(
        &file,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    Ok(file)
}

/// The record in an unlinked file in the run directory, which only this
/// user can enter.
#[cfg(not(target_os = "linux"))]
fn write_record(run: &Path, bytes: &[u8]) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = run.join(format!("handover-{}", std::process::id()));
    match std::fs::remove_file(&path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    file.write_all(bytes)?;
    Ok(file)
}

/// Runs `binary holder <arg>`, with `stdin` on its standard input, and
/// gives its standard output when it exits 0 in time.
async fn run_child(
    binary: &Path,
    arg: &str,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut child = tokio::process::Command::new(binary)
        .arg0("tiller")
        .args(["holder", arg])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("{} did not run: {err}", binary.display()))?;
    let pipe = child.stdin.take();
    let feed = async move {
        if let (Some(mut pipe), Some(bytes)) = (pipe, stdin) {
            // A child that exits early closes the pipe: its exit says why.
            let _ = pipe.write_all(&bytes).await;
        }
    };
    let run = async { tokio::join!(feed, child.wait_with_output()).1 };
    let out = tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| format!("{} {arg} gave no answer in {timeout:?}", binary.display()))?
        .map_err(|err| format!("{} {arg}: {err}", binary.display()))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let said: String = said.trim().chars().take(REASON_MAX).collect();
        return Err(format!("{} {arg} {}: {said}", binary.display(), out.status));
    }
    Ok(out.stdout)
}

/// The new program image's half: the record behind `fd`, and the fds it
/// names, now this image's own, with close-on-exec set on each again.
#[derive(Debug)]
pub struct Resumed {
    pub record: Record,
    pub listener: OwnedFd,
    pub lock: OwnedFd,
    pub masters: Vec<OwnedFd>,
    pub requester: Option<(OwnedFd, u64)>,
}

/// Why the new image cannot resume. It has nothing to run on, so it
/// exits.
#[derive(Debug, thiserror::Error)]
pub enum ResumeError {
    #[error("the handover fd {0}: {1}")]
    Fd(i32, io::Error),
    #[error("the handover record: {0}")]
    Record(#[from] RecordError),
    #[error("the handover record could not be read: {0}")]
    Read(io::Error),
}

/// Reads the record behind `fd` and takes the fds it names.
pub fn resume(fd: i32) -> Result<Resumed, ResumeError> {
    let mut file = File::from(take_fd(fd)?);
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0)).map_err(ResumeError::Read)?;
    Read::by_ref(&mut file)
        .take(RECORD_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(ResumeError::Read)?;
    if bytes.len() > RECORD_MAX {
        return Err(RecordError::Limit("record").into());
    }
    let record = Record::decode(&bytes)?;
    drop(file);

    let mut named: Vec<i32> = [record.listener, record.lock]
        .into_iter()
        .chain(record.panes.iter().map(|pane| pane.master))
        .chain(record.requester.map(|requester| requester.fd))
        .collect();
    named.sort_unstable();
    if let Some(twice) = named.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(RecordError::Unfit(format!("the record names fd {} twice", twice[0])).into());
    }
    let listener = take_fd(record.listener)?;
    let lock = take_fd(record.lock)?;
    let masters = record
        .panes
        .iter()
        .map(|pane| take_fd(pane.master))
        .collect::<Result<_, _>>()?;
    let requester = match record.requester {
        Some(requester) => Some((take_fd(requester.fd)?, requester.id)),
        None => None,
    };
    Ok(Resumed {
        record,
        listener,
        lock,
        masters,
        requester,
    })
}

/// Takes an fd the old image handed over, and sets close-on-exec on it.
fn take_fd(fd: i32) -> Result<OwnedFd, ResumeError> {
    if fd < 3 {
        return Err(ResumeError::Fd(
            fd,
            io::Error::other("not a handed over fd"),
        ));
    }
    #[allow(unsafe_code)]
    // SAFETY: F_GETFD reads one flag of an fd number, and is sound for any
    // number: one that is not open is EBADF.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(ResumeError::Fd(fd, io::Error::last_os_error()));
    }
    #[allow(unsafe_code)]
    // SAFETY: the fd is open (above), and nothing in this image owns it:
    // the old image handed it over across `execve`, the record names each
    // fd once, and `resume` takes each once before anything else opens
    // one.
    let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC)
        .map_err(|err| ResumeError::Fd(fd, err.into()))?;
    Ok(owned)
}

/// Sends the requester `upgrading` and `upgraded` from the new image,
/// then closes its connection: every connection ends at the upgrade.
pub fn answer(socket: OwnedFd, id: u64) {
    use tiller_panes::message::HolderMessage;
    let mut stream = std::os::unix::net::UnixStream::from(socket);
    let frames = [
        HolderMessage::Upgrading { id },
        HolderMessage::Upgraded {
            holder: env!("CARGO_PKG_VERSION").to_owned(),
        },
    ];
    let sent = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_write_timeout(Some(ANSWER_WRITE)))
        .and_then(|()| {
            for frame in frames.iter().filter_map(crate::holder::message_frame) {
                stream.write_all(&frame)?;
            }
            stream.shutdown(std::net::Shutdown::Both)
        });
    if let Err(err) = sent {
        tracing::warn!(%err, "the upgrade's requester was not answered");
    }
}
