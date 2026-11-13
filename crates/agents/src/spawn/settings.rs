//! The settings an orchestrator start needs, checked the one way
//! `tiller orchestrator start` checks them and reported all at once: the
//! project's `checkouts.<project>`, `spawn.pool_root` and
//! `spawn.checkouts_root`, with the orchestrator home and the scratch root
//! they must stay clear of (`protocol.md` 6.4).
//!
//! A start returns the first problem. The board shows every one, and checks
//! a value before it writes it with the same rules, so a value it saves is
//! one the start accepts.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::config::SpawnConfig;

pub const POOL_ROOT: &str = "spawn.pool_root";
pub const CHECKOUTS_ROOT: &str = "spawn.checkouts_root";

/// Why a setting is a problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The key is not in `config.toml`.
    Missing,
    /// The key is set to a value a start refuses.
    Invalid,
}

/// One setting a start would refuse, with the words the start would use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub key: String,
    pub kind: Kind,
    pub problem: String,
}

/// The paths of a start that could be resolved. A path that is missing or
/// refused is `None`, and has a [`Problem`].
#[derive(Debug, Default)]
pub(super) struct Resolved {
    pub checkout: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub pool: Option<PathBuf>,
    pub checkouts: Option<PathBuf>,
}

fn named(key: &str, path: Option<&Path>) -> Result<PathBuf, Problem> {
    let problem = |kind, problem| Problem {
        key: key.to_owned(),
        kind,
        problem,
    };
    let path = path.ok_or_else(|| {
        problem(
            Kind::Missing,
            format!("{key} is not set; tiller orchestrator start needs it"),
        )
    })?;
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(problem(
            Kind::Invalid,
            format!(
                "{key} must be an absolute path without `..`, not {}",
                path.display()
            ),
        ));
    }
    // No `.` parts or trailing slash, so `starts_with` compares what the
    // fragment will name.
    Ok(path.components().collect())
}

