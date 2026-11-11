//! The dialog on an agent's screen, read for the board's answer buttons
//! (`protocol.md` 7.4). Only the layouts below parse: Claude Code's current
//! permission dialog and its folder trust dialog. Any other layout is no
//! dialog, never a guess.

use sha2::{Digest, Sha256};

use crate::api::pane_tail;
use crate::auth::hex;
use crate::dto::{PromptDialog, PromptLayout, PromptOption};

/// What a waiting agent waits on, as its session row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// A permission prompt, for the tool its hook named (empty when no
    /// observation named one).
    Permission(String),
    /// An answer no hook asked for: a dialog only the screen shows.
    Input,
}

/// A dialog read off the screen, with the option its cursor is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    pub prompt: PromptDialog,
    /// The option the `❯` cursor is on; a cursor menu moves from it.
    cursor: u8,
}

impl Dialog {
    /// The keys that pick `option`, or None when the dialog does not offer
    /// it. A permission dialog takes the digit alone; a cursor menu takes
    /// the arrow keys from the cursor to the option's row, then Enter.
    pub fn keys(&self, option: u8) -> Option<Vec<String>> {
        if !self
            .prompt
            .options
            .iter()
            .any(|offered| offered.n == option)
        {
            return None;
        }
        Some(match self.prompt.layout {
            PromptLayout::Permission => vec![option.to_string()],
            PromptLayout::Trust => {
                let arrow = if option > self.cursor { "down" } else { "up" };
                let mut keys = vec![arrow.to_owned(); usize::from(option.abs_diff(self.cursor))];
                keys.push("enter".to_owned());
                keys
            }
        })
    }
}

/// The dialog on `screen` that `wait` can be waiting on: a permission
/// dialog for the hook's tool, or, for an answer no hook asked for, a
/// cursor menu of a known layout.
pub fn read(screen: &str, wait: &Wait) -> Option<Dialog> {
    match wait {
        Wait::Permission(tool) => parse(screen, tool).map(|prompt| Dialog { prompt, cursor: 0 }),
        Wait::Input => trust(screen),
    }
}

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
        layout: PromptLayout::Permission,
        tool: tool.to_owned(),
        title: title.to_owned(),
        command,
        folder: None,
        question: question.to_owned(),
        options,
        screen: fingerprint(dialog),
    })
}

const TRUST_TITLE: &str = "Accessing workspace:";
const TRUST_FOOTER: &str = "Enter to confirm · Esc to cancel";
const TRUST_ASKS: &str = "Quick safety check: ";
const TRUST_OPTIONS: [&str; 2] = ["No, exit", "Yes, I trust this folder"];

