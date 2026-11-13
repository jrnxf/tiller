//! The host config keys the board may set besides the start settings: what
//! each key takes, checked one key at a time so a value it saves is one the
//! code that reads the key accepts. `owner_guard.decider_key` is a secret and
//! is not here; the board never writes it (`tiller config set` does).

use std::path::{Component, Path};

/// The terminal app "Open in terminal" brings to the front
/// (`terminal-holder.md` 11.1). `herdr.terminal` is the key it replaces.
pub const ATTACH_TERMINAL: &str = "attach.terminal";
pub const HERDR_TERMINAL: &str = "herdr.terminal";
pub const STOP_HOOK_MODE: &str = "stop_hook_mode";
pub const FEDERATION_ACCEPT: &str = "federation.accept";
pub const MEMORY_REMOTE: &str = "memory.remote";
pub const SAMPLE_EVERY: &str = "learning.sample_every";
pub const WINDOW_CARDS: &str = "learning.window_cards";
pub const CURATOR_MODEL: &str = "learning.curator_model";
pub const CURATOR_TIMEOUT: &str = "learning.curator_timeout_minutes";
pub const EVALS_DIR: &str = "learning.evals_dir";

/// Every key here, in the order the board lists them.
pub const KEYS: [&str; 10] = [
    ATTACH_TERMINAL,
    HERDR_TERMINAL,
    STOP_HOOK_MODE,
    FEDERATION_ACCEPT,
    MEMORY_REMOTE,
    SAMPLE_EVERY,
    WINDOW_CARDS,
    CURATOR_MODEL,
    CURATOR_TIMEOUT,
    EVALS_DIR,
];

/// The values `stop_hook_mode` takes (`protocol.md` 2.5).
pub const STOP_HOOK_MODES: [&str; 3] = ["off", "observe", "block-once"];

/// Whether the board may set `key` through [`check`].
pub fn is_key(key: &str) -> bool {
    KEYS.contains(&key)
}

/// What a value is checked against besides its own shape.
#[derive(Clone, Copy, Debug, Default)]
pub struct Host<'a> {
    /// `personal` or `work` (`memory.md` section 3); a work host never
    /// pushes its memory repo off the machine.
    pub boundary: Option<&'a str>,
}

fn plain(key: &str, value: &str) -> Result<(), String> {
    if value != value.trim() || value.chars().any(char::is_control) {
        return Err(format!(
            "{key} must not have spaces at the ends or control characters"
        ));
    }
    Ok(())
}

fn whole(key: &str, value: &str, min: i64, max: i64) -> Result<toml::Value, String> {
    let number: i64 = value
        .parse()
        .map_err(|_| format!("{key} is a whole number, not {value:?}"))?;
    if !(min..=max).contains(&number) {
        return Err(format!("{key} is a whole number from {min} to {max}"));
    }
    Ok(toml::Value::Integer(number))
}

/// A git remote the memory repo may push to: ssh or https, the `user@host:path`
/// form, or a folder on this machine.
fn remote(value: &str) -> Result<(), String> {
    let refuse = || {
        format!(
            "{MEMORY_REMOTE} is an ssh or https git address such as git@example.com:team/memory.git, or a full path; not {value}"
        )
    };
    if value.starts_with('-') || value.chars().any(char::is_whitespace) {
        return Err(refuse());
    }
    if let Some((scheme, rest)) = value.split_once("://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        return if matches!(scheme, "ssh" | "https") && !host.is_empty() {
            Ok(())
        } else {
            Err(refuse())
        };
    }
    if value.starts_with('/') {
        return if value.len() > 1 && !value.split('/').any(|part| part == "..") {
            Ok(())
        } else {
            Err(refuse())
        };
    }
    // `user@host:path`
    match value.split_once('@').and_then(|(user, rest)| {
        let (host, path) = rest.split_once(':')?;
        Some((user, host, path))
    }) {
        Some((user, host, path))
            if !user.is_empty()
                && !host.is_empty()
                && !path.is_empty()
                && !path.starts_with(':')
                && user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c)) =>
        {
            Ok(())
        }
        _ => Err(refuse()),
    }
}

