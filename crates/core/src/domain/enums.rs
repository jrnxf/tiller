//! Every enumerated column in `schema.sql` as a Rust enum. The text forms are
//! exactly the values the `CHECK` constraints allow.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok($name::$variant),)+
                    _ => Err(Error::Usage(format!(
                        concat!("not a valid ", stringify!($name), ": {:?} (expected one of: {})"),
                        s,
                        [$($text),+].join(", ")
                    ))),
                }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                value
                    .as_str()?
                    .parse()
                    .map_err(|error: Error| FromSqlError::Other(Box::new(error)))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

text_enum!(HostKind { Local = "local", Remote = "remote" });

text_enum!(
    /// What a pairing allows the board host to do on a peer (ADR 0003): read
    /// it, or also forward the owner's mutations to it.
    PeerScope { Read = "read", Act = "act" }
);

text_enum!(
    /// A project's delivery mode (P10, P11).
    Delivery {
        NoMistakes = "no-mistakes",
        NoMistakesProdOnly = "no-mistakes-prod-only",
        DirectPr = "direct-pr",
        DirectPush = "direct-push",
        LocalOnly = "local-only",
    }
);

text_enum!(
    /// A project's stage. A pre-launch project skips the `no-mistakes`
    /// pipeline and gets a weekly batched review instead
    /// (`docs/design/pre-launch-validation.md`).
    Stage {
        PreLaunch = "pre-launch",
        Launched = "launched",
    }
);

impl Stage {
    /// The delivery mode builders and the land loop follow: a pre-launch
    /// project with a `no-mistakes` mode delivers `direct-pr`; every other
    /// pair keeps `delivery`.
    pub fn effective_delivery(self, delivery: Delivery) -> Delivery {
        match (self, delivery) {
            (Stage::PreLaunch, Delivery::NoMistakes | Delivery::NoMistakesProdOnly) => {
                Delivery::DirectPr
            }
            _ => delivery,
        }
    }
}

text_enum!(ReviewPassKind { Weekly = "weekly", Final = "final" });

text_enum!(ReviewPassState {
    Pending = "pending",
    Running = "running",
    Finished = "finished",
    Abandoned = "abandoned",
});

text_enum!(MergeGate { Enforced = "enforced", Detect = "detect" });

text_enum!(CardKind {
    Ship = "ship",
    Scout = "scout",
    Question = "question",
    Ops = "ops",
});

text_enum!(CardState { Open = "open", Closed = "closed" });

text_enum!(
    /// How a card closed (`state.md` section 7).
    CardOutcome {
        Landed = "landed",
        Reported = "reported",
        Answered = "answered",
        Completed = "completed",
        Cancelled = "cancelled",
        Failed = "failed",
        Abandoned = "abandoned",
        Superseded = "superseded",
    }
);

text_enum!(Size { S = "S", M = "M", L = "L" });

text_enum!(
    /// Every session role, including the orchestrator.
    Role {
        Orchestrator = "orchestrator",
        Scout = "scout",
        Builder = "builder",
        Reviewer = "reviewer",
        Verifier = "verifier",
    }
);

text_enum!(
    /// The roles a run can have: every role but the orchestrator.
    RunRole {
        Scout = "scout",
        Builder = "builder",
        Reviewer = "reviewer",
        Verifier = "verifier",
    }
);

text_enum!(Effort {
    Low = "low",
    Medium = "medium",
    High = "high",
    Xhigh = "xhigh",
    Max = "max",
});

text_enum!(RunOutcome {
    Landed = "landed",
    Reported = "reported",
    Failed = "failed",
    Abandoned = "abandoned",
    Relaunched = "relaunched",
    Cancelled = "cancelled",
    Cut = "cut",
});

text_enum!(
    /// A session's activity verdict. `stale` is never stored; it is derived.
    Activity {
        Working = "working",
        Idle = "idle",
        WaitingPermission = "waiting_permission",
        WaitingInput = "waiting_input",
        Errored = "errored",
        Ended = "ended",
        Unknown = "unknown",
    }
);

text_enum!(AskSource {
    Prompt = "prompt",
    Board = "board",
    Script = "script",
    Import = "import",
});

text_enum!(Disposition {
    Unresolved = "unresolved",
    New = "new",
    Followup = "followup",
    Chat = "chat",
    Cancelled = "cancelled",
});

text_enum!(LinkRelation { Created = "created", Followup = "followup" });

text_enum!(DepReleaseReason {
    Landed = "landed",
    Reported = "reported",
    Answered = "answered",
    Completed = "completed",
    Removed = "removed",
});

text_enum!(HoldReleaseReason {
    Due = "due",
    Answered = "answered",
    Removed = "removed",
});

text_enum!(AttentionKind {
    Decision = "decision",
    Credential = "credential",
    Merge = "merge",
    Review = "review",
    Recovery = "recovery",
    Report = "report",
});

text_enum!(
    /// Why the owner was needed, on the Health page (build-plan T7.1). The
    /// first three are the avoidable interventions; `planned` is the routine
    /// approvals and briefings that are not.
    InterventionKind {
        Correction = "correction",
        UnplannedEscalation = "unplanned_escalation",
        Rework = "rework",
        Planned = "planned",
    }
);

text_enum!(AnsweredVia { Board = "board", Chat = "chat", Cli = "cli" });

text_enum!(PrState { Open = "open", Merged = "merged", Closed = "closed" });

text_enum!(Checks {
    Pending = "pending",
    Success = "success",
    Failure = "failure",
    None = "none",
});

text_enum!(ReviewDecision {
    Approved = "approved",
    ChangesRequested = "changes_requested",
    None = "none",
});

text_enum!(Mergeable {
    Clean = "clean",
    Dirty = "dirty",
    Blocked = "blocked",
    Unknown = "unknown",
});

