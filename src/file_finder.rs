use crate::operation::EndpointProvenance;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use walkdir::WalkDir;

const TEST_DIR_NAMES: &[&str] = &[
    "__tests__",
    "__mocks__",
    "__fixtures__",
    "tests",
    "test",
    "fixtures",
    // An end-to-end suite and its harness. Same class as `tests/`, and missed
    // until #588: an e2e harness commonly stands up a stub of the collector or
    // sibling service it is testing against, so its route registrations were
    // being emitted as producer rows for the package that owns the suite. A
    // real consumer elsewhere in the workspace then matched the stub, and the
    // index answered "served here" about a service that serves nothing of the
    // sort. `e2e` is an ecosystem-wide directory convention, not a runner or
    // library name.
    //
    // The structurally stronger signal is program membership: a harness of
    // this shape usually sits outside the package's compiled program (not
    // reachable from its tsconfig `include`/`files`). It is deliberately not
    // used here. TypeScript's default `include` is `**/*`, so a package that
    // never narrows it reports the harness as in-program and the conventions
    // below would still have to carry the case; resolving `include`/`exclude`
    // properly means `extends` chains, `files` precedence, implicit
    // extensions and the default excludes, which is a subsystem rather than a
    // guard. Tracked as follow-up on #588; until then this list is the signal.
    "e2e",
    // Storybook config/preview files live under .storybook/ — tooling, not
    // product source (same reasoning as the .stories.* suffixes below).
    ".storybook",
];

const TEST_FILE_SUFFIXES: &[&str] = &[
    ".test.ts",
    ".test.tsx",
    ".spec.ts",
    ".spec.tsx",
    ".test.js",
    ".test.jsx",
    ".spec.js",
    ".spec.jsx",
    // Storybook stories are dev-only component showcases: they never run in
    // production, so any fetch/publish inside one is not a real contract
    // surface — and on UI-heavy repos they are numerous enough to matter for
    // LLM-analysis cost (metamask-extension alone has hundreds).
    ".stories.ts",
    ".stories.tsx",
    ".stories.js",
    ".stories.jsx",
];

/// Directory-name conventions that mark a subtree as mock/test-double code
/// while still being product-adjacent source (so its files ARE scanned, unlike
/// [`TEST_DIR_NAMES`] trees, which `find_files` skips entirely). The canonical
/// case is a mock-service-worker handler tree under `src/mocks/`. These are
/// ecosystem-wide path conventions, deliberately NOT a mock-framework package
/// list — provenance must hold for any registration idiom.
///
/// `cypress` is the directory convention of a browser end-to-end suite: its
/// `support/` and `plugins/` files register listeners and stubs that never run
/// in production (carrick#1626). Its `e2e/` and `fixtures/` already fall under
/// [`TEST_DIR_NAMES`]; the rest is kept and tagged rather than skipped, so the
/// file set a scan reads does not change.
const MOCK_DIR_NAMES: &[&str] = &["mocks", "mock", "cypress"];

/// Whether any directory segment of `path`, relative to the scanned
/// `root_dir`, matches one of `dir_names` (case-insensitive). Only checked
/// relative to the root so a scan whose root itself sits inside such a
/// directory (e.g. eval fixtures under `tests/fixtures/`) is unaffected;
/// a path outside `root_dir` conservatively returns false.
fn has_dir_named(path: &Path, root_dir: &Path, dir_names: &[&str]) -> bool {
    let relative_path = match path.strip_prefix(root_dir) {
        Ok(p) => p,
        Err(_) => return false,
    };

    relative_path.ancestors().skip(1).any(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| {
                dir_names
                    .iter()
                    .any(|pattern| name.eq_ignore_ascii_case(pattern))
            })
            .unwrap_or(false)
    })
}

fn has_test_dir(path: &Path, root_dir: &Path) -> bool {
    has_dir_named(path, root_dir, TEST_DIR_NAMES)
}

/// Whether `path` sits under a test directory ([`TEST_DIR_NAMES`]) below
/// `root_dir`: the folders [`find_files`] does not read. For a walk over
/// files that are not TypeScript (the GraphQL walk, carrick#1626), so both
/// read the same tree.
pub fn is_under_test_dir(path: &Path, root_dir: &Path) -> bool {
    has_test_dir(path, root_dir)
}

fn has_test_suffix(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            TEST_FILE_SUFFIXES
                .iter()
                .any(|suffix| lower.ends_with(suffix))
        })
        .unwrap_or(false)
}

fn is_test_path(path: &Path, root_dir: &Path) -> bool {
    has_test_dir(path, root_dir) || has_test_suffix(path)
}

/// Whether a scan rooted at `root_dir` would read this file at all.
///
/// The one statement of what the scanner's input IS: the four extensions
/// [`find_files`] collects, minus the test paths it skips. Purely a question
/// about the path, so it answers for a file that has been deleted as readily
/// as for one on disk.
///
/// It has a second caller because "has the tree moved under the index?" is the
/// same question: a changed `carrick.json`, workflow or editor setting holds no
/// indexed row and cannot make one stale, and counting those as drift told a
/// user who had just finished onboarding that six files had moved under their
/// brand-new index (carrick#1007 item 5).
pub fn is_scanned_source(path: &Path, root_dir: &Path) -> bool {
    let Some(extension) = path.extension() else {
        return false;
    };
    if !matches!(
        extension.to_string_lossy().to_lowercase().as_str(),
        "js" | "ts" | "jsx" | "tsx"
    ) {
        return false;
    }
    !is_test_path(path, root_dir)
}

/// Whether any path segment BELOW the scan root matches an ignore pattern
/// exactly. Matching whole segments (not substrings) keeps files like
/// `src/buildkite.ts` or `lib/distances.ts` in scope, and stripping the root
/// first means an explicitly configured scan root named after a pattern
/// (e.g. a service directory `packages/build`) still gets scanned — only its
/// descendants can trigger the ignore.
fn is_ignored(path: &Path, root_dir: &Path, ignore_patterns: &[&str]) -> bool {
    // A path outside the scan root has no segments "below the root" to test;
    // matching against the full (possibly absolute) path would let ignore
    // patterns hit unrelated leading segments.
    let Ok(relative_path) = path.strip_prefix(root_dir) else {
        return false;
    };
    relative_path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|segment| ignore_patterns.contains(&segment))
            .unwrap_or(false)
    })
}

/// Classify where an endpoint's evidence comes from, structurally, from its
/// source path relative to the scanned root: a file under a mock tree
/// ([`MOCK_DIR_NAMES`]) — or any test path per [`is_test_path`], should one
/// ever reach extraction via a custom include — yields
/// [`EndpointProvenance::Mock`]; everything else is a real
/// [`EndpointProvenance::Route`].
///
/// This is the `is_test_path` classification extended to mock trees (#380):
/// test trees are excluded from scanning outright, while mock trees are kept
/// — their handlers often encode the canonical contract — and tagged so
/// matching and the report can say "this producer shape comes from a mock".
///
/// Conservative by construction: a path that cannot be resolved relative to
/// `root_dir` (e.g. an `include` root outside the service directory) is left
/// as `Route` rather than risking segment matches against prefix directories
/// the scan never chose (the same guard `has_test_dir` uses). Known
/// limitation, logged in #380: mock servers living outside a conventionally
/// named tree are not detected; composing structural signals such as
/// dev-dependency placement of the registering import or reachability from
/// runtime entry points is follow-up work.
pub fn endpoint_provenance(path: &Path, root_dir: &Path) -> EndpointProvenance {
    // A path outside the scan root has no segments below the root to
    // classify; is_test_path's suffix check alone would still fire on it,
    // contradicting the conservative guarantee above.
    if path.strip_prefix(root_dir).is_err() {
        return EndpointProvenance::Route;
    }
    if has_dir_named(path, root_dir, MOCK_DIR_NAMES) || is_test_path(path, root_dir) {
        EndpointProvenance::Mock
    } else {
        EndpointProvenance::Route
    }
}

/// Whether `dir` is the top of a git checkout: it holds `.git`, a directory in
/// a clone and a file in a linked worktree or a submodule. The npm installer
/// asks the same of a sibling repository (`npm/carrick/src/init/repos.ts`,
/// carrick#975).
pub fn is_git_checkout(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// The paths the checkout at `top` declares as its submodules, relative to
/// `top`: every `path` its `.gitmodules` states.
///
/// Read as text, one `path = <value>` line per submodule, which is how git
/// writes the file. A value this reads differently from git names no
/// directory on disk, and the checkout it was meant for is then left out and
/// named in the scan's output, never read by mistake.
fn declared_submodules(top: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(top.join(".gitmodules")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.trim().split_once('='))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case("path"))
        .map(|(_, value)| {
            Path::new(value.trim().trim_matches('"'))
                .components()
                .filter(|part| matches!(part, std::path::Component::Normal(_)))
                .collect()
        })
        .collect()
}

/// Whether `dir` is a git checkout the repository around it does not state:
/// a linked worktree, or a clone somebody made inside the tree.
///
/// Such a directory is another working tree, often a whole copy of the
/// repository it sits in, and no commit of that repository holds a file of
/// it. A submodule is the one checkout a repository does state: the checkout
/// above it lists the path in `.gitmodules` and its commit pins what the
/// directory holds, so a submodule is read as the rest of the tree is.
///
/// "The repository around it" is the nearest directory above `dir` that is a
/// checkout itself, wherever the walk started. With none above it, nothing
/// states `dir`.
pub fn is_separate_checkout(dir: &Path) -> bool {
    if !is_git_checkout(dir) {
        return false;
    }
    // Absolute, and not canonical: a walk started at `.` must still reach the
    // checkout above its own root, and a directory linked in is asked about
    // where the link sits.
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let Some(top) = dir.ancestors().skip(1).find(|above| is_git_checkout(above)) else {
        return true;
    };
    let Ok(relative) = dir.strip_prefix(top) else {
        return true;
    };
    !declared_submodules(top)
        .iter()
        .any(|declared| declared == relative)
}

