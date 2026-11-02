use std::fs;
use std::path::Path;

use rusqlite::Connection;

use super::*;
use crate::domain::*;
use crate::state::asks::{NewAsk, Resolution};
use crate::state::attention::NewAttention;
use crate::state::cards::NewCard;
use crate::state::projects::{Init, NewProject};
use crate::state::runs::NewOrchestrator;
use crate::state::{get, list};
use crate::store::{Actor, Ctx, Store};

fn at(text: &str) -> Timestamp {
    Timestamp::from_jiff(text.parse().unwrap())
}

fn owner() -> Ctx {
    Ctx::new(Actor::Owner)
}

struct Host {
    _dir: tempfile::TempDir,
    paths: Paths,
}

fn host() -> Host {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());
    Host { _dir: dir, paths }
}

/// An initialized store with project `alpha` and its orchestrator. Returns
/// the orchestrator's session id.
fn setup(store: &mut Store, label: &str, boundary: Boundary) -> String {
    store
        .init(
            &owner(),
            Init {
                host_label: label.into(),
                lane_capacity: 2,
                ram_floor_mb: None,
                disk_floor_mb: None,
                boundary,
            },
        )
        .unwrap();
    store
        .project_add(
            &owner(),
            NewProject {
                id: "alpha".into(),
                name: "Alpha".into(),
                repo: Some("example/alpha".into()),
                remote: None,
                default_branch: None,
                delivery: Delivery::DirectPr,
                checks: None,
                capacity: Some(3),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    store
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-orchestrator".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session
        .id
}

fn new_card(title: &str) -> NewCard {
    NewCard {
        project_id: "alpha".into(),
        title: title.into(),
        ..NewCard::default()
    }
}

fn open_decision(store: &mut Store, card: &str, question: &str) -> Attention {
    store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.into(),
                question: question.into(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value
}

fn integrity(path: &Path) -> String {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}

/// T2.3 acceptance (ADR 0002, `recovery.md` R10): the owner's note is
/// acknowledged by the orchestrator, which turns it into a card and asks the
/// owner a question. The nightly backup runs, the live database is then
/// destroyed, and the restore brings back the card, the acknowledgement and
/// the still-pending decision; the damaged file is kept beside it.
#[test]
fn restore_drill_recovers_acknowledged_card_with_pending_decision() {
    let host = host();
    let paths = &host.paths;
    let (card, decision, message) = {
        let mut store = Store::open(&paths.db).unwrap();
        let orchestrator = setup(&mut store, "test-host", Boundary::Personal);
        let recorded = store
            .ask_record(
                &owner(),
                Some("note-1"),
                NewAsk {
                    project_id: "alpha".into(),
                    session_id: None,
                    source: AskSource::Script,
                    text: "make the footer readable".into(),
                },
            )
            .unwrap()
            .value;
        let message = recorded.message.unwrap();
        let as_orchestrator = Ctx::new(Actor::Session(orchestrator));
        store.outbox_ack(&as_orchestrator, &message.id).unwrap();
        let card = store
            .ask_resolve(
                &as_orchestrator,
                &recorded.ask.id,
                Resolution::New(new_card("Readable footer")),
            )
            .unwrap()
            .value
            .card
            .unwrap();
        let decision = open_decision(&mut store, &card.display_id, "Dark or light footer?");

        let done = nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
        assert!(matches!(done, Nightly::Done { .. }), "{done:?}");

        // The world moves on after the backup: the decision is answered.
        store
            .attention_answer(&owner(), None, &decision.id, "dark", AnsweredVia::Cli)
            .unwrap();
        (card, decision, message)
    };

    // The disk eats the live database.
    fs::write(&paths.db, b"not a database any more").unwrap();

    let plan = restore(paths, None, true, at("2026-11-02T09:00:00Z")).unwrap();
    assert!(plan.dry_run);
    assert_eq!(fs::read(&paths.db).unwrap(), b"not a database any more");

    let restored = restore(paths, None, false, at("2026-11-02T09:00:00Z")).unwrap();
    assert!(restored.skipped.is_empty());
    let moved_to = restored.moved_to.unwrap();
    assert_eq!(fs::read(&moved_to).unwrap(), b"not a database any more");

    let store = Store::open(&paths.db).unwrap();
    let back: Card = get(store.conn(), "card", &card.id).unwrap();
    assert_eq!(back.display_id, card.display_id);
    assert_eq!(back.title, "Readable footer");
    let ask: Ask = get(
        store.conn(),
        "ask",
        &list::<AskCard>(store.conn(), "card_id = ?1", &[&card.id]).unwrap()[0].ask_id,
    )
    .unwrap();
    assert_eq!(ask.disposition, Disposition::New);
    let acked: OutboxMessage = get(store.conn(), "outbox message", &message.id).unwrap();
    assert_eq!(acked.state, OutboxState::Acked);
    let pending: Attention = get(store.conn(), "attention", &decision.id).unwrap();
    assert_eq!(pending.question, "Dark or light footer?");
    assert_eq!(pending.answered_at, None, "the decision is pending again");
    assert_eq!(pending.withdrawn_at, None);
    let column: String = store
        .conn()
        .query_row(
            "SELECT \"column\" FROM card_column WHERE id = ?1",
            [&card.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(column, "needs_you");
}

#[test]
fn truncated_backup_is_refused_and_previous_one_restores() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let first = store
        .card_create(&owner(), None, new_card("first"), None)
        .unwrap()
        .value;
    nightly(&mut store, paths, at("2026-11-01T03:00:00Z")).unwrap();
    let second = store
        .card_create(&owner(), None, new_card("second"), None)
        .unwrap()
        .value;
    let Nightly::Done { entry: newest, .. } =
        nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap()
    else {
        panic!("the second night did not back up");
    };
    drop(store);

    // A truncated final record.
    let newest_path = paths.resolve(&newest.file);
    let bytes = fs::read(&newest_path).unwrap();
    fs::write(&newest_path, &bytes[..bytes.len() / 2]).unwrap();

    let error = restore(paths, Some(&newest_path), false, at("2026-11-02T09:00:00Z")).unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(error.to_string().contains("does not verify"), "{error}");

    let restored = restore(paths, None, false, at("2026-11-02T09:00:00Z")).unwrap();
    assert_eq!(restored.skipped.len(), 1);
    assert_eq!(restored.skipped[0].file, newest.file);
    assert!(
        restored.skipped[0].reason.contains("size"),
        "{:?}",
        restored.skipped
    );
    assert_ne!(restored.entry.file, newest.file);

    let store = Store::open(&paths.db).unwrap();
    assert!(get::<Card>(store.conn(), "card", &first.id).is_ok());
    assert_eq!(
        get::<Card>(store.conn(), "card", &second.id)
            .unwrap_err()
            .code(),
        "not_found"
    );
    // The live database it replaced keeps the newer card.
    let kept = Connection::open(restored.moved_to.unwrap()).unwrap();
    let title: String = kept
        .query_row(
            "SELECT title FROM cards WHERE id = ?1",
            [&second.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(title, "second");
}

#[test]
fn retention_never_deletes_pre_restore_file() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let pre_restore = paths.beside_db(".pre-restore-20261001T000000000Z");
    fs::write(&pre_restore, b"the only copy of newer records").unwrap();
    // A manifest that wrongly lists it, as if a bug or a hand edit put it there.
    let mut manifest = Manifest::load(paths).unwrap();
    manifest.entries.push(Entry {
        file: paths.relative(&pre_restore).unwrap(),
        kind: EntryKind::PreMigration,
        size: 0,
        sha256: String::new(),
        schema_version: 1,
        max_seq: 0,
        host_id: None,
        boundary: None,
        at: at("2026-10-01T00:00:00Z"),
        superseded: false,
    });
    manifest.save(paths).unwrap();

    // Seventy nights: past 14 daily and 8 weekly.
    let start = at("2026-07-01T03:00:00Z");
    for night in 0..70 {
        let now = start.plus_minutes(night * 24 * 60);
        let done = nightly(&mut store, paths, now).unwrap();
        assert!(
            matches!(done, Nightly::Done { .. }),
            "night {night}: {done:?}"
        );
    }
    assert_eq!(
        fs::read(&pre_restore).unwrap(),
        b"the only copy of newer records"
    );

    let manifest = Manifest::load(paths).unwrap();
    let nightly: Vec<&Entry> = manifest
        .live()
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::Nightly)
        .collect();
    // The last 14 days (2026-08-26 to 2026-09-08) touch 3 ISO weeks, so the
    // newest of 5 older weeks makes up the 8 weekly.
    assert_eq!(
        nightly.len(),
        14 + 5,
        "{:#?}",
        nightly.iter().map(|e| e.at).collect::<Vec<_>>()
    );
    let on_disk = fs::read_dir(&paths.backups)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "db")
        })
        .count();
    assert_eq!(
        on_disk,
        nightly.len(),
        "a deleted file is still listed, or the reverse"
    );
}

#[test]
fn retention_keeps_an_older_verified_backup_when_kept_ones_rot() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let start = at("2026-07-01T03:00:00Z");
    for night in 0..20 {
        nightly(&mut store, paths, start.plus_minutes(night * 24 * 60)).unwrap();
    }
    // Every backup but the first night's rots on disk.
    let manifest = Manifest::load(paths).unwrap();
    let oldest = manifest.live().last().copied().cloned().unwrap();
    for entry in manifest.live() {
        if entry.file != oldest.file {
            fs::write(paths.resolve(&entry.file), b"rot").unwrap();
        }
    }
    // Night 20 keeps days 7 to 20 and the newest of each week; of those only
    // its own copy verifies, so the first night's, which no rule keeps,
    // stays as the second verified backup.
    let done = nightly(&mut store, paths, start.plus_minutes(20 * 24 * 60)).unwrap();
    let Nightly::Done { retention, .. } = done else {
        panic!("{done:?}");
    };
    assert!(retention.kept.contains(&oldest.file), "{retention:?}");
    assert!(!retention.deleted.contains(&oldest.file));
    assert!(verify(paths, &oldest).is_ok());
    assert!(!retention.unverified.is_empty());
}

