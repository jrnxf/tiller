//! The spawn half of `~/.tiller/config.toml`: the `[dispatch]` rules
//! (`protocol.md` 6.4, P02), the `[quota]` floor and this host's share of it
//! (plan v2 point 35, P04), and the `[spawn]` paths. Fleet roles can neither
//! read nor write the file; `tiller spawn` reads it itself.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tiller_core::domain::{CardKind, Effort, Size};

use super::SpawnError;

/// Everything `tiller spawn` reads from the config file.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnConfig {
    pub dispatch: Dispatch,
    pub quota: Quota,
    pub spawn: Paths,
    /// The local checkout of each project, by project id: where treehouse
    /// cuts the run's worktree from.
    pub checkouts: BTreeMap<String, PathBuf>,
}

/// The dispatch rules: an ordered model list (cheapest first), rules keyed by
/// card kind and size, and a default for anything unmatched.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dispatch {
    pub models: Vec<String>,
    pub rules: Vec<Rule>,
    pub default: Choice,
}

/// One dispatch rule: it matches a card when every field it names matches.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub kind: Option<CardKind>,
    #[serde(default)]
    pub size: Option<Size>,
    pub model: String,
    pub effort: Effort,
}

/// A model and an effort. A table that names one keeps the default other.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Choice {
    pub model: String,
    pub effort: Effort,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            model: "sonnet".into(),
            effort: Effort::Medium,
        }
    }
}

impl Default for Dispatch {
    /// The shipped rules: small work on haiku at low effort, medium work on
    /// sonnet at medium, large work on opus at medium, and sonnet at medium
    /// for everything else. Cards have no field that marks work as hard to
    /// undo, so no rule sends anything to opus at high; the owner raises one
    /// card with `--dispatch`.
    fn default() -> Self {
        let rule = |name: &str, size, model: &str, effort| Rule {
            name: name.into(),
            kind: None,
            size: Some(size),
            model: model.into(),
            effort,
        };
        Self {
            models: ["haiku", "sonnet", "opus", "fable"]
                .map(String::from)
                .into(),
            rules: vec![
                rule("small", Size::S, "haiku", Effort::Low),
                rule("large", Size::L, "opus", Effort::Medium),
                rule("medium", Size::M, "sonnet", Effort::Medium),
            ],
            default: Choice::default(),
        }
    }
}

/// The quota floor. Floors are per account and the feed carries no account
/// id, so each host on an account is configured with the same
/// `account_floor_percent` and its own `host_share_percent`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Quota {
    /// The weekly floor for the whole account (AGENTS.md rule 8: 3%).
    pub account_floor_percent: f64,
    /// This host's fixed share of the account's room above the floor. Every
    /// host on the account may dig only this deep, so the rest stays for the
    /// others: see [`Quota::host_floor`].
    pub host_share_percent: f64,
    /// Points above this host's floor that new work (scout and builder runs)
    /// leaves for review and recovery; reviewer and verifier runs may use
    /// them (plan v2 point 35).
    pub review_reserve_percent: f64,
    /// How old a reading may be before the check fails closed (the feed's
    /// stale mark, `protocol.md` section 8).
    pub max_age_minutes: i64,
    /// Off on a host with no quota feed; spawn then checks no quota at all.
    pub enabled: bool,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            account_floor_percent: 3.0,
            host_share_percent: 100.0,
            review_reserve_percent: 2.0,
            max_age_minutes: 15,
            enabled: true,
        }
    }
}

