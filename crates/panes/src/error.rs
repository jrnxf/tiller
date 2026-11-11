//! The error every [`Panes`](crate::Panes) call and the client return
//! (`terminal-holder.md` 4.2).

use std::io;

use crate::frame::FrameError;
use crate::handshake::ProtoRange;
use crate::message::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum PanesError {
    /// The holder has no such pane.
    #[error("the terminal holder has no pane: {0}")]
    NotFound(String),

    /// The pane's child has exited, so the request cannot apply.
    #[error("the pane has exited: {0}")]
    Exited(String),

    /// The holder did not answer: no socket, a dead socket, a closed
    /// connection or no reply in time. It means "unknown", never "gone":
    /// nothing about the panes follows from it.
    #[error("the terminal holder is unreachable: {0}")]
    Unreachable(String),

    /// The holder answered the request with an error, or this client did
    /// not send a request the holder would refuse (one over the frame
    /// limit). The connection stays fit for the next request.
    #[error("{request} refused: {code}: {message}")]
    Refused {
        request: &'static str,
        code: ErrorCode,
        message: String,
    },

    /// The handshake found no protocol version both sides speak.
    #[error(
        "protocol version mismatch: the terminal holder speaks {holder} and this tiller speaks {client}; upgrade the older of the two"
    )]
    Version {
        client: ProtoRange,
        holder: ProtoRange,
    },

    /// The holder sent something the protocol does not allow.
    #[error("the terminal holder broke the protocol: {0}")]
    Protocol(String),

    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type Result<T, E = PanesError> = std::result::Result<T, E>;

impl PanesError {
    /// The error for a frame that could not be read or written on a
    /// connection: the transport failing is `Unreachable`, a bad frame is
    /// `Protocol`.
    pub(crate) fn from_frame(err: FrameError) -> Self {
        match err {
            FrameError::Io(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                PanesError::Unreachable("no answer in time".into())
            }
            FrameError::Io(err) => PanesError::Unreachable(err.to_string()),
            err @ FrameError::Truncated { .. } => PanesError::Unreachable(err.to_string()),
            err => PanesError::Protocol(err.to_string()),
        }
    }

    /// The error for an `error` reply to `request`.
    pub(crate) fn from_reply(request: &'static str, code: ErrorCode, message: String) -> Self {
        match code {
            ErrorCode::NotFound => PanesError::NotFound(message),
            ErrorCode::Exited => PanesError::Exited(message),
            code => PanesError::Refused {
                request,
                code,
                message,
            },
        }
    }
}
