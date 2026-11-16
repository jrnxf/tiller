//! What the CLI says about a paired host. The CLI reads and writes this
//! host's `state.db` only: the board host's copy of a peer's rows lives in
//! its daemon's memory (`protocol.md` 10.2). So a name that a peer owns is
//! "not found" here, and a list is this host's part of the whole. A person
//! is told both, with the command that runs the same thing on the peer.

use std::ffi::OsString;
use std::path::Path;

use tiller_core::Store;

use crate::app::{App, Failure};

/// The entities a paired host may own under the name the caller typed.
const OWNED_BY_A_HOST: [&str; 7] = [
    "project",
    "card",
    "ask",
    "attention",
    "run",
    "session",
    "approval",
];

/// One host this board host is paired with.
#[derive(Debug, PartialEq)]
struct Peer {
    label: String,
    /// Config `peers.<label>.ssh`: the alias the link dials.
    alias: Option<String>,
}

/// This host and the hosts it is paired with, as a person needs them named.
#[derive(Debug, PartialEq)]
pub struct Elsewhere {
    this: String,
    peers: Vec<Peer>,
}

impl Elsewhere {
    /// The paired hosts, when a person runs the command on a board host
    /// that has one. An agent session gets none: it works on this host.
    pub fn of(app: &App) -> Option<Self> {
        if !app.person() || !app.db.exists() {
            return None;
        }
        let store = app.read().ok()?;
        Self::read(&store, &app.home)
    }

    fn read(store: &Store, home: &Path) -> Option<Self> {
        let peers: Vec<Peer> = store
            .paired_hosts()
            .ok()?
            .into_iter()
            .map(|host| Peer {
                alias: tiller_daemon::peer::peer_alias(home, &host.label),
                label: host.label,
            })
            .collect();
        if peers.is_empty() {
            return None;
        }
        Some(Self {
            this: super::summary::host_label(store),
            peers,
        })
    }

