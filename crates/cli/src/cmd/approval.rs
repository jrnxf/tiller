//! Approvals, landing, grants and away mode (`protocol.md` section 6.4,
//! Attention and approvals). `tiller land` needs the daemon and waits for it.

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_agents::handoff::ReturnBrief;
use tiller_agents::land::DaemonLock;
use tiller_core::domain::{
    AnsweredVia, Approval, ApprovalAction, ApprovalKind, ApprovalState, Checks, GrantKind, Pr,
    Role, Run, Worktree,
};
use tiller_core::state::approvals::{Decision, NewApproval, NewGrant};
use tiller_core::state::{find, get, list, resolve_card, resolve_id};
use tiller_core::{Actor, Ctx, Store, Timestamp};

use super::KeyArg;
use super::attention::chat_ask;
use super::summary;
use crate::app::{App, Done, Failure, Outcome};
use crate::lisbon;

#[derive(Subcommand, Debug)]
pub enum Request {
    /// Ask the owner to approve landing a card
    Request(RequestArgs),
}

#[derive(Args, Debug)]
pub struct RequestArgs {
    /// The card: a display id like alpha-42, a ULID or its short form
    pub card: String,
    /// merge or land_direct
    #[arg(long)]
    pub kind: ApprovalKind,
    /// The PR number, for a merge; the card's only open PR when omitted
    #[arg(long)]
    pub pr: Option<i64>,
    /// merge, squash or rebase for a merge; push for land_direct
    #[arg(long)]
    pub action: Option<ApprovalAction>,
}