#[test]
fn restore_refuses_while_daemon_runs_and_another_hosts_backup() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    drop(store);

    let lock = DaemonLock::acquire(paths).unwrap();
    assert!(daemon_running(paths).unwrap());
    assert_eq!(DaemonLock::acquire(paths).unwrap_err().code(), "refused");
    let error = restore(paths, None, true, Timestamp::now()).unwrap_err();
    assert!(error.to_string().contains("daemon is running"), "{error}");
    drop(lock);
    assert!(!daemon_running(paths).unwrap());

    // Another host's backups directory copied in next to this host's.
    let other = self::host();
    let mut other_store = Store::open(&other.paths.db).unwrap();
    setup(&mut other_store, "other-host", Boundary::Personal);
    nightly(&mut other_store, &other.paths, at("2026-11-02T04:00:00Z")).unwrap();
    drop(other_store);
    fs::remove_dir_all(&paths.backups).unwrap();
    fs::create_dir_all(&paths.backups).unwrap();
    for entry in fs::read_dir(&other.paths.backups).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), paths.backups.join(entry.file_name())).unwrap();
    }
    let before = fs::read(&paths.db).unwrap();
    let error = restore(paths, None, false, Timestamp::now()).unwrap_err();
    assert!(error.to_string().contains("belongs to host"), "{error}");
    assert_eq!(fs::read(&paths.db).unwrap(), before);
}

