//! Links from an issue into the project's repository: the commit a bug was first seen in, the
//! diff that reintroduced a regression, and the source line behind a stack frame.
//!
//! Thermite holds no git credential and never calls the host — these are URLs for the reader to
//! click, assembled from the project's `repo_url` and the release an event reported. They exist
//! only when the release is a git SHA: a forge can show a file at a revision, not at `1.4.2`.

/// The URL layouts that differ between forges. GitHub's is the default for hosts this does not
/// recognise, since GitHub Enterprise and most GitHub-compatible forges share it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Forge {
    GitHub,
    GitLab,
    /// Gitea and Forgejo, including codeberg.org.
    Gitea,
}

/// The forge and the base URL to append paths to, with a trailing `/` or `.git` removed.
/// `None` for anything but http(s) — `set_repo_url` already rejects those — and for Bitbucket,
/// whose layout matches none of the above; a wrong link is worse than none.
fn repo(repo_url: &str) -> Option<(Forge, String)> {
    let rest = repo_url
        .strip_prefix("https://")
        .or_else(|| repo_url.strip_prefix("http://"))?;
    let host = rest.split('/').next()?.to_ascii_lowercase();
    let forge = if host.contains("gitlab") {
        Forge::GitLab
    } else if host == "codeberg.org" || host.contains("gitea") || host.contains("forgejo") {
        Forge::Gitea
    } else if host.contains("bitbucket") {
        return None;
    } else {
        Forge::GitHub
    };
    let base = repo_url.trim_end_matches('/');
    let base = base.strip_suffix(".git").unwrap_or(base);
    Some((forge, base.to_string()))
}

/// Whether a release name is a git SHA — abbreviated ones count — and so names a revision.
pub fn is_git_sha(release: &str) -> bool {
    (7..=40).contains(&release.len()) && release.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit page for a release, when the release is a SHA.
pub fn commit_url(repo_url: &str, release: &str) -> Option<String> {
    if !is_git_sha(release) {
        return None;
    }
    let (forge, base) = repo(repo_url)?;
    Some(match forge {
        Forge::GitLab => format!("{base}/-/commit/{release}"),
        Forge::GitHub | Forge::Gitea => format!("{base}/commit/{release}"),
    })
}

/// The diff between two releases, when both are SHAs. For a regression, `good` is the release the
/// fix was verified against and `bad` the one it is failing in.
pub fn compare_url(repo_url: &str, good: &str, bad: &str) -> Option<String> {
    if !is_git_sha(good) || !is_git_sha(bad) {
        return None;
    }
    let (forge, base) = repo(repo_url)?;
    Some(match forge {
        Forge::GitLab => format!("{base}/-/compare/{good}...{bad}"),
        Forge::GitHub | Forge::Gitea => format!("{base}/compare/{good}...{bad}"),
    })
}

/// Source links for one event: the repository, and the revision the event reported.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLinks {
    forge: Forge,
    base: String,
    sha: String,
}

impl SourceLinks {
    /// `None` unless the project has a repository and the release names a revision in it.
    pub fn new(repo_url: Option<&str>, release: Option<&str>) -> Option<Self> {
        let sha = release.filter(|r| is_git_sha(r))?;
        let (forge, base) = repo(repo_url?)?;
        Some(Self {
            forge,
            base,
            sha: sha.to_string(),
        })
    }