#[derive(Args, Debug)]
pub struct Approve {
    /// An approval (a ULID or its short form), or a card (display id or id):
    /// a card names its latest pending merge approval
    pub approval: String,
    /// Reject instead of accepting
    #[arg(long)]
    pub reject: bool,
    /// The head you reviewed, 7 or more hex characters. An accept needs it;
    /// a different bound head means the approval is stale. A reject does not
    #[arg(long, conflicts_with = "ask")]
    pub sha: Option<String>,
    /// Record the owner's word in the orchestrator chat, citing the ask
    #[arg(long)]
    pub ask: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Land {
    /// An approval, or a card (display id or id): a card lands its latest
    /// pending merge approval
    pub approval: String,
    /// Seconds to wait for the daemon before giving up (the approval stays
    /// accepted and the daemon keeps trying)
    #[arg(long, hide = true, default_value_t = LAND_WAIT_SECS)]
    pub wait: u64,
}

#[derive(Subcommand, Debug)]
pub enum Grant {
    /// A standing decision to land matching work (owner only)
    Create(GrantCreate),
    /// Every grant of a project, newest first, live and ended alike, each
    /// with the number of approvals it accepted
    List {
        /// The project's id (its slug, like alpha)
        project: String,
    },
    /// End a grant
    Revoke {
        /// The grant: a ULID or its short form
        grant: String,
        /// Why the grant ends
        #[arg(long, default_value = "revoked by the owner")]
        reason: String,
    },
}

#[derive(Args, Debug)]
pub struct GrantCreate {
    /// The project's id (its slug, like alpha)
    pub project: String,
    /// merge or land_direct
    #[arg(long)]
    pub kind: GrantKind,
    /// The grant's conditions as a JSON object (`authority.md`, section 6)
    #[arg(long, default_value = "{}")]
    pub conditions: String,
    /// When the grant ends
    #[arg(long)]
    pub expires: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum Away {
    /// Turn away mode on until a date (owner only)
    Start(AwayStart),
    /// Turn away mode off, and print the return brief
    End {
        /// Why away mode ends
        #[arg(long, default_value = "ended by the owner")]
        reason: String,
    },
    /// The return brief: what the contract allowed, spent, shipped and left waiting
    Brief {
        /// The away contract; the newest when omitted
        approval: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct AwayStart {
    /// When away mode ends: 2026-11-07 (09:00 Lisbon), 2026-11-07T14:00 (Lisbon) or an RFC 3339
    /// time with an offset
    #[arg(long)]
    pub until: String,
    /// The projects the away scope covers
    #[arg(long, value_delimiter = ',', required = true)]
    pub projects: Vec<String>,
    /// What the merge authority is while away: `none` (no merges, even under
    /// existing grants), `grants` (existing grants only), or grant conditions
    /// as JSON (also creates a temporary grant per project)
    #[arg(long)]
    pub merge: Option<String>,
    /// Standing orders the orchestrators read while you are away
    #[arg(long)]
    pub orders: Option<String>,
    /// Spend cap: no run starts once this many have started since you left
    #[arg(long)]
    pub max_new_runs: Option<i64>,
    /// Spend cap: the weekly quota floor while away, when stricter than the standing one
    #[arg(long)]
    pub quota_floor: Option<f64>,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run_request(app: &App, cmd: Request) -> Done {
    let Request::Request(args) = cmd;
    request(app, args)
}

fn request(app: &App, args: RequestArgs) -> Done {
    let mut store = app.write()?;
    let card = resolve_card(store.conn(), &args.card)?;
    let (pr_id, branch, head_sha, run_id, default_action) = match args.kind {
        ApprovalKind::Merge => {
            let prs: Vec<Pr> = list(
                store.conn(),
                "card_id = ?1 AND state = 'open' ORDER BY number",
                &[&card.id],
            )?;
            let pr = match args.pr {
                Some(number) => prs.into_iter().find(|pr| pr.number == number),
                None if prs.len() == 1 => prs.into_iter().next(),
                None => {
                    return Err(Failure::usage(format!(
                        "{} has {} open PRs; name one with --pr <n>",
                        card.display_id,
                        prs.len()
                    )));
                }
            }
            .ok_or_else(|| Failure::refused(format!("{} has no such open PR", card.display_id)))?;
            let head = pr.head_sha.clone().ok_or_else(|| {
                Failure::refused(format!(
                    "PR #{} has no cached head yet; the forge poller has not read it",
                    pr.number
                ))
            })?;
            (Some(pr.id), None, head, pr.run_id, ApprovalAction::Squash)
        }
        ApprovalKind::LandDirect => {
            let run: Run = find(
                store.conn(),
                "card_id = ?1 AND role = 'builder' ORDER BY attempt DESC",
                &[&card.id],
            )?
            .ok_or_else(|| {
                Failure::refused(format!("{} has no builder run to land", card.display_id))
            })?;
            let branch = run
                .branch
                .clone()
                .ok_or_else(|| Failure::refused(format!("run {} has no branch", run.id)))?;
            let worktree: Option<Worktree> = match &run.worktree_path {
                Some(path) => find(
                    store.conn(),
                    "host_id = ?1 AND path = ?2 AND gone_at IS NULL",
                    &[&run.host_id, path],
                )?,
                None => None,
            };
            let head = worktree
                .and_then(|worktree| worktree.head_sha)
                .ok_or_else(|| {
                    Failure::refused(format!("no cached head for the worktree of run {}", run.id))
                })?;
            (None, Some(branch), head, Some(run.id), ApprovalAction::Push)
        }
        other => {
            return Err(Failure::usage(format!(
                "a {other} approval is not requested with this command (use merge or land_direct)"
            )));
        }
    };
    let applied = store.approval_request(
        &app.ctx(),
        NewApproval {
            kind: args.kind,
            card: card.id,
            run_id,
            pr_id,
            branch,
            head_sha,
            action: args.action.unwrap_or(default_action),
            memory_paths: None,
            grant_id: None,
        },
    )?;
    Ok(Outcome::applied(applied)
        .with_help("the owner decides with: tiller approve <approval> --sha <head you reviewed>"))
}

pub fn approve(app: &App, args: Approve) -> Done {
    let mut store = app.write()?;
    let id = resolve_land_target(store.conn(), &args.approval)?;
    let approval: Approval = get(store.conn(), "approval", &id)?;
    let about = About::of(store.conn(), &approval);
    let sha_cited = args.sha.clone();
    if let Some(sha) = &sha_cited
        && (sha.len() < 7 || !sha.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Failure::usage(
            "--sha is at least 7 hex characters of the head you reviewed",
        ));
    }
    if let Some(sha) = &sha_cited
        && approval
            .head_sha
            .as_deref()
            .is_none_or(|bound| !bound.starts_with(sha.as_str()))
    {
        return Err(Failure::refused(format!(
            "approval {} is stale: it binds {}, not {sha}",
            approval.id,
            approval.head_sha.as_deref().unwrap_or("no head")
        ))
        .hint("read the approval again and decide on its current head"));
    }
    let normalized =
        json!({"approval": id, "reject": args.reject, "sha": args.sha, "ask": args.ask});
    let keyed = app.key(&store, "approval.decide", &normalized, &args.key.key)?;
    let decision = Decision {
        accept: !args.reject,
        via: if args.ask.is_some() {
            AnsweredVia::Chat
        } else {
            AnsweredVia::Cli
        },
        memory: Vec::new(),
        ask_id: None,
    };
    let (ctx, cited) = match &args.ask {
        None => {
            app.require_owner("tiller approve")?;
            // The owner decides on the head they saw, as the board does.
            if !args.reject && args.sha.is_none() {
                return Err(Failure::usage(
                    "an accept needs --sha <head>: pass the head you reviewed (7 or more hex characters)",
                )
                .hint(about.review_step()));
            }
            (app.ctx(), None)
        }
        Some(reference) => {
            // The chat route (G06): the orchestrator cites the ask in which the
            // owner said it, and the decision is recorded as the owner's.
            let caller = app
                .session(&store)?
                .filter(|session| session.role == Role::Orchestrator)
                .ok_or_else(|| {
                    Failure::forbidden("approve --ask is run by the project's orchestrator")
                })?;
            if caller.project_id != approval.project_id {
                return Err(Failure::forbidden(format!(
                    "the orchestrator of {} may not decide an approval of {}",
                    caller.project_id, approval.project_id
                )));
            }
            let ask = chat_ask(&store, app, reference)?;
            if ask.received_at <= approval.requested_at {
                return Err(Failure::refused(format!(
                    "ask {} is older than the approval request; the owner must say it after the request",
                    ask.id
                )));
            }
            (
                Ctx::new(Actor::Owner).with_args(&keyed.key, &normalized),
                Some(ask),
            )
        }
    };
    let decision = Decision {
        ask_id: cited.as_ref().map(|ask| ask.id.clone()),
        ..decision
    };
    let applied = store.approval_decide(&ctx, Some(&keyed.key), &id, decision)?;
    let outcome = Outcome::applied_as(applied, |decided| {
        json!({
            "approval": decided.approval,
            "memory_approvals": decided.memory_approvals,
            "quoted_ask": cited.as_ref().map(|ask| json!({"id": ask.id, "text": ask.text})),
        })
    })
    .with_key(&keyed);
    if args.reject {
        return Ok(if app.person() {
            outcome.with_summary(about.rejected())
        } else {
            outcome
        });
    }
    if !app.person() {
        return Ok(outcome.with_help("land it with: tiller land <approval>"));
    }
    let daemon = DaemonLock::held(&app.home);
    Ok(outcome
        .with_summary(about.approved(daemon))
        .with_help(format!(
            "tiller land {}: wait for the merge and print its result",
            about.name()
        )))
}

/// What an approval is about, in the owner's words: the card, the pull
/// request and the head it binds.
struct About {
    approval: String,
    card: Option<(String, String)>,
    pr: Option<(i64, String)>,
    head: Option<String>,
    /// The pull request's checks, as the daemon last read them: a failure
    /// lands nothing at this head, and a pending run makes the land wait.
    checks: Option<Checks>,
}

impl About {
    fn of(conn: &rusqlite::Connection, approval: &Approval) -> Self {
        let card = approval
            .card_id
            .as_deref()
            .and_then(|id| get::<tiller_core::domain::Card>(conn, "card", id).ok())
            .map(|card| (card.display_id, card.title));
        let pr = approval
            .pr_id
            .as_deref()
            .and_then(|id| get::<Pr>(conn, "pr", id).ok());
        let checks = pr.as_ref().map(|pr| pr.checks);
        let pr = pr.map(|pr| (pr.number, pr.url));
        Self {
            approval: approval.id.clone(),
            card,
            pr,
            checks,
            head: approval
                .head_sha
                .as_deref()
                .map(|sha| sha.chars().take(12).collect()),
        }
    }

    /// What the owner types for it: the card's id, else the approval's.
    fn name(&self) -> &str {
        self.card
            .as_ref()
            .map_or(self.approval.as_str(), |(id, _)| id.as_str())
    }

    /// "harbor-1 (Add rate limits)", or the approval's id with no card.
    fn subject(&self) -> String {
        match &self.card {
            Some((id, title)) => format!("{id} ({})", summary::quote(title)),
            None => format!("approval {}", self.approval),
        }
    }

    /// The hint of an accept with no `--sha`: where to look, then the command.
    fn review_step(&self) -> String {
        let head = self.head.as_deref().unwrap_or("<head>");
        let look = match &self.pr {
            Some((number, url)) => format!("It binds {head} of PR #{number}: {url}"),
            None => format!("It binds {head}"),
        };
        format!(
            "{look}\nAfter you review that head: tiller approve {} --sha {head}\nTo turn it down: tiller approve {} --reject",
            self.name(),
            self.name()
        )
    }

    fn approved(&self, daemon: bool) -> String {
        let head = self.head.as_deref().unwrap_or("its head");
        let what = match &self.pr {
            Some((number, _)) => format!("PR #{number} of {}", self.subject()),
            None => self.subject(),
        };
        if self.checks == Some(Checks::Failure) {
            // The land sequence ends an approval whose checks failed
            // (`authority.md` section 6, step 3), so say it before it happens.
            format!(
                "Approved {what} at {head}, but its checks fail. Nothing lands: the daemon ends this approval unused. After the checks pass, the orchestrator asks again."
            )
        } else if daemon && self.checks == Some(Checks::Pending) {
            format!("Approved {what} at {head}. The daemon lands it once its checks pass.")
        } else if daemon {
            format!("Approved {what} at {head}. The daemon lands it now.")
        } else {
            format!(
                "Approved {what} at {head}. Nothing lands yet: the daemon is not running (tiller daemon)."
            )
        }
    }

    fn rejected(&self) -> String {
        format!(
            "Rejected the merge of {}. Nothing lands, and its orchestrator hears it.",
            self.subject()
        )
    }

    /// Why nothing can be decided or landed: what became of the newest
    /// approval, and who moves next.
    fn ended(&self, conn: &rusqlite::Connection, state: ApprovalState) -> String {
        let head = self.head.as_deref().unwrap_or("its head");
        let what = match &self.pr {
            Some((number, _)) => format!("PR #{number} at {head}"),
            None => head.to_owned(),
        };
        match state {
            ApprovalState::Stale => {
                let why = stale_reason(conn, &self.approval)
                    .map(|why| format!(" ({why})"))
                    .unwrap_or_default();
                format!(
                    "the approval of {what} ended before the merge{why}. Nothing merged. The orchestrator asks for a new approval when the pull request is ready"
                )
            }
            ApprovalState::Failed => {
                let why = failed_reason(conn, &self.approval)
                    .map(|why| {
                        let why = why.trim().trim_end_matches('.');
                        format!(": {}", why.strip_prefix("gh: ").unwrap_or(why))
                    })
                    .unwrap_or_default();
                format!(
                    "the merge of {what} failed{why}. Nothing merged. The item about it is in tiller attention list --open: answer it, and the orchestrator fixes the pull request and asks again"
                )
            }
            ApprovalState::Rejected => format!("the approval of {what} was rejected"),
            ApprovalState::Succeeded => format!("{what} has landed"),
            other => format!("the approval of {what} is {other}"),
        }
    }

    fn landed(&self, sha: Option<&str>) -> String {
        let what = match &self.pr {
            Some((number, _)) => format!("PR #{number} of {}", self.subject()),
            None => self.subject(),
        };
        match sha {
            Some(sha) => format!(
                "Landed {what}: merged as {}.",
                sha.chars().take(12).collect::<String>()
            ),
            None => format!("Landed {what}."),
        }
    }
}

/// The reason the daemon gave when it marked an approval stale ("checks
/// failed", "head moved to ..."), from the event that did it.
fn stale_reason(conn: &rusqlite::Connection, approval: &str) -> Option<String> {
    conn.query_row(
        "SELECT json_extract(payload, '$.reason') FROM events
         WHERE entity = 'approval' AND entity_id = ?1 AND kind = 'approval.stale'
         ORDER BY seq DESC LIMIT 1",
        [approval],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// The forge's or git's own words for a landing that failed, as the land
/// sequence stored them on the approval.
fn failed_reason(conn: &rusqlite::Connection, approval: &str) -> Option<String> {
    conn.query_row(
        "SELECT json_extract(result, '$.error') FROM approvals WHERE id = ?1",
        [approval],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// `tiller land`: the owner asks the daemon to land and waits for the result.
/// It never merges in this process; the daemon lands every `accepted`
/// approval on its own, so asking is waiting while the daemon holds its lock
/// (`authority.md` section 6, G01).
pub fn land(app: &App, args: Land) -> Done {
    app.require_owner("tiller land")?;
    let store = app.read()?;
    let id = resolve_land_target(store.conn(), &args.approval)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(args.wait);
    let mut shown = std::collections::HashSet::new();
    loop {
        let approval: Approval = get(store.conn(), "approval", &id)?;
        match approval.state {
            ApprovalState::Succeeded => {
                let sha = approval
                    .result
                    .as_ref()
                    .and_then(|result| result.get("sha"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let line = About::of(store.conn(), &approval).landed(sha.as_deref());
                return Ok(Outcome::read(json!({"approval": approval})).with_summary(line));
            }
            ApprovalState::Accepted | ApprovalState::Executing => {
                if !DaemonLock::held(&app.home) {
                    return Err(Failure::unavailable(format!(
                        "tiller land {} needs the daemon, and the daemon is not running",
                        approval.id
                    ))
                    .hint("start it with: tiller daemon; the approval stays accepted"));
                }
                // What the daemon waits on, said once each on stderr (stdout
                // carries only the response).
                let notices = land_notices(&store, &approval.id)?;
                for (attention, question) in &notices {
                    if shown.insert(attention.clone()) {
                        eprintln!("tiller land: waiting: {question} (attention {attention})");
                    }
                }
                if std::time::Instant::now() >= deadline {
                    let mut failure = Failure::unavailable(format!(
                        "approval {} is still {} after {} s; the daemon keeps trying",
                        approval.id, approval.state, args.wait
                    ))
                    .hint(if notices.is_empty() {
                        format!("run tiller land {} again later", approval.id)
                    } else {
                        "answer the open attention notices; the daemon lands it after".to_owned()
                    });
                    failure.current = Some(json!({
                        "approval": approval,
                        "waiting_on": notices
                            .iter()
                            .map(|(id, question)| json!({"attention": id, "question": question}))
                            .collect::<Vec<_>>(),
                    }));
                    return Err(failure);
                }
                std::thread::sleep(LAND_POLL);
            }
            ApprovalState::Uncertain => {
                return Err(Failure::new(
                    "uncertain",
                    8,
                    format!(
                        "the land of {} was sent and its result could not be read",
                        approval.id
                    ),
                )
                .hint("reconcile resolves it from the forge; it is never retried blind"));
            }
            ApprovalState::Requested => {
                return Err(Failure::refused(format!(
                    "approval {} is requested; approve it first",
                    approval.id
                ))
                .hint(About::of(store.conn(), &approval).review_step()));
            }
            state @ (ApprovalState::Stale | ApprovalState::Failed) => {
                let about = About::of(store.conn(), &approval);
                return Err(Failure::refused(format!(
                    "{} did not land: {}",
                    about.name(),
                    about.ended(store.conn(), state)
                )));
            }
            other => {
                let why = approval
                    .result
                    .as_ref()
                    .and_then(|result| result.get("error"))
                    .and_then(Value::as_str)
                    .map(|error| format!(": {error}"))
                    .unwrap_or_default();
                return Err(Failure::refused(format!(
                    "approval {} is {other}{why}",
                    approval.id
                )));
            }
        }
    }
}

/// What `tiller land` or `tiller approve` was given: an approval id stays an
/// approval id; anything else that names a card is that card's latest pending
/// merge approval (one a decision or the daemon has not finished).
fn resolve_land_target(conn: &rusqlite::Connection, input: &str) -> Result<String, Failure> {
    let is_display = tiller_core::ids::parse_display_id(input.trim()).is_some();
    if !is_display {
        match resolve_id(conn, "approvals", "approval", input) {
            Ok(id) => return Ok(id),
            Err(tiller_core::Error::NotFound { candidates, .. }) if candidates.is_empty() => {}
            Err(other) => return Err(other.into()),
        }
    }
    let card = resolve_card(conn, input)?;
    let approvals: Vec<Approval> = list(
        conn,
        "card_id = ?1 AND kind = 'merge' ORDER BY requested_at DESC, id DESC",
        &[&card.id],
    )?;
    let pending = approvals.iter().find(|approval| {
        matches!(
            approval.state,
            ApprovalState::Requested
                | ApprovalState::Accepted
                | ApprovalState::Executing
                | ApprovalState::Uncertain
        )
    });
    if let Some(approval) = pending {
        return Ok(approval.id.clone());
    }
    // Nothing to decide: say what became of the newest one, so the owner
    // does not have to look it up on the board.
    Err(Failure::refused(match approvals.first() {
        // A merge that failed is the news, not the missing approval.
        Some(last) if last.state == ApprovalState::Failed => format!(
            "{} did not land: {}",
            card.display_id,
            About::of(conn, last).ended(conn, last.state)
        ),
        Some(last) => format!(
            "{} has no pending merge approval: {}",
            card.display_id,
            About::of(conn, last).ended(conn, last.state)
        ),
        None => format!("{} has no pending merge approval", card.display_id),
    }))
}

/// How often `tiller land` re-reads the approval while the daemon works.
const LAND_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How long `tiller land` waits for the daemon: CI on a large PR, but not
/// forever on a notice only the owner can answer.
const LAND_WAIT_SECS: u64 = 30 * 60;

/// The open notices the land sequence raised for `approval` (the scrub, a
/// missing App, failed checks): what the daemon waits on.
fn land_notices(store: &Store, approval: &str) -> Result<Vec<(String, String)>, Failure> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT id, question FROM attention
             WHERE kind = 'merge' AND answered_at IS NULL AND withdrawn_at IS NULL
               AND json_valid(evidence) AND json_extract(evidence, '$.approval_id') = ?1
             ORDER BY opened_at, id",
        )
        .map_err(tiller_core::Error::from)?;
    let rows = statement
        .query_map([approval], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(tiller_core::Error::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(tiller_core::Error::from)?;
    Ok(rows)
}

pub fn run_grant(app: &App, cmd: Grant) -> Done {
    app.require_owner("tiller grant")?;
    match cmd {
        Grant::List { project } => {
            let store = app.read()?;
            let grants = store.grants(&project)?;
            let empty = grants.is_empty();
            let outcome = Outcome::read(json!({"project": project, "grants": grants}));
            Ok(if empty {
                outcome.with_help(format!(
                    "no grant yet: `tiller grant create {project} --kind merge --conditions <json>`"
                ))
            } else {
                outcome
            })
        }
        Grant::Create(args) => {
            let mut store = app.write()?;
            let mut conditions: Value = serde_json::from_str(&args.conditions)?;
            if !conditions.is_object() {
                return Err(Failure::usage("--conditions is a JSON object"));
            }
            if let Some(expires) = &args.expires {
                conditions["expires_at"] = json!(lisbon::parse_when(expires)?);
            }
            let keyed = app.key(
                &store,
                "grant.create",
                &json!({"project": args.project, "kind": args.kind, "conditions": conditions}),
                &args.key.key,
            )?;
            let applied = store.grant_create(
                &app.ctx().with_key(&keyed.key),
                NewGrant {
                    project_id: args.project,
                    kind: args.kind,
                    conditions,
                },
            )?;
            Ok(Outcome::applied(applied).with_key(&keyed))
        }
        Grant::Revoke { grant, reason } => {
            let mut store = app.write()?;
            Ok(Outcome::applied(store.grant_revoke(
                &app.ctx(),
                &grant,
                &reason,
            )?))
        }
    }
}

pub fn run_away(app: &App, cmd: Away) -> Done {
    // The brief is for whoever picks the work back up; the owner alone turns
    // away mode on and off.
    match cmd {
        Away::Brief { approval } => {
            let store = app.read()?;
            let Some(brief) =
                ReturnBrief::load(store.conn(), approval.as_deref(), Timestamp::now())?
            else {
                return Ok(Outcome::read(json!({"brief": null})).with_help(
                    "no away contract yet: `tiller away start --until <date> --projects <ids>`",
                ));
            };
            Ok(Outcome::read(brief))
        }
        Away::Start(args) => {
            app.require_owner("tiller away")?;
            let mut store = app.write()?;
            let until = lisbon::parse_when(&args.until)?;
            let mut scope = json!({"until_at": until, "projects": args.projects});
            if let Some(orders) = &args.orders {
                scope["orders"] = json!(orders);
            }
            let mut spend = serde_json::Map::new();
            if let Some(cap) = args.max_new_runs {
                spend.insert("max_new_runs".into(), json!(cap));
            }
            if let Some(floor) = args.quota_floor {
                spend.insert("quota_floor_percent".into(), json!(floor));
            }
            if !spend.is_empty() {
                scope["spend"] = Value::Object(spend);
            }
            if let Some(merge) = &args.merge {
                scope["merge"] = match merge.as_str() {
                    "none" | "grants" => json!(merge),
                    conditions => serde_json::from_str(conditions)?,
                };
            }
            let keyed = app.key(&store, "away.start", &scope, &args.key.key)?;
            let applied = store.away_start(&app.ctx().with_key(&keyed.key), scope)?;
            let outcome = Outcome::applied(applied).with_key(&keyed);
            if !app.person() {
                return Ok(outcome);
            }
            let names: Vec<String> = args
                .projects
                .iter()
                .map(|id| summary::project_name(&store, id))
                .collect();
            let line = summary::away_started(
                &outcome.data,
                &names,
                &lisbon::display(until.as_jiff()),
                &summary::host_label(&store),
            );
            Ok(outcome
                .with_summary(line)
                .with_help("tiller away end: turn it off and print what happened")
                .with_help("tiller away brief: what started, landed and waits, so far"))
        }
        Away::End { reason } => {
            app.require_owner("tiller away")?;
            let mut store = app.write()?;
            let applied = store.away_end(&app.ctx(), &reason)?;
            let brief = match &applied.value {
                Some(away) => ReturnBrief::load(store.conn(), Some(&away.id), Timestamp::now())?,
                None => None,
            };
            Ok(Outcome::applied_as(
                applied,
                |away| json!({"away": away, "brief": brief}),
            ))
        }
    }
}
