//! The shape of a grant's conditions, version 1 (`authority.md` section 6).
//! `grant.create` checks every grant against it, whether the owner CLI, the
//! board's API or away mode made it, so a typo or a wrong type is refused
//! when the grant is made instead of matching something else later.

use std::str::FromStr;

use serde_json::{Map, Value};

use super::enums::{CardKind, Checks, GrantKind, PipelineState, Reviewer};
use crate::Timestamp;
use crate::error::Error;

/// Every key a version 1 conditions object may hold.
const KEYS: &[&str] = &[
    "v",
    "actions",
    "base_ref",
    "head_ref_pattern",
    "opened_by_fleet_run",
    "card_kinds",
    "checks",
    "pipeline",
    "reviews",
    "exclude_paths",
    "expires_at",
    "away_approval_id",
];

/// Every key the `reviews` object may hold.
const REVIEW_KEYS: &[&str] = &["require", "max_high"];

fn refuse<T>(text: String) -> Result<T, Error> {
    Err(Error::Usage(format!("grant conditions: {text}")))
}

/// The text forms of an enum, as "a, b or c".
fn either(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

/// The text forms of an enum, each in quotes, as "a, b or c".
fn either_quoted(words: &[&str]) -> String {
    let quoted: Vec<String> = words.iter().map(|word| format!("\"{word}\"")).collect();
    either(&quoted.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Text a condition compares as it is: not empty, and no space at either end
/// that would keep it from ever matching a branch or a path.
fn clean(text: &str) -> bool {
    !text.is_empty() && text.trim() == text
}

fn names<T: Copy>(all: &[T], text: fn(T) -> &'static str) -> Vec<&'static str> {
    all.iter().map(|each| text(*each)).collect()
}

/// A key that is absent or null does not limit the grant.
fn given<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key).filter(|value| !value.is_null())
}

fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, Error> {
    match given(map, key) {
        None => Ok(None),
        Some(Value::String(text)) if clean(text) => Ok(Some(text)),
        Some(_) => refuse(format!(
            "{key} must be non-empty text with no space at either end, or null"
        )),
    }
}

/// A list of non-empty text, with at least one entry unless `empty` allows none.
fn texts<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    empty: bool,
) -> Result<Option<Vec<&'a str>>, Error> {
    let Some(value) = given(map, key) else {
        return Ok(None);
    };
    let items: Option<Vec<&str>> = value.as_array().and_then(|list| {
        list.iter()
            .map(|item| item.as_str().filter(|text| clean(text)))
            .collect()
    });
    match items {
        Some(items) if empty || !items.is_empty() => Ok(Some(items)),
        _ => {
            let least = if empty { "" } else { "at least one " };
            refuse(format!(
                "{key} must be a list of {least}non-empty text with no space at either end"
            ))
        }
    }
}

/// One of an enum's text forms.
fn one_of<T: FromStr>(map: &Map<String, Value>, key: &str, all: &[&str]) -> Result<(), Error> {
    if let Some(value) = text(map, key)?
        && value.parse::<T>().is_err()
    {
        return refuse(format!(
            "{key} must be {} or null, not \"{value}\"",
            either_quoted(all)
        ));
    }
    Ok(())
}

/// Each entry one of an enum's text forms.
fn each_of<T: FromStr>(map: &Map<String, Value>, key: &str, all: &[&str]) -> Result<(), Error> {
    for value in texts(map, key, false)?.unwrap_or_default() {
        if value.parse::<T>().is_err() {
            return refuse(format!(
                "{key} takes {}, not \"{value}\"",
                either_quoted(all)
            ));
        }
    }
    Ok(())
}

/// The land sequence reads `opened_by_fleet_run` as true or false and
/// `exclude_paths` as a list, never null, so a null there would pass this
/// check and then leave the grant unreadable when it comes to merge.
fn refuse_null(map: &Map<String, Value>) -> Result<(), Error> {
    if let Some(key) = ["opened_by_fleet_run", "exclude_paths"]
        .into_iter()
        .find(|key| map.get(*key).is_some_and(Value::is_null))
    {
        return refuse(format!("{key} cannot be null; leave it out instead"));
    }
    Ok(())
}

fn unknown_keys(map: &Map<String, Value>, known: &[&str], within: &str) -> Result<(), Error> {
    if let Some(key) = map.keys().find(|key| !known.contains(&key.as_str())) {
        return refuse(format!(
            "unknown key \"{key}\"{within}; version 1 takes {}",
            either(known)
        ));
    }
    Ok(())
}

