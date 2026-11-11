//! The Tiller memory document (`memory.md` sections 3 and 7, T7.2): how a
//! `fleet.md` or `projects/<id>.md` file reads as sections of entries, how an
//! edit in the memory view becomes a change to exactly those lines, and the
//! unified diff a proposal carries.
//!
//! A document is plain markdown: `## Section` headings, `- ` list items, and
//! continuation lines indented under an item. Every edit is a splice of the
//! lines it concerns, so the rest of the file stays byte for byte as it was.

use sha2::{Digest, Sha256};

pub use crate::import::plan::{EMPTY_BLOB, NOTE_SECTION, RULE_SECTION, document_path};

/// The third standard section, for the owner's preferences.
pub const PREFERENCE_SECTION: &str = "Preferences";

/// The sections every document shows, in order, with the class of an entry
/// placed in each (`memory.md` section 5).
pub const SECTIONS: [(&str, &str); 3] = [
    (RULE_SECTION, "rule"),
    (NOTE_SECTION, "note"),
    (PREFERENCE_SECTION, "preference"),
];

/// The startup budget a decision may not push a role over (P42).
pub const STARTUP_BUDGET: i64 = 7_500;

/// The class of an entry placed in `section`: a rule unless the section is
/// one of the other standard ones.
pub fn section_class(section: &str) -> &'static str {
    SECTIONS
        .iter()
        .find(|(name, _)| *name == section)
        .map_or("rule", |(_, class)| class)
}

/// Days until a durable entry of `class` is due for review (`memory.md`
/// section 5).
pub fn review_days(class: &str) -> i64 {
    match class {
        "preference" => 365,
        "note" => 90,
        _ => 180,
    }
}

/// One list item: its text (continuation lines joined with newlines, markers
/// and indentation stripped) and its lines, `start..end`, in the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// One `## ` section: its title, the heading's line and its entries in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: String,
    pub line: usize,
    pub entries: Vec<Entry>,
}

/// The sections of a document, in file order. Lines before the first heading
/// and lines that are not list items belong to no entry and are kept as they
/// are.
pub fn parse(text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let mut sections: Vec<Section> = Vec::new();
    let mut open: Option<Entry> = None;
    let close = |sections: &mut Vec<Section>, open: &mut Option<Entry>| {
        if let (Some(entry), Some(section)) = (open.take(), sections.last_mut()) {
            section.entries.push(entry);
        }
    };
    for (n, line) in lines.iter().enumerate() {
        if let Some(title) = line.strip_prefix("## ") {
            close(&mut sections, &mut open);
            sections.push(Section {
                title: title.trim().to_owned(),
                line: n,
                entries: Vec::new(),
            });
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            close(&mut sections, &mut open);
            if !sections.is_empty() {
                open = Some(Entry {
                    text: item.trim_end().to_owned(),
                    start: n,
                    end: n + 1,
                });
            }
        } else if let Some(entry) = open.as_mut() {
            let indented = line.starts_with("  ") || line.starts_with('\t');
            if indented && !line.trim().is_empty() {
                // Blank lines between the item and this one belong to it.
                for _ in entry.end..n {
                    entry.text.push('\n');
                }
                entry.text.push('\n');
                entry.text.push_str(line.trim());
                entry.end = n + 1;
            } else if !line.trim().is_empty() {
                close(&mut sections, &mut open);
            }
        }
    }
    close(&mut sections, &mut open);
    sections
}

