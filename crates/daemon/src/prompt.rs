//! The permission dialog on an agent's screen, read for the board's answer
//! buttons (`protocol.md` 7.4). Only Claude Code's current dialog parses;
//! any other layout is no dialog, never a guess.

use sha2::{Digest, Sha256};

use crate::api::pane_tail;
use crate::auth::hex;
use crate::dto::{PromptDialog, PromptOption};

/// The dialog at the bottom of `screen` (a pane read, ANSI or plain), when
/// it asks about `tool`, the tool the session's hook named.
///
/// The dialog is the rows from the last full-width `─` rule to the
/// `Esc to cancel` footer on the screen's last row: its title is one
/// Claude Code gives `tool`'s dialog, its question ends in `?` and sits on the row above
/// option 1, and its options run 1, 2, 3 with nothing between them but a
/// wrapped option's indented rows. Anything else is `None`.
pub fn parse(screen: &str, tool: &str) -> Option<PromptDialog> {
    let rows = pane_tail(screen, usize::MAX);
    if !rows.last()?.trim_start().starts_with("Esc to cancel") {
        return None;
    }
    let top = rows.iter().rposition(|row| is_rule(row, '─'))?;
    let dialog = &rows[top..];
    let title = dialog.get(1)?.trim();
    if !titles(tool).contains(&title) {
        return None;
    }
    let first = dialog
        .iter()
        .position(|row| option(row).is_some_and(|(n, _)| n == 1))?;
    let question = dialog.get(first.checked_sub(1)?)?.trim();
    if first < 3 || !question.ends_with('?') {
        return None;
    }
    let mut options: Vec<PromptOption> = Vec::new();
    let mut end = first;
    for row in &dialog[first..] {
        if row.is_empty() {
            break;
        }
        if let Some((n, text)) = option(row) {
            if usize::from(n) != options.len() + 1 {
                return None;
            }
            options.push(PromptOption {
                n,
                text: text.to_owned(),
            });
        } else {
            // A wrapped option's next row.
            let last = options.last_mut().filter(|_| row.starts_with("      "))?;
            last.text.push(' ');
            last.text.push_str(row.trim());
        }
        end += 1;
    }
    // Below the options: blank rows, then the footer.
    let footer = dialog.len() - 1;
    if options.len() < 2 || end == footer || dialog[end..footer].iter().any(|r| !r.is_empty()) {
        return None;
    }
    let command = if tool == "Bash" {
        let dashes: Vec<usize> = (0..first).filter(|&i| is_rule(&dialog[i], '╌')).collect();
        let [open, close] = dashes[..] else {
            return None;
        };
        let body: Vec<&str> = dialog[open + 1..close]
            .iter()
            .map(|row| row.strip_prefix(' ').unwrap_or(row))
            .collect();
        Some(body.join("\n"))
    } else {
        None
    };
    Some(PromptDialog {
        tool: tool.to_owned(),
        title: title.to_owned(),
        command,
        question: question.to_owned(),
        options,
        screen: fingerprint(dialog),
    })
}

/// The titles Claude Code gives the permission dialog for `tool`, as its
/// own permission components name them. A tool not listed has no dialog
/// here: its layout is unknown.
fn titles(tool: &str) -> &'static [&'static str] {
    match tool {
        "Bash" => &["Bash command"],
        "Edit" => &["Edit file"],
        "Write" => &["Create file", "Overwrite file", "Write file"],
        "NotebookEdit" => &["Edit notebook"],
        "WebFetch" => &["Fetch"],
        _ if tool.starts_with("mcp__") => &["Tool use"],
        _ => &[],
    }
}

/// The dialog's rows hashed, with the selection cursor left out: an answer
/// is checked against it, so a dialog that changed after the buttons were
/// drawn is refused, while a clock in a status row outside it is not.
fn fingerprint(dialog: &[String]) -> String {
    let text = dialog.join("\n").replace('❯', " ");
    hex(&Sha256::digest(text.as_bytes()))
}

fn is_rule(row: &str, glyph: char) -> bool {
    row.chars().count() >= 20 && row.chars().all(|c| c == glyph)
}

