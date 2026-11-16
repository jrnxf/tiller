//! The owner's way to a paired peer's work: the daemon's owner CLI socket
//! (`authority.md` section 7, Owner CLI socket; `protocol.md` 7.1). The CLI
//! reads and writes this host's `state.db` itself. A peer's rows live in the
//! daemon's memory and its writes go over the daemon's link, so for those
//! the owner's `tiller` asks the daemon, over a Unix socket the kernel names
//! the caller of.
//!
//! The CLI uses it only for the owner at a terminal (its own check comes
//! first), and the daemon checks the caller again. With no daemon, no socket
//! or a refusal, a list is this host's alone and says so, as before.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tiller_core::Actor;
use tiller_core::domain::AttentionKind;

use crate::app::{App, Failure, Outcome};

/// A read waits at most this long: a peer's single record is a `query` over
/// the link, answered within 10 s or not at all (`protocol.md` 10.3).
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The daemon's owner socket, for the owner at a terminal.
pub struct Socket {
    path: PathBuf,
}

/// A request the daemon did not answer with a 2xx.
#[derive(Debug)]
pub struct Refusal {
    /// The HTTP status, or 0 when nothing answered.
    pub status: u16,
    /// The body's `error`, when it carries one.
    pub error: Value,
}

impl Refusal {
    fn unanswered(message: String) -> Self {
        Self {
            status: 0,
            error: json!({"code": "unavailable", "message": message}),
        }
    }

    /// The error's message.
    pub fn message(&self) -> &str {
        self.error["message"].as_str().unwrap_or("no message")
    }
}

impl Socket {
    /// The socket, when the caller is the owner at a terminal and this run
    /// reads the home's own `state.db`, the one the daemon serves.
    pub fn for_owner(app: &App) -> Option<Self> {
        if app.db != app.home.join("state.db") || !matches!(app.actor(), Actor::Owner) {
            return None;
        }
        let path = tiller_daemon::socket::socket_path(&app.home);
        path.exists().then_some(Self { path })
    }

    /// `GET path`, answered with its JSON.
    pub fn get(&self, path: &str) -> Result<Value, Refusal> {
        self.request("GET", path, None, READ_TIMEOUT)
    }

    /// One HTTP/1.0 request: the daemon closes the connection after its
    /// answer, so the answer is everything up to the close.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, Refusal> {
        let unanswered =
            |error: std::io::Error| Refusal::unanswered(format!("the owner socket: {error}"));
        let mut stream = UnixStream::connect(&self.path).map_err(unanswered)?;
        stream.set_read_timeout(Some(timeout)).map_err(unanswered)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(unanswered)?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let mut head = format!("{method} {path} HTTP/1.0\r\nAccept: application/json\r\n");
        if !body.is_empty() {
            head.push_str("Content-Type: application/json\r\n");
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body.as_bytes()))
            .map_err(unanswered)?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).map_err(unanswered)?;
        let (status, value) = parse(&answer)
            .ok_or_else(|| Refusal::unanswered("the owner socket answered no HTTP".into()))?;
        if (200..300).contains(&status) {
            return Ok(value);
        }
        Err(Refusal {
            status,
            error: match value.get("error") {
                Some(error) => error.clone(),
                None => json!({"code": "internal", "message": format!("HTTP {status}")}),
            },
        })
    }
}

/// The status and JSON body of an HTTP/1.x answer.
fn parse(answer: &[u8]) -> Option<(u16, Value)> {
    let end = answer.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(answer.get(..end)?).ok()?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let mut body = answer.get(end + 4..)?;
    let length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())?
    });
    if let Some(length) = length {
        body = body.get(..length)?;
    }
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).ok()?
    };
    Some((status, value))
}

