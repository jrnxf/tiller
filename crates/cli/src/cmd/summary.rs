//! The `summary` line of an owner command: one sentence that says what the
//! command did, with a project's name and a host's label in place of ids.
//! It is read from the same `data` the response prints, so the two cannot
//! disagree. The next step is not here: it goes in `help`.

use serde_json::Value;
use tiller_core::Store;

/// The most characters of the owner's own words a summary quotes.
const QUOTE: usize = 80;

/// A project's display name, or its id when the row cannot be read.
pub fn project_name(store: &Store, id: &str) -> String {
    store
        .project(id)
        .map_or_else(|_| id.to_owned(), |project| project.name)
}

/// This host's label, or "this host" when the row cannot be read.
pub fn host_label(store: &Store) -> String {
    store
        .local_host()
        .map_or_else(|_| "this host".to_owned(), |host| host.label)
}

fn text<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |at, key| at.get(key))
        .and_then(Value::as_str)
}

/// The first line of the owner's words, cut to `QUOTE` characters. Single
/// quotes: TOON would escape double ones.
pub fn quote(words: &str) -> String {
    let line = words.lines().find(|line| !line.trim().is_empty());
    let line = line.unwrap_or_default().trim();
    if line.chars().count() > QUOTE {
        let cut: String = line.chars().take(QUOTE - 3).collect();
        format!("'{}...'", cut.trim_end())
    } else {
        format!("'{line}'")
    }
}

/// `tiller init`.
pub fn initialized(data: &Value) -> String {
    let label = text(data, &["host", "label"]).unwrap_or("this host");
    match data
        .get("host")
        .and_then(|host| host.get("lane_capacity"))
        .and_then(Value::as_i64)
    {
        Some(1) => format!("Tiller is set up on {label}, with room for 1 run at a time."),
        Some(n) => format!("Tiller is set up on {label}, with room for {n} runs at a time."),
        None => format!("Tiller is set up on {label}."),
    }
}

/// `tiller project add`.
pub fn project_added(data: &Value) -> String {
    let id = text(data, &["id"]).unwrap_or("the project");
    let name = text(data, &["name"]).unwrap_or(id);
    format!("Added the project {name} ({id}). It has no orchestrator yet.")
}

/// `tiller orchestrator start`.
pub fn orchestrator_started(project: &str, host: &str, replayed: bool) -> String {
    if replayed {
        format!("The {project} orchestrator already runs on {host}.")
    } else {
        format!("Started the {project} orchestrator on {host}.")
    }
}

/// `tiller card create`.
pub fn card_created(data: &Value, project: &str) -> String {
    let card = text(data, &["display_id"]).unwrap_or("the card");
    let title = quote(text(data, &["title"]).unwrap_or_default());
    let kind = text(data, &["kind"]).unwrap_or("card");
    let priority = data
        .get("priority")
        .and_then(Value::as_i64)
        .map(|p| format!(", P{p}"))
        .unwrap_or_default();
    format!("Filed {card} {title} in {project} ({kind}{priority}). No run has started.")
}

/// `tiller spawn`: `name` is the agent's name, as `tiller attach` takes it.
pub fn spawned(data: &Value, card: &str, host: &str, replayed: bool) -> String {
    let role = text(data, &["run", "role"]).unwrap_or("run");
    let model = text(data, &["run", "model"]);
    let effort = text(data, &["run", "effort"]);
    let dispatch = match (model, effort) {
        (Some(model), Some(effort)) => format!(" ({model}, {effort})"),
        (Some(model), None) => format!(" ({model})"),
        _ => String::new(),
    };
    if replayed {
        format!("The {role} of {card} already runs on {host}{dispatch}; nothing new started.")
    } else {
        format!("Started the {role} of {card} on {host}{dispatch}.")
    }
}

/// `tiller ask note` from the owner's shell.
pub fn asked(data: &Value, project: &str) -> String {
    let words = quote(text(data, &["ask", "text"]).unwrap_or_default());
    if data
        .get("message")
        .is_some_and(|message| !message.is_null())
    {
        format!("Sent to the {project} orchestrator: {words}.")
    } else {
        format!(
            "Recorded for {project}: {words}. No orchestrator runs there, so nobody reads it yet."
        )
    }
}

