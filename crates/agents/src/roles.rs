//! The five fleet roles and their launch material.
//!
//! Each role is one file under `roles/`: a short YAML frontmatter that names
//! the role, its default model and effort and its permission mode, then the
//! prompt body that `tiller spawn` appends to the default system prompt with
//! `--append-system-prompt-file`. Beside it sits `roles/<role>.settings.json`,
//! the settings fragment template with the deny and allow rules, the hooks and
//! the sandbox from `authority.md` section 8, carrying placeholders that
//! [`render_settings`] fills per run. `roles/mcp.json` is the one MCP config
//! every fleet role launches with (`--strict-mcp-config`).
//!
//! The files are embedded with `include_str!`, so the packaged binary is the
//! only source of a role at launch time and the fragment hash that goes into
//! `sessions.config_hash` is a hash of packaged bytes (G35).

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

/// A fleet role. The orchestrator supervises one project; the four worker
/// roles run on one card each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Orchestrator,
    Scout,
    Builder,
    Reviewer,
    Verifier,
}

/// What can go wrong when reading a role file or rendering its fragment.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoleError {
    #[error("role file for {role} has no frontmatter block")]
    MissingFrontmatter { role: Role },
    #[error("role file for {role} has a frontmatter line without a colon: {line:?}")]
    MalformedFrontmatter { role: Role, line: String },
    #[error("role file for {role} is missing the frontmatter key {key:?}")]
    MissingKey { role: Role, key: &'static str },
    #[error("settings template for {role} is not valid JSON: {message}")]
    InvalidTemplate { role: Role, message: String },
    #[error("settings fragment for {role} still carries the placeholder {placeholder}")]
    UnfilledPlaceholder { role: Role, placeholder: String },
    #[error("{placeholder} for {role} must be {requirement}, got {value:?}")]
    InvalidPlaceholder {
        role: Role,
        placeholder: &'static str,
        requirement: &'static str,
        value: String,
    },
}

/// The frontmatter of a role file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    /// Every other key, kept so a check can refuse keys the role standard
    /// does not allow (such as `memory`).
    pub extra: BTreeMap<String, String>,
}

/// The absolute paths and lists a run fills into a settings fragment.
///
/// Every path is absolute and without a trailing slash. In rule syntax the
/// template writes `/<worktree>`, so the filled rule reads `//home/user/...`,
/// the absolute form Claude Code's permission rules use; sandbox entries use
/// the plain path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placeholders {
    /// The packaged `tiller` binary by its absolute Nix store path.
    pub tiller: String,
    /// The session's working directory: the run's worktree, or for the
    /// orchestrator the project checkout it runs in.
    pub worktree: String,
    /// The run's scratch directory, outside the repository and `~/.tiller`.
    pub scratch: String,
    /// The orchestrator's home directory (orchestrator only; empty otherwise).
    pub home: String,
    /// The treehouse pool root (orchestrator only; empty otherwise).
    pub pool: String,
    /// The root of the project checkouts (orchestrator only; empty otherwise).
    pub checkouts: String,
    /// The project's default branch.
    pub default_branch: String,
    /// Hosts the sandbox may reach: the forge and the package registries the
    /// project's checks need. Expands the `<network_allow>` list entry.
    pub network_allow: Vec<String>,
    /// The `no-mistakes` daemon socket, when the project uses it. Expands the
    /// `<no_mistakes_socket>` list entry; `None` removes it.
    pub no_mistakes_socket: Option<String>,
}

/// The packaged MCP config: no servers. Tiller ships no MCP server in v1.
pub const MCP_CONFIG: &str = include_str!("../../../roles/mcp.json");

/// The string placeholders a template may carry, each replaced inside the
/// strings that hold it. All but `<default>` are absolute paths.
pub const STRING_PLACEHOLDERS: [&str; 7] = [
    "<tiller>",
    "<worktree>",
    "<scratch>",
    "<home>",
    "<pool>",
    "<checkouts>",
    "<default>",
];

/// The list placeholders: a list entry that is exactly one of these is
/// replaced by zero or more entries.
pub const LIST_PLACEHOLDERS: [&str; 2] = ["<network_allow>", "<no_mistakes_socket>"];

impl Role {
    /// Every role, orchestrator first.
    pub const ALL: [Role; 5] = [
        Role::Orchestrator,
        Role::Scout,
        Role::Builder,
        Role::Reviewer,
        Role::Verifier,
    ];

