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
        // Named by directory, not by full path: the folder holding them is in
        // the same sentence, and a folder of scratch checkouts otherwise
        // prints eighty absolute paths on one line.
        let names = self
            .repos
            .iter()
            .map(|repo| {
                repo.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| repo.display().to_string())
            })
            .collect::<Vec<_>>();
        format!(
            "The parent folder {} holds {} {}: {}. Run carrick init .. to initialise that workspace.",
            self.directory.display(),
            self.repos.len(),
            if self.repos.len() == 1 {
                "repo"
            } else {
                "repos"
            },
            some_of(&names),
        )
    }
}

/// The first few names, and a count of the rest. Used wherever a message would
/// otherwise print a list nobody can read.
fn some_of(names: &[String]) -> String {
    let shown = names
        .iter()
        .take(NAMES_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(NAMES_SHOWN) {
        0 => shown,
        rest => format!("{shown} and {rest} more"),
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
        let detection = detect(root)?;
        let repos_detected_by = detection.detected_by;
        let mut entries = detection.repos;
        let repos_added = parsed.repos.clone();
        entries.extend(parsed.repos);
        let repos_excluded = parsed.exclude;
        let parent_proposal = if entries.is_empty() || repos_detected_by == "single repository" {
            inspect_parent(root)
        } else {
            None
        };
        if entries.is_empty() {
            // What was looked for and what was there, both of them: a bare
            // "no repos" cannot tell a user whether they are in the wrong
            // folder, whether their manifest is a kind Carrick does not read,
            // or whether every candidate was skipped as an artefact directory
            // (carrick#975).
            let listed = if file.is_file() {
                format!("\n{WORKSPACE_FILE} is here and lists no repos either.")
            } else {
                String::new()
            };
            return Err(format!(
                "no repos in {}.\n{}{listed}\n{}",
                root.display(),
                detection.census,
                parent_proposal
                    .as_ref()
                    .map(ParentProposal::description)
                    .unwrap_or_else(|| format!(
                        "The folder above holds no repos either. Run carrick init in a repository, or list repo paths in a {WORKSPACE_FILE} in the folder that holds them."
                    ))
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
    let paths = detect(parent).ok()?.repos;
    if paths.is_empty() {
        return None;
    }
    Some(ParentProposal {
        directory: parent.to_path_buf(),
        repos: paths
            .into_iter()
            .map(|path| match path.trim_start_matches("./") {
                "." => parent.to_path_buf(),
                name => parent.join(name),
            })
            .collect(),
    })
}

/// What detection found, and what it looked at to find it.
struct Detection {
    /// The sentence naming how the repos were arrived at.
    detected_by: String,
    /// Repo paths relative to the root, or `.` for the root itself.
    repos: Vec<String>,
    /// Rendered whenever `repos` is empty: the files checked and the
    /// directories walked, so a "no repos" answer can be acted on.
    census: String,
}

/// The files that make a directory a repository worth indexing. Named in the
/// census, so what a user is told is what was actually checked.
const REPO_MARKERS: [&str; 6] = [
    ".git",
    "package.json",
    "carrick.json",
    "deno.json",
    "deno.jsonc",
    "pnpm-workspace.yaml",
];

/// The manifests that make the root itself the thing to index.
const ROOT_MANIFESTS: &str =
    "carrick.json, a package.json with workspaces, pnpm-workspace.yaml, deno.json or deno.jsonc";

/// How many directories to name before counting the rest. A folder of scratch
/// checkouts holds dozens, and a wall of names answers nothing.
const NAMES_SHOWN: usize = 3;

/// The same repository detection is used by init and every index build.
fn detect(root: &Path) -> Result<Detection, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let found = |detected_by: &str, repos: Vec<String>| Detection {
        detected_by: detected_by.to_string(),
        repos,
        census: String::new(),
    };
    if std::fs::symlink_metadata(root.join("carrick.json")).is_ok() {
        return Ok(found("carrick.json", vec![".".into()]));
    }
    if !crate::service_derivation::workspace_patterns(root)?.is_empty()
        || root.join("deno.json").is_file()
        || root.join("deno.jsonc").is_file()
    {
        return Ok(found("workspace manifest", vec![".".into()]));
    }
    let mut repos = Vec::new();
    let mut unmarked = Vec::new();
    let mut skipped = 0usize;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        if name.starts_with('.')
            || crate::packages::MANIFEST_SKIP_DIRS.contains(&name.as_str())
            || ["target", "out", "coverage"].contains(&name.as_str())
        {
            skipped += 1;
            continue;
        }
        if is_repo(&entry.path()) {
            repos.push(format!("./{name}"));
        } else {
            unmarked.push(name);
        }
    }
    if !repos.is_empty() {
        repos.sort();
        return Ok(found("sibling repositories", repos));
    }
    if is_repo(root) {
        return Ok(found("single repository", vec![".".into()]));
    }
    unmarked.sort();
    Ok(Detection {
        detected_by: "workspace overrides".into(),
        repos: Vec::new(),
        census: census(root, &unmarked, skipped),
    })
}

/// What detection looked for and what it saw, in two lines a user can act on.
fn census(root: &Path, unmarked: &[String], skipped: usize) -> String {
    let root = root.display();
    let markers = REPO_MARKERS.join(", ");
    let inside = if unmarked.is_empty() && skipped == 0 {
        "Inside it: no directories.".to_string()
    } else {
        let mut parts = vec![format!("{} directories", unmarked.len() + skipped)];
        if skipped > 0 {
            parts.push(format!(
                "{skipped} skipped as dot, build or dependency directories"
            ));
        }
        if !unmarked.is_empty() {
            parts.push(format!(
                "{} holding none of those files ({})",
                unmarked.len(),
                some_of(unmarked)
            ));
        }
        format!("Inside it: {}.", parts.join(", "))
    };
    format!(
        "Looked for: {ROOT_MANIFESTS} in {root}, then a directory inside it holding one of {markers}.\nFound: none of those manifests in {root}. {inside}"
    )
}

fn is_repo(root: &Path) -> bool {
    REPO_MARKERS
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
                .filter(|detection| !detection.repos.is_empty())
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
        assert!(error.contains("holds 1 repo: api."), "{error}");
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
    fn a_repository_whose_only_manifest_is_deno_jsonc_is_one_repo() {
        // The shape a first run was refused on (carrick#975): a git
        // repository, a workspace manifest the package-manager patterns do not
        // cover, and no package.json anywhere in it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("apps/api")).unwrap();
        std::fs::create_dir_all(root.join("packages/shared")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(
            root.join("deno.jsonc"),
            r#"{"workspace": ["./apps/api", "./packages/shared"]}"#,
        )
        .unwrap();
        std::fs::write(root.join("apps/api/deno.json"), r#"{"name": "@scope/api"}"#).unwrap();
        std::fs::write(
            root.join("packages/shared/deno.json"),
            r#"{"name": "@scope/shared"}"#,
        )
        .unwrap();
        let workspace = Workspace::load(&root).unwrap();
        assert_eq!(workspace.repos_detected_by, "workspace manifest");
        assert_eq!(workspace.repos, [root.canonicalize().unwrap()]);
    }

    #[test]
    fn a_git_worktree_checkout_is_a_repository() {
        // A linked worktree's `.git` is a file, not a directory, so detection
        // reads the marker with symlink_metadata rather than is_dir.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("feature-branch");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        std::fs::write(root.join("src/main.ts"), "export const x = 1;\n").unwrap();
        let workspace = Workspace::load(&root).unwrap();
        assert_eq!(workspace.repos_detected_by, "single repository");
        assert_eq!(workspace.repos, [root.canonicalize().unwrap()]);
    }

    #[test]
    fn finding_nothing_states_what_was_looked_for_and_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        for name in ["alpha", "beta", "gamma", "delta"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        std::fs::create_dir(root.join("node_modules")).unwrap();
        std::fs::create_dir(root.join(".cache")).unwrap();
        let error = Workspace::load(&root).unwrap_err();
        assert!(error.starts_with("no repos in "), "{error}");
        assert!(error.contains("deno.jsonc"), "{error}");
        assert!(error.contains("pnpm-workspace.yaml"), "{error}");
        assert!(
            error.contains("Inside it: 6 directories, 2 skipped as dot, build or dependency directories, 4 holding none of those files (alpha, beta, delta and 1 more)."),
            "{error}"
        );
    }

    #[test]
    fn the_parent_proposal_names_a_few_repos_and_counts_the_rest() {
        let proposal = ParentProposal {
            directory: PathBuf::from("/code"),
            repos: (0..6)
                .map(|n| PathBuf::from(format!("/code/repo{n}")))
                .collect(),
        };
        let said = proposal.description();
        assert!(
            said.contains("holds 6 repos: repo0, repo1, repo2 and 3 more."),
            "{said}"
        );
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
        assert!(err.starts_with("no repos in "), "{err}");
        assert!(err.contains(&format!("{WORKSPACE_FILE} is here")), "{err}");
        assert!(err.contains("Inside it: no directories."), "{err}");
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