    /// The file at the event's revision, scrolled to the line. Only for a repository-relative
    /// path: browser SDKs send URLs and native ones often absolute paths, and neither maps onto
    /// a checkout.
    pub fn file(&self, filename: &str, lineno: i64) -> Option<String> {
        let path = filename.strip_prefix("./").unwrap_or(filename);
        if path.is_empty() || path.starts_with('/') || path.contains(':') {
            return None;
        }
        let path = path.replace('\\', "/");
        let Self { forge, base, sha } = self;
        Some(match forge {
            Forge::GitHub => format!("{base}/blob/{sha}/{path}#L{lineno}"),
            Forge::GitLab => format!("{base}/-/blob/{sha}/{path}#L{lineno}"),
            Forge::Gitea => format!("{base}/src/commit/{sha}/{path}#L{lineno}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const OLD: &str = "fedcba9876543210fedcba9876543210fedcba98";

    #[test]
    fn github_layout() {
        let repo = "https://github.com/hauju/thermite-rs";
        assert_eq!(
            commit_url(repo, SHA).as_deref(),
            Some(
                "https://github.com/hauju/thermite-rs/commit/0123456789abcdef0123456789abcdef01234567"
            )
        );
        assert_eq!(
            compare_url(repo, OLD, SHA).as_deref(),
            Some(
                "https://github.com/hauju/thermite-rs/compare/fedcba9876543210fedcba9876543210fedcba98...0123456789abcdef0123456789abcdef01234567"
            )
        );
        let links = SourceLinks::new(Some(repo), Some(SHA)).unwrap();
        assert_eq!(
            links.file("src/main.rs", 42).as_deref(),
            Some(
                "https://github.com/hauju/thermite-rs/blob/0123456789abcdef0123456789abcdef01234567/src/main.rs#L42"
            )
        );
    }

    #[test]
    fn gitlab_layout() {
        let repo = "https://gitlab.example.com/team/app";
        assert_eq!(
            compare_url(repo, OLD, SHA).as_deref(),
            Some(
                "https://gitlab.example.com/team/app/-/compare/fedcba9876543210fedcba9876543210fedcba98...0123456789abcdef0123456789abcdef01234567"
            )
        );
        assert_eq!(
            commit_url(repo, SHA).as_deref(),
            Some(
                "https://gitlab.example.com/team/app/-/commit/0123456789abcdef0123456789abcdef01234567"
            )
        );
        let links = SourceLinks::new(Some(repo), Some(SHA)).unwrap();
        assert_eq!(
            links.file("app/views.py", 7).as_deref(),
            Some(
                "https://gitlab.example.com/team/app/-/blob/0123456789abcdef0123456789abcdef01234567/app/views.py#L7"
            )
        );
    }

    #[test]
    fn gitea_layout() {
        let links = SourceLinks::new(Some("https://codeberg.org/team/app"), Some(SHA)).unwrap();
        assert_eq!(
            links.file("src/lib.rs", 3).as_deref(),
            Some(
                "https://codeberg.org/team/app/src/commit/0123456789abcdef0123456789abcdef01234567/src/lib.rs#L3"
            )
        );
    }

    #[test]
    fn trailing_slash_and_dot_git_are_stripped() {
        assert_eq!(
            commit_url("https://github.com/o/r.git", SHA).as_deref(),
            Some("https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            commit_url("https://github.com/o/r/", SHA).as_deref(),
            Some("https://github.com/o/r/commit/0123456789abcdef0123456789abcdef01234567")
        );
    }

    #[test]
    fn a_version_string_names_no_revision() {
        let repo = "https://github.com/o/r";
        assert!(commit_url(repo, "1.4.2").is_none());
        assert!(compare_url(repo, "1.4.1", "1.4.2").is_none());
        assert!(compare_url(repo, OLD, "myapp@1.4.2").is_none());
        assert!(SourceLinks::new(Some(repo), Some("myapp@1.4.2")).is_none());
        assert!(SourceLinks::new(Some(repo), None).is_none());
        assert!(SourceLinks::new(None, Some(SHA)).is_none());
        // Seven hex characters is the shortest abbreviation git itself produces.
        assert!(is_git_sha("0123abc"));
        assert!(!is_git_sha("0123ab"));
    }

    #[test]
    fn only_repository_relative_paths_link() {
        let links = SourceLinks::new(Some("https://github.com/o/r"), Some(SHA)).unwrap();
        assert!(links.file("/usr/lib/python3/site.py", 1).is_none());
        assert!(links.file("https://cdn.example.com/app.js", 1).is_none());
        assert!(links.file("C:\\app\\main.rs", 1).is_none());
        assert!(links.file("", 1).is_none());
        assert_eq!(
            links.file("./src/main.rs", 1).as_deref(),
            Some(
                "https://github.com/o/r/blob/0123456789abcdef0123456789abcdef01234567/src/main.rs#L1"
            )
        );
        assert_eq!(
            links.file("src\\main.rs", 1).as_deref(),
            Some(
                "https://github.com/o/r/blob/0123456789abcdef0123456789abcdef01234567/src/main.rs#L1"
            )
        );
    }

    #[test]
    fn unknown_hosts_get_the_github_layout_and_bitbucket_gets_none() {
        assert_eq!(
            commit_url("https://git.corp.example/o/r", SHA).as_deref(),
            Some("https://git.corp.example/o/r/commit/0123456789abcdef0123456789abcdef01234567")
        );
        assert!(commit_url("https://bitbucket.org/o/r", SHA).is_none());
        assert!(commit_url("ssh://git@github.com/o/r", SHA).is_none());
    }
}
