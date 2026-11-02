//! Reads a Claude Code pane's composer from its visible viewport, so delivery
//! types only into a composer it has proved empty (`protocol.md` 3.3).
//!
//! The rule is Firstmate's herdr backend's: `empty` is the one verdict that
//! lets anything be typed, so every doubt resolves away from it. Claude Code
//! 2.x draws its composer as a full-width `─` rule, a row starting with `❯`
//! and a no-break space, and another `─` rule, with only its footer below. A
//! bare shell prompt (`❯ ` with a plain space, no rules) is never a composer:
//! it means the agent is gone. Text Claude de-emphasizes (dim placeholder and
//! suggestion text) is not content.

/// What the viewport shows where the composer should be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Composer {
    /// A Claude composer holding nothing, with the agent not mid-turn.
    Empty,
    /// A Claude composer, empty, while the footer says a turn is running.
    Busy,
    /// A Claude composer holding text: the owner's draft, or an earlier
    /// wake-up line that was typed but not submitted.
    Holds(String),
    /// A permission, trust or other choice dialog in place of the composer.
    Dialog,
    /// Anything else: a shell, a half-drawn screen, an unknown shape.
    Unknown,
}

const PROMPT: char = '❯';
const NBSP: char = '\u{a0}';
/// Rows Claude draws below its composer: the mode line and a short hint row.
const MAX_FOOTER_ROWS: usize = 4;

/// Classifies a viewport read with `pane read --source visible --format ansi`.
pub fn classify(ansi: &str) -> Composer {
    let rows: Vec<String> = emphasized_rows(ansi);
    let rules: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| is_rule(row))
        .map(|(i, _)| i)
        .collect();

    if let [.., top, bottom] = rules.as_slice()
        && bottom - top >= 2
        && let Some(first) = rows[top + 1].strip_prefix(PROMPT)
        && (first.is_empty() || first.starts_with(NBSP))
    {
        let footer: Vec<&String> = rows[bottom + 1..]
            .iter()
            .filter(|row| !row.trim().is_empty())
            .collect();
        if footer.len() > MAX_FOOTER_ROWS || footer.iter().any(|row| row.starts_with(PROMPT)) {
            // Rows below the composer that Claude does not draw: a shell
            // printed after Claude exited, or a popup. Not provably empty.
            return match composer_text(&rows[top + 1..*bottom]) {
                text if text.is_empty() => Composer::Unknown,
                text => Composer::Holds(text),
            };
        }
        let text = composer_text(&rows[top + 1..*bottom]);
        if !text.is_empty() {
            return Composer::Holds(text);
        }
        if footer
            .iter()
            .any(|row| row.to_lowercase().contains("esc to interrupt"))
        {
            return Composer::Busy;
        }
        return Composer::Empty;
    }

    let bottom = &rows[rows.len().saturating_sub(16)..];
    if bottom.iter().any(|row| is_dialog_row(row)) {
        Composer::Dialog
    } else {
        Composer::Unknown
    }
}

fn is_rule(row: &str) -> bool {
    let row = row.trim_end();
    row.chars().count() >= 20 && row.chars().all(|c| c == '─')
}

fn is_dialog_row(row: &str) -> bool {
    let trimmed = row.trim();
    if trimmed.contains("Esc to cancel") || trimmed.contains("Do you want to proceed?") {
        return true;
    }
    // A selector: `❯ 1. Yes`, indented under a dialog's title.
    row.starts_with(' ')
        && trimmed
            .strip_prefix(PROMPT)
            .map(str::trim_start)
            .and_then(|rest| rest.split_once(". "))
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The composer's text: the first row after the glyph, then any wrapped rows.
fn composer_text(rows: &[String]) -> String {
    let mut lines = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let row = if i == 0 {
            row.strip_prefix(PROMPT).unwrap_or(row)
        } else {
            row.as_str()
        };
        let line = row.replace(NBSP, " ");
        let line = line.trim();
        if !line.is_empty() {
            lines.push(line.to_owned());
        }
    }
    lines.join("\n")
}

/// The viewport as plain rows, keeping only text that is not de-emphasized:
/// dim (SGR 2) and dark foregrounds are how Claude draws placeholders.
fn emphasized_rows(ansi: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut dim = false;
    let mut dark = false;
    let mut chars = ansi.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.peek() != Some(&'[') {
                    chars.next();
                    continue;
                }
                chars.next();
                let mut params = String::new();
                let mut end = None;
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        end = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if end == Some('m') {
                    apply_sgr(&params, &mut dim, &mut dark);
                }
            }
            '\n' => rows.push(std::mem::take(&mut row)),
            '\r' => {}
            c if dim || dark => {
                // Keep the cell's width so columns still line up.
                if !c.is_control() {
                    row.push(' ');
                }
            }
            c => row.push(c),
        }
    }
    rows.push(row);
    rows.into_iter()
        .map(|r| r.trim_end_matches([' ', '\t']).to_owned())
        .collect()
}

