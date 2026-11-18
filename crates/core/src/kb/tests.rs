use std::path::{Path, PathBuf};

use super::*;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kb")
}

fn app_scopes() -> Vec<String> {
    KbConfig::default().scopes(Some("https://github.com/example/app.git"))
}

/// The kb file format tiller reads is pinned: two notes exactly as kb
/// writes them (one with a quoted title and a block list) and hand-written
/// notes in the same form parse; a note that does not parse is named;
/// `trash/` and `archive/` are never read.
#[test]
fn kb_store_fixture_parses() {
    let read = read(Some(&fixture()));
    assert_eq!(read.state, KbState::Ok);
    let ids: Vec<&str> = read.notes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "finish-the-migration",
            "hidden-note",
            "run-the-gate-first-1",
            "ship-weekly",
            "use-the-board-api",
            "other-repo",
            "global-thing",
            "keep-replies-short",
        ]
    );
    let gate = &read.notes[2];
    assert_eq!(gate.title, "Run: the \"gate\" first #1");
    assert_eq!(gate.scope, "github.com/example/app");
    assert_eq!(gate.kind, "gotcha");
    assert!(gate.indexed);
    assert_eq!(gate.last_verified, "2026-11-18T12:31:05Z");
    assert_eq!(gate.body, "Line one.\n\nLine two: with colon.");
    assert_eq!(read.notes[3].title, "'Ship' weekly");
    assert!(!read.notes[1].indexed);
    assert_eq!(
        read.unparsed,
        [Unparsed {
            file: "global/broken.md".into(),
            scope: "global".into()
        }]
    );
}

/// Only notes in the host's scopes load, and never `open-work` or an
/// unindexed note. The order is scope, then kind, then newest
/// `last_verified`, then id; a note that did not parse is named.
#[test]
fn the_loader_filters_and_orders_the_notes() {
    let rendered = render(&read(Some(&fixture())), &app_scopes(), 0, 7_500);
    let order: Vec<(&str, bool, Option<&str>)> = rendered
        .entries
        .iter()
        .map(|e| (e.id.as_str(), e.loaded, e.reason.as_deref()))
        .collect();
    assert_eq!(
        order,
        [
            ("use-the-board-api", true, None),
            ("ship-weekly", true, None),
            ("run-the-gate-first-1", true, None),
            ("global-thing", true, None),
            ("keep-replies-short", true, None),
            ("global/broken.md", false, Some(UNREADABLE)),
        ]
    );
    assert!(
        rendered
            .text
            .starts_with(&format!("# Memory\n\n{INTRO}\n\n## Use the board API"))
    );
    assert!(rendered.text.contains(
        "## Run: the \"gate\" first #1\ngotcha · github.com/example/app · kb:run-the-gate-first-1\n\nLine one."
    ));
    assert!(!rendered.text.contains("Half done"));
    assert!(!rendered.text.contains("Not indexed"));
    assert!(!rendered.text.contains("Not this project"));
    assert_eq!(rendered.hash, sha256_hex(&rendered.text));

    // Exact repo ids load after the project's own scope, in config order.
    let config = KbConfig {
        kb_scopes: vec![
            "global".into(),
            "github.com/example/other".into(),
            "repo".into(),
        ],
        kb_command: None,
    };
    let scopes = config.scopes(Some("https://github.com/example/app.git"));
    assert_eq!(
        scopes,
        [
            "github.com/example/app",
            "github.com/example/other",
            "global"
        ]
    );
    let rendered = render(&read(Some(&fixture())), &scopes, 0, 7_500);
    assert_eq!(rendered.entries[3].id, "other-repo");

    // A work host that lists only its own scope never loads a global note.
    let only = KbConfig {
        kb_scopes: vec!["repo".into()],
        kb_command: None,
    };
    let rendered = render(
        &read(Some(&fixture())),
        &only.scopes(Some("https://github.com/example/app")),
        0,
        7_500,
    );
    assert!(rendered.entries.iter().all(|e| e.scope != "global"));
    // A project with no repo has no repo scope.
    assert!(only.scopes(None).is_empty());
}