/// What a checkout's git tracks, as far as a walk asks it: every tracked path
/// and every directory that holds one, relative to the checkout's top.
#[derive(Debug, Default)]
struct Tracked {
    /// Tracked paths: files, symlinks and submodules.
    paths: HashSet<PathBuf>,
    /// Every directory a tracked path sits in, below the top.
    dirs: HashSet<PathBuf>,
}

impl Tracked {
    fn from_paths(paths: Vec<String>) -> Tracked {
        let mut tracked = Tracked::default();
        for path in paths {
            let path = PathBuf::from(path);
            for dir in path.ancestors().skip(1) {
                if dir.as_os_str().is_empty() || !tracked.dirs.insert(dir.to_path_buf()) {
                    break;
                }
            }
            tracked.paths.insert(path);
        }
        tracked
    }

    /// Whether git tracks anything at `relative`: a file under it, the path
    /// itself, or a tracked symlink the path is reached through.
    fn tracks_under(&self, relative: &Path) -> bool {
        self.dirs.contains(relative)
            || relative
                .ancestors()
                .any(|path| !path.as_os_str().is_empty() && self.paths.contains(path))
    }
}

/// What git tracks in the checkout at `top`, asked once per checkout per
/// process. `None` when git cannot answer (no git, or no repository there).
fn tracked_in(top: &Path) -> Option<Arc<Tracked>> {
    static TRACKED: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<Tracked>>>>> = OnceLock::new();
    let cache = TRACKED.get_or_init(Default::default);
    if let Some(known) = cache.lock().ok()?.get(top) {
        return known.clone();
    }
    let answer = crate::git_state::tracked_paths(top, &[])
        .ok()
        .map(|paths| Arc::new(Tracked::from_paths(paths)));
    cache.lock().ok()?.insert(top.to_path_buf(), answer.clone());
    answer
}

/// What git ignores and does not track in the checkout at `top`
/// ([`crate::git_state::ignored_paths`], carrick#1902), relative to `top`,
/// asked once per checkout per process. A folder git lists whole is held
/// without its trailing `/`. `None` when git cannot answer.
fn ignored_in(top: &Path) -> Option<Arc<HashSet<PathBuf>>> {
    type Ignored = Option<Arc<HashSet<PathBuf>>>;
    static IGNORED: OnceLock<Mutex<HashMap<PathBuf, Ignored>>> = OnceLock::new();
    let cache = IGNORED.get_or_init(Default::default);
    if let Some(known) = cache.lock().ok()?.get(top) {
        return known.clone();
    }
    let answer = crate::git_state::ignored_paths(top).ok().map(|paths| {
        Arc::new(
            paths
                .iter()
                .map(|path| PathBuf::from(path.trim_end_matches('/')))
                .collect(),
        )
    });
    cache.lock().ok()?.insert(top.to_path_buf(), answer.clone());
    answer
}

/// The checkout `path` sits in, and `path` relative to its top: the nearest
/// directory above `path` that holds `.git`. Absolute, and not canonical, as
/// [`is_separate_checkout`] reads it.
fn in_checkout(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let top = path
        .ancestors()
        .skip(1)
        .find(|above| is_git_checkout(above))?;
    let relative = path.strip_prefix(top).ok()?.to_path_buf();
    Some((top.to_path_buf(), relative))
}

/// Whether `dir` is a dot folder git tracks nothing under (carrick#1607): a
/// tool's cache, a virtual environment, a build's output, kept beside the
/// source and never committed.
///
/// The name alone decides nothing: a framework can define a dot folder as an
/// application's own source (a server-only module folder, say). That folder
/// is committed, so git tracking a file under it is what keeps it read. With
/// no checkout above `dir`, or a git that cannot answer, nothing says the
/// folder is not the repository's, and it is read.
pub fn is_untracked_dot_folder(dir: &Path) -> bool {
    if !dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
    {
        return false;
    }
    let Some((top, relative)) = in_checkout(dir) else {
        return false;
    };
    tracked_in(&top).is_some_and(|tracked| !tracked.tracks_under(&relative))
}

/// Whether git ignores `path` and tracks nothing at it (carrick#1902): the
/// file, or a folder git lists whole.
///
/// Git lists an ignored folder once and nothing below it, so a walk rooted
/// in one (a service `directory`, an `include` root or a scan path the
/// config names) meets nothing here and reads it whole. A file git lists on
/// its own, beside tracked files in a folder an ignore pattern matches, is
/// brought back by naming that file. With no checkout above `path`, or a
/// git that cannot answer, nothing is ignored.
fn is_git_ignored(path: &Path) -> bool {
    let Some((top, relative)) = in_checkout(path) else {
        return false;
    };
    ignored_in(&top).is_some_and(|ignored| ignored.contains(&relative))
}

/// Why a walk stops at a directory below its root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBoundary {
    /// `.git` itself: the repository's own records, never its source.
    Records,
    /// A checkout of its own ([`is_separate_checkout`]).
    SeparateCheckout,
    /// A dot folder git tracks nothing under ([`is_untracked_dot_folder`]).
    UntrackedDotFolder,
    /// A file or folder git ignores and tracks nothing at
    /// ([`is_git_ignored`]). The source walk enters one to read what the
    /// service's own code imports from it ([`walk_service`]); every other
    /// walk stops there.
    Ignored,
}

/// Whether a walk stops at `entry`, and why (carrick#1902).
///
/// **The one rule every walk of a repository's tree shares**: the source walk
/// here, the GraphQL walks, the manifest walks and the workspace-member walk
/// all ask it, so a file one of them leaves out is left out by all of them.
///
/// The walk's own root is never a boundary, whatever it holds: a scan started
/// inside a linked worktree reads that worktree, and a service `directory` or
/// an `include` root that names a checkout reads it whole. Naming it is how a
/// checkout this rule leaves out is brought back.
///
/// A dot folder is a boundary only where git tracks nothing under it
/// (carrick#1607). A committed one is read as any folder is.
///
/// A file or folder git ignores is a boundary ([`is_git_ignored`]): a stray
/// copy of the repository in a backup folder is not its source.
pub fn git_boundary(entry: &walkdir::DirEntry) -> Option<GitBoundary> {
    if entry.depth() == 0 {
        return None;
    }
    boundary_at(entry.path(), entry.file_type().is_dir())
}

/// [`git_boundary`] for `path`, a directory or not, below a walk's root.
fn boundary_at(path: &Path, is_dir: bool) -> Option<GitBoundary> {
    if is_dir {
        if path.file_name().is_some_and(|name| name == ".git") {
            return Some(GitBoundary::Records);
        }
        if is_separate_checkout(path) {
            return Some(GitBoundary::SeparateCheckout);
        }
        if is_untracked_dot_folder(path) {
            return Some(GitBoundary::UntrackedDotFolder);
        }
    }
    is_git_ignored(path).then_some(GitBoundary::Ignored)
}

/// Whether a walk rooted at `root` would stop before it reached `path`
/// ([`git_boundary`]): at `path` itself, or at a folder between the two.
/// For a path a walk did not find, such as a glob's match: a
/// `graphqlSchemas` entry names what comes before its first wildcard, and
/// what lies below that is read as a walk of it would read it.
pub fn left_out_below(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    let parts: Vec<_> = relative.components().collect();
    parts.iter().enumerate().any(|(index, part)| {
        at.push(part);
        let is_dir = index + 1 < parts.len() || at.is_dir();
        boundary_at(&at, is_dir).is_some()
    })
}

/// The entries of a source walk below `dir`: everything the scan may read,
/// with the ignored directories never entered.
///
/// Pruning at the directory rather than filtering at the file is not an
/// optimisation, it is the difference between a walk that terminates in
/// seconds and one that does not terminate usefully at all. The walk follows
/// symlinks, because a service's source can be linked in from elsewhere in the
/// repo. An installed `node_modules` is built out of symlinks: every workspace
/// dependency is a link back into the repo's own source, and those links form
/// a graph with no cycles for the walker's ancestor detection to catch and one
/// distinct path per route through the dependency graph. On a synthetic
/// workspace of 16 packages with two dependencies each, 97 real files were
/// visited 40,477 times; on a real monorepo whose dependencies were installed
/// for the first time, this cost a scan sixteen minutes of silence
/// (carrick#748, carrick#706).
///
/// Sorted so the walk is the same on every host: readdir order differs between
/// APFS and ext4, and any "first/last one wins" over an unsorted walk is a
/// host-dependent result (#569 found one).
///
/// `left_out` are directories below `root`, relative to it, that the walk
/// never enters: the services nested in the one being walked (carrick#553).
/// Pruned for the reason above, and so a nested service's tree is read once.
///
/// `stopped` receives every separate checkout and untracked dot folder the
/// walk stopped at ([`git_boundary`]), as walked, so the caller can say what
/// was left out, and every path git ignores, which the walk enters.
///
/// `exclusion` is the service's `exclude` (carrick#1990): what it matches is
/// never entered either, and is put in `stopped` to be counted.
fn source_entries<'a>(
    root: &'a Path,
    ignore_patterns: &'a [&'a str],
    left_out: &'a [PathBuf],
    exclusion: &'a Exclusion,
    stopped: &'a RefCell<Stopped>,
) -> impl Iterator<Item = walkdir::DirEntry> + 'a {
    WalkDir::new(root)
        .sort_by_file_name()
        .follow_links(true)
        .into_iter()
        .filter_entry(move |entry| {
            if is_ignored(entry.path(), root, ignore_patterns)
                || entry
                    .path()
                    .strip_prefix(root)
                    .is_ok_and(|relative| left_out.iter().any(|dir| relative == dir))
            {
                return false;
            }
            if entry.depth() > 0 && exclusion.excludes(entry.path(), entry.file_type().is_dir()) {
                stopped
                    .borrow_mut()
                    .excluded
                    .push(entry.path().to_path_buf());
                return false;
            }
            let mut stopped = stopped.borrow_mut();
            match git_boundary(entry) {
                None => true,
                Some(GitBoundary::Records) => false,
                Some(GitBoundary::SeparateCheckout) => {
                    stopped.checkouts.push(entry.path().to_path_buf());
                    false
                }
                Some(GitBoundary::UntrackedDotFolder) => {
                    stopped.dot_folders.push(entry.path().to_path_buf());
                    false
                }
                // Entered: what the service's own code imports from it is
                // read ([`keep_what_is_imported`]).
                Some(GitBoundary::Ignored) => {
                    stopped.ignored.push(entry.path().to_path_buf());
                    true
                }
            }
        })
        .filter_map(|e| e.ok())
}