#[test]
fn failed_night_retries_after_an_hour_and_second_failed_night_alarms_once() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let night_one = at("2026-11-01T03:00:00Z");
    nightly(&mut store, paths, night_one).unwrap();

    // Each attempt's temporary path is taken by a directory, so it fails.
    let block = |now: Timestamp| {
        fs::create_dir_all(paths.backups.join(format!("{}.db.tmp", stamp(now)))).unwrap();
    };
    let night_two = night_one.plus_minutes(24 * 60);
    let attempt = |store: &mut Store, now: Timestamp| {
        block(now);
        nightly(store, paths, now).unwrap()
    };
    let first = attempt(&mut store, night_two);
    let Nightly::Failed { next, alarm, .. } = &first else {
        panic!("{first:?}");
    };
    assert_eq!(*next, night_two.plus_minutes(RETRY_AFTER_MINUTES));
    assert!(alarm.is_none());

    assert!(matches!(
        nightly(&mut store, paths, night_two.plus_minutes(30)).unwrap(),
        Nightly::NotDue { .. }
    ));
    let retry = attempt(&mut store, night_two.plus_minutes(60));
    let Nightly::Failed { next, alarm, .. } = &retry else {
        panic!("{retry:?}");
    };
    assert!(alarm.is_none(), "one failed night is not an alarm");
    assert_eq!(*next, night_two.plus_minutes(24 * 60));

    let night_three = night_two.plus_minutes(24 * 60);
    attempt(&mut store, night_three);
    let second_night = attempt(&mut store, night_three.plus_minutes(60));
    let Nightly::Failed {
        alarm: Some(item), ..
    } = &second_night
    else {
        panic!("two failed nights must alarm: {second_night:?}");
    };
    assert_eq!(item.kind, AttentionKind::Recovery);
    let card: Card = get(store.conn(), "card", &item.card_id).unwrap();
    assert_eq!(card.display_id, "fleet-1");
    assert!(
        item.evidence.as_deref().unwrap().contains("backups/"),
        "{item:?}"
    );

    // A third failed night keeps the same item, not a new one.
    let night_four = night_three.plus_minutes(24 * 60);
    attempt(&mut store, night_four);
    let third = attempt(&mut store, night_four.plus_minutes(60));
    let Nightly::Failed {
        alarm: Some(again), ..
    } = &third
    else {
        panic!("{third:?}");
    };
    assert_eq!(again.id, item.id);
    let open: Vec<Attention> = list(
        store.conn(),
        "kind = 'recovery' AND answered_at IS NULL",
        &[],
    )
    .unwrap();
    assert_eq!(open.len(), 1);

    // The next good night clears the streak.
    let good = nightly(&mut store, paths, night_four.plus_minutes(24 * 60)).unwrap();
    assert!(matches!(good, Nightly::Done { .. }), "{good:?}");
    assert!(Manifest::load(paths).unwrap().failures.is_empty());
}

