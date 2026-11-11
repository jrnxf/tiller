//! The longest text Tiller takes for a card title, a card body, a comment,
//! an answer and an ask (`protocol.md` 7.4, "Length caps"), in characters
//! (Unicode scalar values, not bytes). The daemon API and the CLI both check
//! them through [`within`], so the two paths cannot drift.
//! `board/src/lib/limits.ts` mirrors the numbers, and its test reads this
//! file to keep them equal.

use crate::{Error, Result};

pub const TITLE_MAX_CHARS: usize = 1_000;
pub const BODY_MAX_CHARS: usize = 100_000;
pub const COMMENT_MAX_CHARS: usize = 20_000;
pub const ANSWER_MAX_CHARS: usize = 20_000;
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
}