/// The hash a `memories` row keeps of its entry's text.
pub fn text_sha256(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A line as the owner typed it: one line, runs of whitespace collapsed.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn split(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

fn join(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn item(text: &str) -> String {
    format!("- {text}")
}

/// `text` with `entry` added at the end of `section`, which is appended to
/// the document when it is missing.
pub fn add(text: &str, section: &str, entry: &str) -> String {
    let mut lines = split(text);
    let found = parse(text).into_iter().find(|s| s.title == section);
    match found {
        Some(found) => {
            let (at, mut insert) = match found.entries.last() {
                Some(last) => (last.end, vec![item(entry)]),
                None if lines
                    .get(found.line + 1)
                    .is_some_and(|l| l.trim().is_empty()) =>
                {
                    (found.line + 2, vec![item(entry)])
                }
                None => (found.line + 1, vec![String::new(), item(entry)]),
            };
            if lines.get(at).is_some_and(|next| !next.trim().is_empty()) {
                insert.push(String::new());
            }
            let at = at.min(lines.len());
            lines.splice(at..at, insert);
        }
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend([format!("## {section}"), String::new(), item(entry)]);
        }
    }
    join(&lines)
}

/// `text` with `entry` replaced by one line.
pub fn rewrite(text: &str, entry: &Entry, new: &str) -> String {
    let mut lines = split(text);
    lines.splice(entry.start..entry.end, [item(new)]);
    join(&lines)
}

/// `text` without `entry`. A section's last entry takes the blank line
/// before it along, so no run of blank lines is left behind.
pub fn remove(text: &str, entry: &Entry) -> String {
    let mut lines = split(text);
    lines.drain(entry.start..entry.end);
    let blank = |at: usize| lines.get(at).is_none_or(|l: &String| l.trim().is_empty());
    if entry.start > 0 && blank(entry.start - 1) && blank(entry.start) {
        lines.remove(entry.start - 1);
    }
    join(&lines)
}

/// The entries a diff adds, read from its `+` lines as a document of their
/// own: an `add` placement's new item, a rewrite's new text.
pub fn added_entries(diff: &str) -> Vec<String> {
    let mut added = String::new();
    let mut heading = false;
    for line in diff.lines() {
        if line.starts_with("+++ ") {
            continue;
        }
        if let Some(line) = line.strip_prefix('+') {
            if !heading && !line.starts_with("## ") {
                // Entries read under a heading, so an item with none gets one.
                added.push_str("## -\n");
            }
            heading = true;
            added.push_str(line);
            added.push('\n');
        }
    }
    parse(&added)
        .into_iter()
        .flat_map(|section| section.entries)
        .map(|entry| entry.text)
        .collect()
}

/// The `# Memory` part of a launch's prompt (`memory.md` section 2, L3):
/// one `# Memory` heading, then a `## <path>` heading and the text of each
/// document in `docs` (path, text), in the order given. A document whose
/// text is empty renders nothing, and with nothing to render the part is
/// empty. The launcher renders the prompt with it and `mem.decide` counts
/// the budget with it, so the count is of what launches.
pub fn render_loaded<'a>(docs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (path, text) in docs {
        if text.trim().is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str("# Memory\n\n");
        }
        out.push_str(&format!("## {path}\n\n{}\n\n", text.trim_end()));
    }
    out
}

/// The estimated tokens of `text`: four bytes a token, rounded up, the
/// estimate the role files' budget uses too.
pub fn tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}

/// `n` with a comma between each group of three digits, as an error shows
/// a token count.
pub fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `old` with `diff` (one file's unified diff, as [`unified_diff`] writes
/// it) applied exactly: every hunk at its stated line, its context and
/// removed lines matching. `None` when the diff does not apply to `old`.
/// The `---` and `+++` header lines are skipped.
pub fn apply_diff(old: &str, diff: &str) -> Option<String> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let mut out = String::with_capacity(old.len() + diff.len());
    let mut at = 0;
    // The old lines the open hunk still has to read.
    let mut owed: Option<usize> = None;
    let mut last = ' ';
    for line in diff.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("@@ -") {
            if owed.is_some_and(|n| n > 0) {
                return None;
            }
            let range = header.split(' ').next()?;
            let (start, len) = match range.split_once(',') {
                Some((start, len)) => (start.parse::<usize>().ok()?, len.parse::<usize>().ok()?),
                None => (range.parse::<usize>().ok()?, 1),
            };
            let begin = if len == 0 {
                start
            } else {
                start.checked_sub(1)?
            };
            if begin < at || begin > a.len() {
                return None;
            }
            for kept in &a[at..begin] {
                out.push_str(kept);
            }
            at = begin;
            owed = Some(len);
            continue;
        }
        let Some(left) = owed else {
            // The file headers before the first hunk.
            continue;
        };
        if line.starts_with('\\') {
            // The line before has no final newline.
            if last == '+' && out.ends_with('\n') {
                out.pop();
            }
            continue;
        }
        let mut chars = line.chars();
        let mark = chars.next()?;
        let body = chars.as_str();
        let same = |old: &str| {
            old.strip_suffix('\n').unwrap_or(old) == body.strip_suffix('\n').unwrap_or(body)
        };
        match mark {
            ' ' | '-' => {
                let old = a.get(at).filter(|old| same(old))?;
                if left == 0 {
                    return None;
                }
                if mark == ' ' {
                    out.push_str(old);
                }
                at += 1;
                owed = Some(left - 1);
            }
            '+' => out.push_str(body),
            _ => return None,
        }
        last = mark;
    }
    if owed.is_some_and(|n| n > 0) {
        return None;
    }
    for kept in &a[at..] {
        out.push_str(kept);
    }
    Some(out)
}