    /// The role name as `sessions.role` and `runs.role` store it.
    pub fn name(self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Scout => "scout",
            Role::Builder => "builder",
            Role::Reviewer => "reviewer",
            Role::Verifier => "verifier",
        }
    }

    /// The short role name the CLI accepts in run addresses (`alpha-42 build`),
    /// from `protocol.md` 6.1. The orchestrator has no run and no short name.
    pub fn short_name(self) -> Option<&'static str> {
        match self {
            Role::Orchestrator => None,
            Role::Scout => Some("scout"),
            Role::Builder => Some("build"),
            Role::Reviewer => Some("review"),
            Role::Verifier => Some("verify"),
        }
    }

    /// A role by either of its names.
    pub fn parse(name: &str) -> Option<Role> {
        Role::ALL
            .into_iter()
            .find(|role| role.name() == name || role.short_name() == Some(name))
    }

    /// Workers run on a card in a worktree; the orchestrator does not.
    pub fn is_worker(self) -> bool {
        self != Role::Orchestrator
    }

    /// The role file, frontmatter included, as packaged.
    pub fn file(self) -> &'static str {
        match self {
            Role::Orchestrator => include_str!("../../../roles/orchestrator.md"),
            Role::Scout => include_str!("../../../roles/scout.md"),
            Role::Builder => include_str!("../../../roles/builder.md"),
            Role::Reviewer => include_str!("../../../roles/reviewer.md"),
            Role::Verifier => include_str!("../../../roles/verifier.md"),
        }
    }

    /// The settings fragment template, placeholders unfilled, as packaged.
    pub fn settings_template(self) -> &'static str {
        match self {
            Role::Orchestrator => include_str!("../../../roles/orchestrator.settings.json"),
            Role::Scout => include_str!("../../../roles/scout.settings.json"),
            Role::Builder => include_str!("../../../roles/builder.settings.json"),
            Role::Reviewer => include_str!("../../../roles/reviewer.settings.json"),
            Role::Verifier => include_str!("../../../roles/verifier.settings.json"),
        }
    }

    /// The frontmatter and the body of the role file.
    fn split(self) -> Result<(&'static str, &'static str), RoleError> {
        let file = self.file();
        let rest = file
            .strip_prefix("---\n")
            .ok_or(RoleError::MissingFrontmatter { role: self })?;
        let end = rest
            .find("\n---\n")
            .ok_or(RoleError::MissingFrontmatter { role: self })?;
        let body = &rest[end + "\n---\n".len()..];
        Ok((&rest[..end], body.trim_start_matches('\n')))
    }

    /// The parsed frontmatter.
    pub fn frontmatter(self) -> Result<Frontmatter, RoleError> {
        let (front, _) = self.split()?;
        let mut keys = BTreeMap::new();
        for line in front.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let (key, value) =
                line.split_once(':')
                    .ok_or_else(|| RoleError::MalformedFrontmatter {
                        role: self,
                        line: line.to_string(),
                    })?;
            keys.insert(key.trim().to_string(), value.trim().to_string());
        }
        let mut take = |key: &'static str| {
            keys.remove(key)
                .ok_or(RoleError::MissingKey { role: self, key })
        };
        Ok(Frontmatter {
            name: take("name")?,
            description: take("description")?,
            model: take("model")?,
            effort: take("effort")?,
            permission_mode: take("permissionMode")?,
            extra: keys,
        })
    }

    /// The prompt body: the role file without its frontmatter. This is the
    /// text `tiller spawn` writes to the file it passes with
    /// `--append-system-prompt-file`, followed by the rendered memory.
    pub fn prompt_body(self) -> Result<&'static str, RoleError> {
        self.split().map(|(_, body)| body)
    }
}