/// The directories a walk stopped at that a reader is told about.
#[derive(Debug, Default)]
struct Stopped {
    /// Separate checkouts ([`GitBoundary::SeparateCheckout`]).
    checkouts: Vec<PathBuf>,
    /// Dot folders git tracks nothing under
    /// ([`GitBoundary::UntrackedDotFolder`]).
    dot_folders: Vec<PathBuf>,
    /// Files and folders git ignores ([`GitBoundary::Ignored`]). The walk
    /// enters these, and what it finds under one is read only when the
    /// service's own code imports it.
    ignored: Vec<PathBuf>,
    /// Files and folders the service's `exclude` matches ([`Exclusion`]).
    excluded: Vec<PathBuf>,
}

/// What a service's carrick.json `exclude` leaves out (carrick#1990): paths
/// matching its patterns, read in gitignore syntax relative to the service's
/// directory.
///
/// An excluded file is not walked, so it is not analysed or typed and states
/// no row. A file that is not excluded and imports one still resolves the
/// import through the compiler, as it does for any file outside the walk.
/// Nothing is excluded by default or by a name written here.
#[derive(Default)]
pub struct Exclusion {
    root: PathBuf,
    matcher: Option<ignore::gitignore::Gitignore>,
    patterns: usize,
}

impl std::fmt::Debug for Exclusion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Exclusion")
            .field("root", &self.root)
            .field("patterns", &self.patterns)
            .finish()
    }
}

impl Exclusion {
    /// The matcher for `patterns` rooted at `root`, or the reason one of
    /// them is not a pattern.
    fn build(root: &Path, patterns: &[String]) -> Result<Exclusion, String> {
        if patterns.is_empty() {
            return Ok(Exclusion::default());
        }
        let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
        for pattern in patterns {
            builder
                .add_line(None, pattern)
                .map_err(|error| format!("`{pattern}` is not a pattern: {error}"))?;
        }
        let matcher = builder.build().map_err(|error| error.to_string())?;
        Ok(Exclusion {
            root: root.to_path_buf(),
            matcher: Some(matcher),
            patterns: patterns.len(),
        })
    }

    /// Whether every pattern reads as one, for the config to refuse what
    /// would otherwise exclude nothing.
    pub fn check(patterns: &[String]) -> Result<(), String> {
        Self::build(Path::new(""), patterns).map(|_| ())
    }

    /// The service's `exclude`, rooted at its directory. A config that
    /// passed [`Self::check`] always builds; one that did not excludes
    /// nothing.
    pub fn of_service(repo_root: &Path, service: &crate::config::Config) -> Exclusion {
        Self::build(&service_root(repo_root, service), &service.exclude).unwrap_or_default()
    }

    /// How many patterns the service states.
    pub fn patterns(&self) -> usize {
        self.patterns
    }

    /// Whether `path` is excluded: it, or a folder it sits in, matches. A
    /// path outside the service's directory, and the directory itself, never
    /// is.
    pub fn excludes(&self, path: &Path, is_dir: bool) -> bool {
        let Some(matcher) = &self.matcher else {
            return false;
        };
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.as_os_str().is_empty() {
            return false;
        }
        matcher
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }
}

/// What one walk read, and the directories it stopped at.
struct Walked {
    files: Vec<Found>,
    config_file: Option<PathBuf>,
    stopped: Stopped,
}

/// A source file a walk found, and whether it sits at or under a path git
/// ignores ([`GitBoundary::Ignored`]).
#[derive(Debug, Clone)]
struct Found {
    path: PathBuf,
    ignored: bool,
}

/// The source files a walk reads from what it `found`, in walk order, and
/// the ignored ones it leaves out (carrick#1902).
///
/// A file git ignores is read when the files read import it, directly or
/// through another ignored file they import: a client or a set of types a
/// build step generates and the committed code uses. A copy of the
/// repository in a folder git ignores, a backup or a stray worktree, is
/// imported by nothing and stays out. `modules` resolves a specifier the way
/// the call graph does ([`WorkspaceIndex::resolve_module_path`]), and is
/// built only when something ignored was found.
///
/// Specifiers are read from the source as written: `import`, `export ...
/// from`, `import x = require()`, and `require` or `import()` with a literal
/// ([`crate::parser::ModuleReader::loaded_specifiers`]). A specifier the
/// source computes names no file, and what only it would load stays out.
fn keep_what_is_imported(
    found: Vec<Found>,
    modules: impl FnOnce() -> crate::workspace_resolver::WorkspaceIndex,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    if !found.iter().any(|file| file.ignored) {
        return (
            found.into_iter().map(|file| file.path).collect(),
            Vec::new(),
        );
    }
    let modules = modules();
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut read: Vec<bool> = found.iter().map(|file| !file.ignored).collect();
    let mut waiting: HashMap<PathBuf, usize> = found
        .iter()
        .enumerate()
        .filter(|(_, file)| file.ignored)
        .map(|(index, file)| (canonical(&file.path), index))
        .collect();
    let mut queue: Vec<usize> = (0..found.len()).filter(|&index| read[index]).collect();
    let mut reader = crate::parser::ModuleReader::quiet();
    while let Some(index) = queue.pop() {
        if waiting.is_empty() {
            break;
        }
        let importer = &found[index].path;
        for specifier in reader.loaded_specifiers(importer) {
            let Some(target) = modules.resolve_module_path(importer, &specifier) else {
                continue;
            };
            if let Some(imported) = waiting.remove(&canonical(&target)) {
                read[imported] = true;
                queue.push(imported);
            }
        }
    }
    let (kept, left_out): (Vec<_>, Vec<_>) =
        found.into_iter().zip(read).partition(|(_, read)| *read);
    (
        kept.into_iter().map(|(file, _)| file.path).collect(),
        left_out.into_iter().map(|(file, _)| file.path).collect(),
    )
}

/// Find all JavaScript and TypeScript files in a directory
/// Also looks for carrick.json configuration file
/// Returns (js_ts_files, config_file_option)
///
/// Deliberately does NOT report a `package.json`: the walk visits every
/// manifest below `dir` (test fixtures, examples, nested workers) in
/// filesystem readdir order, so "the" manifest it found was whichever came
/// last on that host. A service's manifest is the one at its root and nowhere
/// else — see [`find_service_files`].
///
/// A file git ignores is read only when a file read imports it
/// ([`keep_what_is_imported`]), resolved with the configs nearest each file.
pub fn find_files(dir: &str, ignore_patterns: &[&str]) -> (Vec<PathBuf>, Option<PathBuf>) {
    let walked = find_files_leaving_out(dir, ignore_patterns, &[], &Exclusion::default());
    let (files, _) = keep_what_is_imported(walked.files, || {
        crate::workspace_resolver::WorkspaceIndex::build_with_aliases(Path::new(dir), None)
    });
    (files, walked.config_file)
}

/// [`find_files`] before an ignored file is kept or left out, never entering
/// the `left_out` directories below `dir` or what `exclusion` matches.
fn find_files_leaving_out(
    dir: &str,
    ignore_patterns: &[&str],
    left_out: &[PathBuf],
    exclusion: &Exclusion,
) -> Walked {
    let mut js_ts_files = Vec::new();
    let mut config_file = None;
    let stopped = RefCell::new(Stopped::default());
    let root_path = Path::new(dir);

    for entry in source_entries(root_path, ignore_patterns, left_out, exclusion, &stopped) {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        // The walk stopped at every ignored path above this one before it
        // reached it, so the list already holds them.
        let ignored = stopped
            .borrow()
            .ignored
            .iter()
            .any(|ignored| path.starts_with(ignored));

        if path.file_name().is_some_and(|name| name == "carrick.json") {
            if !ignored {
                config_file = Some(path.to_path_buf());
            }
            continue;
        }

        if is_scanned_source(path, root_path) {
            js_ts_files.push(Found {
                path: path.to_path_buf(),
                ignored,
            });
        }
    }

    Walked {
        files: js_ts_files,
        config_file,
        stopped: stopped.into_inner(),
    }
}

/// How many source files the walk rooted at `root` would have read under the
/// `excluded` paths it did not enter (carrick#1990).
fn count_excluded(excluded: &[PathBuf], root: &Path, ignore_patterns: &[&str]) -> usize {
    excluded
        .iter()
        .map(|path| {
            if !path.is_dir() {
                return usize::from(is_scanned_source(path, root));
            }
            source_entries(
                path,
                ignore_patterns,
                &[],
                &Exclusion::default(),
                &RefCell::default(),
            )
            .filter(|entry| entry.path().is_file() && is_scanned_source(entry.path(), root))
            .count()
        })
        .sum()
}

