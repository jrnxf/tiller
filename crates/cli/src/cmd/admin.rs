//! Owner-side setup: `init`, projects, hosts, config and the owner marker
//! (`protocol.md` section 6.4, Projects, hosts and config). `tiller peer` is
//! [`super::peer`].

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiller_agents::control::{self, Waits};
use tiller_agents::numbering::numbering;
use tiller_core::Applied;
use tiller_core::Store;
use tiller_core::domain::{Boundary, Delivery, HostKind, Project, Stage};
use tiller_core::state::deletion::{DeletionPreview, LiveAgent};
use tiller_core::state::get;
use tiller_core::state::projects::{HostEdit, Init, NewProject, ProjectEdit};
use tiller_herdr::client::Cli;

use super::clearable;
use super::read::{STOP_HOOK_MAX_RATE, STOP_HOOK_MIN_STOPS, intake_stats};
use super::review::checkout_head;
use super::spawn::{configured_herdr, failure};
use crate::app::{App, Done, Failure, Outcome};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// This host's label
    #[arg(long)]
    pub host_label: String,
    /// How many runs may be active at once on this host
    #[arg(long)]
    pub lane_capacity: i64,
    #[arg(long)]
    pub ram_floor_mb: Option<i64>,
    #[arg(long)]
    pub disk_floor_mb: Option<i64>,
    /// personal or work: the data boundary this host keeps
    #[arg(long, default_value = "personal")]
    pub boundary: Boundary,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    /// Add a project this host owns
    Add(ProjectAdd),
    /// Edit a project against the rev you read
    Edit(ProjectEditArgs),
    /// Move a project between pre-launch and launched against the rev you read
    Stage(ProjectStage),
    /// Retire a project
    Retire(ProjectRetire),
    /// Delete a project and every record of it on this host, its events
    /// included, after stopping its running agents; never its checkout,
    /// worktrees or repository
    Delete(ProjectDelete),
    /// List projects
    List,
}

#[derive(Args, Debug)]
pub struct ProjectRetire {
    pub id: String,
    /// Stop the project's running agents first, as a delete does. Without
    /// it they keep running, and tiller names them
    #[arg(long)]
    pub stop_agents: bool,
}

