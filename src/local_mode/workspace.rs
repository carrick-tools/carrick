//! The workspace file, and where the index lives beside it.
//!
//! A workspace is a folder holding a `carrick-workspace.json` that lists the
//! repos, explicitly. There is no directory walk in this version, deliberately
//! (E20): a list a user can read is a list a user can correct, and a walk
//! turns "which repos are indexed" into a question about the scanner's search
//! order.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file that lists the repos, in the folder that holds them.
pub const WORKSPACE_FILE: &str = "carrick-workspace.json";
/// Everything the index writes, beside that file.
pub const INDEX_DIR: &str = ".carrick";
/// Names the workspace root for `touch`/`check` when the file being queried
/// is not under it (a repo listed as `../shared-client`).
pub const WORKSPACE_ENV: &str = "CARRICK_WORKSPACE";

/// `<workspace>/carrick-workspace.json`, as written by hand or by `carrick
/// init`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct WorkspaceFile {
    /// Repo paths, relative to this file or absolute.
    pub repos: Vec<String>,
}

/// A resolved workspace: the root, and every repo that exists on disk.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    /// Absolute, canonicalized repo paths, in the order the file lists them.
    pub repos: Vec<PathBuf>,
    /// Paths the file lists that are not directories on this machine. Kept
    /// rather than dropped: an index that silently covers four of five repos
    /// answers "no consumers" for the fifth.
    pub missing: Vec<String>,
}

impl Workspace {
    /// Read and resolve `<root>/carrick-workspace.json`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let file = root.join(WORKSPACE_FILE);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("could not read {}: {e}", file.display()))?;
        let parsed: WorkspaceFile = serde_json::from_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", file.display()))?;
        if parsed.repos.is_empty() {
            return Err(format!(
                "{} lists no repos. Add the repo paths to scan, for example \
                 {{\"repos\": [\"./api\", \"./web\"]}}",
                file.display()
            ));
        }

        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut repos = Vec::new();
        let mut missing = Vec::new();
        for entry in &parsed.repos {
            let candidate = {
                let raw = PathBuf::from(entry);
                if raw.is_absolute() {
                    raw
                } else {
                    root.join(raw)
                }
            };
            match candidate.canonicalize() {
                Ok(path) if path.is_dir() => repos.push(path),
                _ => missing.push(entry.clone()),
            }
        }
        if repos.is_empty() {
            return Err(format!(
                "none of the {} repo path(s) in {} exist on this machine",
                parsed.repos.len(),
                file.display()
            ));
        }
        Ok(Self {
            root,
            repos,
            missing,
        })
    }

    /// `<workspace>/.carrick/`.
    pub fn index_dir(&self) -> PathBuf {
        self.root.join(INDEX_DIR)
    }

    /// Where the per-service index blobs go: the `LocalDirStorage` cache dir.
    pub fn blobs_dir(&self) -> PathBuf {
        self.index_dir().join("repos")
    }

    /// The joined read model `touch` and `check` answer from.
    pub fn index_file(&self) -> PathBuf {
        self.index_dir().join("index.json")
    }

    /// The join hand-off, written by the join subprocess and read once.
    pub fn join_file(&self) -> PathBuf {
        self.index_dir().join("join.json")
    }
}

/// Find the workspace root for a read-only command, in the order a caller can
/// predict: what the caller said, then what the environment says, then the
/// directories above the file, then the directories above the working
/// directory.
///
/// The last two exist because an editor hook knows a file and nothing else;
/// the first two exist because a repo listed as `../shared-client` is not
/// under the workspace at all, so no walk from the file can reach it.
pub fn locate(explicit: Option<&Path>, file: Option<&Path>) -> Option<PathBuf> {
    // Canonicalized on every path out of here: the output contract states an
    // absolute workspace, and `--workspace .` is the ordinary way a hook
    // invokes this.
    if let Some(root) = explicit {
        return Some(absolute(root));
    }
    if let Ok(root) = std::env::var(WORKSPACE_ENV)
        && !root.is_empty()
    {
        return Some(absolute(Path::new(&root)));
    }
    let from_file = file
        .and_then(|f| f.parent().map(Path::to_path_buf))
        .and_then(|dir| walk_up(&dir));
    if from_file.is_some() {
        return from_file;
    }
    std::env::current_dir().ok().and_then(|dir| walk_up(&dir))
}

/// An absolute path for a directory, falling back to what the caller wrote
/// when it cannot be resolved — a wrong-looking path in the output beats
/// refusing to answer.
fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The nearest ancestor holding a workspace file (or an index built from one).
fn walk_up(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    for dir in start.ancestors() {
        if dir.join(WORKSPACE_FILE).is_file() || dir.join(INDEX_DIR).join("index.json").is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Make `.carrick/` ignore itself, so indexing a workspace never asks the user
/// to change a file they track. Written on every index; cheap and idempotent.
pub fn write_self_ignore(index_dir: &Path) -> std::io::Result<()> {
    std::fs::write(
        index_dir.join(".gitignore"),
        "# Written by `carrick index`. The local index is derived from your\n\
         # source and is rebuilt by re-running the command.\n*\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_with(repos: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(WORKSPACE_FILE), repos).unwrap();
        dir
    }

    #[test]
    fn resolves_relative_repo_paths_against_the_workspace_file() {
        let dir = workspace_with(r#"{"repos": ["./api"]}"#);
        std::fs::create_dir(dir.path().join("api")).unwrap();
        let workspace = Workspace::load(dir.path()).unwrap();
        assert_eq!(workspace.repos.len(), 1);
        assert!(workspace.repos[0].ends_with("api"));
        assert!(workspace.missing.is_empty());
    }

    #[test]
    fn a_listed_repo_that_is_not_on_disk_is_reported_not_dropped() {
        // Silently covering three of four repos answers "no consumers" for
        // the fourth, which is worse than saying so.
        let dir = workspace_with(r#"{"repos": ["./api", "./gone"]}"#);
        std::fs::create_dir(dir.path().join("api")).unwrap();
        let workspace = Workspace::load(dir.path()).unwrap();
        assert_eq!(workspace.repos.len(), 1);
        assert_eq!(workspace.missing, vec!["./gone".to_string()]);
    }

    #[test]
    fn an_empty_repo_list_is_an_error_with_the_shape_to_write() {
        let dir = workspace_with(r#"{"repos": []}"#);
        let err = Workspace::load(dir.path()).unwrap_err();
        assert!(err.contains("lists no repos"), "{err}");
    }

    #[test]
    fn locate_walks_up_from_a_file_to_the_workspace_file() {
        let dir = workspace_with(r#"{"repos": ["./api"]}"#);
        let nested = dir.path().join("api/src");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("index.ts");
        std::fs::write(&file, "").unwrap();
        let found = locate(None, Some(&file)).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn locate_prefers_what_the_caller_said() {
        let dir = workspace_with(r#"{"repos": ["./api"]}"#);
        let other = tempfile::tempdir().unwrap();
        let found = locate(Some(other.path()), Some(&dir.path().join("api/src/x.ts"))).unwrap();
        assert_eq!(found, other.path().canonicalize().unwrap());
    }

    #[test]
    fn locate_answers_with_an_absolute_path() {
        // `--workspace .` is how a hook invokes this, and the contract says
        // the workspace it reports is absolute.
        let dir = workspace_with(r#"{"repos": ["./api"]}"#);
        std::fs::create_dir(dir.path().join("api")).unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let found = locate(Some(Path::new(".")), None).unwrap();
        std::env::set_current_dir(previous).unwrap();
        assert!(found.is_absolute(), "{found:?}");
    }
}