impl Placeholders {
    /// Each string placeholder with its value, in [`STRING_PLACEHOLDERS`] order.
    fn strings(&self) -> [(&'static str, &str); 7] {
        [
            ("<tiller>", &self.tiller),
            ("<worktree>", &self.worktree),
            ("<scratch>", &self.scratch),
            ("<home>", &self.home),
            ("<pool>", &self.pool),
            ("<checkouts>", &self.checkouts),
            ("<default>", &self.default_branch),
        ]
    }

    /// Refuse a value that would render a wrong rule instead of a missing
    /// one: an empty path becomes `Edit(//**)`, a relative path anchors at
    /// the settings file, a trailing slash doubles the separator. Only the
    /// placeholders the template uses are checked, so a worker fill may leave
    /// the orchestrator's directories empty.
    fn check(&self, role: Role, template: &str) -> Result<(), RoleError> {
        for (placeholder, value) in self.strings() {
            if !template.contains(placeholder) {
                continue;
            }
            let ok = if placeholder == "<default>" {
                !value.is_empty() && !value.contains(char::is_whitespace)
            } else {
                value.starts_with('/') && value.len() > 1 && !value.ends_with('/')
            };
            if !ok {
                return Err(RoleError::InvalidPlaceholder {
                    role,
                    placeholder,
                    requirement: if placeholder == "<default>" {
                        "a branch name"
                    } else {
                        "an absolute path without a trailing slash"
                    },
                    value: value.to_string(),
                });
            }
        }
        Ok(())
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Render the role's settings fragment for one run.
///
/// Every string in the template has its placeholders replaced. A list entry
/// that is exactly `<network_allow>` or `<no_mistakes_socket>` is replaced by
/// the corresponding list (zero or more entries). A path the template needs
/// that is empty, relative or ends with a slash is refused before anything is
/// rendered, and a placeholder that is still present afterwards is an error,
/// so a run never starts with a half-filled or over-wide fragment.
pub fn render_settings(role: Role, fill: &Placeholders) -> Result<Value, RoleError> {
    let template = role.settings_template();
    fill.check(role, template)?;
    let mut value: Value =
        serde_json::from_str(template).map_err(|e| RoleError::InvalidTemplate {
            role,
            message: e.to_string(),
        })?;
    fill_value(&mut value, fill);
    if let Some(placeholder) = first_placeholder(&value) {
        return Err(RoleError::UnfilledPlaceholder { role, placeholder });
    }
    Ok(value)
}

fn fill_string(text: &str, fill: &Placeholders) -> String {
    fill.strings()
        .iter()
        .fold(text.to_string(), |text, (placeholder, value)| {
            text.replace(placeholder, value)
        })
}

fn fill_value(value: &mut Value, fill: &Placeholders) {
    match value {
        Value::String(text) => *text = fill_string(text, fill),
        Value::Array(items) => {
            let mut filled = Vec::with_capacity(items.len());
            for item in items.drain(..) {
                match item.as_str() {
                    Some("<network_allow>") => {
                        filled.extend(fill.network_allow.iter().cloned().map(Value::String));
                    }
                    Some("<no_mistakes_socket>") => {
                        filled.extend(fill.no_mistakes_socket.iter().cloned().map(Value::String));
                    }
                    _ => {
                        let mut item = item;
                        fill_value(&mut item, fill);
                        filled.push(item);
                    }
                }
            }
            *items = filled;
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                fill_value(item, fill);
            }
        }
        _ => {}
    }
}

fn first_placeholder(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => STRING_PLACEHOLDERS
            .iter()
            .chain(LIST_PLACEHOLDERS.iter())
            .find(|p| text.contains(**p))
            .map(|p| (*p).to_string()),
        Value::Array(items) => items.iter().find_map(first_placeholder),
        Value::Object(map) => map.values().find_map(first_placeholder),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fleet role's deny list, from `authority.md` section 8, "Every
    /// fleet role", in order, plus the one T4.1 addition at the end.
    const COMMON_DENY: &[&str] = &[
        "Read(~/.tiller/**)",
        "Edit(~/.tiller/**)",
        "Edit(~/.claude/**)",
        "Edit(/<worktree>/.claude/**)",
        "Read(**/.env)",
        "Read(**/.env.*)",
        "Read(**/.envrc)",
        "Read(**/*.pem)",
        "Read(**/*.key)",
        "Bash(gh pr merge *)",
        "Bash(gh-axi pr merge *)",
        "Bash(gh api *)",
        "Bash(gh-axi api *)",
        "Bash(gh auth *)",
        "Bash(curl *)",
        "Bash(wget *)",
        "Bash(http *)",
        "Bash(xh *)",
        "Bash(git push --force*)",
        "Bash(git push -f*)",
        "Bash(git push * --force*)",
        "Bash(git push * -f*)",
        "Bash(git push *:<default>*)",
        "Bash(git push * <default>)",
        "Bash(git push --mirror*)",
        "Bash(git push * --delete *)",
        "Bash(git worktree add *)",
        "Bash(git worktree remove *)",
        "Bash(git worktree move *)",
        "Bash(git worktree prune*)",
        "Bash(treehouse *)",
        "Bash(herdr *)",
        "Bash(claude *)",
        "Bash(npx *claude-code*)",
        "Bash(tiller approve *)",
        "Bash(tiller grant *)",
        "Bash(tiller away *)",
        "Bash(tiller land *)",
        "Bash(tiller board*)",
        "Bash(tiller import *)",
        "Bash(tiller hook *)",
        "Bash(tiller owner *)",
        "Bash(crontab *)",
        "Bash(at *)",
        "Bash(systemd-run *)",
        "Bash(systemctl *)",
        "Bash(launchctl *)",
        "Bash(pkill *)",
        "Bash(killall *)",
        "Bash(script *)",
        "Bash(expect *)",
        "Bash(unbuffer *)",
        "EnterWorktree",
        "ExitWorktree",
        "CronCreate",
        "CronDelete",
        "ScheduleWakeup",
        "RemoteTrigger",
        "Workflow",
        "PushNotification",
        "Agent(isolation:*)",
    ];

    /// The per-role table of `authority.md` section 8: mode, extra deny
    /// rules, allow rules and the sandbox write set.
    struct RoleRow {
        role: Role,
        mode: &'static str,
        extra_deny: &'static [&'static str],
        allow: &'static [&'static str],
        write_set: &'static [&'static str],
    }

    const ROLE_TABLE: &[RoleRow] = &[
        RoleRow {
            role: Role::Orchestrator,
            mode: "default",
            extra_deny: &[
                "Agent",
                "SendMessage",
                "Edit(/<checkouts>/**)",
                "Edit(/<pool>/**)",
                "Bash(git commit *)",
                "Bash(git push *)",
                "Bash(no-mistakes *)",
            ],
            allow: &[
                "Bash(tiller *)",
                "Bash(gh-axi pr view *)",
                "Bash(gh-axi pr list *)",
                "Bash(gh-axi pr checks *)",
                "Bash(gh-axi pr diff *)",
                "Bash(gh-axi run list *)",
                "Bash(gh-axi run view *)",
                "Bash(gh-axi issue view *)",
                "Bash(gh-axi issue list *)",
                "Bash(gh-axi repo view *)",
                "Read(/<home>/**)",
                "Read(/<scratch>/**)",
                "Edit(/<home>/**)",
                "Edit(/<scratch>/**)",
            ],
            write_set: &["<home>", "<scratch>"],
        },
        RoleRow {
            role: Role::Scout,
            mode: "dontAsk",
            extra_deny: &["Bash(git push *)", "Bash(no-mistakes *)"],
            allow: &[
                "Bash",
                "Edit(/<worktree>/**)",
                "Edit(/<scratch>/**)",
                "Read",
                "WebFetch",
                "WebSearch",
                "Agent",
            ],
            write_set: &["<worktree>", "<scratch>"],
        },
        RoleRow {
            role: Role::Builder,
            mode: "dontAsk",
            extra_deny: &[],
            allow: &[
                "Bash",
                "Edit(/<worktree>/**)",
                "Edit(/<scratch>/**)",
                "Read",
                "WebFetch",
                "WebSearch",
                "Agent",
            ],
            write_set: &["<worktree>", "<scratch>"],
        },
        RoleRow {
            role: Role::Reviewer,
            mode: "dontAsk",
            extra_deny: &[
                "Edit",
                "Write",
                "NotebookEdit",
                "Bash(git commit *)",
                "Bash(git push *)",
                "Bash(no-mistakes *)",
            ],
            allow: &["Bash", "Read", "WebFetch", "WebSearch", "Agent"],
            write_set: &["<scratch>"],
        },
        RoleRow {
            role: Role::Verifier,
            mode: "dontAsk",
            extra_deny: &[
                "Bash(git commit *)",
                "Bash(git push *)",
                "Bash(no-mistakes *)",
            ],
            allow: &[
                "Bash",
                "Edit(/<worktree>/**)",
                "Edit(/<scratch>/**)",
                "Read",
                "WebFetch",
            ],
            write_set: &["<worktree>", "<scratch>"],
        },
    ];

    /// The hook events `protocol.md` 4.2 maps to a handler other than
    /// `ignore`, apart from `PreToolUse`, which runs the guard.
    const HOOK_EVENTS: &[&str] = &[
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "PermissionDenied",
        "PostToolUse",
        "PostToolUseFailure",
        "Notification",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "StopFailure",
        "InstructionsLoaded",
        "ConfigChange",
        "CwdChanged",
        "DirectoryAdded",
        "WorktreeCreate",
        "WorktreeRemove",
        "PreCompact",
        "PostCompact",
        "PostModelSwitch",
        "Elicitation",
        "ElicitationResult",
        "SessionEnd",
    ];

    /// P42: the always-loaded context stays under a budget. The whole
    /// startup budget (role plus approved memory) is 7,500 tokens; a role
    /// body gets a third of it, at the usual four bytes per token.
    const ROLE_TOKEN_BUDGET: usize = 2_500;

    fn template(role: Role) -> Value {
        serde_json::from_str(role.settings_template()).unwrap()
    }

    fn strings(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    fn sample_fill(role: Role) -> Placeholders {
        let orchestrator = role == Role::Orchestrator;
        Placeholders {
            tiller: "/nix/store/abc-tiller-0.1.0/bin/tiller".into(),
            worktree: if orchestrator {
                "/home/user/checkouts/alpha".into()
            } else {
                "/home/user/pool/3/alpha".into()
            },
            scratch: "/home/user/scratch/alpha-42-build".into(),
            home: if orchestrator {
                "/home/user/tiller-homes/alpha".into()
            } else {
                String::new()
            },
            pool: if orchestrator {
                "/home/user/pool".into()
            } else {
                String::new()
            },
            checkouts: if orchestrator {
                "/home/user/checkouts".into()
            } else {
                String::new()
            },
            default_branch: "main".into(),
            network_allow: vec!["github.com".into(), "api.github.com".into()],
            no_mistakes_socket: Some("/run/user/1000/no-mistakes.sock".into()),
        }
    }

    #[test]
    fn every_role_has_a_name_a_file_and_a_template() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.name()), Some(role));
            if let Some(short) = role.short_name() {
                assert_eq!(Role::parse(short), Some(role));
            }
            assert!(!role.file().is_empty());
            template(role);
        }
        assert_eq!(Role::parse("build"), Some(Role::Builder));
        assert_eq!(Role::parse("captain"), None);
    }

    #[test]
    fn frontmatter_matches_the_plan_defaults() {
        for role in Role::ALL {
            let fm = role.frontmatter().unwrap();
            assert_eq!(fm.name, role.name(), "{role}: name");
            let (model, mode) = if role == Role::Orchestrator {
                ("fable", "default")
            } else {
                ("opus", "dontAsk")
            };
            assert_eq!(fm.model, model, "{role}: model");
            assert_eq!(fm.effort, "high", "{role}: effort");
            assert_eq!(fm.permission_mode, mode, "{role}: permission mode");
        }
    }

    /// `memory.md` section 9: no role or agent file Tiller launches has a
    /// `memory` field.
    #[test]
    fn no_memory_field() {
        for role in Role::ALL {
            let fm = role.frontmatter().unwrap();
            assert!(
                !fm.extra.contains_key("memory"),
                "{role}: frontmatter carries a memory field"
            );
            assert!(
                fm.extra.is_empty(),
                "{role}: unexpected frontmatter keys {:?}",
                fm.extra.keys().collect::<Vec<_>>()
            );
        }
    }

    /// The role standard: the description is the trigger only, "Use when"
    /// first, under 1,536 characters.
    #[test]
    fn description_is_a_trigger() {
        for role in Role::ALL {
            let fm = role.frontmatter().unwrap();
            assert!(
                fm.description.starts_with("Use when"),
                "{role}: description does not start with 'Use when'"
            );
            assert!(
                fm.description.chars().count() < 1_536,
                "{role}: description too long"
            );
        }
    }

    /// P42 and the role standard's size test: under 500 lines, under 20 KB,
    /// and inside the token budget.
    #[test]
    fn startup_context_stays_under_budget() {
        for role in Role::ALL {
            let body = role.prompt_body().unwrap();
            let lines = body.lines().count();
            let bytes = body.len();
            assert!(lines < 500, "{role}: {lines} lines");
            assert!(bytes < 20 * 1024, "{role}: {bytes} bytes");
            let tokens = bytes.div_ceil(4);
            assert!(
                tokens <= ROLE_TOKEN_BUDGET,
                "{role}: about {tokens} tokens, budget {ROLE_TOKEN_BUDGET}"
            );
            assert!(
                !body.starts_with("---"),
                "{role}: frontmatter leaked into the prompt body"
            );
            assert!(
                body.starts_with("# "),
                "{role}: body does not start with its title"
            );
        }
    }

    /// Hard rule 5: never the em dash.
    #[test]
    fn role_files_use_no_em_dash() {
        for role in Role::ALL {
            assert!(
                !role.file().contains('\u{2014}'),
                "{role}: em dash in role file"
            );
        }
    }

    /// P63: a builder checks UI in a real browser through the browser CLI.
    #[test]
    fn builder_tools_include_browser_cli() {
        let body = Role::Builder.prompt_body().unwrap();
        assert!(body.contains("chrome-devtools-axi"));
        assert!(
            body.contains("timeout -k"),
            "P56: browser commands run under a kill timeout"
        );
    }

    /// P64: GitHub work goes through `gh-axi`. The one plain `gh` use the
    /// owner allows is the attachment upload.
    #[test]
    fn roles_use_gh_axi_not_gh() {
        for role in Role::ALL {
            let body = role.prompt_body().unwrap();
            for (index, _) in body.match_indices("`gh ") {
                let rest = &body[index..];
                assert!(
                    rest.starts_with("`gh pr create|edit --attach`"),
                    "{role}: plain gh command at byte {index}: {:?}",
                    rest.lines().next().unwrap_or("")
                );
            }
            assert!(body.contains("gh-axi"), "{role}: never names gh-axi");
        }
    }

    /// Every worker reports through the same three commands; the orchestrator
    /// carries intake, dispatch and handoff.
    #[test]
    fn role_files_carry_the_status_contract() {
        for role in Role::ALL {
            let body = role.prompt_body().unwrap();
            assert!(
                body.contains("tiller inbox <ULID>"),
                "{role}: wake-up line rule"
            );
            if role.is_worker() {
                for needed in ["tiller status", "tiller done", "needs-decision", "blocked"] {
                    assert!(body.contains(needed), "{role}: missing {needed:?}");
                }
            } else {
                for needed in [
                    "tiller ask resolve",
                    "tiller spawn",
                    "tiller approval request",
                    "tiller approve <approval> --ask <ask>",
                    "tiller handoff write",
                ] {
                    assert!(body.contains(needed), "orchestrator: missing {needed:?}");
                }
            }
        }
    }

    /// P58, P59 and point 31 as prose: the orchestrator never edits project
    /// code, the reviewer has no memory tool.
    #[test]
    fn role_prose_states_the_hard_guardrails() {
        let orchestrator = Role::Orchestrator.prompt_body().unwrap();
        assert!(orchestrator.contains("Never edit project code"));
        assert!(orchestrator.contains("Never start work Tiller does not track"));
        let reviewer = Role::Reviewer.prompt_body().unwrap();
        assert!(reviewer.contains("You have no memory tool"));
        assert!(reviewer.contains("Never edit, commit, push, or approve anything"));
    }

    /// `authority.md` section 8, "Every fleet role": each fragment starts
    /// with the common deny list, in order.
    #[test]
    fn fragments_share_the_common_deny_list() {
        for role in Role::ALL {
            let t = template(role);
            let deny = strings(&t["permissions"]["deny"]);
            assert!(
                deny.len() >= COMMON_DENY.len(),
                "{role}: {} deny rules, common list has {}",
                deny.len(),
                COMMON_DENY.len()
            );
            assert_eq!(
                &deny[..COMMON_DENY.len()],
                COMMON_DENY,
                "{role}: common deny list"
            );
        }
    }

    /// `authority.md` section 8, "Per role": mode, extra deny, allow and the
    /// sandbox write set.
    #[test]
    fn fragments_match_the_role_table() {
        assert_eq!(ROLE_TABLE.len(), Role::ALL.len());
        for row in ROLE_TABLE {
            let t = template(row.role);
            let role = row.role;
            assert_eq!(t["permissions"]["defaultMode"], row.mode, "{role}: mode");
            assert_eq!(
                row.role.frontmatter().unwrap().permission_mode,
                row.mode,
                "{role}: frontmatter mode differs from the fragment"
            );
            let deny = strings(&t["permissions"]["deny"]);
            assert_eq!(
                &deny[COMMON_DENY.len()..],
                row.extra_deny,
                "{role}: extra deny"
            );
            assert_eq!(
                strings(&t["permissions"]["allow"]),
                row.allow,
                "{role}: allow"
            );
            assert_eq!(
                strings(&t["sandbox"]["filesystem"]["allowWrite"]),
                row.write_set,
                "{role}: sandbox write set"
            );
        }
    }

    /// The reviewer's edit tools are removed by bare name, so they never
    /// reach the model (point 31); the orchestrator's delegation tools too
    /// (P59).
    #[test]
    fn bare_name_denies_remove_the_tools_the_role_must_not_have() {
        let reviewer = template(Role::Reviewer);
        let deny = strings(&reviewer["permissions"]["deny"]);
        for tool in ["Edit", "Write", "NotebookEdit"] {
            assert!(deny.contains(&tool), "reviewer: {tool} not denied");
        }
        let orchestrator = template(Role::Orchestrator);
        let deny = strings(&orchestrator["permissions"]["deny"]);
        for tool in ["Agent", "SendMessage", "Workflow"] {
            assert!(deny.contains(&tool), "orchestrator: {tool} not denied");
        }
        for tool in ["Agent", "Edit"] {
            let allow = strings(&orchestrator["permissions"]["allow"]);
            assert!(!allow.contains(&tool), "orchestrator: bare {tool} allowed");
        }
    }

    /// `authority.md` section 7, layers 1 and 5, and G35: auto memory off,
    /// Remote Control off for workers, no claude.ai connectors, the sandbox
    /// that fails closed, and `tiller` as the only excluded command.
    #[test]
    fn fragments_set_the_launch_guards() {
        for role in Role::ALL {
            let t = template(role);
            assert_eq!(t["autoMemoryEnabled"], false, "{role}: auto memory");
            assert_eq!(
                t["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1",
                "{role}: auto memory variable"
            );
            assert_eq!(t["env"]["TILLER_ROLE"], role.name(), "{role}: TILLER_ROLE");
            assert_eq!(t["disableClaudeAiConnectors"], true, "{role}: connectors");
            assert_eq!(
                t["remoteControlAtStartup"], false,
                "{role}: remote control at startup"
            );
            assert_eq!(
                t["disableRemoteControl"],
                role.is_worker(),
                "{role}: Remote Control is off for workers and on for the owner's orchestrator (P68)"
            );
            let sandbox = &t["sandbox"];
            assert_eq!(sandbox["enabled"], true, "{role}: sandbox");
            assert_eq!(sandbox["failIfUnavailable"], true, "{role}: fail closed");
            assert_eq!(
                sandbox["allowUnsandboxedCommands"], false,
                "{role}: no retry"
            );
            assert_eq!(
                strings(&sandbox["excludedCommands"]),
                ["<tiller>", "<tiller> *"],
                "{role}: excluded commands"
            );
            assert_eq!(
                sandbox["network"]["allowLocalBinding"], false,
                "{role}: localhost"
            );
            let deny_read = strings(&sandbox["filesystem"]["denyRead"]);
            assert!(
                deny_read.contains(&"~/.tiller"),
                "{role}: ~/.tiller readable"
            );
            let deny_write = strings(&sandbox["filesystem"]["denyWrite"]);
            assert!(
                deny_write.contains(&"~/.claude"),
                "{role}: ~/.claude writable"
            );
            let excludes = strings(&t["claudeMdExcludes"]);
            for pattern in [
                "**/CLAUDE.md",
                "**/CLAUDE.local.md",
                "**/AGENTS.md",
                "**/.claude/rules/**",
            ] {
                assert!(
                    excludes.contains(&pattern),
                    "{role}: {pattern} not excluded"
                );
            }
        }
    }

    /// `protocol.md` 4.1 and 4.2: every non-ignored event runs
    /// `tiller hook <event>`, and `PreToolUse` runs the guard for every tool.
    #[test]
    fn fragments_register_every_handled_hook_event() {
        for role in Role::ALL {
            let t = template(role);
            let hooks = t["hooks"].as_object().unwrap();
            let mut expected: Vec<&str> = HOOK_EVENTS.to_vec();
            expected.push("PreToolUse");
            expected.sort_unstable();
            let mut registered: Vec<&str> = hooks.keys().map(String::as_str).collect();
            registered.sort_unstable();
            assert_eq!(registered, expected, "{role}: hook events");
            for event in HOOK_EVENTS {
                let command = hooks[*event][0]["hooks"][0]["command"].as_str().unwrap();
                assert_eq!(command, format!("<tiller> hook {event}"), "{role}: {event}");
            }
            let guard = &hooks["PreToolUse"][0];
            assert_eq!(guard["matcher"], ".*", "{role}: guard matcher");
            assert_eq!(
                guard["hooks"][0]["command"],
                format!("<tiller> guard pretool --role {role}"),
                "{role}: guard command"
            );
        }
    }

    #[test]
    fn mcp_config_has_no_servers() {
        let mcp: Value = serde_json::from_str(MCP_CONFIG).unwrap();
        assert_eq!(mcp["mcpServers"], serde_json::json!({}));
    }

    #[test]
    fn render_fills_every_placeholder() {
        for role in Role::ALL {
            let rendered = render_settings(role, &sample_fill(role)).unwrap();
            let text = rendered.to_string();
            for placeholder in STRING_PLACEHOLDERS.iter().chain(LIST_PLACEHOLDERS.iter()) {
                assert!(
                    !text.contains(placeholder),
                    "{role}: {placeholder} left in fragment"
                );
            }
            let hooks = &rendered["hooks"];
            assert_eq!(
                hooks["PreToolUse"][0]["hooks"][0]["command"],
                format!("/nix/store/abc-tiller-0.1.0/bin/tiller guard pretool --role {role}")
            );
            assert_eq!(
                strings(&rendered["sandbox"]["network"]["allowedDomains"]),
                ["github.com", "api.github.com"]
            );
            assert_eq!(
                strings(&rendered["sandbox"]["network"]["allowUnixSockets"]),
                ["/run/user/1000/no-mistakes.sock"]
            );
        }
    }

    /// Rule paths come out in Claude Code's absolute form (`//...`), sandbox
    /// paths in the plain form.
    #[test]
    fn render_writes_absolute_paths_in_both_syntaxes() {
        let builder = render_settings(Role::Builder, &sample_fill(Role::Builder)).unwrap();
        let allow = strings(&builder["permissions"]["allow"]);
        assert!(
            allow.contains(&"Edit(//home/user/pool/3/alpha/**)"),
            "{allow:?}"
        );
        let deny = strings(&builder["permissions"]["deny"]);
        assert!(deny.contains(&"Edit(//home/user/pool/3/alpha/.claude/**)"));
        assert!(deny.contains(&"Bash(git push *:main*)"));
        assert_eq!(
            strings(&builder["sandbox"]["filesystem"]["allowWrite"]),
            [
                "/home/user/pool/3/alpha",
                "/home/user/scratch/alpha-42-build"
            ]
        );

        let orchestrator =
            render_settings(Role::Orchestrator, &sample_fill(Role::Orchestrator)).unwrap();
        let deny = strings(&orchestrator["permissions"]["deny"]);
        assert!(deny.contains(&"Edit(//home/user/checkouts/**)"));
        assert!(deny.contains(&"Edit(//home/user/pool/**)"));
        assert_eq!(
            strings(&orchestrator["sandbox"]["filesystem"]["allowWrite"]),
            [
                "/home/user/tiller-homes/alpha",
                "/home/user/scratch/alpha-42-build"
            ]
        );
    }

    #[test]
    fn render_drops_an_absent_socket_and_an_empty_network_list() {
        let mut fill = sample_fill(Role::Scout);
        fill.no_mistakes_socket = None;
        fill.network_allow.clear();
        let rendered = render_settings(Role::Scout, &fill).unwrap();
        assert_eq!(
            strings(&rendered["sandbox"]["network"]["allowUnixSockets"]),
            [""; 0]
        );
        assert_eq!(
            strings(&rendered["sandbox"]["network"]["allowedDomains"]),
            [""; 0]
        );
    }

    /// One table drives filling and checking, so a placeholder cannot be
    /// known to one and not the other, and no template names a token the
    /// table lacks.
    #[test]
    fn the_placeholder_table_covers_every_template_token() {
        let fill = sample_fill(Role::Orchestrator);
        let names: Vec<&str> = fill.strings().iter().map(|(p, _)| *p).collect();
        assert_eq!(names, STRING_PLACEHOLDERS);
        for role in Role::ALL {
            let mut rest = role.settings_template();
            while let Some(start) = rest.find('<') {
                let tail = &rest[start..];
                let Some(end) = tail.find('>') else { break };
                let token = &tail[..=end];
                let known =
                    STRING_PLACEHOLDERS.contains(&token) || LIST_PLACEHOLDERS.contains(&token);
                assert!(known, "{role}: unknown placeholder {token}");
                rest = &tail[end + 1..];
            }
        }
    }

    /// An empty or relative path would render `Edit(//**)` or anchor a rule
    /// at the settings file; both are refused before rendering.
    #[test]
    fn render_refuses_an_empty_relative_or_slashed_path() {
        let mut empty_home = sample_fill(Role::Orchestrator);
        empty_home.home = String::new();
        assert_eq!(
            render_settings(Role::Orchestrator, &empty_home),
            Err(RoleError::InvalidPlaceholder {
                role: Role::Orchestrator,
                placeholder: "<home>",
                requirement: "an absolute path without a trailing slash",
                value: String::new(),
            })
        );
        let mut relative = sample_fill(Role::Builder);
        relative.worktree = "pool/3/alpha".into();
        assert!(matches!(
            render_settings(Role::Builder, &relative),
            Err(RoleError::InvalidPlaceholder {
                placeholder: "<worktree>",
                ..
            })
        ));
        let mut slashed = sample_fill(Role::Builder);
        slashed.scratch.push('/');
        assert!(matches!(
            render_settings(Role::Builder, &slashed),
            Err(RoleError::InvalidPlaceholder {
                placeholder: "<scratch>",
                ..
            })
        ));
        let mut no_branch = sample_fill(Role::Builder);
        no_branch.default_branch = String::new();
        assert!(matches!(
            render_settings(Role::Builder, &no_branch),
            Err(RoleError::InvalidPlaceholder {
                placeholder: "<default>",
                ..
            })
        ));
        // A worker fill leaves the orchestrator's directories empty, which is
        // fine because a worker template never names them.
        let worker = sample_fill(Role::Builder);
        assert!(worker.home.is_empty());
        render_settings(Role::Builder, &worker).unwrap();
    }

    /// The orchestrator runs in `default` mode so the owner answers prompts;
    /// auto-approving sandboxed Bash would silence them. Workers hold a bare
    /// `Bash` allow rule, so the auto-approval changes nothing for them. Every
    /// role's working directory gets the same shell-level secret and settings
    /// protection.
    #[test]
    fn only_workers_auto_approve_sandboxed_bash() {
        for role in Role::ALL {
            let t = template(role);
            assert_eq!(
                t["sandbox"]["autoAllowBashIfSandboxed"],
                role.is_worker(),
                "{role}: autoAllowBashIfSandboxed"
            );
            let deny_read = strings(&t["sandbox"]["filesystem"]["denyRead"]);
            for path in ["<worktree>/.env", "<worktree>/.envrc"] {
                assert!(
                    deny_read.contains(&path),
                    "{role}: {path} readable from the shell"
                );
            }
            let deny_write = strings(&t["sandbox"]["filesystem"]["denyWrite"]);
            for path in ["<worktree>/.claude", "<worktree>/.git/hooks"] {
                assert!(
                    deny_write.contains(&path),
                    "{role}: {path} writable from the shell"
                );
            }
        }
    }

    #[test]
    fn a_placeholder_left_anywhere_in_the_tree_is_found() {
        let mut filled: Value = serde_json::json!({"permissions": {"deny": ["Edit(/<worktree>/x)", "Read(<scratch>)"]}});
        fill_value(&mut filled, &sample_fill(Role::Builder));
        assert_eq!(first_placeholder(&filled), None);
        let stuck: Value = serde_json::json!({"a": {"b": ["x", {"c": "git push *:<default>*"}]}});
        assert_eq!(first_placeholder(&stuck), Some("<default>".to_string()));
    }
}