/// Where a service's own tree starts: its `directory` under the repo root, or
/// the repo root itself for a single-service repo.
fn service_root(repo_root: &Path, service: &crate::config::Config) -> PathBuf {
    match &service.directory {
        Some(dir) => repo_root.join(dir),
        None => repo_root.to_path_buf(),
    }
}

/// A service's own package or Deno manifest, if it has one.
///
/// The file at the service root and nowhere else: a scan must pick the same
/// manifest on every host, and the root is the only one that is unambiguously
/// the service's. No walk — the caller that only wants the manifest was
/// walking the whole service tree to reach a path it already knew, once per
/// service (carrick#748).
pub fn find_service_manifest(repo_path: &Path, service: &crate::config::Config) -> Option<PathBuf> {
    let root = service_root(repo_path, service);
    ["package.json", "deno.json", "deno.jsonc"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|manifest| manifest.is_file())
}

/// Find JS/TS files for a single service, scoped to its `directory` plus any
/// extra `include` source roots (e.g. shared libraries copied in at build
/// time), all relative to `repo_path`. Also returns that service's
/// `package.json`: the file at the service root itself, if present. Nested
/// manifests (fixtures, examples, a sibling service under a `directory: "."`
/// root) are never candidates — a scan must pick the same manifest on every
/// host, and the root is the only one that is unambiguously the service's.
///
/// A service with `directory: None` scopes to the whole repo, so the
/// single-service path behaves exactly like a plain [`find_files`] walk.
///
/// **A file belongs to the deepest service whose directory holds it**
/// (carrick#553). The walk of the service's own directory never enters the
/// directory of another service nested inside it
/// ([`crate::config::Config::nested_directories`]): that source is the nested
/// service's, and reading it here indexed its routes and functions under both
/// names and sent its files to model analysis twice. The same rule the local
/// read model applies when it names a file's service
/// (`IndexedRepo::service_for`).
///
/// **`include` roots are walked whole.** A root a service names there is
/// source it declares on purpose, so it is read even where it sits inside a
/// nested service's directory, and a nested service inside an include root is
/// read with it.
///
/// **A checkout of its own inside the service is not entered**
/// ([`git_boundary`], carrick#1902). [`walk_service`] also says which ones.
///
/// **A file git ignores is read only when the service's own code imports
/// it** ([`keep_what_is_imported`], carrick#1902), resolved with the
/// `tsconfig` the service names. A generated client the committed code uses
/// is read; a backup copy of the repository is not.
pub fn find_service_files(
    repo_path: &str,
    service: &crate::config::Config,
    ignore_patterns: &[&str],
) -> (Vec<PathBuf>, Option<PathBuf>) {
    let walk = walk_service(repo_path, service, ignore_patterns);
    (walk.files, walk.manifest)
}

/// What a service's walk read, and what it left out for a caller to say so.
#[derive(Debug, Default)]
pub struct ServiceWalk {
    /// The service's source files, as [`find_service_files`] lists them.
    pub files: Vec<PathBuf>,
    /// The service's own manifest ([`find_service_manifest`]).
    pub manifest: Option<PathBuf>,
    /// The separate checkouts below the service's roots that the walk did not
    /// enter, sorted, each under `repo_path` as given. One that holds a root
    /// the config names (an `include` root, a nested service) is not listed:
    /// what was named inside it is read by the walk rooted there.
    pub checkouts_left_out: Vec<PathBuf>,
    /// The dot folders git tracks nothing under that the walk did not enter
    /// (carrick#1607), listed as `checkouts_left_out` is.
    pub dot_folders_left_out: Vec<PathBuf>,
    /// The paths git ignores that the walk left source files out of
    /// (carrick#1902), listed as `checkouts_left_out` is
    /// ([`ignored_left_out`]).
    pub ignored_left_out: Vec<PathBuf>,
    /// What the service's `exclude` left out (carrick#1990).
    pub excluded: ExcludedFiles,
}

/// The ignored paths a reader is told were left out, from the paths the walk
/// found ignored (`stopped`, sorted), the files it read and the ignored
/// source files it left out (carrick#1902).
///
/// A path that holds no source file is not named: an ignored `.env` or log
/// folder was never source. One the walk read nothing under is named whole.
/// Where the service's code imports some of what one holds, the files left
/// out under it are named instead, so the folder is never named as left out
/// when part of it was read.
fn ignored_left_out(stopped: Vec<PathBuf>, read: &[PathBuf], left_out: &[PathBuf]) -> Vec<PathBuf> {
    let outermost: Vec<&PathBuf> = stopped
        .iter()
        .filter(|path| {
            !stopped
                .iter()
                .any(|above| above != *path && path.starts_with(above))
        })
        .collect();
    let mut named = Vec::new();
    for path in outermost {
        let under: Vec<&PathBuf> = left_out
            .iter()
            .filter(|file| file.starts_with(path))
            .collect();
        if under.is_empty() {
            continue;
        }
        if read.iter().any(|file| file.starts_with(path)) {
            named.extend(under.into_iter().cloned());
        } else {
            named.push(path.clone());
        }
    }
    named.sort();
    named
}

/// How many `exclude` patterns a service states, and how many source files
/// they left out of its walk (carrick#1990).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ExcludedFiles {
    pub patterns: usize,
    pub files: usize,
}

