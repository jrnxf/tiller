//! The model and effort of a run (`protocol.md` 6.4, P02): the owner's
//! per-card override wins, then the first rule that matches the card's kind
//! and size, then the default. The orchestrator may only raise the result;
//! a downgrade needs the owner (G28).

use serde_json::{Value, json};
use tiller_core::domain::{Card, Effort};

use super::SpawnError;
use super::config::Dispatch;

/// What dispatch decided, with the payload `run.started` quotes.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub model: String,
    pub effort: Effort,
    pub payload: Value,
}

/// Resolves the run's model and effort. `asked` is the spawn's own
/// `--model --effort`; `owner` says the owner spawns, who may also lower it.
pub fn resolve(
    dispatch: &Dispatch,
    card: &Card,
    asked: Option<(&str, Effort)>,
    owner: bool,
) -> Result<Resolved, SpawnError> {
    let (source, model, effort) = match override_of(card)? {
        Some((model, effort)) => ("override".to_owned(), model, effort),
        None => match dispatch.rules.iter().find(|rule| {
            rule.kind.is_none_or(|kind| kind == card.kind)
                && rule.size.is_none_or(|size| Some(size) == card.size)
        }) {
            Some(rule) => (rule.name.clone(), rule.model.clone(), rule.effort),
            None => (
                "default".to_owned(),
                dispatch.default.model.clone(),
                dispatch.default.effort,
            ),
        },
    };
    let mut payload = json!({"rule": source, "model": model, "effort": effort});
    let Some((asked_model, asked_effort)) = asked else {
        return Ok(Resolved {
            model,
            effort,
            payload,
        });
    };
    let rank = |m: &str| {
        dispatch
            .models
            .iter()
            .position(|known| known == m)
            .ok_or_else(|| {
                SpawnError::Usage(format!(
                    "model {m} is not in dispatch.models ({})",
                    dispatch.models.join(", ")
                ))
            })
    };
    let lower = rank(asked_model)? < rank(&model)? || asked_effort < effort;
    if lower && !owner {
        return Err(SpawnError::Downgrade(format!(
            "{asked_model}:{asked_effort} is below the {source} rule's {model}:{effort}; a downgrade needs the owner (tiller card edit {} --dispatch <model>:<effort>)",
            card.display_id
        )));
    }
    payload["asked"] = json!({"model": asked_model, "effort": asked_effort});
    payload["by_owner"] = json!(lower);
    Ok(Resolved {
        model: asked_model.to_owned(),
        effort: asked_effort,
        payload,
    })
}

fn override_of(card: &Card) -> Result<Option<(String, Effort)>, SpawnError> {
    let Some(value) = &card.dispatch_override else {
        return Ok(None);
    };
    let parsed: tiller_core::state::cards::DispatchOverride = serde_json::from_value(value.clone())
        .map_err(|e| {
            SpawnError::Config(format!(
                "card {} has an unreadable dispatch override: {e}",
                card.display_id
            ))
        })?;
    Ok(Some((parsed.model, parsed.effort)))
}
