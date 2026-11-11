//! Standing grants (P13, `authority.md` section 6): the version 1 conditions,
//! evaluated against what the forge reports now and what `state.db` records,
//! at match time and again in the land sequence (G05).

use rusqlite::Connection;
use serde::Deserialize;
use tiller_core::domain::{
    Approval, ApprovalAction, Card, Checks, Grant, GrantKind, PipelineState, Pr, Review, Run,
    Verdict,
};
use tiller_core::state::approvals::active_away;
use tiller_core::state::review::prelaunch;
use tiller_core::state::{find, get, list};
use tiller_core::{Result, Timestamp};

use super::forge::PullView;

/// The paths every grant excludes, whatever its `exclude_paths` says: what a
/// fleet role loads (`memory.md` section 3), so a change to it always reaches
/// the owner as a memory change (CP2, S01). The project's source-map rows are
/// added to these.
const LOADED_PATHS: &[&str] = &[
    "AGENTS.md",
    "**/AGENTS.md",
    "CLAUDE.md",
    "**/CLAUDE.md",
    "CLAUDE.local.md",
    "**/CLAUDE.local.md",
    ".claude/**",
    "**/.claude/**",
    ".mcp.json",
    "**/.mcp.json",
];

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Reviews {
    require: Vec<String>,
    max_high: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Conditions {
    v: i64,
    actions: Vec<String>,
    base_ref: Option<String>,
    head_ref_pattern: Option<String>,
    opened_by_fleet_run: bool,
    card_kinds: Option<Vec<String>>,
    checks: Option<String>,
    pipeline: Option<String>,
    reviews: Option<Reviews>,
    exclude_paths: Vec<String>,
    expires_at: Option<String>,
}

/// Why a grant does not cover a PR, in the words the board shows.
pub type Miss = String;

/// Away mode with `merge: "none"` allows no merge, even under a standing
/// grant (`authority.md`, Away mode).
pub fn away_blocks(conn: &Connection, now: Timestamp) -> Result<Option<Miss>> {
    Ok(active_away(conn, now)?
        .is_some_and(|away| away.no_merges)
        .then(|| "away mode allows no merges".into()))
}

/// A `land_direct` grant still covers a push: the right kind, readable,
/// neither revoked nor expired, and no away contract forbids merges. The
/// branch and checks conditions are the land sequence's own checks
/// (`authority.md` section 6).
pub fn direct_holds(
    conn: &Connection,
    grant: &Grant,
    now: Timestamp,
) -> Result<std::result::Result<(), Miss>> {
    if grant.kind != GrantKind::LandDirect {
        return Ok(Err(format!("the grant is {}, not land_direct", grant.kind)));
    }
    let Ok(conditions) = serde_json::from_value::<Conditions>(grant.conditions.clone()) else {
        return Ok(Err("the grant's conditions cannot be read".into()));
    };
    if conditions.v != 1 {
        return Ok(Err(format!(
            "grant conditions version {} is unknown",
            conditions.v
        )));
    }
    if grant.revoked_at.is_some() {
        return Ok(Err("the grant is revoked".into()));
    }
    if let Some(expires) = &conditions.expires_at
        && expires.parse::<Timestamp>().map_or(true, |at| at <= now)
    {
        return Ok(Err(format!("the grant expired at {expires}")));
    }
    if let Some(miss) = away_blocks(conn, now)? {
        return Ok(Err(miss));
    }
    Ok(Ok(()))
}

/// Every condition of `grant` against one approval's PR as the forge reports
/// it now. `Ok(())` only when all hold.
pub fn evaluate(
    conn: &Connection,
    grant: &Grant,
    approval: &Approval,
    pr: &Pr,
    view: &PullView,
    now: Timestamp,
) -> Result<std::result::Result<(), Miss>> {
    let Ok(conditions) = serde_json::from_value::<Conditions>(grant.conditions.clone()) else {
        return Ok(Err("the grant's conditions cannot be read".into()));
    };
    if conditions.v != 1 {
        return Ok(Err(format!(
            "grant conditions version {} is unknown",
            conditions.v
        )));
    }
    if grant.revoked_at.is_some() {
        return Ok(Err("the grant is revoked".into()));
    }
    if let Some(expires) = &conditions.expires_at
        && expires.parse::<Timestamp>().map_or(true, |at| at <= now)
    {
        return Ok(Err(format!("the grant expired at {expires}")));
    }
    if let Some(miss) = away_blocks(conn, now)? {
        return Ok(Err(miss));
    }
    if grant.project_id != approval.project_id {
        return Ok(Err("the grant is for another project".into()));
    }
    if !conditions
        .actions
        .iter()
        .any(|action| action == approval.action.as_str())
    {
        return Ok(Err(format!("the grant does not allow {}", approval.action)));
    }
    if approval.memory_paths.is_some() {
        return Ok(Err("a memory change is never accepted by a grant".into()));
    }
    if let Some(base) = &conditions.base_ref
        && &view.base_ref != base
    {
        return Ok(Err(format!("the PR targets {}, not {base}", view.base_ref)));
    }
    if let Some(pattern) = &conditions.head_ref_pattern
        && !glob(pattern, &view.head_ref)
    {
        return Ok(Err(format!("{} does not match {pattern}", view.head_ref)));
    }
    let run: Option<Run> = match &pr.run_id {
        Some(id) => find(conn, "id = ?1", &[id])?,
        None => None,
    };
    if conditions.opened_by_fleet_run {
        let in_scope = match &run {
            Some(run) => {
                let card: Card = get(conn, "card", &run.card_id)?;
                card.project_id == approval.project_id
            }
            None => false,
        };
        if !in_scope {
            return Ok(Err(
                "the PR was not opened by a fleet run of this project".into()
            ));
        }
    }
    if let Some(kinds) = &conditions.card_kinds {
        let card: Card = get(conn, "card", &pr.card_id)?;
        if !kinds.iter().any(|kind| kind == card.kind.as_str()) {
            return Ok(Err(format!("the card is a {} card", card.kind)));
        }
    }
    if let Some(checks) = &conditions.checks
        && view.checks.as_str() != checks
    {
        return Ok(Err(format!("checks are {}, not {checks}", view.checks)));
    }
    // A pre-launch project runs no pipeline: the condition is skipped for a
    // PR with no pipeline run while the owner keeps the project pre-launch,
    // and holds again from launch. A pipeline that ran still counts
    // (`docs/design/pre-launch-validation.md`, D4).
    if let Some(pipeline) = &conditions.pipeline
        && pr.pipeline_state.as_str() != pipeline
        && !(pr.pipeline_state == PipelineState::None && prelaunch(conn, &approval.project_id)?)
    {
        return Ok(Err(format!(
            "the pipeline is {}, not {pipeline}",
            pr.pipeline_state
        )));
    }
    if let Some(reviews) = &conditions.reviews {
        let Some(run) = &run else {
            return Ok(Err("the PR has no run to carry reviews".into()));
        };
        // A review covers the commit it read: only one recorded at the head
        // the forge reports now counts, and a review with no recorded commit
        // (from before migration 16) never does (B11).
        let head = view.head_sha.to_ascii_lowercase();
        let recorded: Vec<Review> = list(
            conn,
            "run_id = ?1 AND head_sha = ?2 ORDER BY recorded_at DESC, id DESC",
            &[&run.id, &head],
        )?;
        for kind in &reviews.require {
            // The newest review of each kind at this head decides.
            match recorded
                .iter()
                .find(|review| review.reviewer.as_str() == kind)
            {
                Some(review) if review.verdict == Verdict::Pass => {
                    if let Some(max) = reviews.max_high
                        && review.findings_high > max
                    {
                        return Ok(Err(format!(
                            "{kind} found {} high findings, more than {max}",
                            review.findings_high
                        )));
                    }
                }
                Some(review) => return Ok(Err(format!("{kind} verdict is {}", review.verdict))),
                None => {
                    return Ok(Err(format!(
                        "no {kind} review is recorded at {}",
                        short(&view.head_sha)
                    )));
                }
            }
        }
    }
    let mut excluded: Vec<String> = LOADED_PATHS.iter().map(|path| (*path).to_owned()).collect();
    excluded.extend(conditions.exclude_paths.iter().cloned());
    excluded.extend(source_map(conn, &approval.project_id)?);
    if let Some(path) = view
        .files
        .iter()
        .find(|file| excluded.iter().any(|pattern| glob(pattern, file)))
    {
        return Ok(Err(format!("the PR changes {path}, which no grant covers")));
    }
    Ok(Ok(()))
}

/// The first 12 characters of a SHA, for a miss the board shows.
fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// The project-relative loaded paths of the project's live source-map rows.
fn source_map(conn: &Connection, project: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT loaded_path FROM memory_sources
         WHERE project_id IN (?1, 'fleet') AND retired_at IS NULL AND loaded_path NOT LIKE '~%'",
    )?;
    let rows = statement.query_map([project], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Whether the forge's checks let a grant land: a grant naming no `checks`
/// still needs them finished and not failed.
pub fn checks_done(checks: Checks) -> bool {
    matches!(checks, Checks::Success | Checks::None)
}

/// The pipeline condition is about `no-mistakes`; a project without it
/// reports `none`.
pub fn pipeline_done(state: PipelineState) -> bool {
    !matches!(state, PipelineState::Running)
}

/// The action a grant lands with: the first it lists that the PR allows.
pub fn action(grant: &Grant) -> Option<ApprovalAction> {
    let conditions: Conditions = serde_json::from_value(grant.conditions.clone()).ok()?;
    conditions
        .actions
        .iter()
        .find_map(|action| action.parse().ok())
}

/// A path glob: `**` crosses directories, `*` and `?` stay inside one.
pub fn glob(pattern: &str, text: &str) -> bool {
    fn go(pattern: &[u8], text: &[u8]) -> bool {
        match pattern {
            [] => text.is_empty(),
            // A trailing `**` takes everything below.
            [b'*', b'*'] => true,
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=text.len())
                    .any(|at| (at == 0 || text[at - 1] == b'/') && go(rest, &text[at..]))
                    || go(rest, text)
            }
            [b'*', rest @ ..] => (0..=text.len())
                .take_while(|&at| at == 0 || text[at - 1] != b'/')
                .any(|at| go(rest, &text[at..])),
            [b'?', rest @ ..] => text.first().is_some_and(|c| *c != b'/') && go(rest, &text[1..]),
            [c, rest @ ..] => text.first() == Some(c) && go(rest, &text[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{LOADED_PATHS, glob};

    fn loaded(file: &str) -> bool {
        LOADED_PATHS.iter().any(|pattern| glob(pattern, file))
    }

    #[test]
    fn loaded_paths_cover_nested_files() {
        for file in [
            "AGENTS.md",
            "docs/AGENTS.md",
            "crates/core/CLAUDE.md",
            "tools/.claude/settings.json",
            ".mcp.json",
            "tools/.mcp.json",
            "a/b/.mcp.json",
        ] {
            assert!(loaded(file), "{file} must need a memory change");
        }
        assert!(!loaded("src/mcp.json"));
        assert!(!loaded("docs/.mcp.json.md"));
    }

    #[test]
    fn globs_follow_path_segments() {
        assert!(glob("fm/*", "fm/alpha-1"));
        assert!(!glob("fm/*", "fm/a/b"));
        assert!(glob(".github/workflows/**", ".github/workflows/ci.yml"));
        assert!(glob(".github/workflows/**", ".github/workflows/x/y.yml"));
        assert!(glob("**/CLAUDE.md", "CLAUDE.md"));
        assert!(glob("**/CLAUDE.md", "crates/core/CLAUDE.md"));
        assert!(!glob("**/CLAUDE.md", "crates/core/CLAUDE.mdx"));
        assert!(glob(".claude/**", ".claude/rules/style.md"));
        assert!(glob("src/?.rs", "src/a.rs"));
        assert!(!glob("src/*.rs", "src/a/b.rs"));
        assert!(!glob("AGENTS.md", "docs/AGENTS.md"));
    }
}
