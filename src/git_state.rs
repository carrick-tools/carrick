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

/// `owner/repo` from this clone's `origin` remote, or `None` when it has none
/// or the remote is not a github.com URL.
///
/// The only identity a laptop can offer the cloud: CI derives it from the
/// signed OIDC claims instead, and never calls this. The hosted index reader
/// resolves the same name the same way, which is what makes a repo the CLI
/// sees the repo the cloud answers about, so there is one copy of it.
pub fn remote_name(repo: &Path) -> Option<String> {
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
    parse_remote(url.trim())
}

/// `owner/repo` from a remote URL in any of the forms git writes.
pub fn parse_remote(remote: &str) -> Option<String> {
    let name = if let Some(name) = remote.strip_prefix("git@github.com:") {
        name.strip_suffix(".git").unwrap_or(name).to_string()
    } else {
        let url = reqwest::Url::parse(remote).ok()?;
        if !url.host_str()?.eq_ignore_ascii_case("github.com")
            || !matches!(url.scheme(), "https" | "ssh")
        {
            return None;
        }
        let name = url.path().strip_prefix('/')?;
        name.strip_suffix(".git").unwrap_or(name).to_string()
    };
    valid_repo_name(&name).then_some(name)
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
        lines.push(
            "This tree has uncommitted changes. They will be indexed, and marked as such. \
             Cached analysis will not be kept for them."
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
             Cached analysis will not be kept for them."
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

    #[test]
    fn remote_normalization_matches_npm_init_url_forms() {
        for (remote, expected) in [
            ("git@github.com:example/api.git", Some("example/api")),
            ("https://GitHub.COM/example/api.git", Some("example/api")),
            ("ssh://git@GitHub.COM/example/api.git", Some("example/api")),
            ("https://github.com/example/api/", None),
            ("https://github.com.evil.test/example/api", None),
            ("/tmp/api", None),
        ] {
            assert_eq!(parse_remote(remote).as_deref(), expected, "{remote}");
        }
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