const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Keep,
    Del,
    Add,
}

/// The unified diff from `old` to `new` for `path`, with three lines of
/// context, as `git apply` takes it. Empty when the texts are equal.
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let ops = edit_script(&a, &b);
    if ops.iter().all(|(op, _, _)| *op == Op::Keep) {
        return String::new();
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (op, _, _))| *op != Op::Keep)
        .map(|(n, _)| n)
        .collect();
    let mut first = 0;
    while first < changed.len() {
        let mut last = first;
        while last + 1 < changed.len() && changed[last + 1] - changed[last] <= 2 * CONTEXT {
            last += 1;
        }
        let from = changed[first].saturating_sub(CONTEXT);
        let to = (changed[last] + CONTEXT + 1).min(ops.len());
        let hunk = &ops[from..to];
        let (a_start, b_start) = (hunk[0].1, hunk[0].2);
        let a_len = hunk.iter().filter(|(op, _, _)| *op != Op::Add).count();
        let b_len = hunk.iter().filter(|(op, _, _)| *op != Op::Del).count();
        let start = |at: usize, len: usize| if len == 0 { at } else { at + 1 };
        out.push_str(&format!(
            "@@ -{},{a_len} +{},{b_len} @@\n",
            start(a_start, a_len),
            start(b_start, b_len)
        ));
        for &(op, i, j) in hunk {
            let (mark, line) = match op {
                Op::Keep => (' ', a[i]),
                Op::Del => ('-', a[i]),
                Op::Add => ('+', b[j]),
            };
            out.push(mark);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
        first = last + 1;
    }
    out
}

