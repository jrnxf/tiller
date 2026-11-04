//! The one error type every core function returns.
//!
//! Each variant maps to one exit code and one error `code` from `protocol.md`
//! section 6.2, so the CLI, the board API and the hooks report the same thing.

use serde_json::Value;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bad arguments: a value that does not parse, a missing field.
    #[error("{0}")]
    Usage(String),

    /// No such entity, or an ambiguous short id (candidates listed).
    #[error("{}", not_found_message(entity, id, candidates))]
    NotFound {
        entity: &'static str,
        id: String,
        candidates: Vec<String>,
    },

    /// `expected_rev` is stale (`state.md` section 8).
    #[error("{}", conflict_message(self))]
    Conflict {
        entity: &'static str,
        id: String,
        current_rev: i64,
        sent_rev: i64,
        /// The first field the caller meant to change, with its current value.
        field: Option<(String, Value)>,
    },

    /// A precondition failed; `fact` names it.
    #[error("{entity} {id}: {fact}")]
    Refused {
        entity: &'static str,
        id: String,
        fact: String,
    },

    /// The actor may not do this (`authority.md`).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The database stayed busy past `busy_timeout`; nothing was written.
    #[error("unavailable: {0}")]
    Unavailable(String),

    /// The database schema is newer than this binary (`recovery.md` R11).
    #[error(
        "schema {found} is newer than this tiller {binary}; upgrade tiller, or run \"tiller migrate down\" with the newer binary first"
    )]
    SchemaTooNew { found: usize, binary: String },

    /// A bug or an unreadable database.
    #[error("internal: {0}")]
    Internal(String),
}

impl Error {
    /// The stable error code from `protocol.md` section 6.2.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Usage(_) => "usage",
            Error::NotFound { .. } => "not_found",
            Error::Conflict { .. } => "conflict",
            Error::Refused { .. } => "refused",
            Error::Forbidden(_) => "forbidden",
            Error::Unavailable(_) => "unavailable",
            Error::SchemaTooNew { .. } | Error::Internal(_) => "internal",
        }
    }

    /// The process exit code from `protocol.md` section 6.2.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Internal(_) | Error::SchemaTooNew { .. } => 1,
            Error::Usage(_) => 2,
            Error::NotFound { .. } => 3,
            Error::Conflict { .. } => 4,
            Error::Refused { .. } => 5,
            Error::Forbidden(_) => 6,
            Error::Unavailable(_) => 7,
        }
    }

    pub(crate) fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Error::NotFound {
            entity,
            id: id.into(),
            candidates: Vec::new(),
        }
    }

    pub(crate) fn refused(
        entity: &'static str,
        id: impl Into<String>,
        fact: impl Into<String>,
    ) -> Self {
        Error::Refused {
            entity,
            id: id.into(),
            fact: fact.into(),
        }
    }
}

fn not_found_message(entity: &str, id: &str, candidates: &[String]) -> String {
    if candidates.is_empty() {
        format!("no {entity} {id}")
    } else {
        format!("{entity} {id} is ambiguous: {}", candidates.join(", "))
    }
}

fn conflict_message(error: &Error) -> String {
    let Error::Conflict {
        entity,
        id,
        current_rev,
        sent_rev,
        field,
    } = error
    else {
        return String::new();
    };
    let mut message =
        format!("conflict: {entity} {id} is at rev {current_rev}, you sent {sent_rev}");
    if let Some((name, value)) = field {
        let shown = match value {
            Value::String(text) => format!("{text:?}"),
            Value::Null => "null".to_owned(),
            other => other.to_string(),
        };
        message.push_str(&format!("; {name} is now {shown}"));
    }
    message
}

/// Starts the message of the [`Error::Unavailable`] a damaged database
/// produces, so the daemon can tell it apart from a busy one and run
/// `quick_check` (`recovery.md` R10).
pub const DAMAGED_PREFIX: &str = "state.db is damaged";

/// `<home>/read-only`: written by a daemon whose `state.db` failed its
/// integrity check, holding the reason. A hook that finds it spools its
/// observation instead of writing (`recovery.md` R10).
pub const READ_ONLY_MARKER: &str = "read-only";

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode;
        match error.sqlite_error_code() {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Error::Unavailable(format!("{DAMAGED_PREFIX}: {error}"))
            }
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
                Error::Unavailable(format!("database busy: {error}"))
            }
            // Preconditions are checked inside BEGIN IMMEDIATE before any write,
            // so a constraint that still fires is a precondition the caller broke
            // in a way the checks did not name. Nothing was written.
            Some(ErrorCode::ConstraintViolation) => Error::Refused {
                entity: "row",
                id: String::new(),
                fact: error.to_string(),
            },
            _ => Error::Internal(error.to_string()),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Internal(format!("json: {error}"))
    }
}

impl From<rusqlite_migration::Error> for Error {
    fn from(error: rusqlite_migration::Error) -> Self {
        match error {
            rusqlite_migration::Error::RusqliteError { err, .. } => err.into(),
            // a hook refuses a step whose precondition the rows do not meet;
            // any other hook error is a bug or a damaged schema
            rusqlite_migration::Error::Hook(fact) => {
                match fact.strip_prefix(crate::migrate::HOOK_REFUSAL) {
                    Some(fact) => Error::refused("schema", "migration", fact),
                    None => Error::Internal(format!("migration: {fact}")),
                }
            }
            other => Error::Internal(format!("migration: {other}")),
        }
    }
}