/// [`find_service_files`], with the directories the walk stopped at.
pub fn walk_service(
    repo_path: &str,
    service: &crate::config::Config,
    ignore_patterns: &[&str],
) -> ServiceWalk {
    let root = Path::new(repo_path);
    let service_root = service_root(root, service);

    // The nested services' directories, relative to this service's own, which
    // is how the walk below sees them.
    let own = crate::config::directory_segments(service.directory.as_deref());
    let left_out: Vec<PathBuf> = service
        .nested_directories
        .iter()
        .map(|nested| crate::config::directory_segments(Some(nested)))
        .filter(|nested| nested.len() > own.len() && nested.starts_with(&own))
        .map(|nested| nested[own.len()..].iter().collect())
        .collect();

    // The carrick.json lives at the repo root, not per service directory, so the
    // config returned here is ignored — config resolution is handled separately.
    let exclusion = Exclusion::of_service(root, service);
    let Walked {
        mut files,
        config_file: _,
        mut stopped,
    } = find_files_leaving_out(
        &service_root.to_string_lossy(),
        ignore_patterns,
        &left_out,
        &exclusion,
    );
    let mut excluded_files = count_excluded(&stopped.excluded, &service_root, ignore_patterns);

    let manifest = find_service_manifest(root, service);

    for inc in &service.include {
        let inc_path = root.join(inc);
        let included = find_files_leaving_out(
            &inc_path.to_string_lossy(),
            ignore_patterns,
            &[],
            &exclusion,
        );
        files.extend(included.files);
        stopped.checkouts.extend(included.stopped.checkouts);
        stopped.dot_folders.extend(included.stopped.dot_folders);
        stopped.ignored.extend(included.stopped.ignored);
        // Only what the directory walk did not already count.
        let new: Vec<PathBuf> = included
            .stopped
            .excluded
            .into_iter()
            .filter(|path| !stopped.excluded.iter().any(|seen| path.starts_with(seen)))
            .collect();
        excluded_files += count_excluded(&new, &inc_path, ignore_patterns);
        stopped.excluded.extend(new);
    }

    // An `include` root may overlap the service directory; keep the first
    // occurrence of each file so a file is never parsed twice. A file one
    // walk found under an ignored folder and another read as named (an
    // `include` root git ignores) is named.
    let named_here: HashSet<PathBuf> = files
        .iter()
        .filter(|file| !file.ignored)
        .map(|file| file.path.clone())
        .collect();
    let mut seen = HashSet::new();
    files.retain(|file| seen.insert(file.path.clone()));
    for file in &mut files {
        file.ignored &= !named_here.contains(&file.path);
    }
    let (files, ignored_files) = keep_what_is_imported(files, || {
        let service_tsconfig = service.alias_tsconfig();
        crate::workspace_resolver::WorkspaceIndex::build_with_aliases(
            root,
            service_tsconfig
                .as_ref()
                .map(|(directory, tsconfig)| (directory.as_path(), tsconfig.as_path())),
        )
    });

    // A directory that holds a root the config names was stopped at by the
    // directory walk and is read, as far as it was named, by the walk rooted
    // inside it: this service's `include` root, or the nested service's own.
    let named: Vec<PathBuf> = service
        .include
        .iter()
        .chain(&service.nested_directories)
        .map(|directory| root.join(directory))
        .collect();
    let left_out_named_nothing = |mut dirs: Vec<PathBuf>| {
        dirs.retain(|dir| !named.iter().any(|named| named.starts_with(dir)));
        dirs.sort();
        dirs.dedup();
        dirs
    };
    let ignored_left_out = ignored_left_out(
        left_out_named_nothing(stopped.ignored),
        &files,
        &ignored_files,
    );

    ServiceWalk {
        files,
        manifest,
        checkouts_left_out: left_out_named_nothing(stopped.checkouts),
        dot_folders_left_out: left_out_named_nothing(stopped.dot_folders),
        ignored_left_out,
        excluded: ExcludedFiles {
            patterns: exclusion.patterns(),
            files: excluded_files,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::tempdir;

    #[test]
    fn service_manifest_accepts_deno_and_prefers_package_json() {
        let repo = tempdir().unwrap();
        let root = repo.path();
        File::create(root.join("deno.jsonc")).unwrap();
        assert_eq!(
            find_service_manifest(root, &crate::config::Config::default()),
            Some(root.join("deno.jsonc"))
        );

        File::create(root.join("deno.json")).unwrap();
        assert_eq!(
            find_service_manifest(root, &crate::config::Config::default()),
            Some(root.join("deno.json"))
        );

        File::create(root.join("package.json")).unwrap();
        assert_eq!(
            find_service_manifest(root, &crate::config::Config::default()),
            Some(root.join("package.json"))
        );
    }

    #[test]
    fn find_files_skips_test_sources() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("__tests__")).expect("__tests__ dir");

        let source_file = root.join("src").join("app.ts");
        let test_file = root.join("__tests__").join("app.spec.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&source_file).expect("source file");
        File::create(&test_file).expect("test file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![source_file]);
        assert_eq!(config, Some(config_path));
    }

    #[test]
    fn find_files_skips_suffixes_and_fixture_dirs() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("services")).expect("services dir");
        fs::create_dir_all(root.join("fixtures").join("seeds")).expect("fixtures dir");

        let normal_file = root.join("services").join("handler.tsx");
        let suffix_file = root.join("services").join("handler.test.tsx");
        let fixture_file = root.join("fixtures").join("seeds").join("seed.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&normal_file).expect("normal file");
        File::create(&suffix_file).expect("suffix file");
        File::create(&fixture_file).expect("fixture file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![normal_file]);
        assert_eq!(config, Some(config_path));
    }

    #[test]
    fn find_files_skips_storybook_stories_and_config() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src").join("components")).expect("components dir");
        fs::create_dir_all(root.join(".storybook")).expect(".storybook dir");

        let component = root.join("src").join("components").join("Button.tsx");
        let story_tsx = root
            .join("src")
            .join("components")
            .join("Button.stories.tsx");
        let story_ts = root.join("src").join("components").join("api.stories.ts");
        let sb_config = root.join(".storybook").join("preview.tsx");
        // "stories" in the basename without the dot-suffix shape stays in.
        let real_module = root.join("src").join("components").join("stories.ts");

        for f in [&component, &story_tsx, &story_ts, &sb_config, &real_module] {
            File::create(f).expect("file");
        }

        let (mut files, _) = find_files(root.to_str().unwrap(), &[]);
        files.sort();

        let mut expected = vec![component, real_module];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_files_allows_root_dir_matching_test_pattern() {
        let tmp = tempdir().expect("temp dir");
        // Create a root directory named "fixtures" which is normally excluded
        let root = tmp.path().join("fixtures");

        fs::create_dir_all(&root).expect("fixtures dir");
        fs::create_dir_all(root.join("src")).expect("src dir");

        let source_file = root.join("src").join("app.ts");
        let config_path = root.join("carrick.json");
        let package_path = root.join("package.json");

        File::create(&source_file).expect("source file");
        File::create(&config_path).expect("config file");
        File::create(&package_path).expect("package file");

        // Pass the "fixtures" directory as the root
        let (files, config) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(files, vec![source_file]);
        assert_eq!(config, Some(config_path));
    }

    const ARTIFACT_IGNORES: &[&str] = &crate::packages::MANIFEST_SKIP_DIRS;

    /// Build a pnpm workspace as installed: every package's `node_modules`
    /// holds a symlink to each workspace package it depends on, and those
    /// point back into the repo's own source. Edges run forward only, so
    /// there is no cycle for a walker's ancestor detection to catch.
    fn installed_workspace(root: &Path, packages: usize, fanout: usize) {
        for i in 1..=packages {
            let pkg = root.join("packages").join(format!("pkg{i}"));
            fs::create_dir_all(pkg.join("src")).expect("package src");
            File::create(pkg.join("src").join("index.ts")).expect("package source");
            File::create(pkg.join("package.json")).expect("package manifest");
        }
        for i in 1..=packages {
            let links = root
                .join("packages")
                .join(format!("pkg{i}"))
                .join("node_modules")
                .join("@repo");
            fs::create_dir_all(&links).expect("link dir");
            for hop in 1..=fanout {
                let j = i + hop;
                if j > packages {
                    continue;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    Path::new("../../../").join(format!("pkg{j}")),
                    links.join(format!("pkg{j}")),
                )
                .expect("workspace link");
            }
        }
    }

    /// The walk must not enter an ignored directory, only decline its files.
    ///
    /// Filtering at the file leaves the same file set and a wildly different
    /// cost: an installed `node_modules` links back into the repo's own
    /// source, and each route through the workspace dependency graph is a
    /// distinct path to the same files. This shape — 12 packages, two
    /// dependencies each, 24 real source files — is visited thousands of
    /// times by a walk that descends into the links (carrick#748).
    #[test]
    #[cfg(unix)]
    fn the_walk_never_enters_an_installed_node_modules() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        installed_workspace(root, 12, 2);

        let stopped = RefCell::default();
        let visited: Vec<PathBuf> =
            source_entries(root, ARTIFACT_IGNORES, &[], &Exclusion::default(), &stopped)
                .map(|entry| entry.path().to_path_buf())
                .collect();

        assert!(
            !visited
                .iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "node_modules")),
            "the walk entered an installed node_modules; {} entries visited",
            visited.len()
        );
        // 12 packages, each a directory plus src/, index.ts and package.json,
        // plus the root and packages/. Every real entry once, nothing twice.
        assert_eq!(
            visited.len(),
            2 + 12 * 4,
            "entries visited: {:?}",
            visited.len()
        );

        // The file set is what it always was.
        let (files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);
        assert_eq!(files.len(), 12, "source files: {files:?}");
    }

    #[test]
    fn find_files_ignore_matches_segments_not_substrings() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("lib")).expect("lib dir");
        fs::create_dir_all(root.join("builder")).expect("builder dir");

        // Basenames and directory names that merely contain an ignore pattern
        // as a substring must still be scanned.
        let substring_in_basename = root.join("src").join("buildkite.ts");
        let substring_in_basename_2 = root.join("lib").join("distances.ts");
        let substring_in_dir = root.join("builder").join("plan.ts");

        for f in [
            &substring_in_basename,
            &substring_in_basename_2,
            &substring_in_dir,
        ] {
            File::create(f).expect("file");
        }

        let (mut files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);
        files.sort();

        let mut expected = vec![
            substring_in_basename,
            substring_in_basename_2,
            substring_in_dir,
        ];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_files_ignores_artifact_dirs_below_root() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("node_modules").join("dep")).expect("node_modules dir");
        fs::create_dir_all(root.join("dist")).expect("dist dir");
        fs::create_dir_all(root.join("build")).expect("build dir");
        fs::create_dir_all(root.join(".next").join("server")).expect(".next dir");
        fs::create_dir_all(root.join(".vite/deps")).unwrap();

        let kept = root.join("src").join("app.ts");
        File::create(&kept).expect("kept file");
        File::create(root.join("node_modules/dep/index.ts")).expect("dep file");
        File::create(root.join("node_modules/dep/package.json")).expect("dep package");
        File::create(root.join("dist/app.js")).expect("dist file");
        File::create(root.join("build/app.js")).expect("build file");
        File::create(root.join(".next/server/page.js")).expect(".next file");
        File::create(root.join(".vite/deps/chunk.js")).unwrap();

        let (files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
    }

    #[test]
    fn find_files_allows_root_dir_named_like_ignore_pattern() {
        let tmp = tempdir().expect("temp dir");
        // The explicitly configured scan root is named after an ignore
        // pattern; only descendants below the root may trigger the ignore.
        let root = tmp.path().join("packages").join("build");

        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("node_modules").join("dep")).expect("node_modules dir");

        let kept = root.join("src").join("index.ts");
        File::create(&kept).expect("kept file");
        File::create(root.join("node_modules/dep/index.ts")).expect("dep file");

        let (files, _) = find_files(root.to_str().unwrap(), ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
    }

    #[test]
    fn find_service_files_scans_service_root_named_like_ignore_pattern() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("packages/build/src")).expect("service dir");
        fs::create_dir_all(root.join("packages/build/node_modules/dep")).expect("dep dir");

        let kept = root.join("packages/build/src/index.ts");
        let pkg = root.join("packages/build/package.json");
        File::create(&kept).expect("kept file");
        File::create(&pkg).expect("package file");
        File::create(root.join("packages/build/node_modules/dep/index.ts")).expect("dep file");

        let service = Config {
            directory: Some("packages/build".to_string()),
            ..Default::default()
        };
        let (files, package) =
            find_service_files(root.to_str().unwrap(), &service, ARTIFACT_IGNORES);

        assert_eq!(files, vec![kept]);
        assert_eq!(package, Some(pkg));
    }

    /// carrick#588 defect 1: an end-to-end harness that stands up a stub on
    /// the same path the package really serves was being scanned like product
    /// source, so the stub's registration became a producer row and a real
    /// consumer elsewhere matched it. Discovery is where that is decided, and
    /// it is decided for both sides of the file at once: nothing under `e2e/`
    /// is analysed at all, so the harness contributes neither endpoints nor
    /// calls, exactly as `tests/` and `__tests__/` already behave.
    ///
    /// Deterministic end to end: the fixture is checked in, and this asserts
    /// on the discovered file list, with no LLM in the loop.
    #[test]
    fn find_service_files_skips_the_e2e_harness_that_stubs_a_real_route() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e-scaffolding");
        let service = crate::config::Config {
            directory: Some("packages/reports-api".to_string()),
            ..Default::default()
        };

        let (files, package_json) =
            find_service_files(&root.to_string_lossy(), &service, &["node_modules"]);

        let service_root = root.join("packages/reports-api");
        assert_eq!(
            files,
            vec![service_root.join("src/routes.ts")],
            "only the real route file is product source; the e2e harness registering a stub on \
             the same path must not be scanned"
        );
        assert_eq!(package_json, Some(service_root.join("package.json")));
    }

    /// The other half of the same guard: should an `e2e/` file ever reach
    /// extraction through a configured `include` root, its endpoints are
    /// tagged rather than passed off as real routes.
    #[test]
    fn endpoint_provenance_tags_e2e_harness_sources() {
        use crate::operation::EndpointProvenance;

        assert_eq!(
            endpoint_provenance(
                Path::new("packages/reports-api/e2e/utils.ts"),
                Path::new("")
            ),
            EndpointProvenance::Mock
        );
        // A file named after the convention is not a harness tree, and a
        // product directory whose name merely starts with it is untouched.
        assert_eq!(
            endpoint_provenance(Path::new("src/api/e2e.ts"), Path::new("")),
            EndpointProvenance::Route
        );
        assert_eq!(
            endpoint_provenance(Path::new("src/e2ee/session.ts"), Path::new("")),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn endpoint_provenance_tags_mock_trees_and_test_paths() {
        use crate::operation::EndpointProvenance;

        let root = Path::new("");
        // The observed real-world shape: an MSW-style handler tree.
        assert_eq!(
            endpoint_provenance(Path::new("src/mocks/handlers.ts"), root),
            EndpointProvenance::Mock
        );
        // Case-insensitive segment match, any depth, singular variant.
        assert_eq!(
            endpoint_provenance(Path::new("src/testing/Mock/server.ts"), root),
            EndpointProvenance::Mock
        );
        // Test-path classification is folded in (defensive: such files are
        // normally excluded from scanning entirely).
        assert_eq!(
            endpoint_provenance(Path::new("src/__mocks__/api.ts"), root),
            EndpointProvenance::Mock
        );
        assert_eq!(
            endpoint_provenance(Path::new("src/api/server.spec.ts"), root),
            EndpointProvenance::Mock
        );
        // A browser end-to-end suite's support tree (carrick#1626).
        assert_eq!(
            endpoint_provenance(Path::new("cypress/support/commands.ts"), root),
            EndpointProvenance::Mock
        );
        // Real product routes stay routes.
        assert_eq!(
            endpoint_provenance(Path::new("src/routes/users.ts"), root),
            EndpointProvenance::Route
        );
        // A FILE named after the convention is not a mock tree.
        assert_eq!(
            endpoint_provenance(Path::new("src/api/mocks.ts"), root),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn endpoint_provenance_outside_scan_root_stays_route() {
        // A test-suffixed file OUTSIDE the scan root must not classify as
        // Mock via the suffix check alone.
        assert_eq!(
            endpoint_provenance(Path::new("elsewhere/foo.spec.ts"), Path::new("service")),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn endpoint_provenance_is_relative_to_scan_root() {
        use crate::operation::EndpointProvenance;

        // A scan rooted INSIDE a mocks/tests prefix (e.g. eval fixtures under
        // tests/fixtures/) must not tag everything mock: only segments below
        // the root count.
        let root = Path::new("tests/fixtures/mocks/repo-a");
        assert_eq!(
            endpoint_provenance(Path::new("tests/fixtures/mocks/repo-a/src/app.ts"), root),
            EndpointProvenance::Route
        );
        assert_eq!(
            endpoint_provenance(
                Path::new("tests/fixtures/mocks/repo-a/src/mocks/handlers.ts"),
                root
            ),
            EndpointProvenance::Mock
        );
        // A path outside the root cannot be classified — conservative Route.
        assert_eq!(
            endpoint_provenance(Path::new("elsewhere/mocks/handlers.ts"), root),
            EndpointProvenance::Route
        );
    }

    #[test]
    fn find_service_files_scopes_to_directory() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // Two services in one repo, plus a file at the root that belongs to
        // neither service's directory.
        fs::create_dir_all(root.join("svc-a/src")).expect("svc-a dir");
        fs::create_dir_all(root.join("svc-b/src")).expect("svc-b dir");

        let a_file = root.join("svc-a/src/index.ts");
        let a_pkg = root.join("svc-a/package.json");
        let b_file = root.join("svc-b/src/index.ts");
        let root_file = root.join("root.ts");

        File::create(&a_file).expect("a file");
        File::create(&a_pkg).expect("a package");
        File::create(&b_file).expect("b file");
        File::create(&root_file).expect("root file");

        let service = Config {
            directory: Some("svc-a".to_string()),
            ..Default::default()
        };
        let (files, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(files, vec![a_file]);
        assert_eq!(package, Some(a_pkg));
    }

    #[test]
    fn find_service_files_merges_include_roots_and_dedups() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("lambdas/check-or-upload")).expect("svc dir");
        fs::create_dir_all(root.join("lambdas/_shared")).expect("shared dir");

        let svc_file = root.join("lambdas/check-or-upload/index.ts");
        let shared_file = root.join("lambdas/_shared/log.ts");
        File::create(&svc_file).expect("svc file");
        File::create(&shared_file).expect("shared file");

        let service = Config {
            directory: Some("lambdas/check-or-upload".to_string()),
            // Include the shared root twice to prove de-duplication.
            include: vec!["lambdas/_shared".to_string(), "lambdas/_shared".to_string()],
            ..Default::default()
        };
        let (mut files, _) = find_service_files(root.to_str().unwrap(), &service, &[]);
        files.sort();

        let mut expected = vec![svc_file, shared_file];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn find_service_files_whole_repo_matches_find_files() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("src")).expect("src dir");
        File::create(root.join("src/app.ts")).expect("source file");
        File::create(root.join("package.json")).expect("package file");

        // directory: None => whole-repo scope, identical to find_files.
        let service = Config::default();
        let (svc_files, svc_pkg) = find_service_files(root.to_str().unwrap(), &service, &[]);
        let (files, _) = find_files(root.to_str().unwrap(), &[]);

        assert_eq!(svc_files, files);
        assert_eq!(svc_pkg, Some(root.join("package.json")));
    }

    // The manifest is the one at the service root — never one found by
    // walking. Before this, the walk assigned every package.json it met, so a
    // service got whichever manifest its host's readdir order visited last:
    // a repo scanned from macOS and from ubuntu-latest indexed different
    // dependency sets for the same commit.

    #[test]
    fn find_service_files_ignores_nested_fixture_manifest() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        fs::create_dir_all(root.join("svc/src")).expect("src dir");
        fs::create_dir_all(root.join("svc/test/fixtures/installed")).expect("fixture dir");
        File::create(root.join("svc/src/app.ts")).expect("source file");
        let root_pkg = root.join("svc/package.json");
        File::create(&root_pkg).expect("root package");
        File::create(root.join("svc/test/fixtures/installed/package.json"))
            .expect("fixture package");

        let service = Config {
            directory: Some("svc".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, Some(root_pkg));
    }

    #[test]
    fn find_service_files_ignores_sibling_service_manifest_under_repo_root() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // A root service (`directory: "."`) whose tree also contains a second
        // service's directory — the site + its worker layout.
        fs::create_dir_all(root.join("src")).expect("src dir");
        fs::create_dir_all(root.join("workers/proxy")).expect("worker dir");
        File::create(root.join("src/app.ts")).expect("source file");
        let root_pkg = root.join("package.json");
        File::create(&root_pkg).expect("root package");
        File::create(root.join("workers/proxy/package.json")).expect("worker package");

        let service = Config {
            directory: Some(".".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, Some(root_pkg));
    }

    #[test]
    fn find_service_files_reports_no_manifest_when_root_has_none() {
        use crate::config::Config;

        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        // Only a nested manifest exists: it must not be promoted to the
        // service's own.
        fs::create_dir_all(root.join("svc/examples/demo")).expect("example dir");
        File::create(root.join("svc/examples/demo/package.json")).expect("example package");

        let service = Config {
            directory: Some("svc".to_string()),
            ..Default::default()
        };
        let (_, package) = find_service_files(root.to_str().unwrap(), &service, &[]);

        assert_eq!(package, None);
    }

    /// A service list as a scan holds it: each `(directory, include root)`
    /// declared, with an empty include for none, and the nested directories
    /// resolved from them the way `Config::load_services` does.
    fn resolved(declared: &[(&str, &str)]) -> Vec<crate::config::Config> {
        let mut services: Vec<crate::config::Config> = declared
            .iter()
            .map(|(directory, include)| crate::config::Config {
                directory: Some((*directory).to_string()),
                include: (!include.is_empty())
                    .then(|| (*include).to_string())
                    .into_iter()
                    .collect(),
                ..Default::default()
            })
            .collect();
        crate::config::Config::resolve_nested(&mut services);
        services
    }

    /// What one service's walk reads, relative to the repo and sorted.
    fn read_by(root: &Path, service: &crate::config::Config) -> Vec<String> {
        let (files, _) = find_service_files(root.to_str().unwrap(), service, ARTIFACT_IGNORES);
        let mut relative: Vec<String> = files
            .iter()
            .map(|file| {
                file.strip_prefix(root)
                    .expect("a walked file is under the repo")
                    .components()
                    .filter_map(|part| match part {
                        std::path::Component::Normal(name) => name.to_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        relative.sort();
        relative
    }

    fn touch(root: &Path, file: &str) {
        let path = root.join(file);
        fs::create_dir_all(path.parent().expect("file parent")).expect("parent dir");
        File::create(path).expect("file");
    }

    /// carrick#553. A service at the root beside a service nested in it: the
    /// nested service's files were read by both walks, so its routes and
    /// functions were indexed under both names.
    #[test]
    fn a_service_walk_leaves_out_a_service_nested_in_it() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, "workers/edge/src/index.ts");
        // A directory beside the nested one whose name only starts the same
        // way is not inside it.
        touch(root, "workers/edge-tools/sync.ts");

        let services = resolved(&[(".", ""), ("workers/edge", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            ["src/orders.ts", "workers/edge-tools/sync.ts"]
        );
        assert_eq!(read_by(root, &services[1]), ["workers/edge/src/index.ts"]);
    }

    /// The same holds at every depth, and however the directories are
    /// spelled: each file is read by the deepest service that holds it.
    #[test]
    fn a_service_nested_two_levels_down_is_left_out_of_both_services_above_it() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "platform/gateway.ts");
        touch(root, "platform/billing/charge.ts");
        touch(root, "platform/billing/webhooks/stripe.ts");

        let services = resolved(&[
            ("./platform/", ""),
            ("platform/billing", ""),
            ("platform/billing/webhooks/", ""),
        ]);
        assert_eq!(read_by(root, &services[0]), ["platform/gateway.ts"]);
        assert_eq!(read_by(root, &services[1]), ["platform/billing/charge.ts"]);
        assert_eq!(
            read_by(root, &services[2]),
            ["platform/billing/webhooks/stripe.ts"]
        );
    }

    /// An `include` root is source a service declares on purpose, so it is
    /// read whole: where it sits inside a nested service, where it is the
    /// nested service's whole directory, and where another service sits inside
    /// it. The nested service still reads its own tree.
    #[test]
    fn an_include_root_is_read_whole_wherever_it_sits() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, "workers/edge/src/index.ts");
        touch(root, "workers/edge/shared/headers.ts");
        touch(root, "libs/format.ts");
        touch(root, "libs/ui/button.ts");

        let services = resolved(&[
            (".", "workers/edge/shared"),
            ("workers/edge", "libs"),
            ("libs/ui", ""),
        ]);
        // The root leaves the nested services out and takes back the one
        // directory it includes.
        assert_eq!(
            read_by(root, &services[0]),
            [
                "libs/format.ts",
                "src/orders.ts",
                "workers/edge/shared/headers.ts"
            ]
        );
        // A service inside an include root is read with the root.
        assert_eq!(
            read_by(root, &services[1]),
            [
                "libs/format.ts",
                "libs/ui/button.ts",
                "workers/edge/shared/headers.ts",
                "workers/edge/src/index.ts"
            ]
        );

        // Including a nested service's whole directory reads all of it.
        let whole = resolved(&[(".", "workers/edge"), ("workers/edge", ""), ("libs/ui", "")]);
        assert_eq!(
            read_by(root, &whole[0]),
            [
                "libs/format.ts",
                "src/orders.ts",
                "workers/edge/shared/headers.ts",
                "workers/edge/src/index.ts"
            ]
        );
    }

    /// Two services that declare one directory both read it. Neither is
    /// inside the other, and nothing says which of them owns the files.
    #[test]
    fn two_services_with_the_same_directory_both_read_it() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "tools/reports/build.ts");
        touch(root, "tools/reports/email/send.ts");

        let services = resolved(&[
            ("tools/reports", ""),
            ("./tools/reports/", ""),
            ("tools/reports/email", ""),
        ]);
        assert_eq!(read_by(root, &services[0]), ["tools/reports/build.ts"]);
        assert_eq!(read_by(root, &services[1]), ["tools/reports/build.ts"]);
        assert_eq!(read_by(root, &services[2]), ["tools/reports/email/send.ts"]);
    }

    /// A service list built without the resolution step carries no nested
    /// directories and walks as it always did.
    #[test]
    fn a_service_with_no_resolved_nesting_walks_its_whole_directory() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, "workers/edge/src/index.ts");
        let alone = crate::config::Config {
            directory: Some(".".to_string()),
            ..Default::default()
        };
        assert_eq!(
            read_by(root, &alone),
            ["src/orders.ts", "workers/edge/src/index.ts"]
        );
    }

    /// The `.git` a linked worktree or a submodule holds: a file naming where
    /// its git directory is.
    fn git_file(root: &Path, dir: &str) {
        fs::create_dir_all(root.join(dir)).expect("checkout dir");
        fs::write(
            root.join(dir).join(".git"),
            "gitdir: /elsewhere/.git/worktrees/copy\n",
        )
        .expect(".git file");
    }

    /// The `.git` a clone holds: a directory.
    fn git_dir(root: &Path, dir: &str) {
        fs::create_dir_all(root.join(dir).join(".git")).expect(".git dir");
    }

    /// The checkouts a service's walk says it left out, relative to the repo.
    fn left_out_by(root: &Path, service: &crate::config::Config) -> Vec<String> {
        walk_service(root.to_str().unwrap(), service, ARTIFACT_IGNORES)
            .checkouts_left_out
            .iter()
            .map(|checkout| {
                checkout
                    .strip_prefix(root)
                    .expect("a checkout left out is under the repo")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    /// carrick#1902. A service at the repository root with two checkouts
    /// inside it: a linked worktree that is a whole copy of the repository,
    /// and a clone of something else. Neither is the repository's tree, and
    /// the walk read both, so every file of the copy was indexed a second time
    /// under the root service.
    #[test]
    fn a_service_walk_does_not_enter_a_checkout_of_its_own() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        touch(root, "src/orders.ts");
        // The copy: a worktree kept in a dot folder the repository ignores.
        git_file(root, ".agent/worktrees/copy");
        touch(root, ".agent/worktrees/copy/src/orders.ts");
        // A clone in a plain folder, so the rule is not about the dot.
        git_dir(root, "vendor/clone");
        touch(root, "vendor/clone/index.ts");
        // A plain folder beside them is read.
        touch(root, "vendor/patched/shim.ts");

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            ["src/orders.ts", "vendor/patched/shim.ts"]
        );
        assert_eq!(
            left_out_by(root, &services[0]),
            [".agent/worktrees/copy", "vendor/clone"]
        );
    }

    /// The repository's own `.git` is its records, never its source. A hook
    /// written in JavaScript sits there on any checkout that has one.
    #[test]
    fn a_walk_does_not_read_the_repositorys_own_git_directory() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, ".git/hooks/pre-commit.js");
        touch(root, "src/orders.ts");

        let services = resolved(&[(".", "")]);
        assert_eq!(read_by(root, &services[0]), ["src/orders.ts"]);
        assert!(left_out_by(root, &services[0]).is_empty());
    }

    /// Where git cannot say what the repository tracks (here, a `.git` that
    /// holds no repository), a dot folder is read as any folder is. A
    /// framework can define one as an application's server-only modules, and
    /// a skip by name would take the request code out of the index with it.
    #[test]
    fn a_dot_folder_that_is_no_checkout_is_still_read() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        touch(root, "app/routes/orders.tsx");
        touch(root, "app/.server/session.ts");
        touch(root, ".tooling/release.ts");

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            [
                ".tooling/release.ts",
                "app/.server/session.ts",
                "app/routes/orders.tsx"
            ]
        );
        assert!(left_out_by(root, &services[0]).is_empty());
    }

    /// Run git in `root` clear of the git environment and configuration the
    /// test process inherited (a pre-commit hook exports `GIT_DIR`).
    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The dot folders a service's walk says it left out, relative to the repo.
    fn dot_folders_left_out_by(root: &Path, service: &crate::config::Config) -> Vec<String> {
        walk_service(root.to_str().unwrap(), service, ARTIFACT_IGNORES)
            .dot_folders_left_out
            .iter()
            .map(|dir| {
                dir.strip_prefix(root)
                    .expect("a folder left out is under the repo")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    /// carrick#1607. A dot folder git tracks nothing in is a tool's: a
    /// virtual environment, a framework's build output. It is left out of the
    /// walk and named. A dot folder git tracks a file in is the repository's
    /// own source and is read, whatever its name.
    #[test]
    fn a_dot_folder_git_tracks_nothing_in_is_left_out() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "app/routes/orders.tsx");
        touch(root, "app/.server/session.ts");
        touch(root, ".venv-docx/lib/site-packages/widget/index.js");
        touch(root, "app/.react-router/types/+types/orders.ts");
        touch(root, ".config/eslint.config.ts");
        touch(root, ".config/generated/schema.ts");
        git(root, &["init", "-q"]);
        git(
            root,
            &[
                "add",
                "app/routes",
                "app/.server",
                ".config/eslint.config.ts",
            ],
        );

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            [
                ".config/eslint.config.ts",
                ".config/generated/schema.ts",
                "app/.server/session.ts",
                "app/routes/orders.tsx"
            ]
        );
        assert_eq!(
            dot_folders_left_out_by(root, &services[0]),
            [".venv-docx", "app/.react-router"]
        );
        assert!(left_out_by(root, &services[0]).is_empty());
    }

    /// A walk's root is never a boundary: a dot folder named as a service's
    /// directory, or as an `include` root, is read whole.
    #[test]
    fn an_untracked_dot_folder_the_config_names_is_read() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, ".tooling/release.ts");
        git(root, &["init", "-q"]);
        git(root, &["add", "src"]);

        let tooling = resolved(&[(".tooling", "")]);
        assert_eq!(read_by(root, &tooling[0]), [".tooling/release.ts"]);

        let included = crate::config::Config {
            directory: Some("src".to_string()),
            include: vec![".tooling".to_string()],
            ..Default::default()
        };
        assert_eq!(
            read_by(root, &included),
            [".tooling/release.ts", "src/orders.ts"]
        );
        assert!(dot_folders_left_out_by(root, &included).is_empty());
    }

    /// The paths git ignores that a service's walk names as left out,
    /// relative to the repo.
    fn ignored_left_out_by(root: &Path, service: &crate::config::Config) -> Vec<String> {
        walk_service(root.to_str().unwrap(), service, ARTIFACT_IGNORES)
            .ignored_left_out
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .expect("a path left out is under the repo")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    /// carrick#1902. A folder and a file git ignores are left out and named.
    /// A file the repository tracks under an ignore pattern is the
    /// repository's, and is read.
    #[test]
    fn what_git_ignores_is_left_out_unless_it_is_tracked() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, "src/env.local.ts");
        touch(root, "backup/src/orders.ts");
        touch(root, "generated/kept.ts");
        touch(root, "generated/stray.ts");
        fs::write(root.join(".gitignore"), "backup/\n*.local.ts\ngenerated/\n")
            .expect(".gitignore");
        git(root, &["init", "-q"]);
        git(root, &["add", "-A"]);
        git(root, &["add", "-f", "generated/kept.ts"]);

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            ["generated/kept.ts", "src/orders.ts"]
        );
        assert_eq!(
            ignored_left_out_by(root, &services[0]),
            ["backup", "generated/stray.ts", "src/env.local.ts"]
        );
    }

    /// carrick#1902, ruling (b): an ignored file the service's own code
    /// imports is read, and so is what it imports in turn, by `import`,
    /// `export * from` or a literal `require`. What nothing imports stays
    /// out, and where part of a folder was read the rest is named file by
    /// file.
    #[test]
    fn an_ignored_file_the_service_imports_is_read() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).expect("src");
        fs::create_dir_all(root.join("generated")).expect("generated");
        fs::write(
            root.join("src/orders.ts"),
            "import { Order } from '../generated/client';\n\
             const rates = require('../generated/rates.js');\n",
        )
        .expect("orders");
        fs::write(
            root.join("generated/client.ts"),
            "export * from './models';\n",
        )
        .expect("client");
        touch(root, "generated/models.ts");
        touch(root, "generated/rates.ts");
        touch(root, "generated/browser.ts");
        fs::write(root.join(".gitignore"), "generated/\n").expect(".gitignore");
        git(root, &["init", "-q"]);
        git(root, &["add", "-A"]);

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            [
                "generated/client.ts",
                "generated/models.ts",
                "generated/rates.ts",
                "src/orders.ts"
            ]
        );
        assert_eq!(
            ignored_left_out_by(root, &services[0]),
            ["generated/browser.ts"]
        );
    }

    /// A root the config names is read whole where git ignores it: as a
    /// service's directory, and as an `include` root.
    #[test]
    fn an_ignored_folder_the_config_names_is_read_whole() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "src/orders.ts");
        touch(root, "codegen/api.ts");
        touch(root, "codegen/models/order.ts");
        fs::write(root.join(".gitignore"), "codegen/\n").expect(".gitignore");
        git(root, &["init", "-q"]);
        git(root, &["add", "-A"]);

        let codegen = resolved(&[("codegen", "")]);
        assert_eq!(
            read_by(root, &codegen[0]),
            ["codegen/api.ts", "codegen/models/order.ts"]
        );

        let included = crate::config::Config {
            directory: Some(".".to_string()),
            include: vec!["codegen".to_string()],
            ..Default::default()
        };
        assert_eq!(
            read_by(root, &included),
            ["codegen/api.ts", "codegen/models/order.ts", "src/orders.ts"]
        );
        assert!(ignored_left_out_by(root, &included).is_empty());
    }

    /// Where git cannot say what it ignores (here, a `.git` that holds no
    /// repository), nothing counts as ignored and the tree is read as it was.
    #[test]
    fn without_an_answer_from_git_nothing_is_ignored() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        fs::write(root.join(".gitignore"), "backup/\n").expect(".gitignore");
        touch(root, "src/orders.ts");
        touch(root, "backup/src/orders.ts");

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            ["backup/src/orders.ts", "src/orders.ts"]
        );
        assert!(ignored_left_out_by(root, &services[0]).is_empty());
    }

    /// carrick#1990: `exclude` reads as a `.gitignore` does, relative to the
    /// service's directory, and the walk says how many source files it left
    /// out.
    #[test]
    fn exclude_patterns_leave_paths_out_of_a_service_walk() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "web/src/orders.ts");
        touch(root, "web/src/orders.scratch.ts");
        touch(root, "web/scripts/backfill.ts");
        touch(root, "web/scripts/nested/report.ts");
        touch(root, "web/src/scripts/kept.ts");
        touch(root, "web/legacy/old.ts");
        touch(root, "web/legacy/keep.ts");
        touch(root, "shared/util.ts");
        let service = crate::config::Config {
            directory: Some("web".to_string()),
            include: vec!["shared".to_string()],
            exclude: vec![
                "/scripts/".to_string(),
                "*.scratch.ts".to_string(),
                "legacy/*".to_string(),
                "!legacy/keep.ts".to_string(),
                "util.ts".to_string(),
            ],
            ..Default::default()
        };
        assert_eq!(
            read_by(root, &service),
            [
                "shared/util.ts",
                "web/legacy/keep.ts",
                "web/src/orders.ts",
                "web/src/scripts/kept.ts"
            ],
            "anchored, glob, negated; a path outside the directory is never matched"
        );
        let walk = walk_service(root.to_str().unwrap(), &service, ARTIFACT_IGNORES);
        assert_eq!(
            walk.excluded,
            ExcludedFiles {
                patterns: 5,
                files: 4
            }
        );
    }

    /// No pattern leaves nothing out and counts nothing, so a repository
    /// without `exclude` walks as it always did.
    #[test]
    fn a_service_with_no_exclude_walks_as_before() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        touch(root, "scripts/backfill.ts");
        let services = resolved(&[(".", "")]);
        assert_eq!(read_by(root, &services[0]), ["scripts/backfill.ts"]);
        assert_eq!(
            walk_service(root.to_str().unwrap(), &services[0], ARTIFACT_IGNORES).excluded,
            ExcludedFiles::default()
        );
    }

    #[test]
    fn a_line_that_is_no_pattern_is_refused() {
        assert!(Exclusion::check(&["scripts/".to_string()]).is_ok());
        // A trailing escape escapes nothing.
        let refused = Exclusion::check(&["src/\\".to_string()]).unwrap_err();
        assert!(refused.contains("`src/\\`"), "{refused}");
    }

    /// Naming a checkout is how it is read: as a service's own `directory`,
    /// as an `include` root, or as the path the scan was started at. Each is
    /// the root of a walk, and a walk's root is never a boundary.
    #[test]
    fn a_checkout_the_config_names_is_read_whole() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        touch(root, "src/orders.ts");
        git_dir(root, "vendor/clone");
        touch(root, "vendor/clone/index.ts");
        touch(root, "vendor/clone/lib/format.ts");

        // An `include` root that is the checkout, and one inside it.
        let included = resolved(&[(".", "vendor/clone")]);
        assert_eq!(
            read_by(root, &included[0]),
            [
                "src/orders.ts",
                "vendor/clone/index.ts",
                "vendor/clone/lib/format.ts"
            ]
        );
        assert!(left_out_by(root, &included[0]).is_empty());
        let partly = resolved(&[(".", "vendor/clone/lib")]);
        assert_eq!(
            read_by(root, &partly[0]),
            ["src/orders.ts", "vendor/clone/lib/format.ts"]
        );
        assert!(left_out_by(root, &partly[0]).is_empty());

        // A service declared at the checkout reads it, and the service above
        // leaves it out as a nested service, not as a checkout.
        let declared = resolved(&[(".", ""), ("vendor/clone", "")]);
        assert_eq!(read_by(root, &declared[0]), ["src/orders.ts"]);
        assert!(left_out_by(root, &declared[0]).is_empty());
        assert_eq!(
            read_by(root, &declared[1]),
            ["vendor/clone/index.ts", "vendor/clone/lib/format.ts"]
        );

        // A scan started at the checkout reads it.
        let (files, _) = find_files(
            root.join("vendor/clone").to_str().unwrap(),
            ARTIFACT_IGNORES,
        );
        assert_eq!(files.len(), 2);
    }

    /// A submodule is the one checkout a repository states: `.gitmodules`
    /// lists its path and the commit pins what it holds. It is read as before,
    /// at any depth, and a checkout beside it that nothing declares is not.
    #[test]
    fn a_submodule_the_repository_declares_is_read() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        fs::write(
            root.join(".gitmodules"),
            "[submodule \"shared\"]\n\tpath = libs/shared\n\turl = https://example.test/shared.git\n\
             [submodule \"quoted\"]\n\tPath = \"libs/quoted/\"\n\turl = ../quoted.git\n",
        )
        .expect(".gitmodules");
        touch(root, "src/orders.ts");
        git_file(root, "libs/shared");
        touch(root, "libs/shared/client.ts");
        git_file(root, "libs/quoted");
        touch(root, "libs/quoted/index.ts");
        git_file(root, "libs/stray");
        touch(root, "libs/stray/index.ts");
        // A submodule of the submodule is declared by the submodule.
        fs::write(
            root.join("libs/shared/.gitmodules"),
            "[submodule \"inner\"]\n\tpath = vendor/inner\n",
        )
        .expect("inner .gitmodules");
        git_file(root, "libs/shared/vendor/inner");
        touch(root, "libs/shared/vendor/inner/codec.ts");
        // The same path one level up is not what the outer repository states.
        git_file(root, "vendor/inner");
        touch(root, "vendor/inner/codec.ts");

        let services = resolved(&[(".", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            [
                "libs/quoted/index.ts",
                "libs/shared/client.ts",
                "libs/shared/vendor/inner/codec.ts",
                "src/orders.ts"
            ]
        );
        assert_eq!(
            left_out_by(root, &services[0]),
            ["libs/stray", "vendor/inner"]
        );
    }

    /// A service inside a larger repository: the repository that declares a
    /// submodule is above the walk's own root, and is still the one asked.
    #[test]
    fn a_submodule_is_declared_by_the_checkout_above_the_walk_root() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        git_dir(root, ".");
        fs::write(
            root.join(".gitmodules"),
            "[submodule \"proto\"]\n\tpath = apps/api/proto\n",
        )
        .expect(".gitmodules");
        touch(root, "apps/api/src/server.ts");
        git_file(root, "apps/api/proto");
        touch(root, "apps/api/proto/messages.ts");

        let services = resolved(&[("apps/api", "")]);
        assert_eq!(
            read_by(root, &services[0]),
            ["apps/api/proto/messages.ts", "apps/api/src/server.ts"]
        );
    }
}