#[derive(Args, Debug)]
pub struct ProjectDelete {
    pub id: String,
    /// Delete without asking. Without it, tiller prints what would be removed
    /// and refuses, or asks for the id on a terminal
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct ProjectStage {
    pub id: String,
    /// pre-launch or launched
    pub stage: Stage,
    #[arg(long)]
    pub rev: i64,
    /// To pre-launch: the reviewed commit to start from, instead of the
    /// default-branch head read now
    #[arg(long)]
    pub from: Option<String>,
}

#[derive(Args, Debug)]
pub struct ProjectAdd {
    pub id: String,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub remote: Option<String>,
    #[arg(long)]
    pub default_branch: Option<String>,
    #[arg(long)]
    pub delivery: Delivery,
    /// The project's checks as JSON
    #[arg(long)]
    pub checks: Option<String>,
    #[arg(long)]
    pub capacity: Option<i64>,
    #[arg(long)]
    pub deploy_workflow: Option<String>,
    #[arg(long)]
    pub deploy_environment: Option<String>,
    #[arg(long)]
    pub deploy_timeout_minutes: Option<i64>,
}

#[derive(Args, Debug)]
pub struct ProjectEditArgs {
    pub id: String,
    #[arg(long)]
    pub rev: i64,
    #[arg(long)]
    pub name: Option<String>,
    /// An empty value clears the field
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub remote: Option<String>,
    #[arg(long)]
    pub default_branch: Option<String>,
    #[arg(long)]
    pub delivery: Option<Delivery>,
    #[arg(long)]
    pub checks: Option<String>,
    #[arg(long)]
    pub capacity: Option<i64>,
    #[arg(long)]
    pub deploy_workflow: Option<String>,
    #[arg(long)]
    pub deploy_environment: Option<String>,
    #[arg(long)]
    pub deploy_timeout_minutes: Option<i64>,
}

#[derive(Subcommand, Debug)]
pub enum HostCmd {
    /// Edit this host's capacity and floors against the rev you read
    Edit(HostEditArgs),
}

#[derive(Args, Debug)]
pub struct HostEditArgs {
    #[arg(long)]
    pub rev: i64,
    #[arg(long)]
    pub lane_capacity: Option<i64>,
    #[arg(long)]
    pub ram_floor_mb: Option<i64>,
    #[arg(long)]
    pub disk_floor_mb: Option<i64>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Set a key in config.toml (dotted keys reach into tables)
    Set { key: String, value: String },
    /// Read one key, or the whole file
    Get { key: Option<String> },
    /// The sha256 of the configuration, for comparing hosts
    Hash,
    /// Compare this host's configuration with a peer's
    Diff { peer: String },
}

#[derive(Subcommand, Debug)]
pub enum OwnerCmd {
    /// Say which system owns this host: tiller or firstmate
    Set {
        name: String,
    },
    Show,
}

pub fn init(app: &App, args: InitArgs) -> Done {
    app.require_owner("tiller init")?;
    let mut store = app.create()?;
    let applied = store.init(
        &app.ctx(),
        Init {
            host_label: args.host_label,
            lane_capacity: args.lane_capacity,
            ram_floor_mb: args.ram_floor_mb,
            disk_floor_mb: args.disk_floor_mb,
            boundary: args.boundary,
        },
    )?;
    Ok(Outcome::applied(applied).with_help("tiller project add <id> --name <n> --delivery <d>"))
}

fn json_arg(text: &str, what: &str) -> Result<Value, Failure> {
    serde_json::from_str(text)
        .map_err(|error| Failure::usage(format!("{what} is not valid JSON: {error}")))
}

pub fn project(app: &App, cmd: ProjectCmd) -> Done {
    if let ProjectCmd::List = cmd {
        let store = app.read()?;
        return Ok(Outcome::read(json!({"projects": store.projects()?})));
    }
    app.require_owner("tiller project")?;
    let mut store = app.write()?;
    match cmd {
        ProjectCmd::List => Err(Failure::new("internal", 1, "project list is a read")),
        ProjectCmd::Add(args) => {
            let checks = args
                .checks
                .as_deref()
                .map(|text| json_arg(text, "--checks"))
                .transpose()?;
            let new = NewProject {
                id: args.id,
                name: args.name,
                repo: args.repo,
                remote: args.remote,
                default_branch: args.default_branch,
                delivery: args.delivery,
                checks,
                capacity: args.capacity,
                deploy_workflow: args.deploy_workflow,
                deploy_environment: args.deploy_environment,
                deploy_timeout_minutes: args.deploy_timeout_minutes,
            };
            // A config.toml that does not parse only hides the checkout: the
            // remote is still looked at.
            let checkout = load_config(app).ok().and_then(|table| {
                table
                    .get("checkouts")
                    .and_then(|checkouts| checkouts.get(&new.id))
                    .and_then(toml::Value::as_str)
                    .map(PathBuf::from)
            });
            let (numbering, note) = numbering(&store, &new, checkout.as_deref())?;
            Ok(
                Outcome::applied(store.project_add_numbered(&app.ctx(), new, numbering)?)
                    .with_warning(note),
            )
        }
        ProjectCmd::Edit(args) => {
            let checks = args
                .checks
                .as_deref()
                .map(|text| json_arg(text, "--checks"))
                .transpose()?;
            Ok(Outcome::applied(store.project_edit(
                &app.ctx(),
                &args.id,
                args.rev,
                ProjectEdit {
                    name: args.name,
                    repo: clearable(args.repo),
                    remote: clearable(args.remote),
                    default_branch: args.default_branch,
                    delivery: args.delivery,
                    checks,
                    capacity: args.capacity,
                    deploy_workflow: clearable(args.deploy_workflow),
                    deploy_environment: clearable(args.deploy_environment),
                    deploy_timeout_minutes: args.deploy_timeout_minutes,
                },
            )?))
        }
        ProjectCmd::Stage(args) => project_stage(app, &mut store, args),
        ProjectCmd::Retire(args) => project_retire(app, &mut store, args),
        ProjectCmd::Delete(args) => project_delete(app, &mut store, args),
    }
}

/// `tiller project delete`: refuses without `--yes`, printing the counts it
/// would remove, unless the owner types the id at a terminal prompt.
fn project_delete(app: &App, store: &mut Store, args: ProjectDelete) -> Done {
    let preview = store.project_delete_preview(&args.id)?;
    if let Some(fact) = &preview.refused {
        let mut failure = match &preview.unblock {
            Some(unblock) => Failure::refused(format!(
                "project {}: {fact}. Nothing was deleted. Next: {}, then delete again",
                args.id, unblock.step
            )),
            None => Failure::refused(format!("project {}: {fact}", args.id)),
        };
        failure.hint = preview
            .unblock
            .as_ref()
            .and_then(|unblock| unblock.command.clone());
        failure.entity = Some("project".into());
        failure.id = Some(args.id.clone());
        failure.current = Some(would_remove(&preview.deletion));
        return Err(failure);
    }
    let summary = removal_summary(&preview.deletion);
    if !args.yes {
        let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        if !terminal || !typed_id(&args.id, &summary, &preview)? {
            let stops = match agent_list(&preview.agents) {
                Some(agents) => format!("stops {agents} and "),
                None => String::new(),
            };
            let mut failure = Failure::refused(format!(
                "project {}: deleting it {stops}removes {summary}; nothing was deleted",
                args.id
            ))
            .hint(format!("tiller project delete {} --yes", args.id));
            failure.entity = Some("project".into());
            failure.id = Some(args.id.clone());
            let mut current = would_remove(&preview.deletion);
            current["would_stop"] = json!(preview.agents);
            current["unlanded"] = json!(preview.unlanded);
            current["open_prs"] = json!(preview.open_prs);
            failure.current = Some(current);
            return Err(failure);
        }
    }
    let herdr = stop_herdr(app, &preview.agents)?;
    // Read before the delete, so a config.toml that does not parse turns into
    // a warning on a delete that happened, not a failure after it.
    let warning = match load_config(app) {
        Ok(table) => table
            .get("checkouts")
            .and_then(|checkouts| checkouts.get(&args.id))
            .and_then(toml::Value::as_str)
            .map(|path| {
                format!(
                    "config.toml still has checkouts.{} = {path:?}; the checkout and that key are left as they are",
                    args.id
                )
            }),
        Err(failure) => Some(format!(
            "{}; any checkouts.{} key in it is left as it is",
            failure.message, args.id
        )),
    };
    let applied = control::delete_project(store, &app.ctx(), &herdr, &Waits::default(), &args.id)
        .map_err(failure)?;
    let warnings: Vec<String> = applied
        .value
        .agents
        .warnings
        .iter()
        .cloned()
        .chain(warning)
        .collect();
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// `tiller project retire`: with `--stop-agents`, stops the project's
/// running agents first; without it, retires and names the agents left
/// running.
fn project_retire(app: &App, store: &mut Store, args: ProjectRetire) -> Done {
    if args.stop_agents {
        let herdr = stop_herdr(app, &store.project_retire_preview(&args.id)?.agents)?;
        let applied =
            control::retire_project(store, &app.ctx(), &herdr, &Waits::default(), &args.id, true)
                .map_err(failure)?;
        let warnings = &applied.value.agents.warnings;
        let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
        return Ok(Outcome::applied(applied).with_warning(warning));
    }
    let running = store.project_retire_preview(&args.id)?.agents;
    let applied = store.project_retire(&app.ctx(), &args.id)?;
    let warning = agent_list(&running).map(|agents| {
        format!(
            "still running: {agents}; tiller agent stop <session> stops one, or retire with --stop-agents"
        )
    });
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// The herdr the stops drive: never driven when nothing runs, so a config
/// that does not load fails only a command that has agents to stop.
fn stop_herdr(app: &App, agents: &[LiveAgent]) -> Result<Cli, Failure> {
    match configured_herdr(app) {
        Ok(herdr) => Ok(herdr),
        Err(_) if agents.is_empty() => Ok(Cli::local()),
        Err(failure) => Err(failure),
    }
}

/// "the orchestrator (session s-1), builder on alpha-4 (session s-2)".
fn agent_list(agents: &[LiveAgent]) -> Option<String> {
    let names: Vec<String> = agents.iter().map(LiveAgent::describe).collect();
    match names.split_first() {
        None => None,
        Some((first, rest)) => {
            let first = if first.starts_with("orchestrator") {
                format!("the {first}")
            } else {
                first.clone()
            };
            Some(
                std::iter::once(first)
                    .chain(rest.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    }
}

/// A refused delete's preview, named for what it is: nothing was removed.
fn would_remove(deletion: &tiller_core::state::deletion::Deletion) -> Value {
    json!({
        "project": deletion.project,
        "would_remove": deletion.removed,
        "would_unlink": deletion.unlinked,
        "would_withdraw_attention": deletion.withdrawn_attention,
    })
}

/// "events: 37, cards: 4, projects: 1": the tables with rows, largest first.
fn removal_summary(deletion: &tiller_core::state::deletion::Deletion) -> String {
    let mut tables: Vec<_> = deletion.removed.iter().collect();
    tables.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut parts: Vec<String> = tables
        .iter()
        .map(|(table, n)| format!("{table}: {n}"))
        .collect();
    if deletion.withdrawn_attention > 0 {
        parts.push(format!(
            "open questions withdrawn: {}",
            deletion.withdrawn_attention
        ));
    }
    parts.join(", ")
}

/// Asks the owner to type the project id; true when they did.
fn typed_id(id: &str, summary: &str, preview: &DeletionPreview) -> Result<bool, Failure> {
    if let Some(agents) = agent_list(&preview.agents) {
        eprintln!("Deleting project {id} first stops {agents}.");
    }
    eprint!(
        "Deleting project {id} removes these rows from this host's state.db, events included:\n  {summary}\n\
         The checkout, worktrees and repository are not touched.\n"
    );
    for work in &preview.unlanded {
        eprintln!("  Not landed, left on disk: {}", work.path);
    }
    for pr in &preview.open_prs {
        eprintln!("  Open PR, left on GitHub: {}", pr.url);
    }
    eprint!("Type {id} to delete it: ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim() == id)
}

fn project_stage(app: &App, store: &mut Store, args: ProjectStage) -> Done {
    // The head is fetched only for a switch that applies: a replay, a stale
    // rev or the current stage needs none.
    let reads_head = store.stage_reads_head(&app.ctx(), &args.id, args.rev, args.stage)?;
    let head = match args.from {
        Some(from) if args.stage == Stage::PreLaunch => Some(from),
        _ if reads_head => {
            let project: Project = get(store.conn(), "project", &args.id)?;
            Some(checkout_head(app, &project)?.1)
        }
        _ => None,
    };
    let applied =
        store.project_stage(&app.ctx(), &args.id, args.rev, args.stage, head.as_deref())?;
    let unvalidated: Vec<String> = applied
        .value
        .unvalidated_prs
        .iter()
        .map(|pr| pr.url.clone())
        .collect();
    let outcome = Outcome::applied(applied);
    Ok(match args.stage {
        Stage::PreLaunch => outcome.with_help(
            "while pre-launch, grants skip their pipeline condition for a PR with no pipeline run; every other condition still applies",
        ),
        Stage::Launched if unvalidated.is_empty() => outcome.with_help(
            "validation is back on; the orchestrator starts the final review with tiller review start",
        ),
        Stage::Launched => outcome.with_help(format!(
            "validation is back on; send each open PR with no pipeline run back through the gate before it merges: {}",
            unvalidated.join(", ")
        )),
    })
}

pub fn host(app: &App, cmd: HostCmd) -> Done {
    app.require_owner("tiller host")?;
    let HostCmd::Edit(args) = cmd;
    let mut store = app.write()?;
    let local = store
        .hosts()?
        .into_iter()
        .find(|host| host.kind == HostKind::Local)
        .ok_or_else(|| Failure::refused("state.db is not initialized; run tiller init"))?;
    let edit = HostEdit {
        lane_capacity: args.lane_capacity,
        ram_floor_mb: args.ram_floor_mb.map(Some),
        disk_floor_mb: args.disk_floor_mb.map(Some),
    };
    Ok(Outcome::applied(store.host_edit(
        &app.ctx(),
        &local.id,
        args.rev,
        edit,
    )?))
}

// -- config.toml -----------------------------------------------------------

pub(crate) fn config_file(app: &App) -> std::path::PathBuf {
    app.home.join("config.toml")
}

pub fn load_config(app: &App) -> Result<toml::Table, Failure> {
    load_config_at(&config_file(app))
}

/// [`load_config`] for a `config.toml` path; a missing file is an empty table.
pub fn load_config_at(path: &Path) -> Result<toml::Table, Failure> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse::<toml::Table>().map_err(|error| {
            Failure::new(
                "internal",
                1,
                format!("config.toml does not parse: {error}"),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => Err(error.into()),
    }
}

/// Writes through a temporary file in the same directory and renames it, so a
/// reader never sees half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), Failure> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", tiller_core::ids::new_ulid()));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn get_path<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let mut current = table.get(parts.next()?)?;
    for part in parts {
        current = current.as_table()?.get(part)?;
    }
    Some(current)
}

fn set_path(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<(), Failure> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(Failure::usage(format!("not a config key: {key:?}")));
    }
    let (last, parents) = parts.split_last().unwrap_or((&"", &[]));
    let mut current = table;
    for part in parents {
        let entry = current
            .entry((*part).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        current = entry
            .as_table_mut()
            .ok_or_else(|| Failure::refused(format!("{part} is not a table in config.toml")))?;
    }
    current.insert((*last).to_owned(), value);
    Ok(())
}

/// A value typed on the command line: TOML when it parses (`3`, `true`,
/// `["a"]`), a plain string otherwise.
fn parse_value(raw: &str) -> toml::Value {
    toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut table| table.remove("v"))
        .unwrap_or_else(|| toml::Value::String(raw.to_owned()))
}

fn config_json(table: &toml::Table) -> Value {
    serde_json::to_value(table).unwrap_or(Value::Null)
}

pub fn config_hash_of(table: &toml::Table) -> String {
    let digest = Sha256::digest(config_json(table).to_string().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Sets one key, then records the change: the file first, so a crash between
/// the two leaves a file newer than its event, which `doctor` reports.
pub(crate) fn set_config(
    app: &App,
    store: &mut tiller_core::Store,
    key: &str,
    value: toml::Value,
) -> Result<Applied<i64>, Failure> {
    let mut table = load_config(app)?;
    if get_path(&table, key) == Some(&value) {
        return Ok(Applied {
            value: 0,
            events: Vec::new(),
            replayed: true,
        });
    }
    set_path(&mut table, key, value.clone())?;
    let text = toml::to_string_pretty(&table).map_err(|error| {
        Failure::new(
            "internal",
            1,
            format!("config.toml cannot be written: {error}"),
        )
    })?;
    write_atomic(&config_file(app), &text)?;
    let recorded = serde_json::to_value(&value).unwrap_or(Value::Null);
    Ok(store.config_changed(&app.ctx(), key, recorded)?)
}

pub fn config(app: &App, cmd: ConfigCmd) -> Done {
    match cmd {
        ConfigCmd::Set { key, value } => {
            app.require_owner("tiller config set")?;
            let mut store = app.write()?;
            let value = parse_value(&value);
            if key == "federation.accept" && !value.is_bool() {
                return Err(Failure::usage(
                    "federation.accept is true or false (ADR 0003)",
                ));
            }
            if key == "stop_hook_mode" && value.as_str() == Some("block-once") {
                let stats = intake_stats(&store, 7)?;
                if !stats.rule_met {
                    let mut failure = Failure::refused(format!(
                        "block-once needs {STOP_HOOK_MIN_STOPS} orchestrator stops in 7 days with a false-block rate under {:.0}%; there are {} stops and a rate of {}",
                        STOP_HOOK_MAX_RATE * 100.0,
                        stats.stops,
                        stats
                            .false_block_rate
                            .map_or("unknown".to_owned(), |rate| format!("{:.1}%", rate * 100.0)),
                    ));
                    failure.current = Some(serde_json::to_value(&stats).unwrap_or(Value::Null));
                    return Err(failure);
                }
            }
            let applied = set_config(app, &mut store, &key, value)?;
            Ok(Outcome::applied_as(applied, |_| json!({"key": key})))
        }
        ConfigCmd::Get { key } => {
            forbid_fleet(app)?;
            let table = load_config(app)?;
            let data = match key {
                None => json!({"config": config_json(&table)}),
                Some(key) => {
                    let value = get_path(&table, &key).ok_or_else(|| {
                        Failure::from(tiller_core::Error::NotFound {
                            entity: "config key",
                            id: key.clone(),
                            candidates: Vec::new(),
                        })
                    })?;
                    json!({"key": key, "value": serde_json::to_value(value).unwrap_or(Value::Null)})
                }
            };
            Ok(Outcome::read(data))
        }
        ConfigCmd::Hash => {
            forbid_fleet(app)?;
            Ok(Outcome::read(
                json!({"hash": config_hash_of(&load_config(app)?)}),
            ))
        }
        ConfigCmd::Diff { peer } => diff(app, &peer),
    }
}

/// Fleet roles can neither read nor write the configuration
/// (`authority.md`, section 8).
fn forbid_fleet(app: &App) -> Result<(), Failure> {
    if app.actor.is_owner() || app.actor == tiller_core::Actor::Daemon {
        Ok(())
    } else {
        Err(Failure::forbidden(format!(
            "{} may not read the configuration",
            app.actor
        )))
    }
}

/// Lists what differs between this host's configuration and a peer's, read
/// through `ssh <alias> tiller config get --json`.
fn diff(app: &App, peer: &str) -> Done {
    forbid_fleet(app)?;
    let table = load_config(app)?;
    let alias = get_path(&table, &format!("peers.{peer}.ssh"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            Failure::from(tiller_core::Error::NotFound {
                entity: "peer",
                id: peer.to_owned(),
                candidates: Vec::new(),
            })
        })?
        .to_owned();
    let ssh = std::env::var("TILLER_SSH").unwrap_or_else(|_| "ssh".into());
    let output = std::process::Command::new(ssh)
        .args(["--", &alias, "tiller", "config", "get", "--json"])
        .output()
        .map_err(|error| Failure::unavailable(format!("cannot run ssh to {peer}: {error}")))?;
    if !output.status.success() {
        return Err(Failure::unavailable(format!(
            "{peer} did not answer the configuration read"
        )));
    }
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        Failure::unavailable(format!("{peer} sent a reply that does not parse: {error}"))
    })?;
    let theirs = reply
        .pointer("/data/config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let ours = config_json(&table).as_object().cloned().unwrap_or_default();
    let mut differences = Vec::new();
    flatten_diff("", &ours, &theirs, &mut differences);
    Ok(Outcome::read(json!({
        "peer": peer,
        "same": differences.is_empty(),
        "hash": config_hash_of(&table),
        "differences": differences,
    })))
}

fn flatten_diff(
    prefix: &str,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
    out: &mut Vec<Value>,
) {
    let mut keys: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (ours.get(key), theirs.get(key)) {
            (Some(Value::Object(a)), Some(Value::Object(b))) => flatten_diff(&path, a, b, out),
            (a, b) if a == b => {}
            (a, b) => out.push(json!({"key": path, "here": a, "there": b})),
        }
    }
}

pub fn owner(app: &App, cmd: OwnerCmd) -> Done {
    match cmd {
        OwnerCmd::Show => Ok(Outcome::read(json!({"owner": app.owner_marker()}))),
        OwnerCmd::Set { name } => {
            app.require_owner("tiller owner set")?;
            if name != "tiller" && name != "firstmate" {
                return Err(Failure::usage("the owner is tiller or firstmate"));
            }
            if app.owner_marker().as_deref() == Some(name.as_str()) {
                return Ok(Outcome::applied(Applied {
                    value: json!({"owner": name}),
                    events: Vec::new(),
                    replayed: true,
                }));
            }
            write_atomic(&app.home.join("owner"), &format!("{name}\n"))?;
            // Flipping back to tiller is recorded; the other direction leaves
            // state.db to the system that now owns the host.
            let mut events = Vec::new();
            if name == "tiller" && app.db.exists() {
                let mut store = app.read()?;
                events = store
                    .config_changed(&app.ctx(), "owner", json!("tiller"))?
                    .events;
            }
            Ok(Outcome::applied(Applied {
                value: json!({"owner": name}),
                events,
                replayed: false,
            }))
        }
    }
}