/// A path segment or query value, percent-encoded.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A list of this host that the daemon can complete with its paired peers'
/// rows (`protocol.md` 7.1, Owner CLI socket).
#[derive(Clone, Copy, Debug)]
pub enum List {
    Recap,
    /// `all`, `active` or `needs`.
    Board(&'static str),
    Agents,
    /// Open items only, or every item.
    Attention {
        open: bool,
    },
    /// Unresolved asks only, or every ask.
    Asks {
        unresolved: bool,
    },
}

/// A peer as the daemon sees it.
struct Peer {
    label: String,
    link: String,
    last_seen_at: Option<String>,
}

/// Every paired peer: the machines with a link, which this host's own row
/// lacks.
fn peers(socket: &Socket) -> Result<Vec<Peer>, Refusal> {
    let machines = socket.get("/api/v1/machines")?;
    Ok(machines["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|machine| {
            Some(Peer {
                label: machine["label"].as_str()?.to_owned(),
                link: machine["link"].as_str()?.to_owned(),
                last_seen_at: machine["last_seen_at"].as_str().map(str::to_owned),
            })
        })
        .collect())
}

/// Adds every paired peer's rows to `data`, a list of this host. It returns
/// what the person should still be told (a link that is not live, rows a
/// link does not carry), or `Err` when the daemon did not answer, so the
/// caller says the list is this host's only.
pub fn add_peers(socket: &Socket, list: List, data: &mut Value) -> Result<Option<String>, Refusal> {
    let peers = peers(socket)?;
    if peers.is_empty() {
        return Ok(None);
    }
    let mut notes: Vec<String> = peers.iter().filter_map(link_note).collect();
    let labels = peers
        .iter()
        .map(|peer| peer.label.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    match list {
        List::Board(filter) => {
            let board = socket.get(&format!("/api/v1/board?filter={filter}"))?;
            push(&mut data["cards"], peer_rows(&board["rows"]).cloned());
            push(&mut data["asked"], peer_rows(&board["asked"]).map(asked));
        }
        List::Agents => {
            let agents = socket.get("/api/v1/agents")?;
            push(
                &mut data["agents"],
                peer_rows(&agents["orchestrators"]).map(agent_group),
            );
        }
        List::Attention { open } => {
            let attention = socket.get("/api/v1/attention")?;
            push(
                &mut data["attention"],
                peer_rows(&attention["items"]).map(|item| {
                    json!({
                        "item": item,
                        "still_current": item["still_current"],
                        "stale": item.get("stale").cloned().unwrap_or(Value::Null),
                    })
                }),
            );
            if !open {
                notes.push(format!(
                    "Of {labels}, this lists the open items only: its answered ones are on its own host."
                ));
            }
        }
        List::Asks { unresolved } => {
            let board = socket.get("/api/v1/board")?;
            let rows = peer_rows(&board["asked"]);
            if unresolved {
                push(
                    &mut data["asks"],
                    rows.map(|row| {
                        let mut row = asked(row);
                        let seconds = row["age_seconds"].as_f64().unwrap_or_default();
                        row["age"] = json!(crate::lisbon::age(seconds));
                        row
                    }),
                );
            } else {
                push(&mut data["asks"], rows.cloned());
                notes.push(format!(
                    "Of {labels}, this lists the unresolved asks only: the link carries no other."
                ));
            }
        }
        List::Recap => recap(socket, data)?,
    }
    Ok((!notes.is_empty()).then(|| notes.join(" ")))
}

/// What a person is told of a link that is not live.
fn link_note(peer: &Peer) -> Option<String> {
    let since = peer
        .last_seen_at
        .as_deref()
        .and_then(|at| at.parse().ok())
        .map(crate::lisbon::display);
    match (peer.link.as_str(), since) {
        ("live", _) => None,
        (link, Some(since)) => Some(format!(
            "{} is not live (link {link}): its rows are as of {since}.",
            peer.label
        )),
        (link, None) => Some(format!(
            "{} is not live (link {link}): none of its rows are listed.",
            peer.label
        )),
    }
}

/// The rows a peer owns: the daemon marks each with its link.
fn peer_rows(rows: &Value) -> impl Iterator<Item = &Value> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter(|row| row.get("link").is_some_and(|link| !link.is_null()))
}

fn push(list: &mut Value, rows: impl Iterator<Item = Value>) {
    if !list.is_array() {
        *list = json!([]);
    }
    if let Some(list) = list.as_array_mut() {
        list.extend(rows);
    }
}

