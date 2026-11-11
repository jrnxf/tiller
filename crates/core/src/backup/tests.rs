use std::fs;
use std::path::Path;

use rusqlite::Connection;

use super::*;
use crate::domain::*;
use crate::state::asks::{NewAsk, Resolution};
use crate::state::attention::NewAttention;
use crate::state::cards::NewCard;
use crate::state::projects::{HostEdit, Init, NewProject, NewRemoteHost};
use crate::state::runs::NewOrchestrator;
use crate::state::{get, list};
use crate::store::{Actor, Ctx, Store};

fn at(text: &str) -> Timestamp {
    Timestamp::from_jiff(text.parse().unwrap())
}

/// The nightly pass as the daemon runs it, on a host whose clock is UTC.
fn nightly(store: &mut Store, paths: &Paths, now: Timestamp) -> Result<Nightly> {
    super::nightly(store, paths, now, &jiff::tz::TimeZone::UTC)
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
    let column = store
        .conn()
        .query_row(
            "SELECT \"column\", needs_you FROM card_column WHERE id = ?1",
            [&card.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .unwrap();
    assert_eq!(column, ("queued".to_owned(), true));
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
    let Nightly::Done(Taken { entry: newest, .. }) =
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
    manifest
        .save(paths, &ManifestLock::acquire(paths).unwrap())
        .unwrap();

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
    let Nightly::Done(Taken { retention, .. }) = done else {
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
    nightly(&mut other_store, &other.paths, at("2026-11-02T03:00:00Z")).unwrap();
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

    // Each attempt's copy fails.
    let night_two = night_one.plus_minutes(24 * 60);
    let attempt = |store: &mut Store, now: Timestamp| {
        FAIL_COPIES.set(true);
        let result = nightly(store, paths, now);
        FAIL_COPIES.set(false);
        result.unwrap()
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
    let first = export_jsonl(store.conn(), &host.paths, &export, Timestamp::now()).unwrap();
    assert_eq!(first.boundary.as_deref(), Some("work"));
    let events = first.tables.iter().find(|t| t.name == "events").unwrap();
    assert!(events.rows > 0);
    assert_eq!(
        export_jsonl(store.conn(), &host.paths, &export, Timestamp::now())
            .unwrap_err()
            .code(),
        "refused",
        "an export never writes into a used directory"
    );
    let recorded = Manifest::load(paths).unwrap().exports;
    assert_eq!(recorded.len(), 1, "the refused export is not recorded");
    assert_eq!(recorded[0].boundary.as_deref(), Some("work"));

    let target = self::host();
    let imported = import_jsonl(&export, &target.paths, Boundary::Work).unwrap();
    assert_eq!(imported.schema_version, crate::migrate::LATEST);
    assert_eq!(
        imported.rows,
        first.tables.iter().map(|t| t.rows).sum::<u64>()
    );
    assert_eq!(integrity(&target.paths.db), "ok");

    let reopened = Store::open(&target.paths.db).unwrap();
    let again = export_jsonl(
        reopened.conn(),
        &target.paths,
        &dir.path().join("again"),
        Timestamp::now(),
    )
    .unwrap();
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
    export_jsonl(store.conn(), &host.paths, &export, Timestamp::now()).unwrap();

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
    export_jsonl(store.conn(), &host.paths, &export, Timestamp::now()).unwrap();

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
    let pre = format!("state.db.pre-{}-0", crate::migrate::LATEST);
    assert!(error.to_string().contains(&pre), "{error}");

    let store = Store::open(&paths.db).unwrap();
    assert!(get::<Card>(store.conn(), "card", &card.id).is_ok());
    let manifest = Manifest::load(paths).unwrap();
    let copy = manifest
        .entries
        .iter()
        .find(|entry| entry.kind == EntryKind::PreMigration)
        .unwrap();
    assert_eq!(copy.file, pre);
    assert_eq!(copy.schema_version, crate::migrate::LATEST);
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
    assert_eq!((down.from, down.to), (crate::migrate::LATEST, 0));
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
    let Nightly::Done(Taken { retention, .. }) = done else {
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
    let Taken { entry, .. } = manual;
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
    let Taken { entry: stale, .. } = first;
    // As if a pass marked it superseded and died before deleting the file.
    let mut manifest = Manifest::load(paths).unwrap();
    manifest.entries[0].superseded = true;
    manifest
        .save(paths, &ManifestLock::acquire(paths).unwrap())
        .unwrap();

    let done = nightly(&mut store, paths, at("2026-11-02T03:00:00Z")).unwrap();
    let Nightly::Done(Taken { retention, .. }) = done else {
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
    export_jsonl(store.conn(), &host.paths, &export, Timestamp::now()).unwrap();
    for dir in [&paths.backups, &export] {
        let mode = fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
}

#[test]
fn a_restored_database_is_owner_only_whatever_the_backup_mode() {
    use std::os::unix::fs::PermissionsExt;

    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let made = backup(store.conn(), paths, at("2026-11-01T15:00:00Z")).unwrap();
    let Taken { entry, .. } = made;
    drop(store);
    fs::set_permissions(
        paths.resolve(&entry.file),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    restore(paths, None, false, at("2026-11-01T16:00:00Z")).unwrap();
    let mode = fs::metadata(&paths.db).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// B14: the daemon starting and a CLI command right after an upgrade both
/// open the old database at once. The migration lock lets one take the copy
/// and migrate while the other waits, re-reads the version and finds nothing
/// to do. Both opens succeed, the database ends at the latest schema with its
/// rows, and the manifest lists the one copy, which verifies.
#[test]
fn concurrent_first_open_of_an_old_database_succeeds_in_both() {
    const OLD: usize = 11;
    let seed = host();
    let mut store = Store::open(&seed.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let card = store
        .card_create(&owner(), None, new_card("survives"), None)
        .unwrap()
        .value;
    drop(store);
    migrate(&seed.paths, OLD, Timestamp::now()).unwrap();

    for trial in 0..20 {
        let host = host();
        let paths = host.paths.clone();
        fs::copy(&seed.paths.db, &paths.db).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let opens: Vec<_> = (0..2)
            .map(|_| {
                let db = paths.db.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    Store::open(&db).map(drop)
                })
            })
            .collect();
        for open in opens {
            let opened = open.join().unwrap();
            assert!(opened.is_ok(), "trial {trial}: {opened:?}");
        }

        let store = Store::open_current(&paths.db).unwrap();
        assert!(get::<Card>(store.conn(), "card", &card.id).is_ok());
        let manifest = Manifest::load(&paths).unwrap();
        let copies: Vec<_> = manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::PreMigration)
            .collect();
        assert_eq!(copies.len(), 1, "trial {trial}: {copies:?}");
        assert_eq!(
            copies[0].file,
            format!("state.db.pre-{OLD}-{}", crate::migrate::LATEST)
        );
        assert_eq!(copies[0].schema_version, OLD);
        assert!(verify(&paths, copies[0]).is_ok());
        let leftovers: Vec<String> = fs::read_dir(&paths.home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert_eq!(leftovers, Vec::<String>::new(), "trial {trial}");
    }
}

/// `host.edit --boundary`: allowed while nothing records the boundary,
/// refused once memory, a backup or an export does (`memory.md` section 8).
#[test]
fn a_boundary_changes_only_while_nothing_records_it() {
    let to = |boundary: Boundary| HostEdit {
        boundary: Some(boundary),
        ..HostEdit::default()
    };
    let boundary = |store: &Store| crate::state::projects::meta(store.conn(), "boundary").unwrap();
    let edit = |store: &mut Store, b: Boundary| {
        let host = store.this_host().unwrap();
        store.host_edit(&owner(), &host.id, host.rev, to(b))
    };

    // Unused: the change writes meta.boundary and one host.edited event.
    let host = host();
    let mut store = Store::open(&host.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    let changed = edit(&mut store, Boundary::Work).unwrap();
    assert_eq!(boundary(&store).as_deref(), Some("work"));
    assert_eq!(changed.events.len(), 1);
    let payload: String = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE seq = ?1",
            [changed.events[0]],
            |row| row.get(0),
        )
        .unwrap();
    assert!(payload.contains(r#""boundary":"work""#), "{payload}");
    assert!(
        edit(&mut store, Boundary::Work).unwrap().events.is_empty(),
        "the same boundary writes nothing"
    );

    // A remote host has no boundary here.
    let remote = store
        .host_add(
            &owner(),
            NewRemoteHost {
                label: "host-b".into(),
                herdr_machine: "host-b".into(),
            },
        )
        .unwrap()
        .value;
    let error = store
        .host_edit(&owner(), &remote.id, remote.rev, to(Boundary::Personal))
        .unwrap_err();
    assert!(error.to_string().contains("remote host keeps"), "{error}");

    // An export records it: refused, and nothing changes.
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    export_jsonl(store.conn(), &host.paths, &export, Timestamp::now()).unwrap();
    let error = edit(&mut store, Boundary::Personal).unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(
        error
            .to_string()
            .contains("an export already records the work boundary"),
        "{error}"
    );
    assert_eq!(boundary(&store).as_deref(), Some("work"));

    // A backup records it.
    let other = self::host();
    let mut store = Store::open(&other.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    backup(store.conn(), &other.paths, Timestamp::now()).unwrap();
    let error = edit(&mut store, Boundary::Work).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a backup already records the personal boundary"),
        "{error}"
    );

    // A memory row records it.
    let third = self::host();
    let mut store = Store::open(&third.paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    store
        .conn()
        .execute_batch(
            "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo,
                                         canonical_path, apply_method, load_mode, added_at)
             VALUES ('src', 'alpha', 'personal', 'tiller_doc', 'alpha.md', '/home/user/memory',
                     'alpha.md', 'tiller_commit', 'rendered', '2026-11-01T00:00:00.000Z');",
        )
        .unwrap();
    let error = edit(&mut store, Boundary::Work).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("memory already records the personal boundary"),
        "{error}"
    );
    assert_eq!(boundary(&store).as_deref(), Some("personal"));
}

/// A nightly pass and a backup by hand (or two by hand) read the manifest,
/// copy, and save it back. Without a lock the second save drops the first
/// one's entry. Both entries must be listed.
#[test]
fn two_backups_at_once_both_land_in_the_manifest() {
    for trial in 0..10 {
        let host = host();
        let mut store = Store::open(&host.paths.db).unwrap();
        setup(&mut store, "test-host", Boundary::Personal);
        drop(store);
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let runs: Vec<_> = (0..2)
            .map(|n| {
                let paths = host.paths.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&paths.db).unwrap();
                    start.wait();
                    backup(
                        store.conn(),
                        &paths,
                        at("2026-11-11T10:00:00Z").plus_minutes(n),
                    )
                })
            })
            .collect();
        start.wait();
        for run in runs {
            run.join().unwrap().unwrap();
        }
        let manifest = Manifest::load(&host.paths).unwrap();
        assert_eq!(manifest.entries.len(), 2, "trial {trial}: {manifest:?}");
        let files = fs::read_dir(&host.paths.backups)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".db")
            })
            .count();
        assert_eq!(files, 2, "trial {trial}");
    }
}

/// A process that holds the migration lock and never lets go (stuck in a
/// copy, or stopped in a debugger) must not block every other `Store::open`
/// for ever. The open waits its limit, then refuses with the holder's pid,
/// and writes nothing. Once the holder lets go, the open migrates.
#[test]
fn an_open_refuses_with_the_holder_once_the_migration_lock_wait_ends() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    drop(store);
    let old = crate::migrate::LATEST - 1;
    migrate(paths, old, Timestamp::now()).unwrap();

    let held = MigrationLock::acquire(paths).unwrap();
    let wait = Duration::from_millis(200);
    MIGRATION_WAIT.set(wait);
    let started = Instant::now();
    let error = Store::open(&paths.db).map(drop).unwrap_err();
    let waited = started.elapsed();
    assert!(waited >= wait, "gave up after {waited:?}");
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    assert_eq!(error.code(), "refused", "{error}");
    let text = error.to_string();
    assert!(
        text.contains(&format!("pid {} (", std::process::id())),
        "{text}"
    );
    assert!(text.contains("state.db.migrate.lock"), "{text}");
    let conn = Connection::open(&paths.db).unwrap();
    assert_eq!(crate::migrate::current_version(&conn).unwrap(), old);
    drop(conn);

    // A holder that writes no line (an older tiller) is not named after the
    // one that let go.
    drop(held);
    let raw = File::open(paths.migration_lock()).unwrap();
    raw.lock().unwrap();
    let error = Store::open(&paths.db).map(drop).unwrap_err();
    assert!(
        error.to_string().contains(": another process has held it"),
        "{error}"
    );

    drop(raw);
    drop(Store::open(&paths.db).unwrap());
    MIGRATION_WAIT.set(MIGRATION_LOCK_WAIT);
}

/// Writes `path` as a file last written `age` ago.
fn write_aged(path: &Path, age: Duration) {
    fs::write(path, b"partial").unwrap();
    let at = std::time::SystemTime::now() - age;
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// A crash between a temporary file's create and its rename leaves
/// `<file>.<ulid>.tmp` behind: a partial backup copy or manifest in
/// `backups/`, or a partial pre-migration copy beside `state.db`, as big as
/// the database. The next writer removes them when it takes the manifest
/// lock, once they have gone unwritten for an hour, and leaves every other
/// file alone. A younger one may be an older tiller's copy in flight, which
/// writes without the manifest lock (R11).
#[test]
fn the_next_backup_removes_temporary_files_a_crash_left() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    backup(store.conn(), paths, at("2026-11-10T03:00:00Z")).unwrap();

    let ulid = crate::ids::new_ulid();
    let stale = [
        paths
            .backups
            .join(format!("20261111T030000000Z.db.{ulid}.tmp")),
        paths
            .backups
            .join(format!("20261111T030000000Z.db.{ulid}.tmp-journal")),
        paths.backups.join(format!("manifest.json.{ulid}.tmp")),
        paths.home.join(format!("state.db.pre-15-16.{ulid}.tmp")),
    ];
    let kept = [
        paths.backups.join("notes.tmp"),
        paths.backups.join(format!("{ulid}.tmp")),
        paths.home.join("state.db.restore.tmp"),
        paths.home.join(format!("config.toml.{ulid}.tmp")),
        paths.home.join(format!(".{ulid}.tmp")),
    ];
    let in_flight = [
        paths
            .backups
            .join(format!("20261111T040000000Z.db.{ulid}.tmp")),
        paths.home.join(format!("state.db.pre-16-17.{ulid}.tmp")),
    ];
    let old = STALE_TMP_AGE + Duration::from_secs(60);
    for path in stale.iter().chain(&kept) {
        write_aged(path, old);
    }
    for path in &in_flight {
        write_aged(path, STALE_TMP_AGE - Duration::from_secs(60));
    }

    backup(store.conn(), paths, at("2026-11-11T03:00:00Z")).unwrap();
    for path in &stale {
        assert!(!path.exists(), "{} is left", path.display());
    }
    for path in kept.iter().chain(&in_flight) {
        assert!(path.exists(), "{} was removed", path.display());
    }
    let manifest = Manifest::load(paths).unwrap();
    assert_eq!(manifest.entries.len(), 2);
    for entry in &manifest.entries {
        assert!(verify(paths, entry).is_ok());
    }
}

/// Every name [`unique_tmp`] writes is one the sweep reads as its own, with
/// SQLite's files beside it: the format lives in one place.
#[test]
fn every_unique_tmp_name_is_one_the_sweep_removes() {
    let host = host();
    let held = ManifestLock::acquire(&host.paths).unwrap();
    for dest in [
        host.paths.backups.join("manifest.json"),
        host.paths.home.join("state.db.pre-16-17"),
    ] {
        let tmp = unique_tmp(&dest, &held);
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(is_unique_tmp(&name), "{name}");
        assert!(is_unique_tmp(&format!("{name}-journal")), "{name}");
        assert_eq!(tmp.parent(), dest.parent());
    }
}

/// A bare relative `--db state.db` has the empty path as its parent: the
/// sweep reads `.` for it, not nothing.
#[test]
fn a_bare_relative_db_sits_in_the_current_directory() {
    assert_eq!(db_dir(Path::new("state.db")), Path::new("."));
    assert_eq!(db_dir(Path::new("x/state.db")), Path::new("x"));
    assert_eq!(
        db_dir(Path::new("/home/user/state.db")),
        Path::new("/home/user")
    );
}

/// The holder line names the program and the parsed subcommand, never a
/// flag's value such as the `--db` path, and takes a program path that is
/// not UTF-8.
#[test]
fn the_holder_line_names_the_subcommand_and_takes_any_program_path() {
    assert_eq!(
        holder_line_of(4242, Some("/home/user/bin/tiller".into()), Some("migrate")),
        "pid 4242 (tiller migrate)\n"
    );
    assert_eq!(holder_line_of(7, None, None), "pid 7 (tiller)\n");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let program = std::ffi::OsStr::from_bytes(b"/home/user/\xff/tiller").to_os_string();
        assert_eq!(
            holder_line_of(7, Some(program), Some("daemon")),
            "pid 7 (tiller daemon)\n"
        );
    }
}

/// The first pass of the daemon's timer sets the first night, the next
/// 03:00 host time, and takes nothing; the pass at that hour takes the
/// first nightly backup.
#[test]
fn the_first_pass_schedules_the_first_night_at_three() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);

    let first = nightly(&mut store, paths, at("2026-11-11T16:00:00Z")).unwrap();
    let tonight = at("2026-11-12T03:00:00Z");
    assert_eq!(first, Nightly::NotDue { next: tonight });
    let manifest = Manifest::load(paths).unwrap();
    assert!(manifest.entries.is_empty(), "{manifest:?}");
    assert_eq!(next_due(&manifest), Some(tonight));

    let early = nightly(&mut store, paths, at("2026-11-12T02:59:00Z")).unwrap();
    assert_eq!(early, Nightly::NotDue { next: tonight });
    let done = nightly(&mut store, paths, at("2026-11-12T03:00:40Z")).unwrap();
    let Nightly::Done(Taken { entry, .. }) = done else {
        panic!("{done:?}");
    };
    assert_eq!(entry.kind, EntryKind::Nightly);
    assert_eq!(
        next_due(&Manifest::load(paths).unwrap()),
        Some(at("2026-11-13T03:00:00Z"))
    );
}

