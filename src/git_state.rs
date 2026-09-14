//! What the tree being scanned looks like to git, and what a laptop run says
//! about it before it uploads.
//!
//! A CI scan runs on a checkout that git made: HEAD is the commit the workflow
//! was dispatched for and nothing is modified. A laptop scan runs on whatever
//! the developer has open, so the index it produces can describe a branch, or
//! code that exists in no commit at all. The upload carries `commit_hash`
//! either way, and without these two facts beside it that field is a claim the
//! index cannot support.
//!
//! The policy is warn, never refuse (David's ruling, 2026-09-11): both
//! conditions are reported separately, the scan proceeds, and `dirty` rides
//! the blob so every later reader can say so. Wire contract:
//! carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §8.4.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// What git says about the tree this run is scanning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitState {
    /// HEAD's commit, or empty when this is not a git repository.
    pub commit: String,
    /// The branch HEAD is on, or `None` on a detached HEAD.
    pub branch: Option<String>,
    /// The working tree carries changes HEAD does not describe.
    pub dirty: bool,
    /// HEAD is `origin/main`'s commit. `None` when `origin/main` does not
    /// resolve in this clone, which is not the same as "it differs".
    pub at_origin_main: Option<bool>,
}

/// Ask git about `repo_path`.
///
/// Every call clears the inherited git environment for the same reason
/// [`crate::cloud_storage::get_current_commit_hash`] does: a pre-commit hook
/// or a harness subprocess can carry a `GIT_DIR` for a different repository,
/// and this would then describe that one.
pub fn inspect(repo_path: &str) -> GitState {
    let run = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };

    let commit = run(&["rev-parse", "HEAD"]).unwrap_or_default();
    if commit.is_empty() {
        return GitState::default();
    }
    let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]).filter(|name| name != "HEAD");
    // `--porcelain` prints one line per path git considers changed, staged or
    // untracked, and nothing at all for a clean tree. Emptiness is the whole
    // answer, so the output is never parsed.
    let dirty = run(&["status", "--porcelain"]).is_some_and(|out| !out.is_empty());
    // `None`, not `false`, when the ref is absent: a clone with no
    // `origin/main` cannot say whether HEAD is at it, and saying "not at
    // origin/main" there would warn every user of a repo whose default branch
    // is called something else.
    let at_origin_main = run(&["rev-parse", "origin/main"]).map(|main| main == commit);

    GitState {
        commit,
        branch,
        dirty,
        at_origin_main,
    }
}

/// The paths under `repo_path` whose content on disk is the content `commit`
/// holds for them, relative to `repo_path`.
///
/// The one question the incremental analysis cache asks, from both ends
/// (carrick#1079). The reader replays a previous scan's answer for a file only
/// when the file is in this set for that scan's commit; the writer keeps an
/// answer only when the file is in this set for HEAD. An answer that survives
/// both therefore describes bytes a commit names, which is what lets a dirty
/// tree keep its cache for every file it did not touch instead of dropping all
/// of it.
///
/// Built from what git tracks, minus what differs from `commit` in the working
/// tree (`git diff <commit> --` compares the commit to the files on disk, so
/// staged and unstaged edits both count). A file git does not track is never
/// in the set: untracked and ignored files alike have no content any commit
/// can vouch for, and the scanner's own walk does not read `.gitignore`.
///
/// Both git calls run from `repo_path` and print paths relative to it
/// (`ls-files` does by default, `diff` with `--relative`), which is the form
/// the cache is keyed by even when `repo_path` is a directory inside a larger
/// repository. `-z` keeps a non-ASCII path byte-exact rather than quoted.
///
/// `Err` carries git's reason and means "git could not answer" (not a
/// repository, a commit this clone does not hold, a shallow history), which is
/// deliberately distinct from an empty set.
pub fn unchanged_since(repo_path: &str, commit: &str) -> Result<HashSet<String>, String> {
    // A value that is not a hex object name never reaches git's argument list,
    // where a leading `-` would read as an option.
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{commit:?} is not a commit id"));
    }
    let run = |args: &[&str]| -> Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    };
    let paths = |text: String| -> Vec<String> {
        text.split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    };
    let changed: HashSet<String> = paths(run(&[
        "diff",
        "--name-only",
        "--no-renames",
        "--relative",
        "-z",
        commit,
        "--",
    ])?)
    .into_iter()
    .collect();
    Ok(paths(run(&["ls-files", "-z"])?)
        .into_iter()
        .filter(|path| !changed.contains(path))
        .collect())
}

