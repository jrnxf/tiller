//! The path a session's hooks and rules name for `tiller`. A session outlives
//! releases, so the path must not be the Nix store file this process runs from:
//! a release moves the store file and the open session would keep calling the
//! old one (`authority.md` section 7, "How roles reach the exempt binary").
//! The path is a link that follows upgrades, or the running binary when no
//! such link exists.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Where Nix puts the immutable copy of every release.
const STORE: &str = "/nix/store";

/// The running binary by a stable absolute path: the link it was invoked
/// through when that link resolves to this very file, else a profile link that
/// does, else its own canonical path (`current_exe`).
pub fn tiller_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let argv0 = std::env::args_os().next();
    let env = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    Ok(stable(
        &exe,
        argv0.as_deref(),
        env("PATH").as_deref(),
        env("HOME").as_deref(),
        env("USER").as_deref(),
    ))
}

fn stable(
    exe: &Path,
    argv0: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    user: Option<&std::ffi::OsStr>,
) -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(argv0) = argv0 {
        let argv0 = Path::new(argv0);
        if argv0.is_absolute() {
            candidates.push(argv0.to_path_buf());
        } else if argv0.components().count() == 1 {
            // Invoked by name: the first `PATH` entry that holds it.
            candidates.extend(
                path.into_iter()
                    .flat_map(std::env::split_paths)
                    .map(|dir| dir.join(argv0)),
            );
        }
    }
    if let Some(home) = home {
        let home = Path::new(home);
        candidates.push(home.join(".nix-profile/bin/tiller"));
        candidates.push(home.join(".local/state/nix/profiles/profile/bin/tiller"));
    }
    if let Some(user) = user {
        let mut link = OsString::from("/etc/profiles/per-user/");
        link.push(user);
        candidates.push(PathBuf::from(link).join("bin/tiller"));
    }
    candidates.push("/run/current-system/sw/bin/tiller".into());
    candidates.push("/nix/var/nix/profiles/default/bin/tiller".into());

    let canonical = |p: &Path| p.canonicalize().ok();
    let exe_canonical = canonical(exe);
    candidates
        .into_iter()
        .filter(|c| c.is_absolute() && !c.starts_with(STORE))
        // Spawn refuses a path that is not a plain word, so a link under a
        // home with a space in it would break the session it was meant to save.
        .filter(|c| c.to_str().is_some_and(tiller_agents::guards::plain_path))
        .find(|c| exe_canonical.is_some() && canonical(c) == exe_canonical)
        .unwrap_or_else(|| exe.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    struct Rig {
        dir: tempfile::TempDir,
        /// A release file and the profile link that points at it.
        exe: PathBuf,
        link: PathBuf,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = root.join("store/tiller-1/bin/tiller");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        let link = root.join("home/.nix-profile/bin/tiller");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&exe, &link).unwrap();
        Rig { dir, exe, link }
    }

    #[test]
    fn the_link_the_binary_was_invoked_through_wins_over_its_store_file() {
        let rig = rig();
        let got = stable(&rig.exe, Some(rig.link.as_os_str()), None, None, None);
        assert_eq!(got, rig.link);
    }

    #[test]
    fn a_bare_name_is_found_on_path() {
        let rig = rig();
        let path = rig.link.parent().unwrap();
        let got = stable(
            &rig.exe,
            Some("tiller".as_ref()),
            Some(path.as_os_str()),
            None,
            None,
        );
        assert_eq!(got, rig.link);
    }

    #[test]
    fn a_store_path_invocation_falls_back_to_the_profile_link() {
        let rig = rig();
        let home = rig.dir.path().canonicalize().unwrap().join("home");
        // The test store is not under /nix/store, so name a path that is.
        let store = Path::new("/nix/store/aaaa-tiller/bin/tiller");
        let got = stable(
            &rig.exe,
            Some(store.as_os_str()),
            None,
            Some(home.as_os_str()),
            None,
        );
        assert_eq!(got, home.join(".nix-profile/bin/tiller"));
    }

    #[test]
    fn a_link_to_another_release_is_not_this_binary() {
        let rig = rig();
        let other = rig
            .dir
            .path()
            .canonicalize()
            .unwrap()
            .join("store/tiller-2/bin/tiller");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, "").unwrap();
        let got = stable(&other, Some(rig.link.as_os_str()), None, None, None);
        assert_eq!(got, other);
    }

    #[test]
    fn nothing_stable_falls_back_to_the_running_binary() {
        let rig = rig();
        assert_eq!(
            stable(&rig.exe, Some("./tiller".as_ref()), None, None, None),
            rig.exe
        );
        assert_eq!(stable(&rig.exe, None, None, None, None), rig.exe);
    }

    /// Spawn refuses a path with a space in it, so such a link is skipped.
    #[test]
    fn a_link_that_is_not_a_plain_word_is_skipped() {
        let rig = rig();
        let odd = rig
            .dir
            .path()
            .canonicalize()
            .unwrap()
            .join("my home/bin/tiller");
        fs::create_dir_all(odd.parent().unwrap()).unwrap();
        symlink(&rig.exe, &odd).unwrap();
        assert_eq!(
            stable(&rig.exe, Some(odd.as_os_str()), None, None, None),
            rig.exe
        );
    }

    /// The home's profile link is a candidate when the binary was invoked by
    /// its store path.
    #[test]
    fn a_home_link_to_another_release_is_not_this_binary() {
        let rig = rig();
        let root = rig.dir.path().canonicalize().unwrap();
        let other = root.join("store/tiller-2/bin/tiller");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, "").unwrap();
        let home = root.join("home");
        assert_eq!(
            stable(&other, None, None, Some(home.as_os_str()), None),
            other
        );
    }
}
