//! The project's `checks` column (`state.md` section 3; P10, P11): the
//! commands a worker runs before it reports done on a `direct-push` project,
//! with a lighter set for a change that touches no logic.
//!
//! Two shapes are stored, and both read back through [`CheckPlan`]:
//!
//! - a JSON array of commands: every change runs all of them;
//! - an object `{"full": [...], "no_logic": [...]}`: `full` is the set for a
//!   change that touches logic, `no_logic` the set for one that does not
//!   (docs, comments, copy, formatting). A missing `no_logic` means no
//!   lighter set, so the full set applies; an empty one means no check at all.

use serde_json::Value;

/// The commands one project runs, read from `projects.checks`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckPlan {
    /// Run for a change that touches logic.
    pub full: Vec<String>,
    /// Run for a change that touches no logic; `None` means the full set.
    pub no_logic: Option<Vec<String>>,
}

impl CheckPlan {
    /// Reads `projects.checks`. A shape that is neither an array of commands
    /// nor the object above is an error naming what is wrong.
    pub fn parse(value: &Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(Self::default()),
            Value::Array(_) => Ok(Self {
                full: commands(value, "checks")?,
                no_logic: None,
            }),
            Value::Object(map) => {
                if let Some(key) = map.keys().find(|key| !matches!(key.as_str(), "full" | "no_logic"))
                {
                    return Err(format!(
                        "checks has an unknown key {key:?}; the keys are \"full\" and \"no_logic\""
                    ));
                }
                Ok(Self {
                    full: map
                        .get("full")
                        .map(|list| commands(list, "checks.full"))
                        .transpose()?
                        .unwrap_or_default(),
                    no_logic: map
                        .get("no_logic")
                        .map(|list| commands(list, "checks.no_logic"))
                        .transpose()?,
                })
            }
            _ => Err("checks must be a JSON array of commands or an object with \"full\" and \"no_logic\"".into()),
        }
    }

    /// Whether the project names no command at all.
    pub fn is_empty(&self) -> bool {
        self.full.is_empty() && self.no_logic.as_ref().is_none_or(Vec::is_empty)
    }

    /// The commands a change runs: the lighter set when the change touches no
    /// logic and the project has one, the full set otherwise.
    pub fn for_change(&self, touches_logic: bool) -> &[String] {
        match (&self.no_logic, touches_logic) {
            (Some(lighter), false) => lighter,
            _ => &self.full,
        }
    }
}

fn commands(value: &Value, name: &str) -> Result<Vec<String>, String> {
    let list = value
        .as_array()
        .ok_or_else(|| format!("{name} must be a JSON array of commands"))?;
    list.iter()
        .map(|item| match item.as_str().map(str::trim) {
            Some(command) if !command.is_empty() => Ok(command.to_owned()),
            _ => Err(format!("{name} must hold non-empty command strings")),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_array_is_the_full_set_for_every_change() {
        let plan = CheckPlan::parse(&json!(["cargo test", "cargo clippy"])).unwrap();
        assert_eq!(plan.for_change(true), ["cargo test", "cargo clippy"]);
        assert_eq!(plan.for_change(false), ["cargo test", "cargo clippy"]);
        assert!(!plan.is_empty());
        assert!(CheckPlan::parse(&json!([])).unwrap().is_empty());
        assert!(CheckPlan::parse(&Value::Null).unwrap().is_empty());
    }

    #[test]
    fn a_no_logic_change_runs_the_lighter_set() {
        let plan = CheckPlan::parse(&json!({
            "full": ["cargo test", "cargo clippy"],
            "no_logic": ["cargo fmt --check"],
        }))
        .unwrap();
        assert_eq!(plan.for_change(true), ["cargo test", "cargo clippy"]);
        assert_eq!(plan.for_change(false), ["cargo fmt --check"]);
    }

    #[test]
    fn no_lighter_set_means_the_full_set_and_an_empty_one_means_none() {
        let plan = CheckPlan::parse(&json!({"full": ["cargo test"]})).unwrap();
        assert_eq!(plan.for_change(false), ["cargo test"]);

        let plan = CheckPlan::parse(&json!({"full": ["cargo test"], "no_logic": []})).unwrap();
        assert!(plan.for_change(false).is_empty());
        assert!(!plan.is_empty());
        assert!(
            CheckPlan::parse(&json!({"full": [], "no_logic": []}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_shape_it_does_not_know_is_refused() {
        for bad in [
            json!("cargo test"),
            json!(3),
            json!([1]),
            json!([""]),
            json!({"light": ["x"]}),
            json!({"full": "cargo test"}),
        ] {
            assert!(CheckPlan::parse(&bad).is_err(), "{bad}");
        }
    }
}