/// Checks every setting a start of `project` reads, and returns what
/// resolved with every problem found, in the order a start would meet them.
/// Without a `project` the checkout is not read, and the homes folder
/// stands for the home.
pub(super) fn resolve(
    config: &SpawnConfig,
    home_root: &Path,
    scratch_root: &Path,
    project: Option<&str>,
) -> (Resolved, Vec<Problem>) {
    let mut problems = Vec::new();
    let mut keep = |found: Result<PathBuf, Problem>| match found {
        Ok(path) => Some(path),
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    let checkout_key = project.map(|project| format!("checkouts.{project}"));
    let checkout = match (&checkout_key, project) {
        (Some(key), Some(project)) => keep(named(
            key,
            config.checkouts.get(project).map(PathBuf::as_path),
        )),
        _ => None,
    };
    let pool = keep(named(POOL_ROOT, config.spawn.pool_root.as_deref()));
    let checkouts = keep(named(
        CHECKOUTS_ROOT,
        config.spawn.checkouts_root.as_deref(),
    ));
    let home = keep(named("spawn.orchestrator_home", Some(home_root))).map(|root| match project {
        Some(project) => root.join(project),
        None => root,
    });
    let scratch = keep(named("spawn.scratch_root", Some(scratch_root)));

    if let (Some(checkout), Some(checkouts), Some(key)) = (&checkout, &checkouts, &checkout_key)
        && !checkout.starts_with(checkouts)
    {
        problems.push(Problem {
            key: key.clone(),
            kind: Kind::Invalid,
            problem: format!(
                "{key} ({}) is not under spawn.checkouts_root ({})",
                checkout.display(),
                checkouts.display()
            ),
        });
    }
    let what_home = if project.is_some() {
        "the orchestrator home"
    } else {
        "the orchestrator homes folder"
    };
    for (what, path) in [(what_home, &home), ("spawn.scratch_root", &scratch)] {
        let Some(path) = path else { continue };
        for (key, root) in [(POOL_ROOT, &pool), (CHECKOUTS_ROOT, &checkouts)] {
            let Some(root) = root else { continue };
            let said = problems
                .iter()
                .any(|problem| problem.key == key && problem.kind == Kind::Invalid);
            if !said && (path.starts_with(root) || root.starts_with(path)) {
                problems.push(Problem {
                    key: key.to_owned(),
                    kind: Kind::Invalid,
                    problem: format!(
                        "{what} {} overlaps {key} ({})",
                        path.display(),
                        root.display()
                    ),
                });
            }
        }
    }
    if let (Some(checkout), Some(key)) = (&checkout, &checkout_key)
        && !checkout.is_dir()
    {
        problems.push(Problem {
            key: key.clone(),
            kind: Kind::Invalid,
            problem: format!("{key} ({}) is not a directory", checkout.display()),
        });
    }
    (
        Resolved {
            checkout,
            home,
            pool,
            checkouts,
        },
        problems,
    )
}

/// Every setting `project`'s start would refuse; with no project, the ones
/// every start needs.
pub fn problems(
    config: &SpawnConfig,
    home_root: &Path,
    scratch_root: &Path,
    project: Option<&str>,
) -> Vec<Problem> {
    resolve(config, home_root, scratch_root, project).1
}

/// Whether `key` is one of the start settings: the two host roots and a
/// project's checkout. [`check`] judges these.
pub fn start_setting(key: &str) -> bool {
    key == POOL_ROOT
        || key == CHECKOUTS_ROOT
        || key
            .strip_prefix("checkouts.")
            .is_some_and(|project| !project.is_empty() && !project.contains('.'))
}

/// Whether `key` is one the board may set: a start setting, or one of the
/// other host keys in [`tiller_core::host_config`]. The decider key is a
/// secret and is neither.
pub fn settable(key: &str) -> bool {
    start_setting(key) || tiller_core::host_config::is_key(key)
}

/// Checks `value` for `key` before it is written: the rules a start applies
/// to that key, on a copy of `config` with the value in place. Returns what
/// else the change leaves wrong (a configured checkout outside a new
/// checkouts root), or the refusal in plain words.
pub fn check(
    config: &SpawnConfig,
    home_root: &Path,
    scratch_root: &Path,
    key: &str,
    value: &str,
) -> Result<Vec<String>, String> {
    if !start_setting(key) {
        return Err(format!("{key} is not a setting the board can change"));
    }
    if value.trim().is_empty() {
        return Err(format!("{key} needs a folder"));
    }
    if value != value.trim() || value.chars().any(char::is_control) {
        return Err(format!(
            "{key} must not have spaces at the ends or control characters"
        ));
    }
    if value.starts_with('~') {
        return Err(format!(
            "{key} must be a full path starting with /, not {value}: `~` is not expanded here"
        ));
    }
    let mut next = config.clone();
    let path = PathBuf::from(value);
    let project = match key {
        POOL_ROOT => {
            next.spawn.pool_root = Some(path);
            None
        }
        CHECKOUTS_ROOT => {
            next.spawn.checkouts_root = Some(path);
            None
        }
        _ => {
            let project = key.trim_start_matches("checkouts.");
            next.checkouts.insert(project.to_owned(), path);
            Some(project)
        }
    };
    let (_, found) = resolve(&next, home_root, scratch_root, project);
    if let Some(problem) = found.into_iter().find(|problem| problem.key == key) {
        return Err(problem.problem);
    }
    let mut warnings = Vec::new();
    if key == CHECKOUTS_ROOT {
        let root: PathBuf = Path::new(value).components().collect();
        for (id, checkout) in &next.checkouts {
            let checkout: PathBuf = checkout.components().collect();
            if !checkout.starts_with(&root) {
                warnings.push(format!(
                    "checkouts.{id} ({}) is not under the new spawn.checkouts_root, so its orchestrator will not start until it is",
                    checkout.display()
                ));
            }
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(pool: Option<&str>, root: Option<&str>, checkouts: &[(&str, &str)]) -> SpawnConfig {
        let mut config = SpawnConfig::default();
        config.spawn.pool_root = pool.map(PathBuf::from);
        config.spawn.checkouts_root = root.map(PathBuf::from);
        for (id, path) in checkouts {
            config
                .checkouts
                .insert((*id).to_owned(), PathBuf::from(path));
        }
        config
    }

    const HOMES: &str = "/home/user/state/orchestrators";
    const SCRATCH: &str = "/home/user/state/runs";

    fn found(config: &SpawnConfig, project: Option<&str>) -> Vec<Problem> {
        problems(config, Path::new(HOMES), Path::new(SCRATCH), project)
    }

    #[test]
    fn a_root_over_the_homes_and_the_scratch_is_one_problem() {
        let all = found(&config(Some("/home/user/state"), None, &[]), None);
        let overlaps = all.iter().filter(|p| p.key == POOL_ROOT).count();
        assert_eq!(overlaps, 1, "{all:?}");
    }

    #[test]
    fn a_value_with_spaces_at_the_ends_is_refused() {
        let refused = check(
            &config(None, None, &[]),
            Path::new(HOMES),
            Path::new(SCRATCH),
            POOL_ROOT,
            "/srv/pool ",
        );
        assert!(refused.is_err(), "{refused:?}");
    }

    #[test]
    fn every_missing_setting_is_named_at_once() {
        let all = found(&config(None, None, &[]), Some("alpha"));
        let keys: Vec<_> = all.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(
            keys,
            ["checkouts.alpha", "spawn.pool_root", "spawn.checkouts_root"]
        );
        assert!(all.iter().all(|p| p.kind == Kind::Missing));
        assert_eq!(
            all[0].problem,
            "checkouts.alpha is not set; tiller orchestrator start needs it"
        );
    }

    #[test]
    fn host_settings_are_reported_without_a_project() {
        let all = found(&config(Some("/srv/pool"), None, &[]), None);
        let keys: Vec<_> = all.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["spawn.checkouts_root"]);
    }

    #[test]
    fn a_relative_or_dotdot_path_is_invalid() {
        let all = found(&config(Some("pool"), Some("/srv/a/../b"), &[]), None);
        assert!(all.iter().all(|p| p.kind == Kind::Invalid), "{all:?}");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn a_checkout_outside_the_root_and_a_root_over_the_home_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("alpha");
        std::fs::create_dir(&checkout).unwrap();
        let config = config(
            Some("/srv/pool"),
            Some("/srv/code"),
            &[("alpha", checkout.to_str().unwrap())],
        );
        let all = found(&config, Some("alpha"));
        assert_eq!(all.len(), 1, "{all:?}");
        assert!(all[0].problem.contains("is not under spawn.checkouts_root"));

        let over = self::config(Some("/home/user/state"), Some("/srv/code"), &[]);
        let all = found(&over, None);
        assert!(
            all.iter()
                .any(|p| p.key == POOL_ROOT && p.problem.contains("overlaps")),
            "{all:?}"
        );
    }

    #[test]
    fn a_value_is_checked_by_the_rules_of_a_start() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let existing = tmp.path().join("alpha");
        std::fs::create_dir(&existing).unwrap();
        let base = config(None, Some(root), &[]);
        let run =
            |key: &str, value: &str| check(&base, Path::new(HOMES), Path::new(SCRATCH), key, value);
        assert!(run(POOL_ROOT, "/srv/pool").is_ok());
        assert!(run(POOL_ROOT, "pool").unwrap_err().contains("absolute"));
        let tilde = format!("{}/pool", '~');
        assert!(run(POOL_ROOT, &tilde).unwrap_err().contains("full path"));
        assert!(run(POOL_ROOT, "").unwrap_err().contains("needs a folder"));
        assert!(
            run(POOL_ROOT, "/home/user/state/runs/pool")
                .unwrap_err()
                .contains("overlaps")
        );
        assert!(run("checkouts.alpha", existing.to_str().unwrap()).is_ok());
        assert!(
            run("checkouts.alpha", "/elsewhere/alpha")
                .unwrap_err()
                .contains("is not under spawn.checkouts_root")
        );
        assert!(
            run("checkouts.beta", &format!("{root}/beta"))
                .unwrap_err()
                .contains("is not a directory")
        );
        assert!(
            run("dispatch.default", "x")
                .unwrap_err()
                .contains("not a setting")
        );
        assert!(run("checkouts.a.b", "/x").is_err());
    }

    #[test]
    fn the_host_keys_are_settable_and_the_secret_is_not() {
        for key in [
            "attach.terminal",
            "stop_hook_mode",
            "federation.accept",
            "memory.remote",
            "learning.sample_every",
            "learning.window_cards",
            "learning.curator_model",
            "learning.curator_timeout_minutes",
            "learning.evals_dir",
            POOL_ROOT,
            "checkouts.alpha",
        ] {
            assert!(settable(key), "{key}");
        }
        for key in [
            "owner_guard.decider_key",
            "owner_guard.enabled",
            "owner_guard",
            "intake_source",
            "learning",
            "attach",
            "checkouts.a.b",
            "",
        ] {
            assert!(!settable(key), "{key}");
        }
        // A host key is not a start setting: `check` holds the start rules only.
        let base = config(None, None, &[]);
        let run = |key: &str| check(&base, Path::new(HOMES), Path::new(SCRATCH), key, "x");
        assert!(
            run("attach.terminal")
                .unwrap_err()
                .contains("not a setting")
        );
        assert!(
            run("owner_guard.decider_key")
                .unwrap_err()
                .contains("not a setting")
        );
    }

    #[test]
    fn a_new_root_warns_about_checkouts_left_outside() {
        let base = config(None, None, &[("alpha", "/srv/old/alpha")]);
        let warnings = check(
            &base,
            Path::new(HOMES),
            Path::new(SCRATCH),
            CHECKOUTS_ROOT,
            "/srv/code",
        )
        .unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("checkouts.alpha"));
    }
}
