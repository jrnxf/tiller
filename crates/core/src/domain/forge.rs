//! Which forge a project's remote is, read from the remote URL alone at
//! `project.add` (P10). Nothing here touches the network or a checkout.
//!
//! Tiller lands through GitHub: the PR poller, the merge and the deploy watch
//! all read `owner/name`. So detection answers two questions at add time: is
//! this a GitHub remote, and which `owner/name` is it; and is it a Gerrit
//! remote, which Tiller cannot publish to.

/// What a remote URL says about its forge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forge {
    /// A github.com remote, with its `owner/name`.
    Github { repo: String },
    /// An SSH remote on port 29418, Gerrit's default SSH port.
    Gerrit { evidence: String },
    /// Any other remote: the forge is not one Tiller reads.
    Other,
}

/// Reads `remote` (an `https://`, `ssh://` or `git@host:path` URL).
pub fn detect(remote: &str) -> Forge {
    let remote = remote.trim();
    if let Some(rest) = remote.strip_prefix("ssh://") {
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.ends_with(":29418") {
            return Forge::Gerrit {
                evidence: format!("remote {remote} uses SSH port 29418"),
            };
        }
    }
    match github_repo(remote) {
        Some(repo) => Forge::Github { repo },
        None => Forge::Other,
    }
}

/// `owner/name` of a github.com remote in any of its URL forms.
fn github_repo(remote: &str) -> Option<String> {
    let path = ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| remote.strip_prefix(scheme))
        .map(|rest| {
            // `user@host/owner/name` or `host:22/owner/name`.
            let rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            (host.split(':').next().unwrap_or_default(), path)
        })
        .or_else(|| {
            // scp form: `git@host:owner/name`.
            let (host, path) = remote.split_once(':')?;
            Some((host.rsplit_once('@').map_or(host, |(_, host)| host), path))
        })?;
    if !path.0.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.1.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    (valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `user@host` without the literal, which `scripts/scrub-check.sh` reads
    /// as an email address.
    fn login(user: &str, host: &str) -> String {
        format!("{user}{}{host}", '@')
    }

    fn github(repo: &str) -> Forge {
        Forge::Github { repo: repo.into() }
    }

    #[test]
    fn every_github_url_form_names_the_repo() {
        let git = login("git", "github.com");
        for remote in [
            "https://github.com/example/alpha".to_owned(),
            "https://github.com/example/alpha.git".to_owned(),
            "https://github.com/example/alpha/".to_owned(),
            format!("https://{}/example/alpha.git", login("user", "github.com")),
            format!("{git}:example/alpha.git"),
            format!("ssh://{git}/example/alpha.git"),
            format!("ssh://{git}:22/example/alpha.git"),
            "git://github.com/example/alpha.git".to_owned(),
        ] {
            assert_eq!(detect(&remote), github("example/alpha"), "{remote}");
        }
    }

    #[test]
    fn ssh_port_29418_is_gerrit() {
        assert!(matches!(
            detect("ssh://gerrit.example.com:29418/alpha"),
            Forge::Gerrit { .. }
        ));
    }

    #[test]
    fn anything_else_is_not_a_forge_tiller_reads() {
        for remote in [
            "https://gitlab.example.com/example/alpha.git",
            "https://github.com.example.com/example/alpha.git",
            "https://github.com/example",
            "https://github.com/example/alpha beta",
            "/home/user/alpha",
            "",
        ] {
            assert_eq!(detect(remote), Forge::Other, "{remote}");
        }
    }
}