/// A daemon that was down for nights takes one backup at its next pass,
/// not one per missed night, and the next night is again at 03:00.
#[test]
fn missed_nights_are_made_up_by_one_backup_at_the_next_pass() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-01T03:00:00Z")).unwrap();

    // Down from the 1st until 09:00 on the 4th.
    let back = at("2026-11-04T09:00:00Z");
    let done = nightly(&mut store, paths, back).unwrap();
    assert!(matches!(done, Nightly::Done { .. }), "{done:?}");
    let again = nightly(&mut store, paths, back.plus_minutes(1)).unwrap();
    let next = at("2026-11-05T03:00:00Z");
    assert_eq!(again, Nightly::NotDue { next });
    let nightly_entries = Manifest::load(paths)
        .unwrap()
        .entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Nightly)
        .count();
    assert_eq!(nightly_entries, 2);
}

/// A make-up backup taken just before 03:00 is that night's: the next one
/// is the following night, not 30 minutes later.
#[test]
fn a_make_up_just_before_the_hour_is_that_nights_backup() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-01T03:00:00Z")).unwrap();

    let back = at("2026-11-04T02:30:00Z");
    let done = nightly(&mut store, paths, back).unwrap();
    assert!(matches!(done, Nightly::Done { .. }), "{done:?}");
    let next = at("2026-11-05T03:00:00Z");
    assert_eq!(
        nightly(&mut store, paths, at("2026-11-04T03:00:00Z")).unwrap(),
        Nightly::NotDue { next }
    );
}

