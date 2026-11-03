//! Standing grant matching (`authority.md` section 6, "Standing grants"),
//! used by R08's rematch after a fast-forward push. Every listed condition
//! must hold; a condition the engine cannot evaluate from what it has
//! (an unknown head branch, unknown changed paths) fails closed, so the PR
//! goes to the owner instead.

use rusqlite::Connection;
use serde_json::Value;
use tiller_core::Timestamp;
use tiller_core::domain::{
    ApprovalAction, Card, Checks, Grant, GrantKind, PipelineState, Pr, PrState, Review, Run,
    Verdict,
};
use tiller_core::state::review::prelaunch;
use tiller_core::state::{find, list};

use crate::sources::PrRead;

/// A glob with `*` (any run of characters but `/`) and `**` (anything).
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) if rest.first() == Some(&'*') => {
                let rest = &rest[1..];
                let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
                (0..=t.len()).any(|i| rec(rest, &t[i..]))
            }
            Some(('*', rest)) => (0..=t.len())
                .take_while(|i| t[..*i].iter().all(|c| *c != '/'))
                .any(|i| rec(rest, &t[i..])),
            Some((c, rest)) => t.first() == Some(c) && rec(rest, &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    rec(&p, &t)
}

fn str_list(value: Option<&Value>) -> Option<Vec<&str>> {
    value
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
}

/// Why a grant did not match, for the event payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoMatch(pub String);

/// Checks one grant against a PR, its card and its run. Returns the action
/// the grant allows (its first listed one).
pub fn check(
    conn: &Connection,
    grant: &Grant,
    pr: &Pr,
    read: Option<&PrRead>,
    card: &Card,
    run: Option<&Run>,
    now: Timestamp,
) -> tiller_core::Result<Result<ApprovalAction, NoMatch>> {
    let c = &grant.conditions;
    let fail = |why: &str| Ok(Err(NoMatch(why.to_owned())));
    if grant.revoked_at.is_some() {
        return fail("grant is revoked");
    }
    if c.get("v").and_then(Value::as_i64) != Some(1) {
        return fail("grant conditions are not version 1");
    }
    if let Some(expires) = c.get("expires_at").and_then(Value::as_str)
        && expires.parse::<Timestamp>().is_ok_and(|at| at <= now)
    {
        return fail("grant expired");
    }
    if pr.state != PrState::Open {
        return fail("PR is not open");
    }
    let Some(action) = str_list(c.get("actions")).and_then(|a| a.first().copied()) else {
        return fail("grant lists no action");
    };
    let Ok(action) = action.parse::<ApprovalAction>() else {
        return fail("grant action is unknown");
    };
    if let Some(base) = c.get("base_ref").and_then(Value::as_str)
        && pr.base_ref.as_deref() != Some(base)
    {
        return fail("PR does not target the grant's base branch");
    }
    if let Some(pattern) = c.get("head_ref_pattern").and_then(Value::as_str) {
        match read.and_then(|r| r.head_ref.as_deref()) {
            Some(head) if glob_matches(pattern, head) => {}
            Some(_) => return fail("PR head branch does not match the grant's pattern"),
            None => return fail("PR head branch is unknown"),
        }
    }
    if c.get("opened_by_fleet_run").and_then(Value::as_bool) == Some(true) {
        let Some(run) = run else {
            return fail("PR was not opened by a fleet run");
        };
        if run.card_id != card.id || card.project_id != grant.project_id {
            return fail("PR's run belongs to another card or project");
        }
    }
    if let Some(kinds) = str_list(c.get("card_kinds"))
        && !kinds.contains(&card.kind.as_str())
    {
        return fail("card kind is outside the grant");
    }
    if let Some(checks) = c.get("checks").and_then(Value::as_str) {
        let required: Checks = checks.parse()?;
        if pr.checks != required {
            return fail("PR checks are not in the required state");
        }
    }
    // Skipped for a PR with no pipeline run while the project is pre-launch,
    // which runs no pipeline (`docs/design/pre-launch-validation.md`, D4).
    if let Some(pipeline) = c.get("pipeline").and_then(Value::as_str) {
        let required: PipelineState = pipeline.parse()?;
        let waived = pr.pipeline_state == PipelineState::None && prelaunch(conn, &card.project_id)?;
        if pr.pipeline_state != required && !waived {
            return fail("pipeline is not in the required state");
        }
    }
    if let Some(reviews) = c.get("reviews").filter(|r| !r.is_null()) {
        let Some(run) = run else {
            return fail("reviews are required but the PR has no run");
        };
        let rows: Vec<Review> = list(conn, "run_id = ?1 ORDER BY recorded_at", &[&run.id])?;
        let required = str_list(reviews.get("require")).unwrap_or_default();
        for reviewer in required {
            let passed = rows
                .iter()
                .any(|r| r.reviewer.as_str() == reviewer && r.verdict == Verdict::Pass);
            if !passed {
                return fail("a required review has not passed");
            }
        }
        if let Some(max_high) = reviews.get("max_high").and_then(Value::as_i64)
            && rows.iter().any(|r| r.findings_high > max_high)
        {
            return fail("a review has too many high findings");
        }
    }
    if let Some(excluded) = str_list(c.get("exclude_paths")).filter(|e| !e.is_empty()) {
        match read.and_then(|r| r.changed_paths.as_ref()) {
            Some(paths) => {
                if paths
                    .iter()
                    .any(|p| excluded.iter().any(|glob| glob_matches(glob, p)))
                {
                    return fail("PR touches an excluded path");
                }
            }
            None => return fail("PR changed paths are unknown"),
        }
    }
    Ok(Ok(action))
}

/// The first live grant of `kind` on the project that matches the PR.
pub fn matching(
    conn: &Connection,
    project_id: &str,
    kind: GrantKind,
    pr: &Pr,
    read: Option<&PrRead>,
    now: Timestamp,
) -> tiller_core::Result<Option<(Grant, ApprovalAction)>> {
    let card: Card = tiller_core::state::get(conn, "card", &pr.card_id)?;
    let run: Option<Run> = match &pr.run_id {
        Some(id) => find(conn, "id = ?1", &[id])?,
        None => None,
    };
    let grants: Vec<Grant> = list(
        conn,
        "project_id = ?1 AND kind = ?2 AND revoked_at IS NULL ORDER BY granted_at, id",
        &[&project_id, &kind],
    )?;
    for grant in grants {
        if let Ok(action) = check(conn, &grant, pr, read, &card, run.as_ref(), now)? {
            return Ok(Some((grant, action)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::glob_matches;

    #[test]
    fn globs_match_one_segment_with_star_and_any_depth_with_double_star() {
        assert!(glob_matches("fm/*", "fm/tiller-t2-7"));
        assert!(!glob_matches("fm/*", "fm/a/b"));
        assert!(glob_matches(
            ".github/workflows/**",
            ".github/workflows/ci.yml"
        ));
        assert!(glob_matches(
            "**/*.sql",
            "crates/core/migrations/0001/up.sql"
        ));
        assert!(!glob_matches("docs/*", "src/lib.rs"));
    }
}