fn exported_hashes(manifest: &ExportManifest) -> Vec<(String, u64, String)> {
    manifest
        .tables
        .iter()
        .map(|t| (t.name.clone(), t.rows, t.sha256.clone()))
        .collect()
}

#[test]
fn export_then_import_reproduces_every_row() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Work);
    let card = store
        .card_create(&owner(), None, new_card("carried"), None)
        .unwrap()
        .value;
    open_decision(&mut store, &card.display_id, "Which way?");
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    let first = export_jsonl(store.conn(), &export, Timestamp::now()).unwrap();
    assert_eq!(first.boundary.as_deref(), Some("work"));
    let events = first.tables.iter().find(|t| t.name == "events").unwrap();
    assert!(events.rows > 0);
    assert_eq!(
        export_jsonl(store.conn(), &export, Timestamp::now())
            .unwrap_err()
            .code(),
        "refused",
        "an export never writes into a used directory"
    );

    let target = self::host();
    let imported = import_jsonl(&export, &target.paths, Boundary::Work).unwrap();
    assert_eq!(imported.schema_version, crate::migrate::LATEST);
    assert_eq!(
        imported.rows,
        first.tables.iter().map(|t| t.rows).sum::<u64>()
    );
    assert_eq!(integrity(&target.paths.db), "ok");

    let reopened = Store::open(&target.paths.db).unwrap();
    let again = export_jsonl(reopened.conn(), &dir.path().join("again"), Timestamp::now()).unwrap();
    assert_eq!(exported_hashes(&again), exported_hashes(&first));
    assert_eq!(again.max_seq, first.max_seq);
    // The imported database keeps working: new events continue the seq.
    drop(reopened);
    let mut reopened = Store::open(&target.paths.db).unwrap();
    let next = reopened
        .card_create(&owner(), None, new_card("after import"), None)
        .unwrap();
    assert!(next.events[0] > first.max_seq);
    assert_eq!(next.value.display_id, "alpha-2");
}