/// A longest-common-subsequence edit script: each step with the index it
/// reads in `a` and in `b` (for an `Add`, `i` is where it lands in `a`).
fn edit_script(a: &[&str], b: &[&str]) -> Vec<(Op, usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::with_capacity(n + m);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((Op::Keep, i, j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push((Op::Del, i, j));
            i += 1;
        } else {
            ops.push((Op::Add, i, j));
            j += 1;
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Memory\n\n## How we work here\n\n- Rebase often.\n- Keep diffs small,\n  one idea each.\n\n  Even docs.\n\n## Watch out for\n\n- The cache lies.\n";

    #[test]
    fn parses_sections_entries_and_continuations() {
        let sections = parse(DOC);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, RULE_SECTION);
        let texts: Vec<&str> = sections[0]
            .entries
            .iter()
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            texts,
            [
                "Rebase often.",
                "Keep diffs small,\none idea each.\n\nEven docs."
            ]
        );
        assert_eq!(
            (sections[0].entries[1].start, sections[0].entries[1].end),
            (5, 9)
        );
        assert_eq!(sections[1].entries[0].text, "The cache lies.");
    }

    #[test]
    fn edits_splice_only_their_lines() {
        let entry = &parse(DOC)[0].entries[1];
        let rewritten = rewrite(DOC, entry, "Keep diffs small.");
        assert_eq!(
            rewritten,
            "# Memory\n\n## How we work here\n\n- Rebase often.\n- Keep diffs small.\n\n## Watch out for\n\n- The cache lies.\n"
        );
        let removed = remove(DOC, &parse(DOC)[0].entries[0]);
        assert!(removed.starts_with("# Memory\n\n## How we work here\n\n- Keep diffs small,\n"));
        let added = add(DOC, NOTE_SECTION, "Clocks drift.");
        assert!(added.ends_with("- The cache lies.\n- Clocks drift.\n"));
        let added = add(DOC, RULE_SECTION, "Name things.");
        assert!(added.contains("  Even docs.\n- Name things.\n\n## Watch out for"));
    }

    #[test]
    fn add_creates_a_missing_section_and_fills_an_empty_one() {
        assert_eq!(
            add("", PREFERENCE_SECTION, "Short PRs."),
            "## Preferences\n\n- Short PRs.\n"
        );
        assert_eq!(
            add(
                "## Preferences\n\n## Watch out for\n",
                PREFERENCE_SECTION,
                "Short PRs."
            ),
            "## Preferences\n\n- Short PRs.\n\n## Watch out for\n"
        );
        assert_eq!(
            add(
                "## Preferences\n## Watch out for\n",
                PREFERENCE_SECTION,
                "Short PRs."
            ),
            "## Preferences\n\n- Short PRs.\n\n## Watch out for\n"
        );
        assert_eq!(
            add("- stray\n", RULE_SECTION, "One."),
            "- stray\n\n## How we work here\n\n- One.\n"
        );
    }

    #[test]
    fn diff_matches_the_import_placement_shape() {
        let new = add("", RULE_SECTION, "Rebase often.");
        let diff = unified_diff("fleet.md", "", &new);
        assert_eq!(
            diff,
            "--- a/fleet.md\n+++ b/fleet.md\n@@ -0,0 +1,3 @@\n+## How we work here\n+\n+- Rebase often.\n"
        );
        assert_eq!(added_entries(&diff), ["Rebase often."]);
    }

    #[test]
    fn diff_keeps_context_and_marks_a_missing_final_newline() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let diff = unified_diff("x.md", old, new);
        assert_eq!(
            diff,
            "--- a/x.md\n+++ b/x.md\n@@ -2,7 +2,7 @@\n b\n c\n d\n-e\n+E\n f\n g\n-h\n\\ No newline at end of file\n+h\n"
        );
        assert_eq!(unified_diff("x.md", new, new), "");
    }

    #[test]
    fn added_entries_reads_rewrites_without_a_heading() {
        let entry = &parse(DOC)[0].entries[0];
        let diff = unified_diff("fleet.md", DOC, &rewrite(DOC, entry, "Rebase daily."));
        assert_eq!(added_entries(&diff), ["Rebase daily."]);
    }

    #[test]
    fn a_diff_applies_back_to_the_text_it_was_made_to() {
        let entry = &parse(DOC)[0].entries[1];
        let cases = [
            (String::new(), add("", RULE_SECTION, "Rebase often.")),
            (DOC.to_owned(), add(DOC, NOTE_SECTION, "Clocks drift.")),
            (DOC.to_owned(), rewrite(DOC, entry, "Keep diffs small.")),
            (DOC.to_owned(), remove(DOC, &parse(DOC)[0].entries[0])),
            (
                "a\nb\nc\nd\ne\nf\ng\nh".to_owned(),
                "a\nb\nc\nd\nE\nf\ng\nh\n".to_owned(),
            ),
            ("a\nb\n".to_owned(), "a\nb".to_owned()),
            ("x\n".to_owned(), String::new()),
            (
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n".to_owned(),
                "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\ntwelve\n".to_owned(),
            ),
        ];
        for (old, new) in cases {
            let diff = unified_diff("x.md", &old, &new);
            assert_eq!(
                apply_diff(&old, &diff).as_deref(),
                Some(new.as_str()),
                "{diff}"
            );
        }
        let diff = unified_diff("x.md", DOC, &add(DOC, NOTE_SECTION, "Clocks drift."));
        assert_eq!(apply_diff("## Other\n\n- Text.\n", &diff), None);
        assert_eq!(
            apply_diff(DOC, "--- a/x.md\n+++ b/x.md\n@@ -90,1 +90,1 @@\n-z\n+y\n"),
            None
        );
    }

    #[test]
    fn the_loaded_part_renders_each_document_under_its_path() {
        assert_eq!(render_loaded([("fleet.md", " \n")]), "");
        assert_eq!(
            render_loaded([
                ("fleet.md", "- One.\n\n"),
                ("projects/a.md", ""),
                ("projects/b.md", "- Two.")
            ]),
            "# Memory\n\n## fleet.md\n\n- One.\n\n## projects/b.md\n\n- Two.\n\n"
        );
        assert_eq!(tokens("abcde"), 2);
        assert_eq!(tokens(""), 0);
        assert_eq!(grouped(7_500), "7,500");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(150), "150");
        assert_eq!(grouped(-1_200), "-1,200");
    }

    #[test]
    fn classes_and_intervals_follow_the_section() {
        assert_eq!(section_class(NOTE_SECTION), "note");
        assert_eq!(section_class("Elsewhere"), "rule");
        assert_eq!(review_days("preference"), 365);
        assert_eq!(one_line("  two\n words  "), "two words");
    }
}