/// A copy by hand after a failed night does not end that night: the retry
/// an hour later still runs, and the failure stays on record until a
/// nightly copy verifies.
#[test]
fn a_backup_by_hand_does_not_end_a_failed_night() {
    let host = host();
    let paths = &host.paths;
    let mut store = Store::open(&paths.db).unwrap();
    setup(&mut store, "test-host", Boundary::Personal);
    nightly(&mut store, paths, at("2026-11-01T03:00:00Z")).unwrap();

    // The night of the 2nd fails: the backups folder cannot take a file.
    let night = at("2026-11-02T03:00:00Z");
    let mut failure = Manifest::load(paths).unwrap();
    failure.failures.push(Failure {
        at: night,
        error: "disk full".into(),
        retry: false,
    });
    failure
        .save(paths, &ManifestLock::acquire(paths).unwrap())
        .unwrap();

    backup(store.conn(), paths, night.plus_minutes(10)).unwrap();
    let manifest = Manifest::load(paths).unwrap();
    assert_eq!(manifest.failures.len(), 1, "the night is not over");
    assert_eq!(
        nightly(&mut store, paths, night.plus_minutes(11)).unwrap(),
        Nightly::NotDue {
            next: night.plus_minutes(60)
        },
        "no second copy before the retry"
    );
    let retried = nightly(&mut store, paths, night.plus_minutes(60)).unwrap();
    assert!(matches!(retried, Nightly::Done { .. }), "{retried:?}");
    assert!(Manifest::load(paths).unwrap().failures.is_empty());
}

/// The night stays at 03:00 host time across a clock change: in Lisbon,
/// 03:00 is 03:00 UTC in winter and 02:00 UTC in summer.
#[test]
fn the_night_follows_the_host_clock_across_a_clock_change() {
    let lisbon = jiff::tz::TimeZone::posix("WET0WEST,M3.5.0/1,M10.5.0").unwrap();
    // Summer time starts at 01:00 UTC on Sunday 29 March 2026.
    let saturday = at("2026-03-28T03:00:00Z");
    assert_eq!(
        night_from(saturday, &lisbon, true),
        at("2026-03-29T02:00:00Z")
    );
    assert_eq!(night_from(saturday, &lisbon, false), saturday);
    // It ends at 01:00 UTC on Sunday 25 October 2026.
    assert_eq!(
        night_from(at("2026-10-24T02:00:00Z"), &lisbon, true),
        at("2026-10-25T03:00:00Z")
    );
}