/// A peer's unresolved ask, in the shape of this host's `asked` rows.
fn asked(row: &Value) -> Value {
    json!({
        "ask": row,
        "age_seconds": row["age_seconds"].as_f64().map(f64::round),
    })
}

/// A peer's orchestrator with its workers, in the shape of this host's
/// agent groups; a worker whose orchestrator ended stands alone.
fn agent_group(row: &Value) -> Value {
    let mut row = row.clone();
    let workers = row
        .as_object_mut()
        .and_then(|row| row.remove("workers"))
        .unwrap_or_else(|| json!([]));
    if row["role"] == "orchestrator" {
        json!({"orchestrator": row, "workers": workers})
    } else {
        json!({"orchestrator": null, "workers": [row]})
    }
}

/// A peer's open decisions join the recap's, most blocking first; one that
/// no longer applies counts as superseded.
fn recap(socket: &Socket, data: &mut Value) -> Result<(), Refusal> {
    let attention = socket.get("/api/v1/attention")?;
    let board = socket.get("/api/v1/board")?;
    let cards: Vec<&Value> = peer_rows(&board["rows"]).collect();
    let mut superseded = data["superseded"].as_i64().unwrap_or_default();
    let mut added = Vec::new();
    for item in peer_rows(&attention["items"]) {
        if item["still_current"] == false {
            superseded += 1;
            continue;
        }
        let card = cards
            .iter()
            .find(|card| card["id"] == item["card_id"] && card["host"] == item["host"]);
        added.push(json!({
            "id": item["id"],
            "card": card.map_or(&item["card_id"], |card| &card["display_id"]),
            "title": card.map(|card| &card["title"]),
            "kind": item["kind"],
            "question": item["question"],
            "evidence": item["evidence"],
            "recommendation": item["recommendation"],
            "consequence": item["consequence"],
            "options": item["options"],
            "opened_at": item["opened_at"],
            "host": item["host"],
        }));
    }
    push(&mut data["decisions"], added.into_iter());
    if let Some(decisions) = data["decisions"].as_array_mut() {
        // Most blocking first, then oldest, as for this host's own.
        decisions.sort_by(|a, b| {
            let key = |row: &Value| {
                (
                    rank(&row["kind"]),
                    row["opened_at"].as_str().map(str::to_owned),
                )
            };
            key(a).cmp(&key(b))
        });
        for (n, row) in decisions.iter_mut().enumerate() {
            row["position"] = json!(n + 1);
        }
    }
    data["superseded"] = json!(superseded);
    Ok(())
}

fn rank(kind: &Value) -> u8 {
    serde_json::from_value::<AttentionKind>(kind.clone()).map_or(u8::MAX, crate::cmd::recap::rank)
}

/// A forwarded write waits at most this long: the daemon's own wait for a
/// peer's `mutate` answer is 60 s (`protocol.md` 10.3).
const WRITE_TIMEOUT: Duration = Duration::from_secs(75);

/// A command on a record this host does not have, which a paired peer may
/// have: the daemon reads it from the peer, or forwards the change over its
/// link (`protocol.md` 10.3). A record is found by its reference, so the
/// command names no host.
#[derive(Debug)]
pub enum Remote {
    /// `card show`, or `card timeline`.
    CardRead {
        card: String,
        timeline: Option<Timeline>,
    },
    /// A change to a card: `method` on `cards/{id}<action>` with `body`.
    CardWrite {
        card: String,
        method: &'static str,
        action: &'static str,
        body: Value,
        key: Option<String>,
    },
    /// `attention answer` with the owner's words.
    Answer {
        item: String,
        answer: String,
        key: Option<String>,
    },
    /// `ask note` on a project.
    AskNote {
        project: String,
        text: String,
        key: Option<String>,
    },
}

/// What `card timeline` reads.
#[derive(Debug)]
pub struct Timeline {
    pub before: Option<i64>,
    pub limit: i64,
}