/// A clone's `origin` remote, and the repository it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The remote as git holds it, with any credentials taken out: a remote
    /// can carry a token in its userinfo half, and this string is printed to
    /// the reader and stored in the local index.
    pub url: String,
    /// `owner/repo`, or `None` when the remote names no such path. The reason
    /// travels with the answer so a surface can say which of the two it is
    /// (carrick#991, carrick#1056).
    pub name: Option<String>,
}

/// This clone's `origin` remote, or `None` when it has none.
pub fn origin(repo: &Path) -> Option<Origin> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let url = url.trim();
    Some(Origin {
        name: parse_remote(url),
        url: without_credentials(url),
    })
}

/// `owner/repo` from this clone's `origin` remote, or `None` when it has none
/// or the remote names no `owner/repo` path.
///
/// The only identity a laptop can offer the cloud: CI derives it from the
/// signed OIDC claims instead, and never calls this. The hosted index reader
/// resolves the same name the same way, which is what makes a repo the CLI
/// sees the repo the cloud answers about, so there is one copy of it.
pub fn remote_name(repo: &Path) -> Option<String> {
    origin(repo)?.name
}

/// `owner/repo` from a remote URL in any of the forms git writes, whatever
/// host it names.
///
/// The host is not a gate. A machine with two GitHub accounts writes its
/// remotes through a per-account ssh alias — `git@github.com-personal:owner/repo`,
/// a name only that user's `~/.ssh/config` can resolve — and that is an
/// ordinary GitHub repository. Refusing it left a repo the cloud had just
/// connected reading as "not connected to a Carrick project" on the machine
/// that connected it (carrick#1056). Whether the cloud knows a name is the
/// cloud's answer: `resolve-repos` returns nothing for a name outside the
/// workspace and `start-scan` refuses a repo this holder may not write, so
/// this reads the name and decides nothing else.
pub fn parse_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = if has_scheme(remote) {
        // Parsed rather than split so a port, a userinfo half or an escape
        // cannot be read as part of the path.
        let url = reqwest::Url::parse(remote).ok()?;
        // A URL naming no host names no repository host: `file:///tmp/api`.
        if url.host_str().is_none_or(str::is_empty) {
            return None;
        }
        url.path().to_string()
    } else {
        // The scp spelling `[user@]host:owner/repo`, which is not a URL. The
        // user half is optional and never read — git writes an alias with no
        // user as `github.com-personal:owner/repo`.
        let after_user = remote.split_once('@').map_or(remote, |(_, rest)| rest);
        let (host, path) = after_user.split_once(':')?;
        if host.is_empty() || host.contains('/') {
            return None;
        }
        path.to_string()
    };
    // The last two segments, so a host that nests groups names its repository
    // rather than nothing. A `.git` suffix is git's, not part of the name.
    let mut segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let repo = segments.pop()?;
    let owner = segments.pop()?;
    let name = format!("{owner}/{}", repo.strip_suffix(".git").unwrap_or(repo));
    valid_repo_name(&name).then_some(name)
}

