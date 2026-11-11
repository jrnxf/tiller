//! `tiller export --jsonl <dir>` and its import into a fresh `state.db`
//! (`protocol.md` section 9; `state.md` sections 7 and 10).
//!
//! An export is a directory: one `<table>.jsonl` per table, one JSON object
//! per row keyed by column name, in rowid order, and `manifest.json`,
//! written last, with the schema version, host, boundary and each file's row
//! count and sha256. The import rebuilds a database from it, on the same
//! schema version as the export, then migrates up: a way to carry a host's
//! records across a schema the backup files cannot cross, and to move a
//! host's ownership (M6). It refuses another boundary's export, so memory
//! records never cross boundaries (`memory.md` section 8).
//!
//! Export data is work data: it stays on the host and is never committed.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

use super::manifest::{ExportRecord, Manifest};
use super::{
    DaemonLock, Paths, create_private_dir, facts, hash_file, hex, io_error, remove_if_exists,
    sync_dir, write_synced,
};
use crate::domain::Boundary;
use crate::error::{Error, Result};
use crate::migrate;
use crate::time::Timestamp;

/// The export's format tag; an import refuses any other.
pub const EXPORT_FORMAT: &str = "tiller-export/v1";

const MANIFEST: &str = "manifest.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub format: String,
    pub schema_version: usize,
    pub host_id: Option<String>,
    pub boundary: Option<String>,
    pub max_seq: i64,
    pub exported_at: Timestamp,
    pub tables: Vec<ExportedTable>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportedTable {
    pub name: String,
    pub file: String,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Imported {
    /// The export's schema version, before the import migrated up.
    pub from_version: usize,
    pub schema_version: usize,
    pub rows: u64,
    pub host_id: Option<String>,
    pub boundary: Option<String>,
}

/// Exports every table of `conn` to `dir`, which must be absent or empty,
/// from one read snapshot, and records the export in the backups manifest
/// of `paths`.
pub fn export_jsonl(
    conn: &Connection,
    paths: &Paths,
    dir: &Path,
    now: Timestamp,
) -> Result<ExportManifest> {
    // An unreadable manifest refuses before any file is written: the export
    // could not be recorded.
    let _lock = super::ManifestLock::acquire(paths)?;
    let mut backups = Manifest::load(paths)?;
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(Error::refused(
            "export",
            dir.display().to_string(),
            "the directory is not empty; name a new one",
        ));
    }
    // The export holds every record, memory included: owner-only.
    create_private_dir(dir)?;

    // One deferred transaction: every file is read from the same snapshot.
    let snapshot = conn.unchecked_transaction()?;
    let facts = facts(&snapshot)?;
    let mut tables = Vec::new();
    for name in table_names(&snapshot)? {
        let columns = columns(&snapshot, &name)?;
        let file = format!("{name}.jsonl");
        let path = dir.join(&file);
        let mut out = HashingWriter::new(File::create(&path).map_err(io_error("create", &path))?);
        let select = format!(
            "SELECT {} FROM {} ORDER BY rowid",
            columns
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(", "),
            quote(&name)
        );
        let mut statement = snapshot.prepare(&select)?;
        let mut rows = statement.query([])?;
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            let mut object = Map::new();
            for (index, column) in columns.iter().enumerate() {
                object.insert(column.clone(), to_json(row.get_ref(index)?, &name, column)?);
            }
            serde_json::to_writer(&mut out, &object)?;
            out.write_all(b"\n").map_err(io_error("write", &path))?;
            count += 1;
        }
        let sha256 = out.finish().map_err(io_error("write", &path))?;
        tables.push(ExportedTable {
            name,
            file,
            rows: count,
            sha256,
        });
    }
    drop(snapshot);

    let manifest = ExportManifest {
        format: EXPORT_FORMAT.into(),
        schema_version: facts.schema_version,
        host_id: facts.host_id,
        boundary: facts.boundary,
        max_seq: facts.max_seq,
        exported_at: now,
        tables,
    };
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    write_synced(&dir.join(MANIFEST), text.as_bytes())?;
    backups.exports.push(ExportRecord {
        host_id: manifest.host_id.clone(),
        boundary: manifest.boundary.clone(),
        max_seq: manifest.max_seq,
        at: now,
    });
    backups.save(paths)?;
    Ok(manifest)
}