/// The render stays within the budget the role file leaves, names each
/// note it skipped, and a later smaller note may still fit.
#[test]
fn the_loader_keeps_to_the_budget() {
    let read = read(Some(&fixture()));
    let full = render(&read, &app_scopes(), 0, 7_500);
    let first = &full.entries[0];
    let head = tokens(&format!("# Memory\n\n{INTRO}\n\n"));
    // Room for the heading and the first note, then for the fourth note but
    // not the two larger ones before it.
    let small = full.entries[3].tokens;
    assert!(small < full.entries[1].tokens && small < full.entries[2].tokens);
    let budget = 1_000 + head + first.tokens + small;
    let rendered = render(&read, &app_scopes(), 1_000, budget);
    let loaded: Vec<&str> = rendered.loaded().map(|e| e.id.as_str()).collect();
    assert_eq!(loaded, ["use-the-board-api", "global-thing"]);
    assert_eq!(rendered.entries[1].reason.as_deref(), Some(OVER_BUDGET));
    assert_eq!(rendered.entries[2].reason.as_deref(), Some(OVER_BUDGET));
    assert!(rendered.used <= budget);
    assert_eq!(rendered.used, budget);

    // A role file that fills the budget loads nothing, not even the heading.
    let none = render(&read, &app_scopes(), 7_500, 7_500);
    assert_eq!(none.text, "");
    assert!(none.loaded().next().is_none());
}

/// A missing store loads nothing; a `notes` path that is not a directory
/// reads as unreadable.
#[test]
fn a_missing_store_loads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let read_missing = read(Some(dir.path()));
    assert_eq!(read_missing.state, KbState::Missing);
    let rendered = render(&read_missing, &app_scopes(), 0, 7_500);
    assert_eq!(rendered.kb, KbState::Missing);
    assert_eq!(rendered.text, "");
    assert_eq!(read(None).state, KbState::Missing);
    std::fs::write(dir.path().join("notes"), "a file").unwrap();
    assert_eq!(read(Some(dir.path())).state, KbState::Unreadable);
}

#[test]
fn repo_urls_become_kb_scopes() {
    for (url, scope) in [
        (
            "https://github.com/example/app.git",
            Some("github.com/example/app"),
        ),
        (
            "https://user@example.org/Example/App",
            Some("example.org/example/app"),
        ),
        (
            "ssh://git@example.com:2222/team/app.git",
            Some("example.com/team/app"),
        ),
        (
            "git@example.org:example/app.git",
            Some("example.org/example/app"),
        ),
        ("https://example.com/a/b c/", Some("example.com/a/b-c")),
        ("/home/user/app", None),
        ("file:///home/user/app", None),
        ("https://example.com", None),
        ("", None),
    ] {
        assert_eq!(repo_scope(url).as_deref(), scope, "{url}");
    }
}

#[test]
fn the_memory_table_is_read_and_old_keys_are_ignored() {
    let table: toml::Table = toml::from_str(
        "[memory]\nremote = \"git@example.com:x.git\"\nkb_scopes = [\"global\"]\n\
         kb_command = [\"kb\"]\n[memory.review_days]\nrule = 30\n",
    )
    .unwrap();
    let config = KbConfig::from_table(&table).unwrap();
    assert_eq!(config.kb_scopes, ["global"]);
    assert_eq!(config.kb_command.as_deref(), Some(&["kb".to_owned()][..]));
    assert_eq!(
        KbConfig::from_table(&toml::Table::new()).unwrap(),
        KbConfig::default()
    );
    let bad: toml::Table = toml::from_str("[memory]\nkb_scopes = [\"Not A Scope\"]\n").unwrap();
    assert!(KbConfig::from_table(&bad).is_err());
}

