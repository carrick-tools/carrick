//! Where an indexed repo's files are read from (carrick#3595).
//!
//! An indexed repo has one identity: the folder the index was built from,
//! whose name keys its blobs, its hosted copy and the re-join. Its files can
//! be read from that folder or from a linked git worktree of it, where an
//! agent edits on a branch. A [`Tree`] carries both, so a check answers for
//! the repo the index holds while reading the bytes and the git state of the
//! checkout the edit was made in.

use std::path::{Path, PathBuf};

/// An indexed repo and the checkout its files are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// The indexed repo's root: what names it and finds its blobs.
    pub repo: PathBuf,
    /// Where its files and git state are read: the repo itself, or a linked
    /// worktree of it.
    pub checkout: PathBuf,
}

impl Tree {
    /// The repo read from its own folder.
    pub fn at(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            checkout: repo.to_path_buf(),
        }
    }

    /// `repo`, read from the linked worktree of it that holds `dir`, or from
    /// its own folder when `dir` is in none. The several-file check reads the
    /// working directory this way: run inside a worktree, it lists that
    /// worktree's changes.
    pub fn from_dir(repo: &Path, dir: &Path) -> Self {
        match linked_checkout(dir) {
            Some(linked) if same_dir(&linked.main, repo) => Self {
                repo: repo.to_path_buf(),
                checkout: linked.top,
            },
            _ => Self::at(repo),
        }
    }
}

/// A linked git worktree: its top, and the top of the repo it was added to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Linked {
    pub top: PathBuf,
    pub main: PathBuf,
}

/// The linked worktree `path` sits in, read from the files git keeps and
/// without starting git: this runs on every `check`. The nearest ancestor
/// holding a `.git` entry decides. A directory there is a main checkout (or
/// a plain repo), so `None`; a file is a linked worktree, whose `gitdir:`
/// line names its admin folder, whose `commondir` names the repo's git
/// folder, whose parent is the repo's top. A bare repo's worktree has no such
/// top, and is `None` too.
pub(super) fn linked_checkout(path: &Path) -> Option<Linked> {
    let start = path.canonicalize().ok()?;
    let top = start.ancestors().find(|dir| dir.join(".git").exists())?;
    let marker = top.join(".git");
    if !marker.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&marker).ok()?;
    let gitdir = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let gitdir = top.join(gitdir);
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim()).canonicalize().ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    Some(Linked {
        top: top.to_path_buf(),
        main: common.parent()?.to_path_buf(),
    })
}

/// Whether two paths name the same folder, resolving links (`/var` and
/// `/private/var` on macOS).
pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A committed repo with a linked worktree at `.claude/worktrees/b`.
    fn repo_with_a_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("create the repo");
        std::fs::write(repo.join("src/a.ts"), "export const a = 1;\n").expect("write a file");
        git(&repo, &["init", "-q", "."]);
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=fixture@carrick.test",
                "-c",
                "user.name=fixture",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "-q", ".claude/worktrees/b", "-b", "b"],
        );
        let repo = repo.canonicalize().expect("the repo exists");
        let worktree = repo.join(".claude/worktrees/b");
        (dir, repo, worktree)
    }

    #[test]
    fn a_path_in_a_linked_worktree_names_the_worktree_and_its_repo() {
        let (_dir, repo, worktree) = repo_with_a_worktree();
        let linked = linked_checkout(&worktree.join("src/a.ts")).expect("a linked worktree");
        assert_eq!(linked.top, worktree);
        assert_eq!(linked.main, repo);
        assert_eq!(
            Tree::from_dir(&repo, &worktree.join("src")),
            Tree {
                repo: repo.clone(),
                checkout: worktree,
            }
        );
    }

    /// Must not change: a path in the main checkout, and a repo with no
    /// worktree, read from the repo's own folder.
    #[test]
    fn a_path_in_the_main_checkout_is_read_from_the_repo() {
        let (_dir, repo, _worktree) = repo_with_a_worktree();
        assert_eq!(linked_checkout(&repo.join("src/a.ts")), None);
        assert_eq!(Tree::from_dir(&repo, &repo.join("src")), Tree::at(&repo));
        let other = tempfile::tempdir().expect("temp dir");
        assert_eq!(Tree::from_dir(&repo, other.path()), Tree::at(&repo));
    }
}
