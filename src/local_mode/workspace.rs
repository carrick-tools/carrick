//! The workspace file, and where the index lives beside it.
//!
//! Repositories are derived at one level. An optional workspace file adds
//! paths and excludes names; init never rewrites those overrides.

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
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// An advisory result from inspecting the immediate parent once.
#[derive(Debug, Clone, Serialize)]
pub struct ParentProposal {
    pub directory: PathBuf,
    pub repos: Vec<PathBuf>,
}

impl ParentProposal {
    pub fn description(&self) -> String {
        format!(
            "The parent folder {} holds {} repo(s): {}. Run carrick init .. to initialise that workspace.",
            self.directory.display(),
            self.repos.len(),
            self.repos
                .iter()
                .map(|repo| repo.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
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
    pub repos_detected_by: String,
    pub repos_added: Vec<String>,
    pub repos_excluded: Vec<String>,
    pub parent_proposal: Option<ParentProposal>,
}

impl Workspace {
    /// Read and resolve `<root>/carrick-workspace.json`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let file = root.join(WORKSPACE_FILE);
        let parsed: WorkspaceFile = match std::fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("could not parse {}: {e}", file.display()))?,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&file).is_err() =>
            {
                WorkspaceFile::default()
            }
            Err(e) => return Err(format!("could not read {}: {e}", file.display())),
        };
        let (repos_detected_by, mut entries) = detect(root)?;
        let repos_added = parsed.repos.clone();
        entries.extend(parsed.repos);
        let repos_excluded = parsed.exclude;
        let parent_proposal = if entries.is_empty() || repos_detected_by == "single repository" {
            inspect_parent(root)
        } else {
            None
        };
        if entries.is_empty() {
            return Err(format!(
                "{} lists no repos and none were detected. {}",
                root.display(),
                parent_proposal.as_ref().map(ParentProposal::description).unwrap_or_else(|| format!("The immediate parent yielded no workspace proposal. Add repo paths to {WORKSPACE_FILE}."))
            ));
        }

        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut repos = Vec::new();
        let mut missing = Vec::new();
        for entry in &entries {
            let candidate = {
                let raw = PathBuf::from(entry);
                if raw.is_absolute() {
                    raw
                } else {
                    root.join(raw)
                }
            };
            let excluded =
                repos_excluded.iter().any(|excluded| {
                    Path::new(entry)
                        .file_name()
                        .is_some_and(|name| name == excluded.as_str())
                        || root.join(excluded).canonicalize().ok().is_some_and(|path| {
                            candidate.canonicalize().ok().as_ref() == Some(&path)
                        })
                });
            if excluded {
                continue;
            }
            match candidate.canonicalize() {
                Ok(path) if path.is_dir() => {
                    if !repos.contains(&path) {
                        repos.push(path);
                    }
                }
                _ => missing.push(entry.clone()),
            }
        }
        if repos.is_empty() {
            return Err(format!(
                "none of the {} repo path(s) in {} exist on this machine",
                entries.len(),
                file.display()
            ));
        }

        // Checked here, before anything is scanned. A scan records a repo by
        // its directory name, so two repos sharing one name write to the same
        // blob file — and by the time the second scan has finished, the first
        // repo's answers are already gone. Refusing the workspace costs a
        // message; refusing it later costs an index.
        let mut seen: Vec<&str> = Vec::new();
        for repo in &repos {
            let Some(name) = repo.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if seen.contains(&name) {
                return Err(format!(
                    "two repos in {} are both named '{name}'. A scan records a repo by its                      directory name, so one would overwrite the other's index — rename one                      directory, or list only one of them",
                    file.display()
                ));
            }
            seen.push(name);
        }
        Ok(Self {
            root,
            repos,
            missing,
            repos_detected_by,
            repos_added,
            repos_excluded,
            parent_proposal,
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

/// Use detection directly, not Workspace::load, so this cannot recurse to
/// grandparents or apply/scan the proposed workspace.
fn inspect_parent(root: &Path) -> Option<ParentProposal> {
    let root = root.canonicalize().ok()?;
    let parent = root.parent()?;
    let (_, paths) = detect(parent).ok()?;
    if paths.is_empty() {
        return None;
    }
    Some(ParentProposal {
        directory: parent.to_path_buf(),
        repos: paths.into_iter().map(|path| parent.join(path)).collect(),
    })
}

/// The same repository detection is used by init and every index build.
fn detect(root: &Path) -> Result<(String, Vec<String>), String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    if std::fs::symlink_metadata(root.join("carrick.json")).is_ok() {
        return Ok(("carrick.json".into(), vec![".".into()]));
    }
    if !crate::service_derivation::workspace_patterns(root)?.is_empty()
        || root.join("deno.json").is_file()
        || root.join("deno.jsonc").is_file()
    {
        return Ok(("workspace manifest".into(), vec![".".into()]));
    }
    let mut repos = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.')
            || crate::packages::MANIFEST_SKIP_DIRS.contains(&name.as_str())
            || ["target", "out", "coverage"].contains(&name.as_str())
            || !entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            continue;
        }
        if is_repo(&entry.path()) {
            repos.push(format!("./{name}"));
        }
    }
    if !repos.is_empty() {
        repos.sort();
        return Ok(("sibling repositories".into(), repos));
    }
    if is_repo(root) {
        return Ok(("single repository".into(), vec![".".into()]));
    }
    Ok(("workspace overrides".into(), Vec::new()))
}