/// A model name or alias the curator is launched with (`claude --model`).
fn model(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:[]".contains(c))
        || value.starts_with('-')
    {
        return Err(format!(
            "{CURATOR_MODEL} is a model name such as sonnet or opus, not {value:?}"
        ));
    }
    Ok(())
}

/// The folder that holds `evals/run.sh`.
fn evals_dir(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.starts_with('~') {
        return Err(format!(
            "{EVALS_DIR} must be a full path starting with /, not {value}: `~` is not expanded here"
        ));
    }
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(format!(
            "{EVALS_DIR} must be an absolute path without `..`, not {value}"
        ));
    }
    if !path.is_dir() {
        return Err(format!("{EVALS_DIR} ({value}) is not a directory"));
    }
    if !path.join("evals/run.sh").is_file() {
        return Err(format!("{EVALS_DIR} ({value}) has no evals/run.sh"));
    }
    Ok(())
}

/// Checks `value` for `key` and returns it as the TOML the reader of the key
/// takes, or the refusal in plain words. It does not judge the switching rule
/// of `stop_hook_mode`: that reads the store (`state::stop_hook`).
pub fn check(key: &str, value: &str, host: Host<'_>) -> Result<toml::Value, String> {
    if !is_key(key) {
        return Err(format!("{key} is not a setting the board can change"));
    }
    plain(key, value)?;
    let text = |value: &str| Ok(toml::Value::String(value.to_owned()));
    match key {
        ATTACH_TERMINAL | HERDR_TERMINAL => {
            if value.is_empty() {
                return Err(format!("{key} names the terminal app a pane opens in"));
            }
            if value.chars().count() > 100 {
                return Err(format!("{key} is a short app name"));
            }
            text(value)
        }
        STOP_HOOK_MODE => {
            if STOP_HOOK_MODES.contains(&value) {
                text(value)
            } else {
                Err(format!(
                    "{key} is off, observe or block-once, not {value:?}"
                ))
            }
        }
        FEDERATION_ACCEPT => match value {
            "true" => Ok(toml::Value::Boolean(true)),
            "false" => Ok(toml::Value::Boolean(false)),
            _ => Err(format!("{key} is true or false (ADR 0003)")),
        },
        MEMORY_REMOTE => {
            // An empty remote keeps the memory repo on this machine.
            if !value.is_empty() {
                if host.boundary == Some("work") {
                    return Err(format!(
                        "{key} stays empty on a work host: its memory repo is never pushed off the machine"
                    ));
                }
                remote(value)?;
            }
            text(value)
        }
        SAMPLE_EVERY => whole(key, value, 1, 1_000_000),
        WINDOW_CARDS => whole(key, value, 1, 10_000),
        CURATOR_MODEL => {
            model(value)?;
            text(value)
        }
        CURATOR_TIMEOUT => whole(key, value, 1, 1_440),
        _ => {
            evals_dir(value)?;
            text(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(key: &str, value: &str) -> toml::Value {
        check(key, value, Host::default()).unwrap_or_else(|error| panic!("{key}={value}: {error}"))
    }

    fn refused(key: &str, value: &str) -> String {
        check(key, value, Host::default()).expect_err(&format!("{key}={value} should be refused"))
    }

    #[test]
    fn the_secret_key_and_unknown_keys_are_not_settable() {
        for key in [
            "owner_guard.decider_key",
            "owner_guard.endpoint",
            "owner_guard",
            "spawn.pool_root",
            "learning",
            "learning.nope",
            "",
        ] {
            assert!(!is_key(key), "{key}");
            assert!(refused(key, "x").contains("not a setting"), "{key}");
        }
        assert!(KEYS.iter().all(|key| !key.ends_with("decider_key")));
    }

    #[test]
    fn each_key_takes_the_type_its_reader_takes() {
        assert_eq!(ok(ATTACH_TERMINAL, "Ghostty"), "Ghostty".into());
        assert_eq!(ok(HERDR_TERMINAL, "Ghostty"), "Ghostty".into());
        assert_eq!(ok(STOP_HOOK_MODE, "block-once"), "block-once".into());
        assert_eq!(ok(FEDERATION_ACCEPT, "false"), false.into());
        assert_eq!(ok(SAMPLE_EVERY, "5"), 5.into());
        assert_eq!(ok(WINDOW_CARDS, "10"), 10.into());
        assert_eq!(
            ok(CURATOR_MODEL, "claude-sonnet-4-5"),
            "claude-sonnet-4-5".into()
        );
        assert_eq!(ok(CURATOR_TIMEOUT, "20"), 20.into());
    }

    #[test]
    fn values_a_reader_would_refuse_are_refused_here() {
        assert!(refused(HERDR_TERMINAL, "").contains("names the terminal"));
        assert!(refused(ATTACH_TERMINAL, "").contains("names the terminal"));
        assert!(refused(HERDR_TERMINAL, " Ghostty").contains("spaces at the ends"));
        assert!(refused(STOP_HOOK_MODE, "block once").contains("off, observe or block-once"));
        assert!(refused(STOP_HOOK_MODE, "").contains("off, observe"));
        assert!(refused(FEDERATION_ACCEPT, "yes").contains("true or false"));
        assert!(refused(FEDERATION_ACCEPT, "1").contains("true or false"));
        for (key, value) in [
            (SAMPLE_EVERY, "0"),
            (SAMPLE_EVERY, "-1"),
            (SAMPLE_EVERY, "1.5"),
            (SAMPLE_EVERY, "many"),
            (WINDOW_CARDS, "0"),
            (WINDOW_CARDS, "10001"),
            (CURATOR_TIMEOUT, "0"),
            (CURATOR_TIMEOUT, "1441"),
        ] {
            assert!(
                refused(key, value).contains("whole number"),
                "{key}={value}"
            );
        }
        assert!(refused(CURATOR_MODEL, "").contains("model name"));
        assert!(refused(CURATOR_MODEL, "--help").contains("model name"));
        assert!(refused(CURATOR_MODEL, "son net").contains("model name"));
    }

    #[test]
    fn a_memory_remote_is_a_private_git_address_or_empty() {
        for value in [
            "",
            "git@example.com:team/memory.git",
            "ssh://git@example.com/team/memory.git",
            "https://example.com/team/memory.git",
            "/srv/git/memory.git",
        ] {
            ok(MEMORY_REMOTE, value);
        }
        for value in [
            "http://example.com/memory.git",
            "git://example.com/memory.git",
            "ext::sh -c touch% /tmp/x",
            "--upload-pack=x",
            "example.com/memory.git",
            "git@example.com",
            "ssh://",
            "/",
            "/srv/../etc/memory.git",
        ] {
            assert!(
                refused(MEMORY_REMOTE, value).contains("memory.remote"),
                "{value}"
            );
        }
    }

    #[test]
    fn a_work_host_keeps_its_memory_local() {
        let work = Host {
            boundary: Some("work"),
        };
        assert!(check(MEMORY_REMOTE, "git@example.com:team/m.git", work).is_err());
        assert!(check(MEMORY_REMOTE, "", work).is_ok());
        let personal = Host {
            boundary: Some("personal"),
        };
        assert!(check(MEMORY_REMOTE, "git@example.com:team/m.git", personal).is_ok());
    }

    #[test]
    fn the_evals_folder_must_hold_the_runner() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_str().unwrap();
        assert!(refused(EVALS_DIR, dir).contains("no evals/run.sh"));
        std::fs::create_dir(tmp.path().join("evals")).unwrap();
        std::fs::write(tmp.path().join("evals/run.sh"), "#!/bin/sh\n").unwrap();
        assert_eq!(ok(EVALS_DIR, dir), dir.into());
        assert!(refused(EVALS_DIR, "evals").contains("absolute"));
        let tilde = format!("{}/evals", '~');
        assert!(refused(EVALS_DIR, &tilde).contains("full path"));
        assert!(refused(EVALS_DIR, "/nonexistent/evals").contains("not a directory"));
        assert!(refused(EVALS_DIR, &format!("{dir}/../x")).contains("absolute"));
    }
}
