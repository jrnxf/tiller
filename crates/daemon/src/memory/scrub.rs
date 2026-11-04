//! The scrub hook on every memory commit (`memory.md` section 8): the rules
//! of `scripts/scrub-check.sh`, checked on the added lines and the commit
//! message before the commit leaves the repo lock. A finding names the rule
//! and the line, never the text that tripped it.

use std::path::Path;

use sha2::{Digest, Sha256};
use tiller_agents::land::scrub::ScrubList;

/// The host's private names: `~/.tiller/scrub-terms.sha256`, plus the
/// optional plain-text list `TILLER_SCRUB_TERMS` names, kept outside any repo.
pub fn terms(home: &Path) -> ScrubList {
    let mut terms = std::fs::read_to_string(home.join("scrub-terms.sha256")).unwrap_or_default();
    if let Some(extra) = std::env::var_os("TILLER_SCRUB_TERMS") {
        terms.push('\n');
        terms.push_str(&std::fs::read_to_string(extra).unwrap_or_default());
    }
    ScrubList::parse(&terms)
}

/// The first finding in `text`, as `<rule> on line <n>`.
pub fn check(text: &str, list: &ScrubList) -> Option<String> {
    text.lines()
        .enumerate()
        .find_map(|(at, line)| rule(line, list).map(|rule| format!("{rule} on line {}", at + 1)))
}

/// The first finding in the lines a unified diff adds (what a commit brings
/// in), numbered by their line in the new file.
pub fn check_diff(diff: &str, list: &ScrubList) -> Option<String> {
    let mut line: Option<usize> = None;
    for text in diff.lines() {
        if let Some(header) = text.strip_prefix("@@ ") {
            line = header
                .split(' ')
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|range| range.split(',').next()?.parse().ok());
            continue;
        }
        // The file headers, before the first hunk.
        let Some(at) = line.as_mut() else { continue };
        if let Some(added) = text.strip_prefix('+') {
            if let Some(rule) = rule(added, list) {
                return Some(format!("{rule} on line {at}"));
            }
            *at += 1;
        } else if !text.starts_with('-') && !text.starts_with('\\') {
            *at += 1;
        }
    }
    None
}

/// A rule's check, on the line and on its lowercase copy.
type Check = fn(&[u8], &[u8]) -> bool;

/// The first rule `line` breaks.
fn rule(line: &str, list: &ScrubList) -> Option<&'static str> {
    let b = line.as_bytes();
    let lower = line.to_ascii_lowercase();
    let l = lower.as_bytes();
    let checks: [(&'static str, Check); 8] = [
        ("home path", home_path),
        // Split so this repo's own scrub check passes.
        ("mounted volume", |b, _| {
            contains(b, concat!("/Volume", "s/").as_bytes())
        }),
        ("home-relative path", home_relative),
        ("LAN link", |_, l| lan_link(l)),
        ("LAN hostname", |_, l| lan_hostname(l)),
        ("machine name", |_, l| machine_name(l)),
        ("email address", |b, _| email(b)),
        ("IP address", |b, _| private_ip(b)),
    ];
    if let Some((name, _)) = checks.iter().find(|(_, check)| check(b, l)) {
        return Some(name);
    }
    private_term(line, list).then_some("private name")
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn token(b: u8) -> bool {
    word(b) || b == b'.' || b == b'-'
}

/// The end of the run of bytes from `at` that match `class`.
fn run(b: &[u8], at: usize, class: impl Fn(u8) -> bool) -> usize {
    at + b[at.min(b.len())..]
        .iter()
        .take_while(|&&c| class(c))
        .count()
}

fn contains(b: &[u8], needle: &[u8]) -> bool {
    finds(b, needle).next().is_some()
}

/// Every position `needle` starts at.
fn finds<'a>(b: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    (0..b.len().saturating_sub(needle.len() - 1)).filter(move |&at| b[at..].starts_with(needle))
}

fn before(b: &[u8], at: usize) -> Option<u8> {
    at.checked_sub(1).map(|at| b[at])
}

/// `/Users/<name>` or `/home/<name>` other than `/home/user`, and paths under `/root`.
fn home_path(b: &[u8], _: &[u8]) -> bool {
    let named = |at: usize| {
        let end = run(b, at, token);
        end > at && !(b[at..end].starts_with(b"user") && !b.get(at + 4).copied().is_some_and(word))
    };
    let homes = [&b"/Users/"[..], b"/home/"]
        .iter()
        .any(|home| finds(b, home).any(|at| named(at + home.len())));
    let roots = [&b"/root/"[..], b"/var/root/"].iter().any(|root| {
        finds(b, root).any(|at| {
            !before(b, at).is_some_and(|c| word(c) || c == b'.')
                && run(b, at + root.len(), token) > at + root.len()
        })
    });
    homes || roots
}