/// Claude Code's folder trust dialog at the bottom of `screen`: the rows
/// from the last full-width `─` rule to the `Enter to confirm · Esc to
/// cancel` footer on the screen's last row. Its title is `Accessing
/// workspace:`, the folder is the row after the blank one under it, a row
/// asks the `Quick safety check:` question, and the menu is the two rows
/// above the blank row over the footer, `No, exit` then `Yes, I trust this
/// folder`, one of them under the `❯` cursor. Anything else is `None`.
fn trust(screen: &str) -> Option<Dialog> {
    let rows = pane_tail(screen, usize::MAX);
    if rows.last()?.trim() != TRUST_FOOTER {
        return None;
    }
    let top = rows.iter().rposition(|row| is_rule(row, '─'))?;
    let dialog = &rows[top..];
    if dialog.get(1)?.trim() != TRUST_TITLE || !dialog.get(2)?.is_empty() {
        return None;
    }
    let folder = dialog.get(3)?.trim();
    if !folder.starts_with('/') && !folder.starts_with('~') {
        return None;
    }
    let question = dialog
        .iter()
        .find_map(|row| row.trim().strip_prefix(TRUST_ASKS))
        .and_then(|rest| rest.find('?').map(|end| &rest[..=end]))?;
    let footer = dialog.len() - 1;
    let gap = footer
        .checked_sub(1)
        .filter(|&gap| dialog[gap].is_empty())?;
    let start = dialog[..gap].iter().rposition(String::is_empty)? + 1;
    let menu = &dialog[start..gap];
    if start <= 3 || menu.len() != TRUST_OPTIONS.len() {
        return None;
    }
    let mut cursor = None;
    for (index, (row, text)) in menu.iter().zip(TRUST_OPTIONS).enumerate() {
        if row.strip_prefix(" ❯ ") == Some(text) {
            if cursor.replace(index).is_some() {
                return None;
            }
        } else if row.strip_prefix("   ") != Some(text) {
            return None;
        }
    }
    let cursor = u8::try_from(cursor? + 1).ok()?;
    let options = TRUST_OPTIONS
        .iter()
        .zip(1u8..)
        .map(|(text, n)| PromptOption {
            n,
            text: (*text).to_owned(),
        })
        .collect();
    Some(Dialog {
        prompt: PromptDialog {
            layout: PromptLayout::Trust,
            tool: String::new(),
            title: TRUST_TITLE.to_owned(),
            command: None,
            folder: Some(folder.to_owned()),
            question: question.to_owned(),
            options,
            screen: fingerprint(dialog),
        },
        cursor,
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
        // The trust dialog is a cursor menu, never a permission dialog.
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

    const TRUST: &str = include_str!("../../herdr/tests/fixtures/claude/trust-dialog.ansi");

    /// The trust dialog without its colors, to change a row of it.
    fn trust_rows() -> String {
        pane_tail(TRUST, usize::MAX).join("\n")
    }

    #[test]
    fn claude_codes_trust_dialog_reads_as_a_known_cursor_menu() {
        let dialog = read(TRUST, &Wait::Input).unwrap();
        insta::assert_json_snapshot!(dialog.prompt);
        // The cursor starts on No, exit: Yes is one row down.
        assert_eq!(dialog.keys(1), Some(vec!["enter".to_owned()]));
        assert_eq!(
            dialog.keys(2),
            Some(vec!["down".to_owned(), "enter".to_owned()])
        );
        assert_eq!(dialog.keys(3), None);
        assert_eq!(dialog.keys(0), None);
    }

    #[test]
    fn the_trust_keys_start_from_where_the_cursor_is() {
        let moved = trust_rows()
            .replacen(" ❯ No, exit", "   No, exit", 1)
            .replacen(
                "   Yes, I trust this folder",
                " ❯ Yes, I trust this folder",
                1,
            );
        let dialog = read(&moved, &Wait::Input).unwrap();
        assert_eq!(
            dialog.keys(1),
            Some(vec!["up".to_owned(), "enter".to_owned()])
        );
        assert_eq!(dialog.keys(2), Some(vec!["enter".to_owned()]));
        // The cursor is not part of the dialog's hash.
        let shown = read(TRUST, &Wait::Input).unwrap();
        assert_eq!(dialog.prompt.screen, shown.prompt.screen);
    }

    #[test]
    fn a_permission_wait_never_reads_the_trust_dialog_and_an_input_wait_never_reads_a_permission_one()
     {
        assert_eq!(read(TRUST, &Wait::Permission("Bash".to_owned())), None);
        assert_eq!(read(TRUST, &Wait::Permission(String::new())), None);
        assert_eq!(read(BASH, &Wait::Input), None);
        let bash = read(BASH, &Wait::Permission("Bash".to_owned())).unwrap();
        assert_eq!(bash.keys(4), Some(vec!["4".to_owned()]));
        assert_eq!(bash.keys(5), None);
    }

    #[test]
    fn a_cursor_menu_of_an_unknown_layout_is_none_never_a_guess() {
        let trust = trust_rows();
        let changed = [
            // Another title: a menu the daemon does not know.
            trust.replace("Accessing workspace:", "Select a model:"),
            // Options in another order, or with other words.
            trust
                .replace(" ❯ No, exit", " ❯ Yes, I trust this folder")
                .replacen("   Yes, I trust this folder", "   No, exit", 1),
            trust.replace("Yes, I trust this folder", "Yes, proceed"),
            // A third option.
            trust.replace(
                "   Yes, I trust this folder",
                "   Yes, I trust this folder\n   Yes, always",
            ),
            // No cursor, or two.
            trust.replace(" ❯ No, exit", "   No, exit"),
            trust.replace("   Yes, I trust this folder", " ❯ Yes, I trust this folder"),
            // No folder under the title.
            trust.replace(" /home/user/project\n", " project\n"),
            // No question.
            trust.replace("Quick safety check: ", "Note: "),
            // Another footer, or one that is not the last row.
            trust.replace("Enter to confirm", "Space to confirm"),
            format!("{trust}\nmore"),
            // A digit on each row: the numbered layout, not this one.
            trust
                .replace(" ❯ No, exit", " ❯ 1. No, exit")
                .replace("   Yes, I trust", "   2. Yes, I trust"),
        ];
        for screen in changed {
            assert_eq!(read(&screen, &Wait::Input), None, "{screen}");
        }
    }

    #[test]
    fn a_trust_dialog_drawn_in_the_output_above_the_real_one_is_not_read() {
        let trust = trust_rows();
        // The real one at the bottom asks about another folder.
        let below = trust.replace("/home/user/project\n", "/home/user/other\n");
        let dialog = read(&format!("{trust}\n● Done.\n{below}"), &Wait::Input).unwrap();
        assert_eq!(dialog.prompt.folder.as_deref(), Some("/home/user/other"));
        // Output under it means it is not the dialog the pane waits on.
        assert_eq!(read(&format!("{trust}\n● Done.\n"), &Wait::Input), None);
    }
}
