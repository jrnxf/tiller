//! `tiller thread <project>`: the owner's chat with a project's orchestrator,
//! the same items the board's chat reads from `GET /api/v1/projects/{id}/thread`
//! (`protocol.md` 7.6). `--mark-read` marks its unread replies read, as
//! opening the chat on the board does.

use clap::Args;
use serde_json::json;
use tiller_daemon::views;

use crate::app::{App, Done, Outcome};

#[derive(Args, Debug)]
pub struct Thread {
    /// The project's id (its slug, like alpha)
    pub project: String,
    /// Only items older than this seq: the last page's next_before
    #[arg(long)]
    pub before: Option<i64>,
    /// How many items, 1 to 200
    #[arg(long, default_value_t = views::THREAD_PAGE)]
    pub limit: i64,
    /// Mark every unread reply of the project read, as opening the chat on
    /// the board does (owner only)
    #[arg(long)]
    pub mark_read: bool,
}

pub fn run(app: &App, args: Thread) -> Done {
    let marked = match args.mark_read {
        true => {
            app.require_owner("tiller thread --mark-read")?;
            let mut store = app.write()?;
            let project = store.project(&args.project)?;
            Some(store.thread_read(&app.ctx(), &project.id)?)
        }
        false => None,
    };
    let store = app.read()?;
    let thread = views::thread(&store, &args.project, args.before, Some(args.limit))?;
    let mut outcome = Outcome::read(&thread);
    if let Some(before) = thread.next_before {
        outcome = outcome.with_help(format!(
            "Older items: tiller thread {} --before {before}",
            args.project
        ));
    }
    if let Some(marked) = marked {
        outcome.data["marked_read"] = json!(marked.value.len());
        outcome.mutation = Some((marked.events, marked.replayed));
    }
    Ok(outcome)
}