fn apply_sgr(params: &str, dim: &mut bool, dark: &mut bool) {
    let codes: Vec<&str> = if params.is_empty() {
        vec!["0"]
    } else {
        params.split(';').collect()
    };
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            "0" => {
                *dim = false;
                *dark = false;
            }
            "2" => *dim = true,
            "22" => *dim = false,
            "39" => *dark = false,
            "30" | "90" => *dark = true,
            "38" => match codes.get(i + 1) {
                Some(&"5") => {
                    let n: u16 = codes.get(i + 2).and_then(|n| n.parse().ok()).unwrap_or(7);
                    *dark = matches!(n, 0 | 8 | 232..=243);
                    i += 2;
                }
                Some(&"2") => {
                    let channel = |k: usize| -> u16 {
                        codes.get(i + k).and_then(|n| n.parse().ok()).unwrap_or(255)
                    };
                    *dark = channel(2).max(channel(3)).max(channel(4)) < 100;
                    i += 4;
                }
                _ => {}
            },
            "48" => {
                // Background colours carry no emphasis; skip their arguments.
                match codes.get(i + 1) {
                    Some(&"5") => i += 2,
                    Some(&"2") => i += 4,
                    _ => {}
                }
            }
            c if c.len() == 2 && (c.starts_with('3') || c.starts_with('9')) => *dark = false,
            _ => {}
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Composer {
        let path = format!(
            "{}/tests/fixtures/claude/{name}.ansi",
            env!("CARGO_MANIFEST_DIR")
        );
        classify(&std::fs::read_to_string(path).unwrap())
    }

    // Each fixture is a real Claude Code 2.1.312 viewport read through herdr
    // 0.9.5 in an isolated lab session, with paths and names scrubbed.

    #[test]
    fn idle_claude_composer_is_empty() {
        assert_eq!(fixture("empty"), Composer::Empty);
        assert_eq!(fixture("empty-manual-mode"), Composer::Empty);
    }

    #[test]
    fn mid_turn_composer_is_busy_not_empty() {
        assert_eq!(fixture("working"), Composer::Busy);
    }

    #[test]
    fn draft_and_unsubmitted_wake_line_are_held_text() {
        assert_eq!(fixture("draft"), Composer::Holds("half typed draft".into()));
        assert_eq!(
            fixture("wake-line"),
            Composer::Holds("tiller inbox 01JC2ZB6N0E2K4M6P8R0S2T4V6 # steer alpha-42".into())
        );
    }

    #[test]
    fn permission_and_trust_prompts_are_dialogs() {
        assert_eq!(fixture("permission-dialog"), Composer::Dialog);
        assert_eq!(fixture("trust-dialog"), Composer::Dialog);
    }

    #[test]
    fn shell_after_claude_exits_is_never_empty() {
        assert_ne!(fixture("exited"), Composer::Empty);
        assert_ne!(fixture("exited-after-popup"), Composer::Empty);
        // Claude's last frame left an empty composer on screen, and the shell
        // prompt sits below it.
        let rule = "─".repeat(40);
        let screen = format!(
            "{rule}\n❯\u{a0}\n{rule}\n  ? for shortcuts\n\nResume this session with:\nclaude --resume 0\nhost ~ \n❯ \n"
        );
        assert_eq!(classify(&screen), Composer::Unknown);
    }

    #[test]
    fn bare_shell_prompt_with_the_same_glyph_is_unknown() {
        let screen = "host ~ \n\u{1b}[0m\u{1b}[38;5;5m❯\u{1b}[0m \n";
        assert_eq!(classify(screen), Composer::Unknown);
    }

    #[test]
    fn dim_placeholder_is_not_content() {
        let rule = "─".repeat(40);
        let screen = format!(
            "{rule}\n❯\u{a0}\u{1b}[2mTry \"fix the tests\"\u{1b}[0m\n{rule}\n  ? for shortcuts\n"
        );
        assert_eq!(classify(&screen), Composer::Empty);
        let screen = format!("{rule}\n❯\u{a0}typed\u{1b}[2m suggestion\u{1b}[0m\n{rule}\n");
        assert_eq!(classify(&screen), Composer::Holds("typed".into()));
    }

    #[test]
    fn wrapped_draft_keeps_every_row() {
        let rule = "─".repeat(40);
        let screen = format!("{rule}\n❯\u{a0}first part of a\n  long draft\n{rule}\n");
        assert_eq!(
            classify(&screen),
            Composer::Holds("first part of a\nlong draft".into())
        );
    }
}
