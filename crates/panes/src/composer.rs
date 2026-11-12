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
    /// A Claude composer, empty, while a turn is running: the footer says
    /// "esc to interrupt" (older Claude Code), or the spinner row is drawn
    /// above the composer (current Claude Code).
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
            // printed after Claude exited, or a popup. Neither empty nor a
            // line Enter would submit to Claude.
            return Composer::Unknown;
        }
        let text = composer_text(&rows[top + 1..*bottom]);
        if !text.is_empty() {
            return Composer::Holds(text);
        }
        if footer
            .iter()
            .any(|row| row.to_lowercase().contains("esc to interrupt"))
            || spinner_above(&rows[..*top])
        {
            return Composer::Busy;
        }
        return Composer::Empty;
    }

    // Claude draws a short dialog under the last reply, not at the screen's
    // bottom, so count the 16 rows from the last row that holds anything.
    let end = rows
        .iter()
        .rposition(|row| !row.trim().is_empty())
        .map_or(0, |i| i + 1);
    let bottom = &rows[end.saturating_sub(16)..end];
    if bottom.iter().any(|row| is_dialog_row(row)) {
        Composer::Dialog
    } else {
        Composer::Unknown
    }
}

/// Glyphs Claude cycles through at the start of its spinner row.
const SPINNER_GLYPHS: [char; 7] = ['*', '·', '✢', '✳', '✶', '✻', '✽'];
/// Most rows above the composer's top rule that the spinner scan walks. A
/// task list adds up to six rows (five items and a `+N` row), a queued
/// message two, and a blank row and a tip row sit there too.
const SPINNER_REACH: usize = 12;

/// Whether the turn is running: the nearest flush-left row above the
/// composer's top rule is the spinner row. Blank rows and indented rows (a
/// task list, a tool-progress row, a tip) are walked over. Any other row, such
/// as `● reply text` or a finished turn's `✻ Cooked for 2s · done` row, ends the
/// turn above it, so a spinner further up is scrollback.
fn spinner_above(rows: &[String]) -> bool {
    rows.iter()
        .rev()
        .take(SPINNER_REACH)
        .find(|row| !row.trim().is_empty() && !row.starts_with(char::is_whitespace))
        .is_some_and(|row| is_spinner(row))
}