/// Refuses conditions that are not a version 1 object a `kind` grant takes:
/// an unknown key, a value of the wrong type, or a value no record can have.
pub fn validate_grant_conditions(kind: GrantKind, conditions: &Value) -> Result<(), Error> {
    let Some(map) = conditions.as_object() else {
        return refuse("the conditions are a JSON object".into());
    };
    if map.get("v").and_then(Value::as_i64) != Some(1) {
        return Err(Error::Usage(
            "grant conditions must be version 1 (\"v\": 1)".into(),
        ));
    }
    unknown_keys(map, KEYS, "")?;
    refuse_null(map)?;

    let allowed: &[&str] = match kind {
        GrantKind::Merge => &["merge", "squash", "rebase"],
        GrantKind::LandDirect => &["push"],
    };
    let actions: Option<Vec<&str>> = map
        .get("actions")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().map(Value::as_str).collect());
    if !actions.is_some_and(|list| !list.is_empty() && list.iter().all(|a| allowed.contains(a))) {
        return Err(Error::Usage(format!(
            "a {kind} grant's actions must be some of {}",
            allowed.join(", ")
        )));
    }

    text(map, "base_ref")?;
    text(map, "head_ref_pattern")?;
    if let Some(value) = given(map, "opened_by_fleet_run")
        && !value.is_boolean()
    {
        return refuse("opened_by_fleet_run must be true or false".into());
    }
    each_of::<CardKind>(map, "card_kinds", &names(CardKind::ALL, CardKind::as_str))?;
    one_of::<Checks>(map, "checks", &names(Checks::ALL, Checks::as_str))?;
    if kind == GrantKind::LandDirect && given(map, "pipeline").is_some() {
        return refuse("a land_direct grant has no pipeline condition".into());
    }
    one_of::<PipelineState>(
        map,
        "pipeline",
        &names(PipelineState::ALL, PipelineState::as_str),
    )?;
    if let Some(reviews) = given(map, "reviews") {
        let Some(reviews) = reviews.as_object() else {
            return refuse("reviews must be an object with require and max_high, or null".into());
        };
        unknown_keys(reviews, REVIEW_KEYS, " in reviews")?;
        let require: Option<Vec<&str>> = given(reviews, "require")
            .and_then(Value::as_array)
            .and_then(|list| list.iter().map(Value::as_str).collect())
            .filter(|list: &Vec<&str>| !list.is_empty());
        let Some(require) = require else {
            return refuse("reviews.require must name at least one reviewer".into());
        };
        let all = names(Reviewer::ALL, Reviewer::as_str);
        for value in require {
            if value.parse::<Reviewer>().is_err() {
                return refuse(format!(
                    "reviews.require takes {}, not \"{value}\"",
                    either_quoted(&all)
                ));
            }
        }
        if let Some(max) = given(reviews, "max_high")
            && !max.as_i64().is_some_and(|max| max >= 0)
        {
            return refuse("reviews.max_high must be a whole number, 0 or more, or null".into());
        }
    }
    // An empty list adds no exclusion: the source-map paths stay excluded.
    texts(map, "exclude_paths", true)?;
    if let Some(value) = given(map, "expires_at")
        && !value
            .as_str()
            .is_some_and(|at| at.parse::<Timestamp>().is_ok())
    {
        return refuse("expires_at must be an RFC 3339 time or null".into());
    }
    text(map, "away_approval_id")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    fn refused(kind: GrantKind, conditions: Value) -> String {
        match validate_grant_conditions(kind, &conditions) {
            Err(Error::Usage(text)) => text,
            other => panic!("{conditions} was not refused: {other:?}"),
        }
    }

    #[test]
    fn the_full_example_of_authority_md_is_accepted() {
        let full = json!({
            "v": 1,
            "actions": ["squash"],
            "base_ref": "main",
            "head_ref_pattern": "fm/*",
            "opened_by_fleet_run": true,
            "card_kinds": ["ship"],
            "checks": "success",
            "pipeline": "passed",
            "reviews": { "require": ["code-review"], "max_high": 0 },
            "exclude_paths": [".github/workflows/**"],
            "expires_at": null,
            "away_approval_id": null
        });
        validate_grant_conditions(GrantKind::Merge, &full).unwrap();
        let direct = json!({"v": 1, "actions": ["push"], "checks": "success"});
        validate_grant_conditions(GrantKind::LandDirect, &direct).unwrap();
        let ending = json!({"v": 1, "actions": ["merge"], "expires_at": "2099-02-01T09:00:00Z"});
        validate_grant_conditions(GrantKind::Merge, &ending).unwrap();
    }

    #[test]
    fn version_and_actions_are_checked_as_before() {
        assert!(refused(GrantKind::Merge, json!({"actions": ["merge"]})).contains("version 1"));
        assert!(
            refused(GrantKind::Merge, json!({"v": 2, "actions": ["merge"]})).contains("version 1")
        );
        assert!(refused(GrantKind::Merge, json!({"v": 1})).contains("some of merge"));
        assert!(refused(GrantKind::Merge, json!({"v": 1, "actions": []})).contains("some of"));
        assert!(
            refused(GrantKind::Merge, json!({"v": 1, "actions": ["push"]})).contains("some of")
        );
        assert!(
            refused(GrantKind::LandDirect, json!({"v": 1, "actions": ["merge"]})).contains("push")
        );
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let typo = refused(
            GrantKind::Merge,
            json!({"v": 1, "actions": ["merge"], "exclude_path": ["migrations/**"]}),
        );
        assert!(typo.contains("unknown key \"exclude_path\""), "{typo}");
        let inner = refused(
            GrantKind::Merge,
            json!({"v": 1, "actions": ["merge"], "reviews": {"require": ["human"], "max_medium": 0}}),
        );
        assert!(inner.contains("\"max_medium\" in reviews"), "{inner}");
    }

    #[test]
    fn each_key_takes_only_its_own_type_and_values() {
        let base = || json!({"v": 1, "actions": ["merge"]});
        let with = |key: &str, value: Value| {
            let mut conditions = base();
            conditions[key] = value;
            refused(GrantKind::Merge, conditions)
        };
        assert!(with("base_ref", json!(3)).contains("base_ref must be non-empty text"));
        assert!(with("base_ref", json!(" ")).contains("base_ref"));
        assert!(with("head_ref_pattern", json!(["a/*"])).contains("head_ref_pattern"));
        assert!(with("opened_by_fleet_run", json!("yes")).contains("true or false"));
        assert!(with("card_kinds", json!([])).contains("card_kinds must be a list"));
        assert!(with("card_kinds", json!("ship")).contains("card_kinds must be a list"));
        let kind = with("card_kinds", json!(["ship", "bug"]));
        assert!(
            kind.contains("\"ship\", \"scout\", \"question\" or \"ops\", not \"bug\""),
            "{kind}"
        );
        assert!(with("checks", json!("green")).contains("checks must be"));
        assert!(with("pipeline", json!(true)).contains("pipeline must be"));
        assert!(with("reviews", json!(["code-review"])).contains("reviews must be an object"));
        assert!(with("reviews", json!({"max_high": 0})).contains("at least one reviewer"));
        assert!(with("reviews", json!({"require": []})).contains("at least one reviewer"));
        let who = with("reviews", json!({"require": ["codex"]}));
        assert!(who.contains("not \"codex\""), "{who}");
        let high = with("reviews", json!({"require": ["human"], "max_high": -1}));
        assert!(high.contains("max_high must be a whole number"), "{high}");
        assert!(with("exclude_paths", json!("migrations/**")).contains("exclude_paths"));
        assert!(with("exclude_paths", json!([""])).contains("exclude_paths"));
        assert!(with("expires_at", json!("next week")).contains("RFC 3339"));
        assert!(with("away_approval_id", json!(7)).contains("away_approval_id"));
        let direct = refused(
            GrantKind::LandDirect,
            json!({"v": 1, "actions": ["push"], "pipeline": "passed"}),
        );
        assert!(direct.contains("no pipeline"), "{direct}");
    }

    #[test]
    fn a_null_key_does_not_limit_the_grant() {
        let nulls = json!({
            "v": 1, "actions": ["rebase"], "base_ref": null, "head_ref_pattern": null,
            "card_kinds": null, "checks": null, "pipeline": null,
            "reviews": {"require": ["human"], "max_high": null}, "expires_at": null,
            "away_approval_id": null
        });
        validate_grant_conditions(GrantKind::Merge, &nulls).unwrap();
        // No extra exclusion: the source-map paths stay out all the same.
        let none = json!({"v": 1, "actions": ["rebase"], "exclude_paths": []});
        validate_grant_conditions(GrantKind::Merge, &none).unwrap();
    }

    #[test]
    fn a_key_the_land_sequence_reads_without_null_refuses_null() {
        for key in ["opened_by_fleet_run", "exclude_paths"] {
            let mut conditions = json!({"v": 1, "actions": ["merge"]});
            conditions[key] = Value::Null;
            let text = refused(GrantKind::Merge, conditions);
            assert!(text.contains(&format!("{key} cannot be null")), "{text}");
        }
    }

    #[test]
    fn text_with_a_space_at_either_end_is_refused() {
        let with = |key: &str, value: Value| {
            let mut conditions = json!({"v": 1, "actions": ["merge"]});
            conditions[key] = value;
            refused(GrantKind::Merge, conditions)
        };
        assert!(with("base_ref", json!("main ")).contains("no space at either end"));
        assert!(with("head_ref_pattern", json!(" fm/*")).contains("head_ref_pattern"));
        assert!(with("exclude_paths", json!(["docs/** "])).contains("exclude_paths"));
    }
}
