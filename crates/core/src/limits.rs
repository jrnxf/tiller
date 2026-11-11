//! The longest text Tiller takes for a card title, a card body, a comment,
//! an answer, a steer and an ask (`protocol.md` 7.4, "Length caps"), in
//! characters (Unicode scalar values, not bytes). The store checks them in
//! the write itself (`card.create`, `card.edit`, the comment, the answer, the
//! steer, `ask.record` and the import plan), so every writer is held to the
//! same numbers: the CLI, the board API, a forwarded mutation, the hooks and
//! the transcript tailer get the same usage refusal. `tiller import` refuses
//! the whole plan instead, naming the card (exit 5), since it writes nothing
//! of a plan that does not fit. The API checks again before it forwards.
//! `board/src/lib/limits.ts` mirrors the numbers, and its test reads this
//! file to keep them equal.

use crate::{Error, Result};

pub const TITLE_MAX_CHARS: usize = 1_000;
pub const BODY_MAX_CHARS: usize = 100_000;
pub const COMMENT_MAX_CHARS: usize = 20_000;
pub const ANSWER_MAX_CHARS: usize = 20_000;
/// A steer is one line typed into an agent's pane.
pub const STEER_MAX_CHARS: usize = 20_000;
/// An ask is noted whole to the orchestrator, and again at every
/// orchestrator start until it is resolved. It is counted without the space
/// around it.
pub const ASK_NOTE_MAX_CHARS: usize = 8_000;

/// A usage error when `text` runs past `max` characters. `what` names the
/// field as the message says it: "title is at most 1000 characters".
pub fn within(what: &str, text: &str, max: usize) -> Result<()> {
    if text.chars().count() > max {
        return Err(Error::Usage(format!("{what} is at most {max} characters")));
    }
    Ok(())
}

/// A card's title and body, each when present.
pub fn card_text(title: Option<&str>, body: Option<&str>) -> Result<()> {
    within("title", title.unwrap_or_default(), TITLE_MAX_CHARS)?;
    within("body", body.unwrap_or_default(), BODY_MAX_CHARS)
}

/// An ask, counted without the space around it.
pub fn ask(text: &str) -> Result<()> {
    within("an ask", text.trim(), ASK_NOTE_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn counts_characters_not_bytes() {
        within("title", &"é".repeat(TITLE_MAX_CHARS), TITLE_MAX_CHARS).unwrap();
        let error = within("title", &"a".repeat(TITLE_MAX_CHARS + 1), TITLE_MAX_CHARS)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "title is at most 1000 characters");
    }

    #[test]
    fn an_ask_counts_without_the_space_around_it() {
        ask(&format!("  {}\n", "a".repeat(ASK_NOTE_MAX_CHARS))).unwrap();
        let error = ask(&"a".repeat(ASK_NOTE_MAX_CHARS + 1))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "an ask is at most 8000 characters");
    }
}
