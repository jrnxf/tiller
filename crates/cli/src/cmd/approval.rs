//! Approvals, landing, grants and away mode (`protocol.md` section 6.4,
//! Attention and approvals). `tiller land` needs the daemon and waits for it.

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::domain::{
    AnsweredVia, Approval, ApprovalAction, ApprovalKind, GrantKind, Pr, Role, Run, Worktree,
};
use tiller_core::state::approvals::{Decision, NewApproval, NewGrant};
use tiller_core::state::{find, get, list, resolve_card, resolve_id};
use tiller_core::{Actor, Ctx};

use super::KeyArg;
use super::attention::chat_ask;
use crate::app::{App, Done, Failure, Outcome};
use crate::lisbon;

#[derive(Subcommand, Debug)]
pub enum Request {
    /// Ask the owner to approve landing a card
    Request(RequestArgs),
}

#[derive(Args, Debug)]
pub struct RequestArgs {
    pub card: String,
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
    pub approval: String,
    /// Reject instead of accepting
    #[arg(long)]
    pub reject: bool,
    /// The head you reviewed; a different bound head means the approval is stale
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
    pub approval: String,
}

#[derive(Subcommand, Debug)]
pub enum Grant {
    /// A standing decision to land matching work (owner only)
    Create(GrantCreate),
    /// End a grant
    Revoke {
        grant: String,
        #[arg(long, default_value = "revoked by the owner")]
        reason: String,
    },
}

#[derive(Args, Debug)]
pub struct GrantCreate {
    pub project: String,
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
    /// Turn away mode off
    End {
        #[arg(long, default_value = "ended by the owner")]
        reason: String,
    },
}

#[derive(Args, Debug)]
pub struct AwayStart {
    #[arg(long)]
    pub until: String,
    /// The projects the away scope covers
    #[arg(long, value_delimiter = ',', required = true)]
    pub projects: Vec<String>,
    /// Conditions for the temporary merge grants away mode creates, as JSON
    #[arg(long)]
    pub merge: Option<String>,
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
                "card_id = ?1 AND role = 'builder' ORDER BY attempt DESC LIMIT 1",
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
    Ok(Outcome::applied(applied).with_help("the owner decides with: tiller approve <approval>"))
}

pub fn approve(app: &App, args: Approve) -> Done {
    let mut store = app.write()?;
    let id = resolve_id(store.conn(), "approvals", "approval", &args.approval)?;
    let approval: Approval = get(store.conn(), "approval", &id)?;
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
    };
    let (ctx, cited) = match &args.ask {
        None => {
            app.require_owner("tiller approve")?;
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
            (Ctx::new(Actor::Owner), Some(ask))
        }
    };
    let applied = store.approval_decide(&ctx, Some(&keyed.key), &id, decision)?;
    Ok(Outcome::applied_as(applied, |decided| {
        json!({
            "approval": decided.approval,
            "memory_approvals": decided.memory_approvals,
            "quoted_ask": cited.as_ref().map(|ask| json!({"id": ask.id, "text": ask.text})),
        })
    })
    .with_key(&keyed)
    .with_help("land it with: tiller land <approval>"))
}

pub fn land(_app: &App, args: Land) -> Done {
    Err(Failure::unavailable(format!(
        "tiller land {} needs the daemon, and the daemon is not running",
        args.approval
    ))
    .hint("the owner asks the daemon to land; start it with: tiller daemon"))
}

pub fn run_grant(app: &App, cmd: Grant) -> Done {
    app.require_owner("tiller grant")?;
    let mut store = app.write()?;
    match cmd {
        Grant::Create(args) => {
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
        Grant::Revoke { grant, reason } => Ok(Outcome::applied(store.grant_revoke(
            &app.ctx(),
            &grant,
            &reason,
        )?)),
    }
}

pub fn run_away(app: &App, cmd: Away) -> Done {
    app.require_owner("tiller away")?;
    let mut store = app.write()?;
    match cmd {
        Away::Start(args) => {
            let until = lisbon::parse_when(&args.until)?;
            let mut scope = json!({"until_at": until, "projects": args.projects});
            if let Some(merge) = &args.merge {
                scope["merge"] = serde_json::from_str(merge)?;
            }
            let keyed = app.key(&store, "away.start", &scope, &args.key.key)?;
            let applied = store.away_start(&app.ctx().with_key(&keyed.key), scope)?;
            Ok(Outcome::applied(applied).with_key(&keyed))
        }
        Away::End { reason } => {
            let applied = store.away_end(&app.ctx(), &reason)?;
            Ok(Outcome::applied_as(applied, |away| json!({"away": away})))
        }
    }
}