/// With no `kb_command`, Keep runs the kb plugin's CLI with bun, from the
/// `kb@` entry of the Claude Code plugin list.
#[test]
fn the_kb_command_comes_from_the_plugin_list() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(KbConfig::default().command(Some(dir.path())), None);
    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    std::fs::write(
        dir.path().join("plugins/installed_plugins.json"),
        r#"{"version": 2, "plugins": {"other@x": [{"installPath": "/home/user/other"}],
            "kb@example": [{"scope": "user", "installPath": "/home/user/kb"}]}}"#,
    )
    .unwrap();
    assert_eq!(
        KbConfig::default().command(Some(dir.path())),
        Some(vec![
            "bun".to_owned(),
            "/home/user/kb/cli/main.ts".to_owned()
        ])
    );
    let set = KbConfig {
        kb_command: Some(vec!["kb".into()]),
        ..KbConfig::default()
    };
    assert_eq!(set.command(None), Some(vec!["kb".to_owned()]));
}

#[test]
fn front_matter_forms() {
    let note = |front: &str| parse(&format!("---\n{front}\n---\nbody\n"));
    let base = "id: a\ntitle: T\nscope: global\nkind: gotcha\nlast_verified: 2026-11-18T10:00:00Z";
    assert!(note(base).is_some());
    assert_eq!(
        note(&base.replace("title: T", "title: 'It''s'"))
            .unwrap()
            .title,
        "It's"
    );
    assert_eq!(
        note(&base.replace("title: T", "title: \"a\\u00e9\""))
            .unwrap()
            .title,
        "aé"
    );
    assert!(note(&base.replace("kind: gotcha", "kind: rumour")).is_none());
    assert!(note(&base.replace("scope: global", "scope: Bad Scope")).is_none());
    assert!(note(&format!("{base}\nindexed: maybe")).is_none());
    assert!(note(&base.replace("id: a\n", "")).is_none());
    assert!(parse("no front matter").is_none());
    assert!(
        parse(&format!("---\n{base}\n---\n\n")).is_none(),
        "an empty body"
    );
}

/// A fake kb command: a shell script that prints `out` and exits `code`,
/// and saves its stdin and arguments next to itself.
#[cfg(unix)]
fn fake_kb(dir: &Path, out: &str, code: i32) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt as _;
    let script = dir.join("kb");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > \"$(dirname \"$0\")/args\"\ncat > \"$(dirname \"$0\")/stdin\"\nprintf '%s' '{out}'\nexit {code}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    vec![script.to_string_lossy().into_owned()]
}

/// Keep succeeds only when kb names an id: `created` and `exists` both do;
/// an error result, a bad exit and a result with no id refuse with kb's
/// message.
#[cfg(unix)]
#[test]
fn kb_add_needs_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let input = serde_json::json!({"title": "T", "kind": "gotcha"});
    let command = fake_kb(dir.path(), r#"{"status":"created","id":"t","path":"x"}"#, 0);
    assert_eq!(
        add(&command, Some(repo.path()), &input).unwrap(),
        Added {
            id: "t".into(),
            existed: false
        }
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("stdin")).unwrap(),
        input.to_string()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("args")).unwrap(),
        format!("add --json --repo {}", repo.path().display())
    );
    let command = fake_kb(dir.path(), r#"{"status":"exists","id":"t"}"#, 0);
    assert!(add(&command, None, &input).unwrap().existed);
    let command = fake_kb(dir.path(), r#"{"error":"scope is not known"}"#, 1);
    assert_eq!(
        add(&command, None, &input).unwrap_err(),
        "kb refused the note: scope is not known"
    );
    let command = fake_kb(dir.path(), r#"{"status":"created","id":"t"}"#, 2);
    assert!(add(&command, None, &input).is_err());
    let command = fake_kb(dir.path(), r#"{"status":"created"}"#, 0);
    assert!(add(&command, None, &input).is_err());
    assert!(add(&[], None, &input).is_err());
}