fn is_repo(root: &Path) -> bool {
    [
        ".git",
        "package.json",
        "carrick.json",
        "deno.json",
        "deno.jsonc",
        "pnpm-workspace.yaml",
    ]
    .iter()
    .any(|name| std::fs::symlink_metadata(root.join(name)).is_ok())
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
    std::env::current_dir().ok().and_then(|dir| {
        walk_up(&dir).or_else(|| {
            detect(&dir)
                .ok()
                .filter(|(_, repos)| !repos.is_empty())
                .map(|_| absolute(&dir))
        })
    })
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
    fn detects_repos_and_preserves_additions_and_exclusions() {
        let dir = workspace_with(r#"{"repos":["./manual"],"exclude":["web"]}"#);
        for name in ["api", "web", "manual"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("api/package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("web/carrick.json"), "{}").unwrap();
        let workspace = Workspace::load(dir.path()).unwrap();
        assert_eq!(workspace.repos_detected_by, "sibling repositories");
        assert_eq!(workspace.repos.len(), 2);
        assert_eq!(workspace.repos_added, ["./manual"]);
        assert_eq!(workspace.repos_excluded, ["web"]);
        assert!(workspace.repos.iter().any(|p| p.ends_with("manual")));
    }

    #[test]
    fn explicit_repo_config_wins_over_nested_package_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("carrick.json"), "{}").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/package.json"), "{}").unwrap();
        let workspace = Workspace::load(dir.path()).unwrap();
        assert_eq!(workspace.repos_detected_by, "carrick.json");
        assert_eq!(workspace.repos, [dir.path().canonicalize().unwrap()]);
        assert!(!dir.path().join(WORKSPACE_FILE).exists());
    }

    #[test]
    fn plain_repo_proposes_actual_parent_siblings_without_widening() {
        let dir = tempfile::tempdir().unwrap();
        for repo in ["api", "web"] {
            std::fs::create_dir(dir.path().join(repo)).unwrap();
            std::fs::write(dir.path().join(repo).join("package.json"), "{}").unwrap();
        }
        let root = dir.path().join("api");
        let workspace = Workspace::load(&root).unwrap();
        assert_eq!(workspace.repos, [root.canonicalize().unwrap()]);
        let proposal = workspace.parent_proposal.unwrap();
        assert_eq!(proposal.directory, dir.path().canonicalize().unwrap());
        assert_eq!(proposal.repos.len(), 2);
        assert!(proposal.repos.iter().any(|repo| repo.ends_with("web")));
        assert!(!dir.path().join(INDEX_DIR).exists());
        assert!(!root.join(WORKSPACE_FILE).exists());
    }

    #[test]
    fn no_repo_proposes_parent_once_and_never_walks_to_grandparent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty/deep")).unwrap();
        std::fs::create_dir(dir.path().join("api")).unwrap();
        std::fs::write(dir.path().join("api/package.json"), "{}").unwrap();
        let error = Workspace::load(&dir.path().join("empty")).unwrap_err();
        assert!(error.contains("1 repo(s)"), "{error}");
        assert!(error.contains("api"), "{error}");
        assert!(error.contains("carrick init .."), "{error}");
        let deep = Workspace::load(&dir.path().join("empty/deep")).unwrap_err();
        assert!(
            !deep.contains("api"),
            "must not inspect grandparent: {deep}"
        );
        assert!(!dir.path().join(INDEX_DIR).exists());
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
    fn two_repos_with_one_directory_name_are_refused_before_anything_is_scanned() {
        // The scan keys its output by directory name, so the second would
        // overwrite the first — and by then the first repo's answers are gone.
        let dir = workspace_with(r#"{"repos": ["./one/api", "./two/api"]}"#);
        std::fs::create_dir_all(dir.path().join("one/api")).unwrap();
        std::fs::create_dir_all(dir.path().join("two/api")).unwrap();
        let err = Workspace::load(dir.path()).unwrap_err();
        assert!(err.contains("both named 'api'"), "{err}");
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