/// Builds a new `state.db` at `paths.db` from the export in `dir`. Refuses
/// while the daemon runs, when `state.db` already exists, when a file does
/// not match the export's manifest, or when the export's boundary is not
/// `boundary`. Nothing is written at `paths.db` unless every row imports and
/// every foreign key holds.
pub fn import_jsonl(dir: &Path, paths: &Paths, boundary: Boundary) -> Result<Imported> {
    // A new host has no home yet; only an import may create it.
    if !paths.home.as_os_str().is_empty() && !paths.home.exists() {
        create_private_dir(&paths.home)?;
    }
    let _lock = DaemonLock::acquire(paths)?;
    if paths.db.exists() {
        return Err(Error::refused(
            "state.db",
            paths.db.display().to_string(),
            "already exists; an export imports only into a new state.db",
        ));
    }
    let manifest = read_manifest(dir)?;
    if manifest.boundary.as_deref() != Some(boundary.as_str()) {
        return Err(Error::refused(
            "export",
            dir.display().to_string(),
            format!(
                "its boundary is {}, this host's is {boundary}",
                manifest.boundary.as_deref().unwrap_or("unset")
            ),
        ));
    }
    for table in &manifest.tables {
        let path = table_path(dir, &table.file)?;
        let (_, sha256) = hash_file(&path)?;
        if sha256 != table.sha256 {
            return Err(Error::refused(
                "export",
                &table.file,
                "sha256 differs from the export's manifest",
            ));
        }
    }

    let staged = paths.beside_db(".import.tmp");
    remove_if_exists(&staged)?;
    let result = build(dir, &manifest, &staged);
    match result {
        Ok(imported) => {
            fs::rename(&staged, &paths.db).map_err(io_error("rename", &staged))?;
            sync_dir(&paths.db)?;
            Ok(imported)
        }
        Err(error) => {
            let _ = fs::remove_file(&staged);
            Err(error)
        }
    }
}

fn read_manifest(dir: &Path) -> Result<ExportManifest> {
    let path = dir.join(MANIFEST);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Error::refused(
                "export",
                dir.display().to_string(),
                "has no manifest.json; the export is incomplete",
            ));
        }
        Err(error) => return Err(io_error("read", &path)(error)),
    };
    let manifest: ExportManifest = serde_json::from_str(&text)
        .map_err(|error| Error::Usage(format!("{}: {error}", path.display())))?;
    if manifest.format != EXPORT_FORMAT {
        return Err(Error::Usage(format!(
            "{} is format {:?}; this tiller reads {EXPORT_FORMAT}",
            path.display(),
            manifest.format
        )));
    }
    if manifest.schema_version > migrate::LATEST {
        return Err(Error::SchemaTooNew {
            found: manifest.schema_version,
            binary: env!("CARGO_PKG_VERSION").to_owned(),
        });
    }
    Ok(manifest)
}

