//! The host config keys the board may set besides the start settings: what
//! each key takes, checked one key at a time so a value it saves is one the
//! code that reads the key accepts. `owner_guard.decider_key` is a secret and
//! is not here; the board never writes it (`tiller config set` does).

/// The terminal app "Open in terminal" brings to the front
/// (`terminal-holder.md` 11.1).
pub const ATTACH_TERMINAL: &str = "attach.terminal";
pub const STOP_HOOK_MODE: &str = "stop_hook_mode";
pub const FEDERATION_ACCEPT: &str = "federation.accept";

/// Every key here, in the order the board lists them.
pub const KEYS: [&str; 3] = [ATTACH_TERMINAL, STOP_HOOK_MODE, FEDERATION_ACCEPT];

/// Keys of the old memory store and the learning loop (ADR 0005). Nothing
/// reads them now; `tiller doctor` names each one still set.
const RETIRED: [&str; 3] = ["memory.remote", "memory.review_days", "learning"];

/// The retired keys `config.toml` still sets, as dotted names, in order.
pub fn retired(table: &toml::Table) -> Vec<String> {
    let mut found = Vec::new();
    for key in RETIRED {
        let mut parts = key.split('.');
        let Some(first) = parts.next() else { continue };
        let value = parts.try_fold(table.get(first), |value, part| {
            Some(value.and_then(|v| v.get(part)))
        });
        match value.flatten() {
            Some(toml::Value::Table(inner)) => {
                found.extend(inner.keys().map(|k| format!("{key}.{k}")));
            }
            Some(_) => found.push(key.to_owned()),
            None => {}
        }
    }
    found
}

/// The values `stop_hook_mode` takes (`protocol.md` 2.5).
pub const STOP_HOOK_MODES: [&str; 3] = ["off", "observe", "block-once"];

/// Whether the board may set `key` through [`check`].
pub fn is_key(key: &str) -> bool {
    KEYS.contains(&key)
}

fn plain(key: &str, value: &str) -> Result<(), String> {
    if value != value.trim() || value.chars().any(char::is_control) {
        return Err(format!(
            "{key} must not have spaces at the ends or control characters"
        ));
    }
    Ok(())
}

/// Checks `value` for `key` and returns it as the TOML the reader of the key
/// takes, or the refusal in plain words. It does not judge the switching rule
/// of `stop_hook_mode`: that reads the store (`state::stop_hook`).
pub fn check(key: &str, value: &str) -> Result<toml::Value, String> {
    if !is_key(key) {
        return Err(format!("{key} is not a setting the board can change"));
    }
    plain(key, value)?;
    let text = |value: &str| Ok(toml::Value::String(value.to_owned()));
    match key {
        ATTACH_TERMINAL => {
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
        _ => match value {
            "true" => Ok(toml::Value::Boolean(true)),
            "false" => Ok(toml::Value::Boolean(false)),
            _ => Err(format!("{key} is true or false (ADR 0003)")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(key: &str, value: &str) -> toml::Value {
        check(key, value).unwrap_or_else(|error| panic!("{key}={value}: {error}"))
    }

    fn refused(key: &str, value: &str) -> String {
        check(key, value).expect_err(&format!("{key}={value} should be refused"))
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
        assert_eq!(ok(STOP_HOOK_MODE, "block-once"), "block-once".into());
        assert_eq!(ok(FEDERATION_ACCEPT, "false"), false.into());
    }

    #[test]
    fn values_a_reader_would_refuse_are_refused_here() {
        assert!(refused(ATTACH_TERMINAL, "").contains("names the terminal"));
        assert!(refused(ATTACH_TERMINAL, " Ghostty").contains("spaces at the ends"));
        assert!(refused(STOP_HOOK_MODE, "block once").contains("off, observe or block-once"));
        assert!(refused(STOP_HOOK_MODE, "").contains("off, observe"));
        assert!(refused(FEDERATION_ACCEPT, "yes").contains("true or false"));
        assert!(refused(FEDERATION_ACCEPT, "1").contains("true or false"));
    }

    #[test]
    fn the_retired_keys_are_named_and_no_longer_settable() {
        for key in [
            "memory.remote",
            "learning.sample_every",
            "learning.evals_dir",
        ] {
            assert!(!is_key(key), "{key}");
        }
        let table: toml::Table =
            "[memory]\nremote = \"git@example.com:team/m.git\"\nkb_scopes = [\"global\"]\n\
             [memory.review_days]\nrule = 30\n[learning]\nsample_every = 5\nwindow_cards = 10\n"
                .parse()
                .unwrap();
        assert_eq!(
            retired(&table),
            [
                "memory.remote",
                "memory.review_days.rule",
                "learning.sample_every",
                "learning.window_cards"
            ]
        );
        assert!(retired(&toml::Table::new()).is_empty());
    }
}