    /// The paired hosts by label: "peer", or "alpha or beta".
    fn labels(&self) -> String {
        self.peers
            .iter()
            .map(|peer| peer.label.as_str())
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// The same command on the peer, when one peer with an alias makes it
    /// certain where to run it. `ssh -t`: the peer takes an owner command
    /// only from a terminal.
    fn command(&self, args: &[String]) -> Option<String> {
        let [peer] = self.peers.as_slice() else {
            return None;
        };
        let alias = peer.alias.as_deref()?;
        let mut line = format!("ssh -t {} tiller", quote(alias));
        for arg in args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        Some(line)
    }

    /// Says where else a name may live, on a `not_found` that names one.
    pub fn explain(&self, failure: &mut Failure, args: &[String]) {
        if failure.code != "not_found" || failure.hint.is_some() {
            return;
        }
        let owned = failure
            .entity
            .as_deref()
            .is_some_and(|entity| OWNED_BY_A_HOST.contains(&entity));
        if !owned {
            return;
        }
        failure.message = format!(
            "{} on {}. This command reads {} only, and {} may have it: the board shows every host",
            failure.message,
            self.this,
            self.this,
            self.labels(),
        );
        failure.hint = self.command(args);
    }

    /// The warning of a list that covers this host only.
    pub fn only_here(&self, args: &[String]) -> String {
        let mut line = format!(
            "This lists {} only. {} is paired and is not counted here: the board shows every host.",
            self.this,
            self.labels(),
        );
        if let Some(command) = self.command(args) {
            line.push_str(&format!(" For {}, run: {command}", self.labels()));
        }
        line
    }

    /// The warning of an away command. Away mode is one host's contract
    /// (`authority.md` section 6): its spend cap, its merge rule and its
    /// orders bind this host only, and the board host cannot start it on a
    /// peer. A person who leaves must not believe that the peer is covered.
    pub fn away_here(&self, starting: bool, args: &[String]) -> String {
        let peers = self.labels();
        if !starting {
            let mut line = format!(
                "This covers {} only. {peers} is paired and keeps an away mode of its own, \
                 which is not shown here.",
                self.this,
            );
            if let Some(command) = self.command(args) {
                line.push_str(&format!(" For {peers}, run: {command}"));
            }
            return line;
        }
        let mut line = format!(
            "Away mode is on for {} only. {peers} is paired and is not covered: its projects \
             keep running with no away limit, and its grants still merge.",
            self.this,
        );
        let start = ["away", "start"].map(str::to_owned);
        if let Some(command) = self.command(&start) {
            line.push_str(&format!(
                " To cover {peers} too, run: {command} --until <date> --projects <ids>"
            ));
        } else {
            line.push_str(&format!(
                " To cover {peers} too, run `tiller away start` in a shell there."
            ));
        }
        line
    }
}

/// The arguments to repeat on a peer: the ones the caller typed, without
/// `--db` and its path, which name a file of this host.
pub fn typed(args: impl Iterator<Item = OsString>) -> Vec<String> {
    let mut kept = Vec::new();
    let mut skip = false;
    for arg in args.skip(1) {
        let arg = arg.to_string_lossy().into_owned();
        if skip {
            skip = false;
        } else if arg == "--db" {
            skip = true;
        } else if !arg.starts_with("--db=") {
            kept.push(arg);
        }
    }
    kept
}

/// One argument as a POSIX shell reads it back unchanged.
fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+".contains(c));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_peer(alias: Option<&str>) -> Elsewhere {
        Elsewhere {
            this: "main".into(),
            peers: vec![Peer {
                label: "peer".into(),
                alias: alias.map(str::to_owned),
            }],
        }
    }

    fn not_found(entity: &'static str, id: &str) -> Failure {
        Failure::from(tiller_core::Error::NotFound {
            entity,
            id: id.into(),
            candidates: Vec::new(),
        })
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn a_missing_project_names_the_peer_and_the_command_for_it() {
        let mut failure = not_found("project", "pylon");
        one_peer(Some("peer.example.com")).explain(
            &mut failure,
            &args(&["ask", "note", "pylon", "--text", "What's the port?"]),
        );
        assert_eq!(
            failure.message,
            "no project pylon on main. This command reads main only, and peer may have it: \
             the board shows every host"
        );
        assert_eq!(
            failure.hint.as_deref(),
            Some("ssh -t peer.example.com tiller ask note pylon --text 'What'\\''s the port?'")
        );
        assert_eq!(failure.exit, 3);
    }

    #[test]
    fn two_peers_or_no_alias_name_the_hosts_without_a_command() {
        let mut failure = not_found("card", "pylon-2");
        one_peer(None).explain(&mut failure, &args(&["card", "show", "pylon-2"]));
        assert!(failure.message.contains("peer may have it"));
        assert_eq!(failure.hint, None);

        let two = Elsewhere {
            this: "main".into(),
            peers: vec![
                Peer {
                    label: "alpha".into(),
                    alias: Some("alpha.example.com".into()),
                },
                Peer {
                    label: "beta".into(),
                    alias: Some("beta.example.com".into()),
                },
            ],
        };
        let mut failure = not_found("card", "pylon-2");
        two.explain(&mut failure, &args(&["card", "show", "pylon-2"]));
        assert!(failure.message.contains("alpha or beta may have it"));
        assert_eq!(failure.hint, None);
    }

    #[test]
    fn other_failures_stay_as_they_are() {
        let mut ambiguous = Failure::from(tiller_core::Error::NotFound {
            entity: "card",
            id: "01".into(),
            candidates: vec!["01A".into(), "01B".into()],
        });
        let before = ambiguous.message.clone();
        one_peer(Some("peer.example.com")).explain(&mut ambiguous, &args(&["card", "show", "01"]));
        assert_eq!(ambiguous.message, before);
        assert_eq!(ambiguous.hint.as_deref(), Some("use the full id"));

        let mut host = not_found("host", "nowhere");
        let before = host.message.clone();
        one_peer(Some("peer.example.com")).explain(&mut host, &args(&["agents"]));
        assert_eq!(host.message, before);

        let mut refused = Failure::refused("no");
        one_peer(Some("peer.example.com")).explain(&mut refused, &args(&["spawn"]));
        assert_eq!(refused.message, "no");
        assert_eq!(refused.hint, None);
    }

    #[test]
    fn a_list_says_it_covers_this_host_only() {
        assert_eq!(
            one_peer(Some("peer.example.com")).only_here(&args(&["recap"])),
            "This lists main only. peer is paired and is not counted here: the board shows \
             every host. For peer, run: ssh -t peer.example.com tiller recap"
        );
        assert_eq!(
            one_peer(None).only_here(&args(&["recap"])),
            "This lists main only. peer is paired and is not counted here: the board shows \
             every host."
        );
    }

    #[test]
    fn away_mode_says_that_the_peer_is_not_covered() {
        assert_eq!(
            one_peer(Some("peer.example.com")).away_here(true, &args(&["away", "start"])),
            "Away mode is on for main only. peer is paired and is not covered: its projects \
             keep running with no away limit, and its grants still merge. To cover peer too, \
             run: ssh -t peer.example.com tiller away start --until <date> --projects <ids>"
        );
        assert_eq!(
            one_peer(None).away_here(true, &args(&["away", "start"])),
            "Away mode is on for main only. peer is paired and is not covered: its projects \
             keep running with no away limit, and its grants still merge. To cover peer too, \
             run `tiller away start` in a shell there."
        );
        assert_eq!(
            one_peer(Some("peer.example.com")).away_here(false, &args(&["away", "brief"])),
            "This covers main only. peer is paired and keeps an away mode of its own, which \
             is not shown here. For peer, run: ssh -t peer.example.com tiller away brief"
        );
    }

    #[test]
    fn the_typed_arguments_leave_out_the_database_path() {
        let typed = typed(
            [
                "tiller",
                "--db",
                "/home/user/state.db",
                "card",
                "show",
                "pylon-2",
                "--db=/home/user/state.db",
                "--json",
            ]
            .into_iter()
            .map(OsString::from),
        );
        assert_eq!(typed, args(&["card", "show", "pylon-2", "--json"]));
    }

    #[test]
    fn quote_keeps_plain_words_and_wraps_the_rest() {
        assert_eq!(quote("pylon-2"), "pylon-2");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("two words"), "'two words'");
        assert_eq!(quote("$(id)"), "'$(id)'");
    }
}