fn build(dir: &Path, manifest: &ExportManifest, staged: &Path) -> Result<Imported> {
    let mut conn = Connection::open(staged)?;
    migrate::to_version(&mut conn, manifest.schema_version)?;
    // Rows go in table by table, so keys are checked once, all together,
    // before the commit.
    conn.pragma_update(None, "foreign_keys", false)?;

    let tx = conn.transaction()?;
    let schema_tables = table_names(&tx)?;
    let mut exported: Vec<&str> = manifest.tables.iter().map(|t| t.name.as_str()).collect();
    exported.sort_unstable();
    if exported != schema_tables.iter().map(String::as_str).collect::<Vec<_>>() {
        return Err(Error::refused(
            "export",
            dir.display().to_string(),
            format!(
                "its tables are not schema {}'s: {}",
                manifest.schema_version,
                exported.join(", ")
            ),
        ));
    }

    let mut total = 0;
    for table in &manifest.tables {
        let columns = columns(&tx, &table.name)?;
        // Replace what the migration seeded (meta.schema_version) with the
        // export's rows.
        tx.execute(&format!("DELETE FROM {}", quote(&table.name)), [])?;
        let insert = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quote(&table.name),
            columns
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(", "),
            (1..=columns.len())
                .map(|n| format!("?{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut statement = tx.prepare(&insert)?;
        let path = table_path(dir, &table.file)?;
        let reader = BufReader::new(File::open(&path).map_err(io_error("open", &path))?);
        let mut rows = 0u64;
        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(io_error("read", &path))?;
            let at = || format!("{}:{}", table.file, index + 1);
            let object: Map<String, Value> = serde_json::from_str(&line)
                .map_err(|error| Error::Usage(format!("{}: {error}", at())))?;
            if object.len() != columns.len() || columns.iter().any(|c| !object.contains_key(c)) {
                return Err(Error::refused(
                    "export",
                    at(),
                    format!("the row's columns are not {}'s", table.name),
                ));
            }
            let values = columns
                .iter()
                .map(|column| {
                    to_sql(&object[column]).ok_or_else(|| {
                        Error::Usage(format!("{}: column {column} is not a scalar", at()))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            statement.execute(params_from_iter(values))?;
            rows += 1;
        }
        if rows != table.rows {
            return Err(Error::refused(
                "export",
                &table.file,
                format!("has {rows} rows, its manifest says {}", table.rows),
            ));
        }
        total += rows;
    }

    let broken: Vec<String> = {
        let mut statement = tx.prepare("PRAGMA foreign_key_check")?;
        let rows = statement.query_map([], |row| {
            Ok(format!(
                "{} row {} -> {}",
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if !broken.is_empty() {
        return Err(Error::refused(
            "export",
            dir.display().to_string(),
            format!("foreign keys do not hold: {}", broken.join("; ")),
        ));
    }
    let imported = facts(&tx)?;
    if imported.schema_version != manifest.schema_version
        || imported.host_id != manifest.host_id
        || imported.boundary != manifest.boundary
        || imported.max_seq != manifest.max_seq
    {
        return Err(Error::refused(
            "export",
            dir.display().to_string(),
            "its rows do not match its manifest's host, boundary or event seq",
        ));
    }
    tx.commit()?;

    migrate::up(&mut conn)?;
    let schema_version = migrate::current_version(&conn)?;
    conn.close().map_err(|(_, error)| Error::from(error))?;
    File::open(staged)
        .and_then(|file| file.sync_all())
        .map_err(io_error("sync", staged))?;
    Ok(Imported {
        from_version: manifest.schema_version,
        schema_version,
        rows: total,
        host_id: imported.host_id,
        boundary: imported.boundary,
    })
}

/// Every table, sorted by name; SQLite's own tables are not records.
fn table_names(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = statement.query_map([], |row| row.get(0))?;
    Ok(names.collect::<rusqlite::Result<_>>()?)
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
    let names = statement.query_map([table], |row| row.get(0))?;
    Ok(names.collect::<rusqlite::Result<_>>()?)
}

/// A table's file inside the export: a bare file name, never a path out.
fn table_path(dir: &Path, file: &str) -> Result<PathBuf> {
    let name = Path::new(file);
    if name.components().count() != 1 || name.file_name().is_none() {
        return Err(Error::Usage(format!(
            "export file name {file:?} is not a bare name"
        )));
    }
    Ok(dir.join(name))
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn to_json(value: ValueRef<'_>, table: &str, column: &str) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::from(n),
        ValueRef::Real(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(bytes) => Value::String(
            std::str::from_utf8(bytes)
                .map_err(|error| Error::Internal(format!("{table}.{column}: {error}")))?
                .to_owned(),
        ),
        ValueRef::Blob(_) => {
            return Err(Error::Internal(format!(
                "{table}.{column} holds a blob, which the JSONL export does not carry"
            )));
        }
    })
}

fn to_sql(value: &Value) -> Option<SqlValue> {
    match value {
        Value::Null => Some(SqlValue::Null),
        Value::String(text) => Some(SqlValue::Text(text.clone())),
        Value::Number(n) => n
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| n.as_f64().map(SqlValue::Real)),
        Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Writes through a buffer and hashes every byte written.
struct HashingWriter {
    inner: BufWriter<File>,
    hasher: Sha256,
}

impl HashingWriter {
    fn new(file: File) -> Self {
        Self {
            inner: BufWriter::new(file),
            hasher: Sha256::new(),
        }
    }

    fn finish(mut self) -> io::Result<String> {
        self.inner.flush()?;
        self.inner.get_ref().sync_all()?;
        Ok(hex(&self.hasher.finalize()))
    }
}

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