impl Remote {
    /// Reads or changes the record on the paired peer that has it. `None`
    /// means no peer has it, so this host's `not_found` stands.
    pub fn send(self, socket: &Socket) -> Option<Result<Outcome, Failure>> {
        let peers = match peers(socket) {
            Ok(peers) => peers,
            Err(refusal) => return Some(Err(failure(&refusal))),
        };
        let labels: Vec<&str> = peers.iter().map(|peer| peer.label.as_str()).collect();
        match self {
            Self::CardRead { card, timeline } => {
                let (label, detail) = match find_card(socket, &labels, &card)? {
                    Ok(found) => found,
                    Err(failure) => return Some(Err(failure)),
                };
                Some(card_read(socket, label, detail, timeline))
            }
            Self::CardWrite {
                card,
                method,
                action,
                body,
                key,
            } => {
                let (label, detail) = match find_card(socket, &labels, &card)? {
                    Ok(found) => found,
                    Err(failure) => return Some(Err(failure)),
                };
                let id = detail["card"]["id"].as_str().unwrap_or(&card).to_owned();
                let path = format!("/api/v1/cards/{}{action}", encode(&id));
                Some(write(socket, method, &path, label, body, key, &card))
            }
            Self::Answer { item, answer, key } => {
                let (label, id) = match find_item(socket, &item)? {
                    Ok(found) => found,
                    Err(failure) => return Some(Err(failure)),
                };
                let path = format!("/api/v1/attention/{}/answer", encode(&id));
                let body = json!({"answer": answer});
                Some(write(socket, "POST", &path, label, body, key, &item))
            }
            Self::AskNote { project, text, key } => {
                let projects = match socket.get("/api/v1/projects") {
                    Ok(projects) => projects,
                    Err(refusal) => return Some(Err(failure(&refusal))),
                };
                let label = projects["projects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|row| row["id"] == project.as_str())
                    .find_map(|row| row["host"].as_str().filter(|host| labels.contains(host)))?
                    .to_owned();
                let path = format!("/api/v1/projects/{}/asks", encode(&project));
                let body = json!({"text": text});
                Some(write(socket, "POST", &path, label, body, key, &project))
            }
        }
    }
}

/// The peer that has `card`, and the card as it reads it. A peer that does
/// not answer is named only when no other peer has the card.
fn find_card(
    socket: &Socket,
    labels: &[&str],
    card: &str,
) -> Option<Result<(String, Value), Failure>> {
    let mut unanswered = None;
    for label in labels {
        let path = format!("/api/v1/cards/{}?host={}", encode(card), encode(label));
        match socket.get(&path) {
            Ok(detail) => return Some(Ok(((*label).to_owned(), detail))),
            Err(refusal) if refusal.status == 404 => {}
            Err(refusal) => unanswered = Some(failure(&refusal)),
        }
    }
    unanswered.map(Err)
}

/// The peer whose item is `item`, a ULID or its short form, and the item's
/// full id.
fn find_item(socket: &Socket, item: &str) -> Option<Result<(String, String), Failure>> {
    let attention = match socket.get("/api/v1/attention") {
        Ok(attention) => attention,
        Err(refusal) => return Some(Err(failure(&refusal))),
    };
    let wanted = item.to_ascii_uppercase();
    let row = peer_rows(&attention["items"]).find(|row| {
        row["id"].as_str().is_some_and(|id| {
            id == wanted
                || (wanted.len() == tiller_core::ids::short(id).len() && id.ends_with(&wanted))
        })
    })?;
    Some(Ok((
        row["host"].as_str()?.to_owned(),
        row["id"].as_str()?.to_owned(),
    )))
}

fn card_read(
    socket: &Socket,
    label: String,
    mut detail: Value,
    timeline: Option<Timeline>,
) -> Result<Outcome, Failure> {
    let display = detail["card"]["display_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let summary = format!("{display} is on {label}: read through this host's daemon.");
    let Some(timeline) = timeline else {
        detail["host"] = Value::String(label);
        return Ok(Outcome::read(detail).with_summary(summary));
    };
    let id = detail["card"]["id"].as_str().unwrap_or_default();
    let mut path = format!(
        "/api/v1/cards/{}/timeline?host={}",
        encode(id),
        encode(&label)
    );
    if let Some(before) = timeline.before {
        path.push_str(&format!("&before={before}"));
    }
    let mut read = socket.get(&path).map_err(|refusal| failure(&refusal))?;
    if let Some(entries) = read["entries"].as_array_mut() {
        entries.truncate(usize::try_from(timeline.limit).unwrap_or(0));
    }
    read["host"] = Value::String(label);
    Ok(Outcome::read(read).with_summary(summary))
}

/// Sends one change for the peer `label` and shapes its answer as this
/// host's own. Without `--key` the request gets a new key, which the answer
/// shows, so that a retry after a timeout replays it.
fn write(
    socket: &Socket,
    method: &str,
    path: &str,
    label: String,
    mut body: Value,
    key: Option<String>,
    named: &str,
) -> Result<Outcome, Failure> {
    let minted = key.is_none();
    let key = key.unwrap_or_else(tiller_core::ids::new_ulid);
    body["host"] = Value::String(label.clone());
    body["idempotency_key"] = Value::String(key.clone());
    let answer = socket
        .request(method, path, Some(&body), WRITE_TIMEOUT)
        .map_err(|refusal| failure(&refusal))?;
    let events = answer["events"]
        .as_array()
        .map(|events| events.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let replayed = answer["replayed"] == true;
    let mut outcome = Outcome::read(answer["data"].clone()).with_summary(format!(
        "{label} applied the change to {named}, through this host's daemon."
    ));
    outcome.mutation = Some((events, replayed));
    outcome.warning = answer["warning"].as_str().map(str::to_owned);
    if minted {
        outcome.key = Some(key);
    }
    Ok(outcome)
}

/// The daemon's refusal as the CLI's own failure, with the code and exit
/// status a refusal of this host's would have (`protocol.md` 6.2).
fn failure(refusal: &Refusal) -> Failure {
    let (code, exit) = match refusal.error["code"].as_str().unwrap_or_default() {
        "usage" => ("usage", 2),
        "not_found" => ("not_found", 3),
        "conflict" => ("conflict", 4),
        "refused" => ("refused", 5),
        "peer_refused" => ("peer_refused", 5),
        "forbidden" => ("forbidden", 6),
        "read_scope" => ("read_scope", 6),
        "unavailable" => ("unavailable", 7),
        "peer_unreachable" => ("peer_unreachable", 7),
        _ if refusal.status >= 500 || refusal.status == 0 => ("unavailable", 7),
        _ => ("refused", 5),
    };
    let mut failure = Failure::new(code, exit, refusal.message());
    let text = |name: &str| refusal.error[name].as_str().map(str::to_owned);
    failure.entity = text("entity");
    failure.id = text("id");
    failure.hint = text("hint");
    failure.current = refusal.error.get("current").cloned();
    failure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_to_its_length() {
        let answer = b"HTTP/1.0 403 Forbidden\r\ncontent-length: 11\r\n\r\n{\"ok\":false}trailing";
        assert_eq!(parse(answer), None, "a cut body is no JSON");
        let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n{\"a\":1}\n";
        assert_eq!(parse(answer), Some((200, json!({"a": 1}))));
        assert_eq!(parse(b"garbage"), None);
    }

    #[test]
    fn a_value_is_encoded_for_a_url() {
        assert_eq!(encode("alpha-42"), "alpha-42");
        assert_eq!(encode("a b/c?d"), "a%20b%2Fc%3Fd");
    }

    #[test]
    fn a_peer_agent_keeps_its_workers_under_it() {
        let group = agent_group(&json!({"role": "orchestrator", "session_id": "s1",
            "workers": [{"role": "build", "session_id": "s2"}], "link": "live"}));
        assert_eq!(group["orchestrator"]["session_id"], "s1");
        assert_eq!(group["workers"][0]["session_id"], "s2");
        assert!(group["orchestrator"].get("workers").is_none());
        let alone = agent_group(&json!({"role": "build", "session_id": "s3", "workers": []}));
        assert_eq!(alone["orchestrator"], Value::Null);
        assert_eq!(alone["workers"][0]["session_id"], "s3");
    }
}
