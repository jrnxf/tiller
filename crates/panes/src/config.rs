//! The `[holder]` table of `config.toml` (`terminal-holder.md` 2.3, 2.6 and
//! 11.1): where the socket is, and the size a pane starts at.
//!
//! ```toml
//! [holder]
//! socket = "/home/user/.tiller/run/holder.sock"
//! default_size = "120x40"
//! ```

use std::path::{Path, PathBuf};

use crate::message::Size;

/// The `[holder]` keys, each unset when the file does not set it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HolderConfig {
    /// `[holder] socket`, an absolute path.
    pub socket: Option<PathBuf>,
    /// `[holder] default_size`, as `"<cols>x<rows>"`.
    pub default_size: Option<Size>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} does not parse: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("config.toml [holder] {key}: {message}")]
    Key { key: &'static str, message: String },
}

/// `<home>/config.toml`.
pub fn config_path(home: &Path) -> PathBuf {
    home.join("config.toml")
}

/// The `[holder]` table of `<home>/config.toml`. A missing file, or one
/// with no `[holder]` table, sets nothing.
pub fn load(home: &Path) -> Result<HolderConfig, ConfigError> {
    let path = config_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HolderConfig::default());
        }
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    let table: toml::Table = text
        .parse()
        .map_err(|error: toml::de::Error| ConfigError::Parse {
            path,
            message: error.message().to_owned(),
        })?;
    from_table(&table)
}

/// The `[holder]` table of a parsed `config.toml`.
pub fn from_table(table: &toml::Table) -> Result<HolderConfig, ConfigError> {
    let Some(holder) = table.get("holder") else {
        return Ok(HolderConfig::default());
    };
    let holder = holder.as_table().ok_or(ConfigError::Key {
        key: "table",
        message: "[holder] is a table".into(),
    })?;
    let mut config = HolderConfig::default();
    for (key, value) in holder {
        match key.as_str() {
            "socket" => config.socket = Some(socket(value)?),
            "default_size" => config.default_size = Some(default_size(value)?),
            other => {
                return Err(ConfigError::Key {
                    key: "table",
                    message: format!(
                        "{other:?} is not a key; the keys are socket and default_size"
                    ),
                });
            }
        }
    }
    Ok(config)
}

fn socket(value: &toml::Value) -> Result<PathBuf, ConfigError> {
    let bad = |message: &str| ConfigError::Key {
        key: "socket",
        message: message.into(),
    };
    let path = value
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| bad("is a path"))?;
    if !path.is_absolute() {
        return Err(bad("is an absolute path"));
    }
    Ok(path)
}

fn default_size(value: &toml::Value) -> Result<Size, ConfigError> {
    let bad = || ConfigError::Key {
        key: "default_size",
        message: format!("is \"<cols>x<rows>\", such as \"120x40\", not {value}"),
    };
    let (cols, rows) = value
        .as_str()
        .and_then(|text| text.split_once('x'))
        .ok_or_else(bad)?;
    Ok(Size {
        cols: cols.trim().parse().map_err(|_| bad())?,
        rows: rows.trim().parse().map_err(|_| bad())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<HolderConfig, ConfigError> {
        from_table(&text.parse().unwrap())
    }

    #[test]
    fn no_holder_table_sets_nothing() {
        assert_eq!(parse("").unwrap(), HolderConfig::default());
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load(home.path()).unwrap(), HolderConfig::default());
    }

    #[test]
    fn the_holder_table_sets_the_socket_and_the_size() {
        let config =
            parse("[holder]\nsocket = \"/home/user/holder.sock\"\ndefault_size = \"100x30\"")
                .unwrap();
        assert_eq!(
            config.socket.as_deref(),
            Some(Path::new("/home/user/holder.sock"))
        );
        assert_eq!(
            config.default_size,
            Some(Size {
                cols: 100,
                rows: 30
            })
        );
    }

    #[test]
    fn a_bad_holder_key_is_refused_by_name() {
        for (text, key) in [
            ("[holder]\nsocket = \"run/holder.sock\"", "socket"),
            ("[holder]\nsocket = 3", "socket"),
            ("[holder]\ndefault_size = \"wide\"", "default_size"),
            ("[holder]\ndefault_size = \"120x\"", "default_size"),
            ("[holder]\nsokcet = \"/home/user/h.sock\"", "sokcet"),
        ] {
            let error = parse(text).unwrap_err().to_string();
            assert!(error.contains(key), "{text}: {error}");
        }
    }
}
