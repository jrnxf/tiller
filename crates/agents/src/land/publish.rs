//! The publish of a direct-pr builder run (`authority.md` section 6, G12):
//! the worker only commits inside its sandbox, and the daemon pushes the
//! run's branch and opens its PR outside it, as the owner's login. The
//! sandbox keeps the owner's GitHub credential out of every worker.
//!
//! A pass takes every active builder run on this host whose last status is
//! `done`, whose project is effectively `direct-pr` and whose worker recorded
//! a PR title and body (`tiller pr describe`). For each it checks the
//! worktree has no uncommitted changes to tracked files, pushes the branch
//! head (never forced), finds or opens the PR and records both
//! (`publish.done`). A run whose record is [`current`] is left alone, so a
//! retry or a restart never opens a second PR: before opening one the forge
//! is asked for an open PR on the branch, which finds a PR opened just
//! before a crash. Every later `done` publishes again: a new head is pushed
//! to the same PR, a new description is set as its title and body, and the
//! worker hears what changed. A recorded PR that is no longer open gets no
//! push, since a run has one PR.
//!
//! A failure is recorded on the run (`publish.failed`), raised to the owner
//! as a `recovery` attention item carrying the real error, one open item per
//! head and step, and sent to the worker as a note. The pass tries again at
//! once for a new head or a new done report, and after [`RETRY_MINUTES`]
//! otherwise.

use std::path::Path;

use serde_json::json;
use tiller_core::domain::{
    AttentionKind, Delivery, OutboxKind, PipelineState, Pr, PrState, Project, Publish, Run,
};
use tiller_core::state::approvals::NewPr;
use tiller_core::state::attention::NewAttention;
use tiller_core::state::outbox::NewMessage;
use tiller_core::state::publishes::{current, publish};
use tiller_core::state::runs::live_session;
use tiller_core::state::{get, list};
use tiller_core::{Store, Timestamp};

use super::direct::{self, Pushed};
use super::forge::{Identity, NewPull, PullRef, PullText};
use super::scrub::scrub;
use super::{DaemonLock, LandError, Lander, Result};

/// How long a failed publish of an unchanged head waits before the next try.
pub const RETRY_MINUTES: i64 = 15;

/// What one publish attempt came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Published {
    /// The head is pushed and the run's PR recorded; `opened` when this
    /// attempt opened or found the PR rather than keeping the recorded one.
    Done {
        run: String,
        sha: String,
        url: String,
        opened: bool,
    },
    /// The attempt failed; the owner has an attention item with `error`.
    Failed {
        run: String,
        sha: Option<String>,
        error: String,
    },
}

/// One publish pass.
#[derive(Debug, Default)]
pub struct Publishing {
    pub published: Vec<Published>,
    /// A run whose publish could not even be recorded, and the error; the
    /// pass goes on with the next run.
    pub errors: Vec<(String, LandError)>,
}

/// What a publish did to the run's PR, besides the push.
struct Outcome {
    pr: Pr,
    /// The title and body were set from a newer description.
    edited: bool,
}

/// The part of a publish that failed: one attention item per head and step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Worktree,
    Push,
    Pr,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Step::Worktree => "worktree",
            Step::Push => "push",
            Step::Pr => "pr",
        }
    }
}

struct Failure {
    step: Step,
    sha: Option<String>,
    error: String,
}

