//! Owner-side setup: `init`, projects, hosts, config and the owner marker
//! (`protocol.md` section 6.4, Projects, hosts and config). `tiller peer` is
//! [`super::peer`].

use std::io::Write;
use std::path::Path;

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiller_core::Applied;
use tiller_core::Store;
use tiller_core::domain::{Boundary, Delivery, HostKind, Project, Stage};
use tiller_core::state::get;
use tiller_core::state::projects::{HostEdit, Init, NewProject, ProjectEdit};

use super::clearable;
use super::read::{STOP_HOOK_MAX_RATE, STOP_HOOK_MIN_STOPS, intake_stats};
use super::review::checkout_head;
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
    Retire { id: String },
    /// List projects
    List,
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
            Ok(Outcome::applied(store.project_add(
                &app.ctx(),
                NewProject {
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
                },
            )?))
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
        ProjectCmd::Retire { id } => Ok(Outcome::applied(store.project_retire(&app.ctx(), &id)?)),
    }
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

fn config_file(app: &App) -> std::path::PathBuf {
    app.home.join("config.toml")
}

pub fn load_config(app: &App) -> Result<toml::Table, Failure> {
    match std::fs::read_to_string(config_file(app)) {
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