/// `<glyph> <Word>…`, optionally followed by ` (<n>s · ...)`, flush left. A
/// finished turn's `✻ Cooked for 2s · done 6:01 PM` has no `…`, and reply text
/// is indented, so neither matches.
fn is_spinner(row: &str) -> bool {
    let mut chars = row.chars();
    let (Some(glyph), Some(' ')) = (chars.next(), chars.next()) else {
        return false;
    };
    if !SPINNER_GLYPHS.contains(&glyph) {
        return false;
    }
    let rest = chars.as_str();
    let Some((word, tail)) = rest.split_once('…') else {
        return false;
    };
    word.starts_with(|c: char| c.is_ascii_uppercase())
        && word
            .chars()
            .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
        && (tail.is_empty() || tail.starts_with(" ("))
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
        // The herdr crate keeps the frozen snapshots of these fixtures.
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
        // A wake-up line left in that frame is not held text: Enter would go
        // to the shell, which would run it.
        let screen = format!(
            "{rule}\n❯\u{a0}tiller inbox 01JC2ZB6N0E2K4M6P8R0S2T4V6 # steer\n{rule}\n  ? for shortcuts\n\nhost ~ \n❯ \n"
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

    #[test]
    fn spinner_row_above_an_empty_composer_is_busy() {
        let rule = "─".repeat(40);
        for spinner in [
            "✻ Examining…",
            "· Examining… (3s · ↓ 120 tokens)",
            "* Fiddle-faddling… (running Stop hooks… 1/2 · 2s)",
            "✽ Examining… (4s · ↓ 9 tokens · thinking)",
            // Verbs with an accent or an apostrophe.
            "✶ Sautéing… (2s)",
            "✻ Jiggin'…",
        ] {
            let screen = format!("{spinner}\n\n{rule}\n❯\u{a0}\n{rule}\n  ? for shortcuts\n");
            assert_eq!(classify(&screen), Composer::Busy, "{spinner}");
        }
        // A tool-progress row or a tip may sit between spinner and composer.
        let screen =
            format!("✢ Examining… (2s)\n  ⎿  Tip: invented tip\n\n{rule}\n❯\u{a0}\n{rule}\n");
        assert_eq!(classify(&screen), Composer::Busy);
    }

    #[test]
    fn typed_text_beats_the_spinner() {
        let rule = "─".repeat(40);
        let screen = format!("✻ Examining… (2s)\n\n{rule}\n❯\u{a0}next idea\n{rule}\n");
        assert_eq!(classify(&screen), Composer::Holds("next idea".into()));
    }

    #[test]
    fn finished_reply_that_looks_like_a_spinner_stays_empty() {
        let rule = "─".repeat(40);
        for last in [
            // The finished-turn row.
            "✻ Cooked for 2s · done 6:01 PM",
            // Reply text is indented, whatever it says.
            "  * Waiting… (3s later it returned)",
            "● Waiting… (3s later it returned)",
            "  ✻ Examining… (2s)",
            // No glyph, or an ordinary sentence.
            "Examining… (2s)",
            "✻ the file ends with a dot… (3s)",
            "✻ Examining… and then more text",
        ] {
            let screen =
                format!("● It works.\n{last}\n\n{rule}\n❯\u{a0}\n{rule}\n  ? for shortcuts\n");
            assert_eq!(classify(&screen), Composer::Empty, "{last}");
        }
    }

    #[test]
    fn spinner_far_above_the_composer_is_scrollback() {
        let rule = "─".repeat(40);
        let screen = format!("✻ Examining… (2s)\n● a\n● b\n● c\n● d\n\n{rule}\n❯\u{a0}\n{rule}\n");
        assert_eq!(classify(&screen), Composer::Empty);
    }

    #[test]
    fn spinner_over_a_task_list_is_busy() {
        let rule = "─".repeat(40);
        let screen = format!(
            "✶ Planning… (6s · ↓ 210 tokens)\n  ⎿  ◻ step one\n     ◻ step two\n     ◻ step three\n     ◻ step four\n     ◻ step five\n      … +1 pending\n\n{rule}\n❯\u{a0}\n{rule}\n  ? for shortcuts\n"
        );
        assert_eq!(classify(&screen), Composer::Busy);
    }

    #[test]
    fn spinner_under_a_queued_message_is_busy() {
        let rule = "─".repeat(40);
        let screen = format!(
            "❯ After that, say the word queued.\n  ctrl+enter to send now\n\n✻ Waiting… (9s)\n\n{rule}\n❯\u{a0}\n{rule}\n"
        );
        assert_eq!(classify(&screen), Composer::Busy);
    }

    #[test]
    fn finished_turn_with_a_task_summary_stays_empty() {
        let rule = "─".repeat(40);
        let screen = format!(
            "● All done.\n\n✻ Cooked for 9s · done 6:01 PM\n  6 tasks (6 done, 0 open)\n  ✔ step one\n  ✔ step two\n\n{rule}\n❯\u{a0}\n{rule}\n  ? for shortcuts\n"
        );
        assert_eq!(classify(&screen), Composer::Empty);
    }

    #[test]
    fn spinner_beyond_the_reach_is_scrollback() {
        let rule = "─".repeat(40);
        let tail = "  ⎿  ◻ invented item\n".repeat(SPINNER_REACH);
        let screen = format!("✻ Examining… (2s)\n{tail}{rule}\n❯\u{a0}\n{rule}\n");
        assert_eq!(classify(&screen), Composer::Empty);
    }

    #[test]
    fn a_dialog_above_blank_rows_is_a_dialog() {
        let rule = "─".repeat(40);
        let blank = "\n".repeat(30);
        let screen = format!(
            "❯ Create a file.\n● Write(notes.txt)\n{rule}\n Create file\n notes.txt\n Do you want to create notes.txt?\n ❯ 1. Yes\n   2. No\n Esc to cancel{blank}"
        );
        assert_eq!(classify(&screen), Composer::Dialog);
    }

    #[test]
    fn a_dialog_row_in_scrollback_far_above_blank_rows_is_unknown() {
        let blank = "\n".repeat(30);
        let filler = "● invented reply line\n".repeat(16);
        let screen = format!(" Esc to cancel\n{filler}{blank}");
        assert_eq!(classify(&screen), Composer::Unknown);
    }
}