fn failure(step: Step, sha: Option<&str>, error: impl Into<String>) -> Failure {
    Failure {
        step,
        sha: sha.map(str::to_owned),
        error: error.into(),
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

impl Lander<'_> {
    /// One pass over the runs waiting to be published.
    pub fn publish_ready(&self, lock: &DaemonLock, store: &mut Store) -> Result<Publishing> {
        let runs: Vec<Run> = list(
            store.conn(),
            "role = 'builder' AND ended_at IS NULL AND last_status_line LIKE 'done:%'
             AND host_id = (SELECT value FROM meta WHERE key = 'host_id')
             AND id IN (SELECT run_id FROM publishes)
             ORDER BY started_at, id",
            &[],
        )?;
        let now = Timestamp::now();
        let mut pass = Publishing::default();
        for run in runs {
            match self.publish_run(lock, store, &run, now) {
                Ok(Some(done)) => pass.published.push(done),
                Ok(None) => {}
                Err(error) => pass.errors.push((run.id.clone(), error)),
            }
        }
        Ok(pass)
    }

    fn publish_run(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        run: &Run,
        now: Timestamp,
    ) -> Result<Option<Published>> {
        let project: Project = get(
            store.conn(),
            "project",
            &store.card(&run.card_id)?.project_id,
        )?;
        if project.effective_delivery() != Delivery::DirectPr {
            return Ok(None);
        }
        let Some(record) = publish(store.conn(), &run.id)? else {
            return Ok(None);
        };
        self.publish(lock, store, &project, run, &record, now)
    }

    /// One run, or `None` when there is nothing to do yet.
    fn publish(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        project: &Project,
        run: &Run,
        record: &Publish,
        now: Timestamp,
    ) -> Result<Option<Published>> {
        let (Some(worktree), Some(branch)) = (&run.worktree_path, &run.branch) else {
            let why = failure(Step::Worktree, None, "the run has no worktree or branch");
            return self.publish_failed(lock, store, run, record, now, why);
        };
        let worktree = Path::new(worktree);
        let head = match direct::branch_head(worktree, branch) {
            Ok(head) => head,
            Err(error) => {
                let why = failure(Step::Worktree, None, format!("read {branch}: {error}"));
                return self.publish_failed(lock, store, run, record, now, why);
            }
        };
        if current(record, run, &head) {
            return Ok(None);
        }
        if waiting(run, record, Some(&head), now) {
            return Ok(None);
        }
        match self.push_and_open(lock, store, project, run, record, worktree, branch, &head) {
            Ok(Outcome { pr, edited }) => {
                let ctx = lock.ctx();
                let opened = record.pr_id.is_none();
                let done = store.publish_done(&ctx, &run.id, &head, &pr.id)?;
                self.withdraw_failures(lock, store, &run.id, None)?;
                if !done.events.is_empty() {
                    let at = short(&head);
                    let url = &pr.url;
                    let body = if opened {
                        format!(
                            "The daemon pushed {branch} at {at} and opened {url}. Keep committing; report done again to push new commits to it."
                        )
                    } else if record.pushed_sha.as_deref() != Some(head.as_str()) && edited {
                        format!(
                            "The daemon pushed {branch} at {at} to {url} and set its title and body from your description."
                        )
                    } else if record.pushed_sha.as_deref() != Some(head.as_str()) {
                        format!("The daemon pushed {branch} at {at} to {url}.")
                    } else if edited {
                        format!(
                            "The daemon set the title and body of {url} from your description; {branch} at {at} was already pushed."
                        )
                    } else {
                        format!(
                            "{url} is up to date: {branch} at {at} is pushed and its title and body match your description."
                        )
                    };
                    self.tell(
                        lock,
                        store,
                        run,
                        &format!("publish:{}:{}:done", run.id, done.value.rev),
                        body,
                    )?;
                }
                Ok(Some(Published::Done {
                    run: run.id.clone(),
                    sha: head,
                    url: pr.url,
                    opened,
                }))
            }
            Err(why) => self.publish_failed(lock, store, run, record, now, why),
        }
    }

    /// Checks the worktree, pushes `head` as `branch` and returns the run's
    /// PR: the recorded one, an open one on the branch, or a new one. A
    /// recorded PR that is no longer open is not pushed to.
    #[allow(clippy::too_many_arguments)]
    fn push_and_open(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        project: &Project,
        run: &Run,
        record: &Publish,
        worktree: &Path,
        branch: &str,
        head: &str,
    ) -> std::result::Result<Outcome, Failure> {
        let state = |error: tiller_core::Error| failure(Step::Pr, Some(head), error.to_string());
        let forge =
            |error: super::forge::ForgeError| failure(Step::Pr, Some(head), error.to_string());
        match direct::uncommitted(worktree, head) {
            Ok(listed) if listed.is_empty() => {}
            Ok(listed) => {
                return Err(failure(
                    Step::Worktree,
                    Some(head),
                    format!(
                        "the worktree has uncommitted changes to tracked files; commit or revert them:\n{listed}"
                    ),
                ));
            }
            Err(error) => {
                return Err(failure(
                    Step::Worktree,
                    Some(head),
                    format!("read the worktree status: {error}"),
                ));
            }
        }
        // The land scrub (P17), before anything leaves the machine: a title
        // or body that would be refused at merge is not posted at all.
        let asks = super::project_asks(store, &project.id)
            .map_err(|error| failure(Step::Pr, Some(head), error.to_string()))?;
        let description = format!("{}\n{}", record.title, record.body);
        if let Some(why) = scrub(&description, &asks, &self.scrub, PipelineState::None) {
            return Err(failure(
                Step::Pr,
                Some(head),
                format!(
                    "the PR description was not posted: {why}. Edit it with `tiller pr describe` and report done again"
                ),
            ));
        }
        let Some(repo) = project.repo.clone() else {
            return Err(failure(
                Step::Pr,
                Some(head),
                format!("project {} names no GitHub repo", project.id),
            ));
        };
        let recorded = match &record.pr_id {
            Some(id) => {
                let pr: Pr = get(store.conn(), "pr", id).map_err(state)?;
                let text = self.forge.pull_text(&repo, pr.number).map_err(forge)?;
                if text.state != PrState::Open {
                    return Err(failure(
                        Step::Pr,
                        Some(head),
                        format!(
                            "{} is {}, so nothing was pushed: a run has one PR, and new work needs a new run",
                            pr.url, text.state
                        ),
                    ));
                }
                Some((pr, text))
            }
            None => None,
        };
        let remote = project.remote.clone().unwrap_or_else(|| "origin".into());
        match direct::push(worktree, &remote, head, branch, &Identity::Owner) {
            Pushed::Done => {}
            Pushed::Refused(error) | Pushed::Unknown(error) => {
                return Err(failure(
                    Step::Push,
                    Some(head),
                    format!("push {branch} to {remote}: {error}"),
                ));
            }
        }
        if let Some((pr, text)) = recorded {
            let edited = self.describe(&repo, pr.number, &text, record, head)?;
            return Ok(Outcome { pr, edited });
        }
        let base = run
            .base_branch
            .clone()
            .unwrap_or_else(|| project.default_branch.clone());
        let found = self.forge.find_pull(&repo, branch).map_err(forge)?;
        let (pull, edited): (PullRef, bool) = match found {
            // Opened before a crash, perhaps from an older description.
            Some(pull) => {
                let text = self.forge.pull_text(&repo, pull.number).map_err(forge)?;
                let edited = self.describe(&repo, pull.number, &text, record, head)?;
                (pull, edited)
            }
            None => {
                let new = NewPull {
                    repo: repo.clone(),
                    head: branch.into(),
                    base: base.clone(),
                    title: record.title.clone(),
                    body: record.body.clone(),
                };
                let pull = match self.forge.open_pull(&new) {
                    Ok(pull) => pull,
                    // The forge refused a second PR for the branch, or the
                    // reply was lost: the open PR is the run's.
                    Err(error) => match self.forge.find_pull(&repo, branch) {
                        Ok(Some(pull)) => pull,
                        _ => return Err(forge(error)),
                    },
                };
                (pull, false)
            }
        };
        store
            .pr_link(
                &lock.ctx(),
                NewPr {
                    card: run.card_id.clone(),
                    run_id: Some(run.id.clone()),
                    number: pull.number,
                    url: pull.url,
                    head_sha: Some(pull.head_sha),
                    base_ref: Some(pull.base_ref),
                },
            )
            .map(|applied| Outcome {
                pr: applied.value,
                edited,
            })
            .map_err(state)
    }

    /// Sets PR `number`'s title and body from the worker's description when
    /// they differ from `text`, what the forge holds; whether it did.
    fn describe(
        &self,
        repo: &str,
        number: i64,
        text: &PullText,
        record: &Publish,
        head: &str,
    ) -> std::result::Result<bool, Failure> {
        let same = |held: &str, described: &str| held.replace("\r\n", "\n").trim() == described;
        if same(&text.title, &record.title) && same(&text.body, &record.body) {
            return Ok(false);
        }
        self.forge
            .edit_pull(repo, number, &record.title, &record.body)
            .map_err(|error| failure(Step::Pr, Some(head), error.to_string()))?;
        Ok(true)
    }

    fn publish_failed(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        run: &Run,
        record: &Publish,
        now: Timestamp,
        why: Failure,
    ) -> Result<Option<Published>> {
        if waiting(run, record, why.sha.as_deref(), now) {
            return Ok(None);
        }
        // A done report since the last try, not the backoff, set this one off.
        let reported = run
            .last_status_at
            .is_some_and(|at| record.attempted_at.is_none_or(|tried| at >= tried));
        let ctx = lock.ctx();
        let failed = store.publish_failed(&ctx, &run.id, why.sha.as_deref(), &why.error)?;
        let sha = why.sha.as_deref().unwrap_or("unknown");
        let base = format!("publish:{}:{sha}:{}", run.id, why.step.as_str());
        self.withdraw_failures(lock, store, &run.id, Some(&base))?;
        let branch = run.branch.as_deref().unwrap_or("its branch");
        // One open item per head and step: a repeat of an open one raises
        // nothing new. Each write of the record has its own key, so the same
        // failure after a withdrawn item opens a new one.
        let key = format!("{base}:{}", failed.value.rev);
        let raised = !self
            .failures(store, &run.id)?
            .iter()
            .any(|(_, open)| *open == base);
        if raised {
            store.attention_open(
                &ctx,
                Some(&key),
                NewAttention {
                    card: run.card_id.clone(),
                    kind: AttentionKind::Recovery,
                    question: format!(
                        "The daemon could not publish {branch} at {}: {}",
                        short(sha),
                        why.error
                    ),
                    evidence: Some(
                        json!({
                            "publish_run_id": run.id,
                            "publish_key": base,
                            "head_sha": why.sha,
                            "step": why.step.as_str(),
                            "error": why.error,
                        })
                        .to_string(),
                    ),
                    recommendation: Some(format!(
                        "Fix the cause; the daemon tries again on the worker's next done report or within {RETRY_MINUTES} minutes."
                    )),
                    run_id: Some(run.id.clone()),
                    ..NewAttention::default()
                },
            )?;
        }
        if raised || reported {
            let body = format!(
                "The daemon could not publish {branch} at {}: {}\nIf the cause is in your worktree, fix it, commit and report done again. Otherwise the owner has it.",
                short(sha),
                why.error
            );
            self.tell(lock, store, run, &format!("{key}:note"), body)?;
        }
        Ok(Some(Published::Failed {
            run: run.id.clone(),
            sha: why.sha,
            error: why.error,
        }))
    }

    /// The run's open publish failure items: each id and its publish key.
    fn failures(&self, store: &Store, run: &str) -> Result<Vec<(String, String)>> {
        let mut statement = store
            .conn()
            .prepare(
                "SELECT id, json_extract(evidence, '$.publish_key') FROM attention
                 WHERE opened_by_run_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL
                   AND json_valid(evidence) AND json_extract(evidence, '$.publish_run_id') = ?1
                   AND json_extract(evidence, '$.publish_key') IS NOT NULL
                 ORDER BY opened_at, id",
            )
            .map_err(tiller_core::Error::from)?;
        let rows = statement
            .query_map([run], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(tiller_core::Error::from)?
            .collect::<rusqlite::Result<_>>()
            .map_err(tiller_core::Error::from)?;
        Ok(rows)
    }

    /// Withdraws the run's open publish failure items, but those for `keep`.
    fn withdraw_failures(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        run: &str,
        keep: Option<&str>,
    ) -> Result<()> {
        let reason = match keep {
            Some(_) => "the publish failed again for another reason",
            None => "the daemon published the run",
        };
        for (id, key) in self.failures(store, run)? {
            if Some(key.as_str()) != keep {
                store.attention_withdraw(&lock.ctx(), &id, reason)?;
            }
        }
        Ok(())
    }

    /// A note to the run's live worker, once per `key`.
    fn tell(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        run: &Run,
        key: &str,
        body: String,
    ) -> Result<()> {
        let Some(session) = live_session(store.conn(), &run.id)? else {
            return Ok(());
        };
        store.outbox_enqueue(
            &lock.ctx(),
            Some(key),
            NewMessage {
                target_session_id: session.id,
                kind: OutboxKind::Note,
                body,
                card_id: Some(run.card_id.clone()),
                reply_to: None,
            },
        )?;
        Ok(())
    }
}

/// Whether a failed publish of `sha` waits: the same head failed less than
/// [`RETRY_MINUTES`] ago and the worker has not reported done since. A
/// report in the same millisecond as the failure counts as later, so the
/// worst case is one extra try, never a missed one.
pub(super) fn waiting(run: &Run, record: &Publish, sha: Option<&str>, now: Timestamp) -> bool {
    let (Some(_), Some(at)) = (&record.error, record.attempted_at) else {
        return false;
    };
    record.attempted_sha.as_deref() == sha
        && run.last_status_at.is_none_or(|reported| reported < at)
        && now < at.plus_minutes(RETRY_MINUTES)
}