/// Where spawn finds its tools and writes a run's launch files.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// The run scratch root; a run's directory is `<scratch_root>/<run id>`.
    /// Outside every repository and outside `~/.tiller`. Empty means
    /// `~/.local/state/tiller/runs`.
    pub scratch_root: Option<PathBuf>,
    /// The worktree pool command and its leading arguments.
    pub treehouse: Vec<String>,
    /// The Claude Code command.
    pub claude: String,
    /// The herdr command and its leading arguments, `["herdr"]` for the
    /// local server's default session.
    pub herdr: Vec<String>,
    /// The herdr session's API socket, which `workspace.move` needs; without
    /// it the layout pass fixes labels but not order.
    pub herdr_socket: Option<PathBuf>,
    /// Hosts the worker sandbox may reach (`<network_allow>`).
    pub network_allow: Vec<String>,
    /// The `no-mistakes` daemon socket, when this host has one.
    pub no_mistakes_socket: Option<String>,
    /// The treehouse pool root, which the orchestrator may not edit
    /// (`<pool>`). `tiller orchestrator start` needs it.
    pub pool_root: Option<PathBuf>,
    /// The root the project checkouts sit under, which the orchestrator may
    /// not edit (`<checkouts>`). `tiller orchestrator start` needs it.
    pub checkouts_root: Option<PathBuf>,
    /// The root of the orchestrator homes; a project's is
    /// `<orchestrator_home>/<project>`. Outside the pool and the checkouts.
    /// Empty means `~/.local/state/tiller/orchestrators`.
    pub orchestrator_home: Option<PathBuf>,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            scratch_root: None,
            treehouse: vec!["treehouse".into()],
            claude: "claude".into(),
            herdr: vec!["herdr".into()],
            herdr_socket: None,
            network_allow: ["github.com", "api.github.com"].map(String::from).into(),
            no_mistakes_socket: None,
            pool_root: None,
            checkouts_root: None,
            orchestrator_home: None,
        }
    }
}

impl SpawnConfig {
    /// Reads the spawn tables of a parsed config file; the file's other
    /// tables belong to other commands and are ignored.
    pub fn from_table(table: &toml::Table) -> Result<Self, SpawnError> {
        let mut own = toml::Table::new();
        for key in ["dispatch", "quota", "spawn", "checkouts"] {
            if let Some(value) = table.get(key) {
                own.insert(key.into(), value.clone());
            }
        }
        let config: Self = own
            .try_into()
            .map_err(|e: toml::de::Error| SpawnError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), SpawnError> {
        let dispatch = &self.dispatch;
        let known = |model: &str| dispatch.models.iter().any(|m| m == model);
        for choice in dispatch
            .rules
            .iter()
            .map(|r| (&r.name, &r.model))
            .chain([(&"default".to_owned(), &dispatch.default.model)])
        {
            if !known(choice.1) {
                return Err(SpawnError::Config(format!(
                    "dispatch rule {} names model {}, which is not in dispatch.models",
                    choice.0, choice.1
                )));
            }
        }
        let spawn = &self.spawn;
        if spawn.treehouse.is_empty() || spawn.herdr.is_empty() || spawn.claude.is_empty() {
            return Err(SpawnError::Config(
                "spawn: treehouse, herdr and claude each name a command".into(),
            ));
        }
        let quota = &self.quota;
        if !(0.0..100.0).contains(&quota.account_floor_percent)
            || !(quota.host_share_percent > 0.0 && quota.host_share_percent <= 100.0)
            || quota.review_reserve_percent < 0.0
            || quota.max_age_minutes <= 0
        {
            return Err(SpawnError::Config(
                "quota: account_floor_percent in [0, 100), host_share_percent in (0, 100], review_reserve_percent >= 0, max_age_minutes > 0".into(),
            ));
        }
        Ok(())
    }
}

impl Quota {
    /// The weekly percentage left at or under which this host stops
    /// dispatching. A host with share `s` of the room above the account
    /// floor `f` may use only that share, so it stops at
    /// `f + (100 - f) * (1 - s)`: a host with the whole share stops at the
    /// account floor, and one with a quarter of it stops once three
    /// quarters of the room remain, which it leaves to the other hosts.
    pub fn host_floor(&self) -> f64 {
        let floor = self.account_floor_percent;
        floor + (100.0 - floor) * (1.0 - self.host_share_percent / 100.0)
    }
}