/// `~/<name>`, except the dot directories a doc may name.
fn home_relative(b: &[u8], _: &[u8]) -> bool {
    finds(b, b"~/").any(|at| {
        if before(b, at).is_some_and(|c| token(c) || c == b'~' || c == b'/') {
            return false;
        }
        let start = at + 2;
        if run(b, start, token) == start {
            return false;
        }
        let allowed = [
            &b".tiller"[..],
            b".claude",
            b".config",
            b".local",
            b".cache",
        ]
        .iter()
        .any(|dir| {
            b[start..].starts_with(dir) && !b.get(start + dir.len()).copied().is_some_and(word)
        });
        !allowed
    })
}

fn lan_link(l: &[u8]) -> bool {
    finds(l, b"lan.").any(|at| {
        if before(l, at).is_some_and(word) {
            return false;
        }
        let label = at + 4;
        let end = run(l, label, |c| c.is_ascii_alphanumeric() || c == b'-');
        end > label
            && l.get(end) == Some(&b'.')
            && run(l, end + 1, |c| c.is_ascii_lowercase()) >= end + 3
    })
}

fn lan_hostname(l: &[u8]) -> bool {
    let label = |c: u8| c.is_ascii_alphanumeric() || c == b'-';
    [
        &b".lan"[..],
        b".local",
        b".internal",
        b".home.arpa",
        b".ts.net",
    ]
    .iter()
    .any(|suffix| {
        finds(l, suffix).any(|at| {
            let mut start = at;
            while start > 0 && label(l[start - 1]) {
                start -= 1;
            }
            // Some start at least two characters back with no `.`, word
            // character or `$` before it.
            let opens = (start..at.saturating_sub(1))
                .any(|s| !before(l, s).is_some_and(|c| word(c) || c == b'.' || c == b'$'));
            let end = at + suffix.len();
            let next = l.get(end).copied();
            let closes = !next.is_some_and(|c| word(c) || c == b'-')
                && !(next == Some(b'.') && l.get(end + 1).copied().is_some_and(word));
            opens && closes
        })
    })
}

fn machine_name(l: &[u8]) -> bool {
    finds(l, b"agents").any(|at| {
        if before(l, at).is_some_and(word) {
            return false;
        }
        let mut next = at + 6;
        if matches!(l.get(next), Some(b'"' | b'\'' | b'`')) {
            next += 1;
        }
        if l.get(next) != Some(&b' ') {
            return false;
        }
        let rest = &l[next + 1..];
        [&b"host"[..], b"machine", b"box"]
            .iter()
            .any(|noun| rest.starts_with(noun) && !rest.get(noun.len()).copied().is_some_and(word))
    })
}

fn email(b: &[u8]) -> bool {
    let local = |c: u8| word(c) || matches!(c, b'.' | b'%' | b'+' | b'-');
    b.iter()
        .enumerate()
        .filter(|(_, c)| **c == b'@')
        .any(|(at, _)| {
            let mut start = at;
            while start > 0 && local(b[start - 1]) {
                start -= 1;
            }
            if !b[start..at].iter().copied().any(word) {
                return false;
            }
            let domain = &b[at + 1..];
            let allowed = [&b"example.com"[..], b"example.org", b"example.net"]
                .iter()
                .any(|ok| {
                    domain.starts_with(ok) && !domain.get(ok.len()).copied().is_some_and(word)
                });
            if allowed {
                return false;
            }
            let end = run(domain, 0, |c| {
                c.is_ascii_alphanumeric() || c == b'-' || c == b'.'
            });
            let labels: Vec<&[u8]> = domain[..end].split(|c| *c == b'.').collect();
            let first = labels.iter().take_while(|label| !label.is_empty()).count();
            (1..first).any(|i| {
                let label = labels[i];
                let letters = label.iter().take_while(|c| c.is_ascii_alphabetic()).count();
                letters >= 2 && label.get(letters).is_none_or(|c| *c == b'-')
            })
        })
}