#[test]
fn import_refuses_a_tampered_file_another_boundary_and_an_existing_database() {
    let host = host();
    let mut store = Store::open(&host.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    export_jsonl(store.conn(), &export, Timestamp::now()).unwrap();

    let target = self::host();
    let error = import_jsonl(&export, &target.paths, Boundary::Work).unwrap_err();
    assert!(error.to_string().contains("boundary"), "{error}");
    assert!(!target.paths.db.exists());

    let error = import_jsonl(&export, &host.paths, Boundary::Personal).unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");

    let cards = export.join("cards.jsonl");
    let mut text = fs::read_to_string(&cards).unwrap();
    text.push_str(&text.clone());
    fs::write(&cards, text).unwrap();
    let error = import_jsonl(&export, &target.paths, Boundary::Personal).unwrap_err();
    assert!(error.to_string().contains("sha256"), "{error}");
    assert!(!target.paths.db.exists());
    assert!(!target.paths.beside_db(".import.tmp").exists());
}

#[test]
fn import_refuses_rows_whose_foreign_keys_do_not_hold() {
    let host = host();
    let mut store = Store::open(&host.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    store
        .card_create(&owner(), None, new_card("orphan"), None)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    export_jsonl(store.conn(), &export, Timestamp::now()).unwrap();

    // Drop every project but keep the manifest consistent with the files.
    let projects = export.join("projects.jsonl");
    fs::write(&projects, "").unwrap();
    let manifest_path = export.join("manifest.json");
    let mut manifest: ExportManifest =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entry = manifest
        .tables
        .iter_mut()
        .find(|t| t.name == "projects")
        .unwrap();
    entry.rows = 0;
    entry.sha256 = hash_file(&projects).unwrap().1;
    fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

    let target = self::host();
    let error = import_jsonl(&export, &target.paths, Boundary::Personal).unwrap_err();
    assert!(error.to_string().contains("foreign keys"), "{error}");
    assert!(!target.paths.db.exists());
}

#[test]
fn older_binary_refuses_newer_schema_without_writing() {
    let host = host();
    let paths = &host.paths;
    drop(Store::open(&paths.db).unwrap());
    {
        let conn = Connection::open(&paths.db).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        // Fold the WAL in, so the bytes below are the whole database.
        let _: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
    }
    let before = fs::read(&paths.db).unwrap();
    let error = migrate(paths, crate::migrate::LATEST, Timestamp::now()).unwrap_err();
    assert_eq!(error.code(), "internal");
    assert!(error.to_string().contains("schema 99 is newer"), "{error}");
    assert_eq!(fs::read(&paths.db).unwrap(), before);
    assert!(!paths.manifest().exists(), "no copy was taken or listed");
    assert!(Store::open(&paths.db).is_err());
    assert_eq!(fs::read(&paths.db).unwrap(), before);
}

#[test]
fn failed_migration_rolls_back_and_keeps_copy() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let card = store
        .card_create(&owner(), None, new_card("survives"), None)
        .unwrap()
        .value;
    drop(store);

    let error = schema::migrate_with(paths, 0, Timestamp::now(), |conn, _| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM attention", [])?;
        tx.execute("DROP TABLE cards", [])?;
        Err(crate::Error::Internal("step 1 down failed".into()))
    })
    .unwrap_err();
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert!(error.to_string().contains("state.db.pre-1-0"), "{error}");

    let store = Store::open(&paths.db).unwrap();
    assert!(get::<Card>(store.conn(), "card", &card.id).is_ok());
    let manifest = Manifest::load(paths).unwrap();
    let copy = manifest
        .entries
        .iter()
        .find(|entry| entry.kind == EntryKind::PreMigration)
        .unwrap();
    assert_eq!(copy.file, "state.db.pre-1-0");
    assert_eq!(copy.schema_version, 1);
    assert!(verify(paths, copy).is_ok());
}