/// Whether a remote is written in a URL form (`scheme://…`) rather than the
/// scp one. Told apart by the `//`, not by whether a URL parser accepts it: a
/// host alias with no user (`github.com-personal:owner/repo`) parses as a URL
/// whose scheme is the host, and would otherwise be read as neither form.
fn has_scheme(remote: &str) -> bool {
    let Some((scheme, _)) = remote.split_once("://") else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// A remote with its userinfo removed, for printing.
///
/// A URL-form remote can carry a token (`https://x-access-token:TOKEN@host/…`),
/// and the one remote this module prints is the one it could not read a name
/// from — which includes a credential-carrying URL with no repository path.
fn without_credentials(remote: &str) -> String {
    if !has_scheme(remote) {
        return remote.to_string();
    }
    let Ok(mut url) = reqwest::Url::parse(remote) else {
        return remote.to_string();
    };
    if url.username().is_empty() && url.password().is_none() {
        return remote.to_string();
    }
    if url.set_username("").is_err() || url.set_password(None).is_err() {
        // A URL that cannot hold a username cannot be stripped of one, and
        // printing it as it stands would print the credential.
        return "a remote carrying credentials".to_string();
    }
    url.to_string()
}

/// Whether a string is a `owner/repo` name and nothing else — no path escape,
/// no empty half.
pub fn valid_repo_name(name: &str) -> bool {
    let parts: Vec<_> = name.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}

/// The repo-relative short form the warning names HEAD by.
fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// The line every laptop run ends its warnings on. CI is where a routine scan
/// belongs: it runs on a clean checkout of the default branch, and its rows
/// supersede a laptop's.
pub const LEAVE_ROUTINE_SCANS_TO_CI: &str = "Leave routine scans to CI: a CI scan runs on a clean checkout of the default branch, \
     and its index replaces whatever a laptop scan wrote.";

/// What this run warns about before it uploads, in order, or empty when there
/// is nothing to say.
///
/// Pure so the wording is pinned by a test without a git repository. Both
/// conditions are separate and both are reported: a dirty tree on a feature
/// branch is two problems, not one.
pub fn warnings(state: &GitState) -> Vec<String> {
    let mut lines = Vec::new();
    if state.at_origin_main == Some(false) {
        lines.push(format!(
            "This tree is at {} on {}, not origin/main. The index will describe this branch \
             until the next CI scan.",
            short(&state.commit),
            state.branch.as_deref().unwrap_or("a detached HEAD"),
        ));
    }
    if state.dirty {
        // The cost clause is the part a laptop run cannot see for itself: an
        // uncommitted file keeps no cache entry, so the next scan, CI's
        // included, asks the model about it again (carrick#993 row 17). Every
        // file the change did not touch keeps its entry (carrick#1079).
        lines.push(
            "This tree has uncommitted changes. They will be indexed, and marked as such. \
             Cached analysis will not be kept for the changed files, and the next CI scan \
             re-analyses them."
                .to_string(),
        );
    }
    if !lines.is_empty() {
        lines.push(LEAVE_ROUTINE_SCANS_TO_CI.to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_main() -> GitState {
        GitState {
            commit: "4f2a1c9abcdef0123456789".to_string(),
            branch: Some("main".to_string()),
            dirty: false,
            at_origin_main: Some(true),
        }
    }

    #[test]
    fn a_clean_tree_at_origin_main_says_nothing() {
        assert!(warnings(&at_main()).is_empty());
    }

    /// The two conditions are independent, and a tree that fails both is told
    /// about both — the branch warning is about where the index will point,
    /// the dirty warning about what will be in it.
    #[test]
    fn both_conditions_are_reported_separately() {
        let state = GitState {
            branch: Some("feature/x".to_string()),
            dirty: true,
            at_origin_main: Some(false),
            ..at_main()
        };
        let lines = warnings(&state);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            "This tree is at 4f2a1c9 on feature/x, not origin/main. The index will describe \
             this branch until the next CI scan."
        );
        assert_eq!(
            lines[1],
            "This tree has uncommitted changes. They will be indexed, and marked as such. \
             Cached analysis will not be kept for the changed files, and the next CI scan \
             re-analyses them."
        );
        assert_eq!(lines[2], LEAVE_ROUTINE_SCANS_TO_CI);
    }

    /// Warn, never refuse: the warning is a list of sentences and nothing
    /// about it can stop a run. Pinned because the ruling is the opposite of
    /// what a guard usually does.
    #[test]
    fn a_dirty_tree_at_origin_main_warns_about_the_tree_only() {
        let state = GitState {
            dirty: true,
            ..at_main()
        };
        let lines = warnings(&state);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("uncommitted changes"));
        assert!(!lines[0].contains("origin/main"));
    }

    /// A clone with no `origin/main` cannot say HEAD is away from it. Saying
    /// so anyway would warn every user whose default branch is named
    /// something else.
    #[test]
    fn an_unresolvable_origin_main_is_not_a_branch_warning() {
        let state = GitState {
            branch: Some("trunk".to_string()),
            at_origin_main: None,
            ..at_main()
        };
        assert!(warnings(&state).is_empty());
    }

    #[test]
    fn a_detached_head_is_named_as_one() {
        let state = GitState {
            branch: None,
            at_origin_main: Some(false),
            ..at_main()
        };
        assert!(warnings(&state)[0].contains("on a detached HEAD"));
    }

    /// The real thing, against a repository built for the test: a fresh commit
    /// with a modified file reads dirty, and the same tree reads clean once
    /// the change is committed.
    #[test]
    fn inspect_reads_a_real_repository() {
        let dir = std::env::temp_dir().join(format!("carrick-git-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .unwrap();
            assert!(ok.status.success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);

        let clean = inspect(&dir.to_string_lossy());
        assert!(!clean.dirty, "a committed tree is clean: {clean:?}");
        assert_eq!(clean.branch.as_deref(), Some("main"));
        assert_eq!(clean.commit.len(), 40);
        // No remote in this repository, so the comparison cannot be made.
        assert_eq!(clean.at_origin_main, None);

        std::fs::write(dir.join("a.txt"), "two").unwrap();
        assert!(inspect(&dir.to_string_lossy()).dirty);

        // An untracked file is a change the commit does not describe, so it
        // counts: its analysis would be cached against a commit it is not in.
        git(&["checkout", "-q", "--", "a.txt"]);
        assert!(!inspect(&dir.to_string_lossy()).dirty);
        std::fs::write(dir.join("b.txt"), "new").unwrap();
        assert!(inspect(&dir.to_string_lossy()).dirty);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every form git writes, read for the name and nothing else.
    ///
    /// The host is deliberately not a gate (carrick#1056): an ssh alias is a
    /// GitHub repository under a name only the user's ssh config resolves, and
    /// a name the workspace does not hold is refused by the cloud, which is
    /// the only side that can know. This is where the Rust reader differs from
    /// `repoIdentity` in the npm package, which resolves the alias through
    /// `ssh -G` because it decides what to OFFER the cloud.
    #[test]
    fn a_remote_names_its_repo_in_every_form_git_writes() {
        for (remote, expected) in [
            ("git@github.com:example/api.git", Some("example/api")),
            // The ticket's remote: a per-account ssh alias.
            (
                "git@github.com-personal:example/api.git",
                Some("example/api"),
            ),
            // The same alias as git writes it when the config names no user.
            ("github.com-personal:example/api.git", Some("example/api")),
            ("https://GitHub.COM/example/api.git", Some("example/api")),
            ("ssh://git@GitHub.COM/example/api.git", Some("example/api")),
            (
                "git+ssh://git@github.com/example/api.git",
                Some("example/api"),
            ),
            // A trailing slash is git's spelling, not a missing name.
            ("https://github.com/example/api/", Some("example/api")),
            // Another host entirely: it parses, and the cloud decides whether
            // it is a repository this workspace holds.
            ("https://gitlab.com/example/api.git", Some("example/api")),
            ("git@gitlab.com:group/sub/api.git", Some("sub/api")),
            // A credential in the URL is not part of the name.
            (
                "https://x-access-token:secret@github.com/example/api.git",
                Some("example/api"),
            ),
            // No `owner/repo` path to read.
            ("/tmp/api", None),
            ("https://github.com/example", None),
            ("git@github.com:api.git", None),
            ("file:///tmp/example/api.git", None),
            ("", None),
        ] {
            assert_eq!(parse_remote(remote).as_deref(), expected, "{remote}");
        }
    }

    /// The remote is printed when no name could be read from it, and a remote
    /// can carry a token. The redaction happens in the reader, before anything
    /// stores it.
    #[test]
    fn a_printed_remote_carries_no_credentials() {
        assert_eq!(
            without_credentials("https://x-access-token:secret@github.com/example"),
            "https://github.com/example"
        );
        assert!(!without_credentials("https://user:pw@host.test/").contains("pw"));
        // Nothing to strip: the string is returned exactly as git wrote it,
        // not as a URL parser would rewrite it.
        assert_eq!(
            without_credentials("git@github.com-personal:example/api.git"),
            "git@github.com-personal:example/api.git"
        );
        assert_eq!(
            without_credentials("https://github.com/example/api.git"),
            "https://github.com/example/api.git"
        );
    }

    /// The reader answers with the remote beside the name, so a surface can
    /// say "this remote names no repository" rather than "not connected".
    #[test]
    fn origin_reports_the_remote_it_could_not_name() {
        let dir = std::env::temp_dir().join(format!("carrick-git-origin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        // No origin at all: nothing to report.
        assert_eq!(origin(&dir), None);
        assert_eq!(remote_name(&dir), None);

        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com-personal:example/api.git",
        ]);
        assert_eq!(
            origin(&dir),
            Some(Origin {
                url: "git@github.com-personal:example/api.git".to_string(),
                name: Some("example/api".to_string()),
            })
        );

        git(&["remote", "set-url", "origin", "/srv/mirrors/api"]);
        assert_eq!(
            origin(&dir),
            Some(Origin {
                url: "/srv/mirrors/api".to_string(),
                name: None,
            })
        );
        assert_eq!(remote_name(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that is not a git repository answers with the default rather
    /// than a false claim about a branch.
    #[test]
    fn a_non_repository_reads_empty() {
        let dir = std::env::temp_dir().join(format!("carrick-git-none-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = inspect(&dir.to_string_lossy());
        assert_eq!(state, GitState::default());
        assert!(warnings(&state).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
