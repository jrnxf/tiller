//! The PR body scrub of land step 4 (P17, G30): a body that quotes an ask
//! longer than 40 characters, names a scrub-list term, or claims a pipeline
//! result the PR does not have is never merged.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use tiller_core::domain::PipelineState;

/// The longest run of an ask's words a body may repeat.
const QUOTE_LIMIT: usize = 40;

/// Terms a public body must not carry: plain terms (matched as
/// case-insensitive substrings) and SHA-256 hashes of single tokens (matched
/// against each token of the body and its parts split on `.`, `_` and `-`),
/// the same rule as `scripts/scrub-check.sh`.
#[derive(Clone, Debug, Default)]
pub struct ScrubList {
    pub phrases: Vec<String>,
    pub hashes: HashSet<String>,
}

impl ScrubList {
    /// One term per line; a 64-digit hex line is a hash. `#` starts a comment.
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        for line in text.lines() {
            let term = line.split('#').next().unwrap_or_default().trim();
            if term.is_empty() {
                continue;
            }
            if term.len() == 64 && term.bytes().all(|b| b.is_ascii_hexdigit()) {
                list.hashes.insert(term.to_ascii_lowercase());
            } else {
                list.phrases.push(term.to_lowercase());
            }
        }
        list
    }
}

/// Why a body is refused, quoting only the rule and never the leaked text
/// itself, so the refusal can be shown anywhere.
pub fn scrub(
    body: &str,
    asks: &[String],
    list: &ScrubList,
    pipeline: PipelineState,
) -> Option<String> {
    let flat = normalize(body);
    for ask in asks {
        let ask = normalize(ask);
        if ask.chars().count() > QUOTE_LIMIT && quotes(&flat, &ask) {
            return Some(format!(
                "the PR body quotes an ask of more than {QUOTE_LIMIT} characters"
            ));
        }
    }
    if list
        .phrases
        .iter()
        .any(|phrase| flat.contains(phrase.as_str()))
    {
        return Some("the PR body names a scrub-list term".into());
    }
    if !list.hashes.is_empty() && tokens(&flat).any(|token| list.hashes.contains(&hex(token))) {
        return Some("the PR body names a scrub-list term".into());
    }
    if pipeline != PipelineState::Passed && claims_pipeline(body) {
        return Some(format!(
            "the PR body claims a passed pipeline, and the pipeline is {pipeline}"
        ));
    }
    None
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Whether `body` repeats any run of more than [`QUOTE_LIMIT`] characters
/// of `ask`, starting at a word.
fn quotes(body: &str, ask: &str) -> bool {
    let chars: Vec<char> = ask.chars().collect();
    let window = QUOTE_LIMIT + 1;
    (0..=chars.len().saturating_sub(window))
        .filter(|&at| at == 0 || chars[at - 1] == ' ')
        .any(|at| {
            let run: String = chars[at..at + window].iter().collect();
            body.contains(&run)
        })
}

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .filter(|token| !token.is_empty())
        .flat_map(|token| {
            std::iter::once(token).chain(
                token
                    .split(['.', '_', '-'])
                    .filter(move |part| !part.is_empty() && *part != token),
            )
        })
}

fn hex(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A line or sentence that says the pipeline or `no-mistakes` passed. It
/// reads the body as written: flattening first would join a heading to the
/// next line's words.
fn claims_pipeline(body: &str) -> bool {
    body.split(['.', '\n', ';']).map(normalize).any(|sentence| {
        (sentence.contains("no-mistakes") || sentence.contains("pipeline"))
            && ["passed", "green", "succeeded", "✅"]
                .iter()
                .any(|word| sentence.contains(word))
            && ![
                "not passed",
                "not green",
                "hasn't",
                "has not",
                "pending",
                "running",
            ]
            .iter()
            .any(|word| sentence.contains(word))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipeline_claim_is_read_line_by_line() {
        for body in [
            "no-mistakes passed on this head.",
            "## Checks\n\nThe   Pipeline\tPASSED",
            "Lint is pending\nno-mistakes: green",
        ] {
            assert!(claims_pipeline(body), "{body:?}");
        }
        for body in [
            "The pipeline is pending.",
            // A heading and the next line are not one sentence.
            "## Pipeline\nUnit tests passed",
            "no-mistakes has not\n passed yet",
        ] {
            assert!(!claims_pipeline(body), "{body:?}");
        }
    }
}
