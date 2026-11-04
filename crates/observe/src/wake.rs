//! The wake-up line grammar (`protocol.md` section 3.1), which both the
//! `UserPromptSubmit` hook and the transcript tailer recognize.

/// The outbox id when `prompt` is exactly a wake-up line:
/// `^tiller inbox [0-9A-HJKMNP-TV-Z]{26}( # (wake|steer|answer|note)( [a-z][a-z0-9]*(-[a-z][a-z0-9]*)*-[0-9]+)?)?$`
/// on the prompt with surrounding whitespace trimmed, the project part of the
/// display id also holding [`tiller_core::ids::validate_project_id`].
pub fn outbox_id(prompt: &str) -> Option<&str> {
    let rest = prompt.trim().strip_prefix("tiller inbox ")?;
    let (id, comment) = match rest.split_once(' ') {
        Some((id, comment)) => (id, Some(comment)),
        None => (rest, None),
    };
    if !is_ulid(id) {
        return None;
    }
    match comment {
        None => Some(id),
        Some(comment) => valid_comment(comment).then_some(id),
    }
}

fn is_ulid(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)))
}

fn valid_comment(comment: &str) -> bool {
    let Some(rest) = comment.strip_prefix("# ") else {
        return false;
    };
    let (kind, display) = match rest.split_once(' ') {
        Some((kind, display)) => (kind, Some(display)),
        None => (rest, None),
    };
    if !matches!(kind, "wake" | "steer" | "answer" | "note") {
        return false;
    }
    display.is_none_or(valid_display_id)
}

fn valid_display_id(display: &str) -> bool {
    let Some((project, number)) = display.rsplit_once('-') else {
        return false;
    };
    tiller_core::ids::validate_project_id(project).is_ok()
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::outbox_id;

    const ID: &str = "01JC2ZB6N0E2K4M6P8R0S2T4V6";

    #[test]
    fn accepts_every_form_of_the_grammar() {
        for line in [
            format!("tiller inbox {ID}"),
            format!("  tiller inbox {ID} # wake\n"),
            format!("tiller inbox {ID} # steer alpha-42"),
            format!("tiller inbox {ID} # note a1-7"),
            format!("tiller inbox {ID} # steer acme-web2-staging-12"),
        ] {
            assert_eq!(outbox_id(&line), Some(ID), "{line:?}");
        }
    }

    #[test]
    fn rejects_near_misses() {
        for line in [
            format!("tiller inbox {}", &ID[..25]),
            format!("tiller inbox {}", ID.replace('B', "I")),
            format!("tiller inbox {} ", ID.to_lowercase()),
            format!("tiller inbox {ID} # ping"),
            format!("tiller inbox {ID} # steer Alpha-42"),
            format!("tiller inbox {ID} # steer a-42"),
            format!("tiller inbox {ID} # steer alpha-"),
            format!("tiller inbox {ID} # steer alpha--42"),
            format!("tiller inbox {ID} # steer alpha-2-42"),
            format!("tiller inbox {ID} # steer web2-staging-"),
            format!("tiller inbox {ID} please"),
            format!("please run tiller inbox {ID}"),
            format!("tiller inbox {ID}\nand also this"),
        ] {
            assert_eq!(outbox_id(&line), None, "{line:?}");
        }
    }
}