fn private_ip(b: &[u8]) -> bool {
    let digit = |c: u8| c.is_ascii_digit();
    (0..b.len()).any(|at| {
        if !digit(b[at]) || before(b, at).is_some_and(|c| digit(c) || c == b'.') {
            return false;
        }
        let mut octets = [0u32; 4];
        let mut next = at;
        for (i, octet) in octets.iter_mut().enumerate() {
            let end = run(b, next, digit);
            if end == next || end - next > 3 {
                return false;
            }
            *octet = std::str::from_utf8(&b[next..end])
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(999);
            next = end;
            if i < 3 {
                if b.get(next) != Some(&b'.') {
                    return false;
                }
                next += 1;
            }
        }
        let tail = run(b, next, |c| digit(c) || c == b'.');
        if b[next..tail].iter().copied().any(digit) {
            return false;
        }
        let [a, x, y, _] = octets;
        let documentation = matches!((a, x, y), (192, 0, 2) | (198, 51, 100) | (203, 0, 113));
        !(octets.iter().any(|o| *o > 255)
            || a == 0
            || a == 127
            || documentation
            || octets == [192, 168, 1, 20])
    })
}

/// A private name: a hashed token (whole, or a part split on `.`, `_` and
/// `-`) or a plain phrase from the extra list.
fn private_term(line: &str, list: &ScrubList) -> bool {
    let lower = line.to_lowercase();
    if list
        .phrases
        .iter()
        .any(|phrase| lower.contains(phrase.as_str()))
    {
        return true;
    }
    if list.hashes.is_empty() {
        return false;
    }
    let hashed = |part: &str| {
        let hex: String = Sha256::digest(part.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        list.hashes.contains(&hex)
    };
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .map(|t| t.trim_matches(['.', '_', '-']))
        .filter(|t| !t.is_empty())
        .any(|t| {
            hashed(t)
                || t.split(['.', '_', '-'])
                    .any(|part| !part.is_empty() && hashed(part))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(line: &str) -> Option<&'static str> {
        let hash: String = Sha256::digest(b"secretproj")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        rule(
            line,
            &ScrubList::parse(&format!("{hash}\nbuild box seven\n")),
        )
    }

    #[test]
    fn every_rule_flags_its_leak() {
        let leaks = [
            (concat!("see /User", "s/someone/notes"), "home path"),
            (concat!("cd /hom", "e/alex/src"), "home path"),
            (concat!("in /roo", "t/.ssh"), "home path"),
            (concat!("copy from /Volume", "s/archive"), "mounted volume"),
            (concat!("open ~", "/notes/todo.md"), "home-relative path"),
            (concat!("served at lan", ".example.co"), "LAN link"),
            (concat!("ssh nas", ".lan first"), "LAN hostname"),
            (concat!("the printer", ".local box"), "LAN hostname"),
            (concat!("runs on the agents", " host"), "machine name"),
            (concat!("mail sam@", "mail.test.io"), "email address"),
            (concat!("ping 10.0", ".4.7"), "IP address"),
            (concat!("the 192.168", ".1.40 router"), "IP address"),
            ("ship the secretproj fix", "private name"),
            ("a part: alpha-secretproj.v2", "private name"),
            ("ask the Build Box Seven", "private name"),
        ];
        for (line, rule) in leaks {
            assert_eq!(flags(line), Some(rule), "{line}");
        }
    }

    #[test]
    fn documentation_values_pass() {
        let clean = [
            "Use /home/user/project in examples",
            "The memory repo is ~/.tiller/memory",
            "Settings live in ~/.config/tiller",
            "Mail owner@example.com about it",
            "Point it at 192.0.2.10, 198.51.100.4 or 203.0.113.9",
            "Bind 127.0.0.1 and 192.168.1.20 only",
            "Version 1.2.3.4.5 is not an address",
            "Run cargo test before landing",
            "Planes land on the runway",
        ];
        for line in clean {
            assert_eq!(flags(line), None, "{line}");
        }
    }

    #[test]
    fn a_finding_names_the_line_and_not_the_text() {
        let list = ScrubList::default();
        let found = check(concat!("fine\nmail sam@", "mail.test.io\n"), &list).unwrap();
        assert_eq!(found, "email address on line 2");
        let diff = concat!(
            "--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+mail sam@",
            "mail.test.io\n"
        );
        assert_eq!(check_diff(diff, &list).unwrap(), "email address on line 3");
        assert_eq!(
            check_diff(
                concat!(
                    "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-sam@",
                    "mail.test.io\n+fine\n"
                ),
                &list
            ),
            None
        );
    }
}