#[test]
fn migration_copy_is_kept_until_the_next_nightly_backup_verifies() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    drop(store);

    let down = migrate(paths, 0, at("2026-11-02T10:00:00Z")).unwrap();
    assert_eq!((down.from, down.to), (1, 0));
    let copy = down.copy.unwrap();
    assert_eq!(migrate(paths, 0, Timestamp::now()).unwrap().copy, None);
    let up = migrate(paths, crate::migrate::LATEST, at("2026-11-02T10:05:00Z")).unwrap();
    assert_eq!(up.copy, None, "an empty database has nothing to keep");

    // Restore the pre-migration copy as the owner would after a bad down.
    let restored = restore(
        paths,
        Some(&paths.resolve(&copy)),
        false,
        at("2026-11-02T10:10:00Z"),
    )
    .unwrap();
    assert_eq!(restored.entry.file, copy);
    let mut store = Store::open(&paths.db).unwrap();
    let host_id: Option<String> = crate::state::projects::meta(store.conn(), "host_id").unwrap();
    assert!(host_id.is_some());

    let done = nightly(&mut store, paths, at("2026-11-03T03:00:00Z")).unwrap();
    let Nightly::Done { retention, .. } = done else {
        panic!("{done:?}");
    };
    assert_eq!(retention.deleted, vec![copy.clone()]);
    assert!(!paths.resolve(&copy).exists());
    assert!(
        restored.moved_to.unwrap().exists(),
        "the pre-restore file stays"
    );
}

#[test]
fn manual_backup_and_a_late_timer_never_move_the_nightly_schedule() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let night = at("2026-11-01T03:00:00Z");
    nightly(&mut store, paths, night).unwrap();

    let manual = backup(store.conn(), paths, at("2026-11-01T15:00:00Z")).unwrap();
    let Nightly::Done { entry, .. } = manual else {
        panic!("{manual:?}");
    };
    assert_eq!(entry.kind, EntryKind::Manual);
    let next = night.plus_minutes(24 * 60);
    assert_eq!(next_due(&Manifest::load(paths).unwrap()), Some(next));

    // The timer fires 40 minutes late; the night after is still at 03:00.
    nightly(&mut store, paths, next.plus_minutes(40)).unwrap();
    assert_eq!(
        next_due(&Manifest::load(paths).unwrap()),
        Some(next.plus_minutes(24 * 60))
    );
}

#[test]
fn unreadable_manifest_alarms_at_once() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-01T03:00:00Z")).unwrap();
    fs::write(paths.manifest(), b"{\"entries\": [").unwrap();

    let failed = nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    let Nightly::Failed {
        alarm: Some(item), ..
    } = &failed
    else {
        panic!("{failed:?}");
    };
    assert_eq!(item.kind, AttentionKind::Recovery);
    assert!(item.evidence.as_deref().unwrap().contains("does not parse"));
    // The broken manifest is left for the owner, never replaced.
    assert_eq!(fs::read(paths.manifest()).unwrap(), b"{\"entries\": [");
}

#[test]
fn retention_finishes_a_cleanup_an_earlier_pass_left() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let first = backup(store.conn(), paths, at("2026-11-01T03:00:00Z")).unwrap();
    let Nightly::Done { entry: stale, .. } = first else {
        panic!("{first:?}");
    };
    // As if a pass marked it superseded and died before deleting the file.
    let mut manifest = Manifest::load(paths).unwrap();
    manifest.entries[0].superseded = true;
    manifest.save(paths).unwrap();

    let done = nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    let Nightly::Done { retention, .. } = done else {
        panic!("{done:?}");
    };
    assert_eq!(retention.deleted, vec![stale.file.clone()]);
    assert!(!paths.resolve(&stale.file).exists());
    assert!(
        Manifest::load(paths)
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.file != stale.file)
    );
}

#[test]
fn restore_moves_a_stale_wal_aside_with_the_live_file() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    drop(store);
    // The live file is gone, but a WAL from it is still there.
    fs::remove_file(&paths.db).unwrap();
    let wal = with_suffix(&paths.db, "-wal");
    fs::write(&wal, b"stale wal").unwrap();

    let restored = restore(paths, None, false, at("2026-11-02T09:00:00Z")).unwrap();
    let moved_to = restored.moved_to.unwrap();
    assert!(!wal.exists());
    assert_eq!(
        fs::read(with_suffix(&moved_to, "-wal")).unwrap(),
        b"stale wal"
    );
    assert_eq!(integrity(&paths.db), "ok");
}

#[cfg(unix)]
#[test]
fn backups_and_exports_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    export_jsonl(store.conn(), &export, Timestamp::now()).unwrap();
    for dir in [&paths.backups, &export] {
        let mode = fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
}