/// `tiller attention answer`.
pub fn answered(data: &Value) -> String {
    let question = quote(text(data, &["attention", "question"]).unwrap_or_default());
    let answer = quote(text(data, &["attention", "answer"]).unwrap_or_default());
    let mut line = format!("Answered {question} with {answer}.");
    if data
        .get("message")
        .is_some_and(|message| !message.is_null())
    {
        line.push_str(" The agent that asked is told.");
    }
    if let Some(card) = text(data, &["closed_card", "display_id"]) {
        line.push_str(&format!(" {card} is closed."));
    }
    if data.get("still_current") == Some(&Value::Bool(false)) {
        line.push_str(" The question was no longer current when you answered.");
    }
    line
}

/// `tiller away start`: `names` are the covered projects' names, `until` is
/// the end as the owner reads it, and `host` is the one host it binds.
pub fn away_started(data: &Value, names: &[String], until: &str, host: &str) -> String {
    let projects = match names {
        [] => "no project".to_owned(),
        [one] => one.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    };
    let mut line = format!("Away mode is on for {projects} on {host} until {until} (Lisbon).");
    match data
        .get("scope")
        .and_then(|scope| scope.get("spend"))
        .and_then(|spend| spend.get("max_new_runs"))
        .and_then(Value::as_i64)
    {
        Some(1) => line.push_str(&format!(" At most 1 new run starts on {host}.")),
        Some(n) => line.push_str(&format!(" At most {n} new runs start on {host}.")),
        None => line.push_str(" No cap on new runs."),
    }
    match data.get("scope").and_then(|scope| scope.get("merge")) {
        Some(Value::String(merge)) if merge == "none" => line.push_str(" Nothing merges."),
        Some(Value::Object(_)) => {
            line.push_str(" Your grants and the conditions you gave merge.");
        }
        _ => line.push_str(" Only your existing grants merge."),
    }
    line
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_quote_is_the_first_line_and_is_cut() {
        assert_eq!(quote("\n  Ship it  \nand more"), "'Ship it'");
        let long = "x".repeat(200);
        let cut = quote(&long);
        assert_eq!(cut.chars().count(), QUOTE + 2);
        assert!(cut.ends_with("...'"));
    }

    #[test]
    fn away_mode_names_the_projects_the_end_and_the_limits() {
        let data = json!({"scope": {"spend": {"max_new_runs": 1}, "merge": "none"}});
        assert_eq!(
            away_started(
                &data,
                &["Alpha".to_owned(), "Beta".to_owned()],
                "Tue 2026-11-17 09:00",
                "host-a"
            ),
            "Away mode is on for Alpha and Beta on host-a until Tue 2026-11-17 09:00 (Lisbon). \
             At most 1 new run starts on host-a. Nothing merges."
        );
        let open = json!({"scope": {}});
        assert_eq!(
            away_started(
                &open,
                &["Alpha".to_owned()],
                "Tue 2026-11-17 09:00",
                "host-a"
            ),
            "Away mode is on for Alpha on host-a until Tue 2026-11-17 09:00 (Lisbon). \
             No cap on new runs. Only your existing grants merge."
        );
        let conditions = json!({"scope": {"spend": {"max_new_runs": 6}, "merge": {"v": 1}}});
        assert!(
            away_started(&conditions, &["Alpha".to_owned()], "x", "host-a").ends_with(
                "At most 6 new runs start on host-a. Your grants and the conditions you gave merge."
            )
        );
    }

    #[test]
    fn a_spawn_names_the_card_the_host_and_the_model() {
        let data = json!({"run": {"role": "builder", "model": "sonnet", "effort": "medium"}});
        assert_eq!(
            spawned(&data, "alpha-42", "host-a", false),
            "Started the builder of alpha-42 on host-a (sonnet, medium)."
        );
        assert_eq!(
            spawned(&data, "alpha-42", "host-a", true),
            "The builder of alpha-42 already runs on host-a (sonnet, medium); nothing new started."
        );
    }

    #[test]
    fn an_ask_with_no_orchestrator_says_nobody_reads_it() {
        let sent = json!({"ask": {"text": "Is it done?"}, "message": {"id": "m"}});
        assert_eq!(
            asked(&sent, "Alpha"),
            "Sent to the Alpha orchestrator: 'Is it done?'."
        );
        let unsent = json!({"ask": {"text": "Is it done?"}, "message": null});
        assert!(asked(&unsent, "Alpha").contains("nobody reads it yet"));
    }

    #[test]
    fn an_answer_says_what_else_it_did() {
        let data = json!({
            "attention": {"question": "Overlap or cut over?", "answer": "Overlap"},
            "message": {"id": "m"},
            "closed_card": {"display_id": "alpha-41"},
            "still_current": true,
        });
        assert_eq!(
            answered(&data),
            "Answered 'Overlap or cut over?' with 'Overlap'. The agent that asked is told. alpha-41 is closed."
        );
    }
}
