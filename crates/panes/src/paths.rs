//! Where the holder's socket lives (`terminal-holder.md` 2.3). The caller
//! resolves the Tiller home (`TILLER_HOME`) as it does for everything else.

use std::path::{Path, PathBuf};

/// The longest socket path macOS takes (`sun_path`), and so the longest the
/// holder accepts on every host.
pub const SUN_PATH_MAX: usize = 104;

/// The holder's run directory, `<home>/run`, kept at mode `0700`.
pub fn run_dir(home: &Path) -> PathBuf {
    home.join("run")
}

/// The holder's socket, `<home>/run/holder.sock`.
pub fn socket_path(home: &Path) -> PathBuf {
    run_dir(home).join("holder.sock")
}

#[derive(Debug, thiserror::Error)]
#[error(
    "the holder socket path {} is {len} bytes, over the {SUN_PATH_MAX}-byte limit; set a shorter TILLER_HOME",
    path.display()
)]
pub struct SocketPathTooLong {
    pub path: PathBuf,
    pub len: usize,
}

/// Checks a socket path against [`SUN_PATH_MAX`], counting its closing NUL.
pub fn check_socket_path(path: &Path) -> Result<(), SocketPathTooLong> {
    let len = path.as_os_str().len();
    if len + 1 > SUN_PATH_MAX {
        return Err(SocketPathTooLong {
            path: path.to_owned(),
            len,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_is_in_the_run_directory() {
        assert_eq!(
            socket_path(Path::new("/home/user/.tiller")),
            Path::new("/home/user/.tiller/run/holder.sock")
        );
    }

    #[test]
    fn a_socket_path_over_the_macos_limit_is_refused() {
        let fits = PathBuf::from(format!("/{}", "a".repeat(SUN_PATH_MAX - 2)));
        assert!(check_socket_path(&fits).is_ok());
        let over = PathBuf::from(format!("/{}", "a".repeat(SUN_PATH_MAX - 1)));
        let err = check_socket_path(&over).unwrap_err();
        assert_eq!(err.len, SUN_PATH_MAX);
        assert!(err.to_string().contains("TILLER_HOME"), "{err}");
    }
}