text_enum!(PipelineState {
    None = "none",
    Running = "running",
    Passed = "passed",
    Failed = "failed",
});

text_enum!(GrantKind { Merge = "merge", LandDirect = "land_direct" });

text_enum!(ApprovalKind {
    Merge = "merge",
    LandDirect = "land_direct",
    Memory = "memory",
    Away = "away",
});

text_enum!(ApprovalAction {
    Merge = "merge",
    Squash = "squash",
    Rebase = "rebase",
    Push = "push",
    Apply = "apply",
    Away = "away",
});

text_enum!(ApprovalState {
    Requested = "requested",
    Accepted = "accepted",
    Rejected = "rejected",
    Executing = "executing",
    Succeeded = "succeeded",
    Failed = "failed",
    Uncertain = "uncertain",
    Stale = "stale",
});

text_enum!(ReservationRelease {
    Ended = "ended",
    SpawnFailed = "spawn_failed",
    Reconciled = "reconciled",
});

text_enum!(OutboxKind {
    Wake = "wake",
    Steer = "steer",
    Answer = "answer",
    Note = "note",
});

text_enum!(OutboxState {
    Pending = "pending",
    Delivered = "delivered",
    Acked = "acked",
    Failed = "failed",
});

text_enum!(ObservationSource {
    Hook = "hook",
    Transcript = "transcript",
    Herdr = "herdr",
    Liveness = "liveness",
    StatusLine = "status_line",
});

text_enum!(TriggerKind {
    Event = "event",
    Date = "date",
    Standing = "standing",
});

text_enum!(ObligationState {
    Open = "open",
    Met = "met",
    Cancelled = "cancelled",
});

text_enum!(ArtifactKind {
    Page = "page",
    Screenshot = "screenshot",
    Report = "report",
    Video = "video",
    Other = "other",
});

text_enum!(Reviewer {
    CodeReview = "code-review",
    SpecReview = "spec-review",
    NoMistakes = "no-mistakes",
    Human = "human",
});

text_enum!(Coverage { Diff = "diff", Spec = "spec", Full = "full" });

text_enum!(Verdict {
    Pass = "pass",
    Fail = "fail",
    AskUser = "ask-user",
});

text_enum!(
    /// The state word of a worker's `tiller status` line.
    StatusState {
        Working = "working",
        NeedsDecision = "needs-decision",
        Blocked = "blocked",
        Paused = "paused",
        Done = "done",
        Failed = "failed",
    }
);

text_enum!(
    /// The result of a deploy watch (`state.md` section 7).
    DeployResult {
        Success = "success",
        Failed = "failed",
        Cancelled = "cancelled",
        Superseded = "superseded",
        Missing = "missing",
        TimedOut = "timed_out",
    }
);

text_enum!(
    /// The board column a card is in (`state.md` section 6). Never stored.
    Column {
        Asked = "asked",
        Queued = "queued",
        Working = "working",
        NeedsYou = "needs_you",
        Landing = "landing",
        Done = "done",
    }
);

text_enum!(Boundary { Personal = "personal", Work = "work" });

impl From<RunRole> for Role {
    fn from(role: RunRole) -> Self {
        match role {
            RunRole::Scout => Role::Scout,
            RunRole::Builder => Role::Builder,
            RunRole::Reviewer => Role::Reviewer,
            RunRole::Verifier => Role::Verifier,
        }
    }
}

impl RunRole {
    /// The short role name used in herdr agent names and run addresses
    /// (`protocol.md` section 6.1).
    pub fn short_name(self) -> &'static str {
        match self {
            RunRole::Scout => "scout",
            RunRole::Builder => "build",
            RunRole::Reviewer => "review",
            RunRole::Verifier => "verify",
        }
    }

    /// Parses a short role name or a full role name.
    pub fn from_short_name(text: &str) -> Option<Self> {
        RunRole::ALL
            .iter()
            .copied()
            .find(|role| role.short_name() == text || role.as_str() == text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_forms_round_trip_through_sql_and_json() {
        for delivery in Delivery::ALL {
            assert_eq!(delivery.as_str().parse::<Delivery>().unwrap(), *delivery);
            let json = serde_json::to_string(delivery).unwrap();
            assert_eq!(serde_json::from_str::<Delivery>(&json).unwrap(), *delivery);
        }
        assert!("no_mistakes".parse::<Delivery>().is_err());
    }

    #[test]
    fn effective_delivery_table() {
        use Delivery::*;
        let table = [
            (Stage::PreLaunch, NoMistakes, DirectPr),
            (Stage::PreLaunch, NoMistakesProdOnly, DirectPr),
            (Stage::PreLaunch, DirectPr, DirectPr),
            (Stage::PreLaunch, DirectPush, DirectPush),
            (Stage::PreLaunch, LocalOnly, LocalOnly),
            (Stage::Launched, NoMistakes, NoMistakes),
            (Stage::Launched, NoMistakesProdOnly, NoMistakesProdOnly),
            (Stage::Launched, DirectPr, DirectPr),
            (Stage::Launched, DirectPush, DirectPush),
            (Stage::Launched, LocalOnly, LocalOnly),
        ];
        assert_eq!(table.len(), Stage::ALL.len() * Delivery::ALL.len());
        for (stage, delivery, effective) in table {
            assert_eq!(
                stage.effective_delivery(delivery),
                effective,
                "{stage} with {delivery}"
            );
        }
    }

    #[test]
    fn short_role_names_resolve() {
        assert_eq!(RunRole::from_short_name("build"), Some(RunRole::Builder));
        assert_eq!(
            RunRole::from_short_name("reviewer"),
            Some(RunRole::Reviewer)
        );
        assert_eq!(RunRole::from_short_name("orchestrator"), None);
    }
}
