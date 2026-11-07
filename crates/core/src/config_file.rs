//! `config.toml` on disk, as `tiller config set` and the board's Add a host
//! write it: read whole, one dotted key set, then written through a temporary
//! file and a rename, so a reader never sees half a file.

use std::io::Write;
use std::path::Path;

/// Why `config.toml` was not read or written.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config.toml does not parse: {0}")]
    Parse(String),
    #[error("not a config key: {0:?}")]
    BadKey(String),
    #[error("{0} is not a table in config.toml")]
    NotATable(String),
    #[error("config.toml cannot be written: {0}")]
    Serialize(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The file at `path` as a table; a missing file is an empty table.
pub fn load(path: &Path) -> Result<toml::Table, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|error| ConfigError::Parse(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => Err(error.into()),
    }
}

/// The value at dotted `key`, if every part of it is there.
pub fn get<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut parts = key.split('.');
    let mut current = table.get(parts.next()?)?;
    for part in parts {
        current = current.as_table()?.get(part)?;
    }
    Some(current)
}

/// Sets dotted `key` to `value`, making the tables on the way.
pub fn set(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<(), ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = parts
        .split_last()
        .filter(|_| parts.iter().all(|p| !p.is_empty()))
    else {
        return Err(ConfigError::BadKey(key.to_owned()));
    };
    let mut current = table;
    for part in parents {
        current = current
            .entry((*part).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| ConfigError::NotATable((*part).to_owned()))?;
    }
    current.insert((*last).to_owned(), value);
    Ok(())
}

/// Writes `table` to `path`.
pub fn store(path: &Path, table: &toml::Table) -> Result<(), ConfigError> {
    let text =
        toml::to_string_pretty(table).map_err(|error| ConfigError::Serialize(error.to_string()))?;
    Ok(write_atomic(path, &text)?)
}

/// Writes `text` through a temporary file in the same directory and renames
/// it, so a reader never sees half a file. A failed write leaves no
/// temporary file behind.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", crate::ids::new_ulid()));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_joins_the_file_and_keeps_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        assert!(load(&path).unwrap().is_empty());
        std::fs::write(&path, "[federation]\naccept = true\n").unwrap();
        let mut table = load(&path).unwrap();
        set(&mut table, "peers.host-b.ssh", "b.example.com".into()).unwrap();
        store(&path, &table).unwrap();
        let table = load(&path).unwrap();
        assert_eq!(get(&table, "federation.accept"), Some(&true.into()));
        assert_eq!(
            get(&table, "peers.host-b.ssh"),
            Some(&"b.example.com".into())
        );
        let names: Vec<_> = std::fs::read_dir(home.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left");
    }

    #[test]
    fn a_bad_key_or_file_is_refused() {
        let mut table: toml::Table = "peers = 3".parse().unwrap();
        assert!(matches!(
            set(&mut table, "peers.host-b", "x".into()),
            Err(ConfigError::NotATable(_))
        ));
        assert!(matches!(
            set(&mut table, "a..b", "x".into()),
            Err(ConfigError::BadKey(_))
        ));
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "[peers\n").unwrap();
        assert!(matches!(load(&path), Err(ConfigError::Parse(_))));
    }
}