/// An option row, `   2. text`, or ` ❯ 1. text` with the cursor on it.
fn option(row: &str) -> Option<(u8, &str)> {
    let rest = row
        .strip_prefix(" ❯ ")
        .or_else(|| row.strip_prefix("   "))?;
    let mut chars = rest.chars();
    let n = chars.next()?.to_digit(10).filter(|&n| n > 0)?;
    let text = chars.as_str().strip_prefix(". ")?;
    Some((u8::try_from(n).ok()?, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH: &str = include_str!("../../herdr/tests/fixtures/claude/permission-dialog.ansi");
    const EDIT: &str = include_str!("../tests/fixtures/prompt/edit.txt");

    /// The Bash dialog without its colors, to change a row of it.
    fn bash() -> String {
        pane_tail(BASH, usize::MAX).join("\n")
    }

    #[test]
    fn claude_codes_bash_dialog_parses_with_its_command() {
        insta::assert_json_snapshot!(parse(BASH, "Bash"));
    }

    #[test]
    fn claude_codes_edit_dialog_parses_without_a_command() {
        insta::assert_json_snapshot!(parse(EDIT, "Edit"));
    }

    #[test]
    fn a_wrapped_option_is_one_option() {
        let screen = include_str!("../tests/fixtures/prompt/bash-wrapped.txt");
        let dialog = parse(screen, "Bash").unwrap();
        assert_eq!(dialog.command.as_deref(), Some("rm -rf build/cache"));
        assert_eq!(dialog.options.len(), 4);
        assert_eq!(
            dialog.options[1].text,
            "Yes, and always allow access to /home/user/project/build/cache from this project"
        );
    }

    #[test]
    fn a_dialog_for_another_tool_than_the_hook_named_is_none() {
        assert_eq!(parse(BASH, "Edit"), None);
        assert_eq!(parse(EDIT, "Bash"), None);
        assert_eq!(parse(EDIT, "Unknown"), None);
        let created = EDIT.replace(" Edit file", " Create file");
        assert_eq!(parse(&created, "Edit"), None);
        assert!(parse(&created, "Write").is_some());
    }

    #[test]
    fn a_screen_without_the_dialog_at_its_bottom_is_none() {
        for name in ["working", "trust-dialog", "draft", "exited-after-popup"] {
            let screen = std::fs::read_to_string(format!(
                "{}/../herdr/tests/fixtures/claude/{name}.ansi",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            assert_eq!(parse(&screen, "Bash"), None, "{name}");
        }
        // The dialog scrolled up, with output below it.
        assert_eq!(parse(&format!("{BASH}\n● Done.\n"), "Bash"), None);
    }

    #[test]
    fn an_unknown_layout_is_none_never_a_guess() {
        let bash = bash();
        let changed = [
            // Options out of order.
            bash.replace("   2. Yes", "   5. Yes"),
            // A row inside the options that is neither an option nor a wrap.
            bash.replace("   4. No", "   4. No\nAlso this"),
            // No question above option 1.
            bash.replace("Do you want to proceed?", "Proceed"),
            // A third dashed rule: the command cannot be told apart.
            bash.replace(" rm -f nothing", "╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n rm -f nothing"),
            // A footer that is not the last row.
            bash.replace("Tab to amend", "Tab to amend\nmore"),
        ];
        for screen in changed {
            assert_eq!(parse(&screen, "Bash"), None, "{screen}");
        }
    }

    #[test]
    fn a_dialog_drawn_in_the_output_above_the_real_one_is_not_read() {
        let fake = "────────────────────────────────────────\n Bash command\n\
                    ╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n echo fake\n╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌\n \
                    Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel\n";
        let dialog = parse(&format!("{fake}{BASH}"), "Bash").unwrap();
        assert_eq!(
            dialog.command.as_deref(),
            Some("rm -f nothing-here.txt && touch made.txt")
        );
    }

    #[test]
    fn the_fingerprint_ignores_the_cursor_and_rows_above_the_dialog() {
        let bash = bash();
        let screen = parse(BASH, "Bash").unwrap().screen;
        assert_eq!(parse(&bash, "Bash").unwrap().screen, screen);
        let moved = bash
            .replacen(" ❯ 1. Yes", "   1. Yes", 1)
            .replacen("   4. No", " ❯ 4. No", 1);
        assert_eq!(parse(&moved, "Bash").unwrap().screen, screen);
        let above = bash.replace(
            "Your login expires in 1 day",
            "Your login expires in 2 days",
        );
        assert_eq!(parse(&above, "Bash").unwrap().screen, screen);
        let changed = bash.replace("touch made.txt\n", "touch other.txt\n");
        assert_ne!(parse(&changed, "Bash").unwrap().screen, screen);
    }
}
