//! Which services a `--changed-only` scan analyses, and which it carries
//! (carrick#2227). Contract: `docs/reference/runner.md`.
//!
//! A carried service is not analysed: its stored generation stands in for it
//! in the run, exactly as a held-back service's does. A service is carried
//! only when nothing it reads has changed since the commit its own stored
//! blob names. What it reads (its [`ServiceInputs`]) is computed on the tree
//! being scanned: its roots, the transitive closure of its imports as the
//! scanner's own resolver reads them, the workspace packages that closure
//! enters, its tsconfig's `extends` chain and the directories that config
//! includes, its manifest and its declared GraphQL schemas. A service's inputs
//! at its stored commit can differ from these only when one of its own files
//! changed, which already counts as a change.
//!
//! When carrick.json names no `tsconfig` for a service, its tsconfig is the
//! nearest `tsconfig.json` at or above its directory.
//!
//! [`decide`] is the rule, over a stored blob, this run's config, the paths
//! changed since that blob's commit and the service's inputs. [`plan`] runs it
//! for every service of a scan and logs each answer.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use tracing::{debug, info};

use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use crate::dependency_move::{DependencyMove, DependencyReach, dependency_move, loaded_packages};
use crate::git_state::ChangedPath;

/// What `--changed-only` together with `--no-cache` is told: a usage error.
pub const NO_CACHE_CONFLICT: &str =
    "--changed-only cannot be combined with --no-cache, which re-analyses every service.";

/// The `close-scan` reason of a `--changed-only` run that writes nothing.
pub const NO_CHANGE_REASON: &str = "no_change";

fn requested_slot() -> &'static OnceLock<bool> {
    static REQUESTED: OnceLock<bool> = OnceLock::new();
    &REQUESTED
}

/// Record `--changed-only` for the run. A second call is ignored.
pub fn request() {
    let _ = requested_slot().set(true);
}

/// Whether this run was told `--changed-only`.
pub fn requested() -> bool {
    requested_slot().get().copied().unwrap_or(false)
}

/// Why a service is analysed rather than carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NoStored,
    OtherScanner,
    StoredDirty,
    StoredIncomplete,
    ConfigChanged,
    CommitMissing,
    UnresolvedImport,
    ExtendsUnfollowed,
    RepoInputChanged,
    InputsChanged,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::NoStored => "no_stored",
            Reason::OtherScanner => "other_scanner",
            Reason::StoredDirty => "stored_dirty",
            Reason::StoredIncomplete => "stored_incomplete",
            Reason::ConfigChanged => "config_changed",
            Reason::CommitMissing => "commit_missing",
            Reason::UnresolvedImport => "unresolved_import",
            Reason::ExtendsUnfollowed => "extends_unfollowed",
            Reason::RepoInputChanged => "repo_input_changed",
            Reason::InputsChanged => "inputs_changed",
        }
    }
}

/// What a `--changed-only` scan does with one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Its stored generation, written at `commit`, stands in for it.
    Carried { commit: String },
    /// It is analysed as a full scan would, for `reason`.
    Analysed {
        reason: Reason,
        detail: String,
        /// What the plan judged to move nothing this service reads
        /// (carrick#3126): a pull request's type-reach decision keeps reach
        /// through exactly these changes.
        ruled_out: RuledOut,
    },
}

/// The repo-input changes the plan ruled out for one service, so type reach
/// does not judge them a second time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuledOut {
    /// The service loads nothing the dependency-only change moved.
    pub dependencies: bool,
    /// Top-relative paths of changed `.gitignore` files that move no
    /// untracked path the service reads.
    pub gitignores: BTreeSet<String>,
}

impl Scope {
    pub fn is_carried(&self) -> bool {
        matches!(self, Scope::Carried { .. })
    }

    fn analysed(reason: Reason, detail: impl Into<String>) -> Self {
        Scope::Analysed {
            reason,
            detail: detail.into(),
            ruled_out: RuledOut::default(),
        }
    }

    fn analysed_ruling(reason: Reason, detail: impl Into<String>, ruled_out: RuledOut) -> Self {
        Scope::Analysed {
            reason,
            detail: detail.into(),
            ruled_out,
        }
    }
}

/// The scanner release and cache format this run writes.
#[derive(Debug, Clone, Copy)]
pub struct ThisRun<'a> {
    pub scanner_version: &'a str,
    pub cache_version: u32,
    /// Whether this run asks the model: only such a run can ask for an
    /// extraction config a stored blob went without.
    pub asks_model: bool,
}

/// One root a service's walk reads: a directory relative to the repository's
/// top level (`""` for all of it), minus the directories of services nested
/// inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Root {
    pub dir: String,
    pub left_out: Vec<String>,
}

/// What one service reads, every path relative to the repository's top level
/// with `/` separators, so it compares with what `git diff` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceInputs {
    pub roots: Vec<Root>,
    /// Files outside the roots that the service reads: its import closure,
    /// its config chain, its manifest, its declared schemas.
    pub files: BTreeSet<String>,
    /// Directories read whole: a workspace package the closure enters, a
    /// directory its tsconfig includes.
    pub dirs: BTreeSet<String>,
    /// Directories where a path appearing or disappearing can change what an
    /// unchanged import resolves to: each holds a closure file, or is where a
    /// relative specifier was looked up.
    pub lookup_dirs: BTreeSet<String>,
    /// Every import nothing accounts for, with what it could resolve to.
    pub unresolved: Vec<UnresolvedImport>,
    /// `extends` values a config it reads could not follow.
    pub unfollowed_extends: Vec<String>,
}

/// An import the resolver could not follow, and the files it could name.
///
/// It keeps a service from being carried only when a changed path is one of
/// them (carrick#2826): an import that cannot reach a changed file cannot
/// have changed what the service reads. A build-time copy that is not on disk
/// (`./shared/log`) or a package that is not installed is the ordinary case on
/// a checkout nobody built, and it used to analyse every service in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedImport {
    /// `<file> imports <specifier>`.
    pub at: String,
    pub could_be: CouldBe,
}

/// The repo paths an unresolved import could name, top-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CouldBe {
    /// Nothing narrows it: any change could be it.
    Anything,
    /// A relative specifier, or an internal path with no file: the path
    /// itself, with any extension, or a directory index under it.
    Path(String),
    /// A config mapping whose target is missing: anything under its root.
    Under(String),
    /// A bare specifier no manifest declares: a file whose path, extension
    /// dropped, ends with it (a `baseUrl` import the resolver missed). A
    /// package changes through its manifest or lockfile, which is a repo
    /// input of every service already, or a dependency-only move the service
    /// loads ([`crate::dependency_move`]).
    Named(String),
}

impl CouldBe {
    fn names(&self, path: &str) -> bool {
        match self {
            CouldBe::Anything => true,
            CouldBe::Path(base) => {
                path == base
                    || path
                        .strip_prefix(base.as_str())
                        .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('/'))
            }
            CouldBe::Under(root) => under(path, root.trim_end_matches('/')),
            CouldBe::Named(specifier) => {
                let stem = path.rsplit_once('.').map_or(path, |(stem, _)| stem);
                let stem = stem.strip_suffix("/index").unwrap_or(stem);
                stem == specifier || stem.ends_with(&format!("/{specifier}"))
            }
        }
    }
}

/// Says whether an edited test source still loads what it loaded, and
/// declares nothing program-wide ([`test_edit_keeps_imports`]).
type KeepsImports<'a> = &'a mut dyn FnMut(&ChangedPath) -> bool;

impl ServiceInputs {
    /// Whether `change` touches an input of this service that is not under
    /// one of its roots.
    fn reads_outside_roots(&self, change: &ChangedPath, keeps_imports: KeepsImports) -> bool {
        let in_root = self.roots.iter().any(|root| {
            under(&change.path, &root.dir)
                && !root.left_out.iter().any(|dir| under(&change.path, dir))
        });
        !in_root && self.reads(change, keeps_imports)
    }

    /// Whether `change` touches an input of this service (carrick#3114).
    ///
    /// Under a root or a directory read whole, only a file some pass reads
    /// counts ([`read_by_input_rule`]), and a test source counts unless it was
    /// edited and still loads what it loaded. An exact closure file, and an
    /// add or delete beside one, always count.
    fn reads(&self, change: &ChangedPath, keeps_imports: KeepsImports) -> bool {
        let path = change.path.as_str();
        if self.files.contains(path)
            || (change.adds_or_deletes() && self.lookup_dirs.contains(parent_of(path)))
        {
            return true;
        }
        let roots = self
            .roots
            .iter()
            .filter(|root| {
                under(path, &root.dir) && !root.left_out.iter().any(|dir| under(path, dir))
            })
            .map(|root| root.dir.as_str());
        let dirs = self
            .dirs
            .iter()
            .filter(|dir| under(path, dir))
            .map(String::as_str);
        let bases: Vec<&str> = roots.chain(dirs).collect();
        if bases.is_empty() || !read_by_input_rule(path) {
            return false;
        }
        let is_test =
            |base: &&str| crate::file_finder::is_test_source(Path::new(path), Path::new(base));
        if bases.iter().all(is_test) {
            return !(change.status == 'M' && keeps_imports(change));
        }
        true
    }
}

/// Whether a scan reads `path` under the published input rule
/// (`carrickInputs`, [`INPUT_RULE`] 1): a repo-input or deploy-config name, a
/// source, GraphQL or config extension, or no extension. The one statement of
/// the rule in Rust; `the_published_rule_reads_what_the_scanner_reads` holds it
/// to the published field. Tests count as read here, as the rule says.
pub fn read_by_input_rule(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let extension = name
        .rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
        .map(|(_, ext)| ext);
    let named = REPO_INPUT_NAMES
        .iter()
        .chain(&crate::service_derivation::DEPLOY_CONFIG)
        .any(|known| known.to_lowercase() == name);
    let by_extension = extension.is_some_and(|ext| {
        crate::file_finder::is_source_extension(ext)
            || REPO_INPUT_EXTENSIONS.contains(&ext)
            || CONFIG_EXTENSIONS.contains(&ext)
    });
    named || by_extension || extension.is_none()
}

/// Whether an edit to the test source at `path` left its module edges alone:
/// both copies parse as modules, neither augments types program-wide, and both
/// load the same specifiers (carrick#3114). The receiver trace reads which
/// files only tests import, and the type program loads augmentations and
/// scripts from tests, so only an edit that moves neither is skippable. A copy
/// that cannot be read says no.
fn test_edit_keeps_imports(
    reads: &ClosureReads,
    path: &str,
    before: Option<String>,
    after: Option<String>,
) -> bool {
    let (Some(before), Some(after)) = (before, after) else {
        return false;
    };
    let file = Path::new(path);
    let mut reader = reads.reader.borrow_mut();
    let before = reader.module_shape_of_text(file, "before", before);
    let after = reader.module_shape_of_text(file, "after", after);
    match (before, after) {
        (Some(before), Some(after)) => {
            before.is_module
                && after.is_module
                && !before.augments_types
                && !after.augments_types
                && before.reach == after.reach
        }
        _ => false,
    }
}

/// Whether `path` is a `.gitignore`, wherever it sits.
pub(crate) fn is_gitignore(path: &str) -> bool {
    path.rsplit('/').next() == Some(".gitignore")
}

impl ServiceInputs {
    /// Whether a `.gitignore` is in a workspace package the service reads
    /// whole. An install that copies the package by its pack list reads the
    /// package's `.gitignore` when it has no `files` field and no `.npmignore`.
    fn reads_the_dir_of(&self, gitignore: &str) -> bool {
        let dir = parent_of(gitignore);
        self.dirs.iter().any(|read| under(dir, read))
    }
}

/// Whether an edit to `gitignore` can move what this service reads
/// (carrick#3113).
///
/// A `.gitignore` decides only which untracked paths a walk leaves out: a
/// tracked file is read whatever it says, and an untracked file it does not
/// ignore is already listed as added. So an edit counts against a service when
/// an untracked path under the `.gitignore`'s folder is one the service reads
/// and its walk could enter, or when the service reads the folder whole
/// ([`ServiceInputs::reads_the_dir_of`]).
///
/// `untracked` is every untracked path, ignored or not
/// ([`crate::git_state::untracked_paths`]); `None` when git could not list
/// them, and then the edit counts. `walk_enters(base, path, is_dir)` says
/// whether a walk reading `base` could enter `path` below it.
fn gitignore_moves(
    gitignore: &str,
    inputs: &ServiceInputs,
    untracked: Option<&[String]>,
    mut walk_enters: impl FnMut(&str, &str, bool) -> bool,
) -> bool {
    let Some(untracked) = untracked else {
        return true;
    };
    if inputs.reads_the_dir_of(gitignore) {
        return true;
    }
    let governed = parent_of(gitignore);
    untracked.iter().any(|entry| {
        let is_dir = entry.ends_with('/');
        let path = entry.trim_end_matches('/');
        if !under(path, governed) && !(is_dir && under(governed, path)) {
            return false;
        }
        // An untracked folder that holds something the service reads whole
        // (a root, a directory, a closure file) is not narrowed.
        if is_dir
            && (inputs.roots.iter().any(|root| under(&root.dir, path))
                || inputs.dirs.iter().any(|dir| under(dir, path))
                || inputs.files.iter().any(|file| under(file, path)))
        {
            return true;
        }
        if inputs.files.contains(path) {
            return true;
        }
        let bases = inputs
            .roots
            .iter()
            .filter(|root| {
                under(path, &root.dir) && !root.left_out.iter().any(|dir| under(path, dir))
            })
            .map(|root| root.dir.as_str())
            .chain(
                inputs
                    .dirs
                    .iter()
                    .filter(|dir| under(path, dir))
                    .map(String::as_str),
            );
        let mut bases: Vec<&str> = bases.collect();
        bases.sort_unstable();
        bases.dedup();
        bases
            .into_iter()
            .any(|base| walk_enters(base, path, is_dir))
    })
}

/// Whether a walk rooted at `base` (repository-relative) could enter `path`
/// below it: none of the folders between is one the walk skips by name
/// (`skip`), and none is a stop the walk makes whatever `.gitignore` says
/// ([`crate::file_finder::boundary_other_than_ignored`]). The walk's own root
/// is never a stop. `stops` remembers each folder's answer.
fn walk_could_enter(
    top: &Path,
    skip: &[&str],
    base: &str,
    path: &str,
    is_dir: bool,
    stops: &mut HashMap<PathBuf, bool>,
) -> bool {
    let below = path
        .strip_prefix(base)
        .unwrap_or(path)
        .trim_start_matches('/');
    if below.is_empty() {
        return true;
    }
    let segments: Vec<&str> = below.split('/').collect();
    let folders = if is_dir {
        segments.len()
    } else {
        segments.len() - 1
    };
    let mut at = top.join(base);
    for segment in &segments[..folders] {
        if skip.contains(segment) {
            return false;
        }
        at.push(segment);
        let stopped = *stops.entry(at.clone()).or_insert_with(|| {
            crate::file_finder::boundary_other_than_ignored(&at, true).is_some()
        });
        if stopped {
            return false;
        }
    }
    true
}

/// Whether `path` is `dir` or under it. `""` is the whole repository.
pub(crate) fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// The file names that are an input of every service wherever they sit: the
/// lockfiles, every manifest (internal package names are read across the
/// whole repository), the workspace files, what the walk ignores, and the
/// scan's own config.
///
/// The package-manager and git config files are named too, because they
/// decide what the scan reads: `.npmrc`, `.yarnrc`, `.yarnrc.yml` and
/// `bunfig.toml` shape the installs and so `node_modules`, `.gitmodules`
/// decides which directories are separate checkouts, and `.gitattributes` is
/// applied by git when it writes the checkout.
///
/// With [`REPO_INPUT_EXTENSIONS`], [`CONFIG_EXTENSIONS`],
/// [`crate::file_finder::SOURCE_EXTENSIONS`] and a service's
/// [`ServiceInputs`] (its tsconfig chain, its declared GraphQL schemas), this
/// is the one definition of the files a scan reads (carrick#2227). It is
/// published as `carrickInputs` in `npm/carrick/package.json` (carrick#2234,
/// held equal by `npm_manifest_inputs_equal_the_constants`) so a filter that
/// decides whether a push needs a scan reads it from there rather than
/// restating it.
pub const REPO_INPUT_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "deno.lock",
    "package.json",
    "pnpm-workspace.yaml",
    "deno.json",
    "deno.jsonc",
    ".gitignore",
    "carrick.json",
    // Read by the npm and pnpm installs and the library store
    // (`library_store.rs`, `scripts/install-scanned-deps.sh`).
    ".npmrc",
    // Read by the classic Yarn install.
    ".yarnrc",
    // Read by the Yarn installs (`library_store.rs`, `untrusted_yarn.rs`).
    ".yarnrc.yml",
    // Read by the bun install.
    "bunfig.toml",
    // `file_finder::declared_submodules` decides `is_separate_checkout`.
    ".gitmodules",
    // Git applies it (eol, filters) when it writes the checkout.
    ".gitattributes",
];

/// The extensions whose files are an input of every service: the GraphQL
/// schema catalogue covers the whole repository.
pub const REPO_INPUT_EXTENSIONS: &[&str] = &["graphql", "gql", "graphqls"];

/// The extensions of files a service reads by name: its tsconfig chain, a
/// Deno import map, a JSON module it imports. These are per service
/// (`service_inputs`), so the published rule reads every one. It is not a
/// repo input: [`is_repo_input`] does not read it, and a JSON change
/// re-analyses only the services whose inputs hold that file.
pub const CONFIG_EXTENSIONS: &[&str] = &["json", "jsonc"];

/// The version of the published input rule (`carrickInputs.rule`).
///
/// Rule 1, over repository-relative `/` paths: the basename is the text
/// after the last `/`, compared lowercased on both sides. A path is read when
/// its basename equals an entry of `repo_input_names` or `derive_names`, when
/// its extension equals an entry of `source_extensions`,
/// `repo_input_extensions` or `config_extensions`, or when its basename has no
/// extension. The extension is the text after the basename's last `.` when
/// that `.` is neither its first nor its last character. A push needs a scan
/// when any path it adds, modifies, removes or renames from is read.
///
/// Bump it when the rule changes, not when a list does.
pub const INPUT_RULE: u32 = 1;

/// Whether a change to `path` re-analyses every service
/// ([`REPO_INPUT_NAMES`], [`REPO_INPUT_EXTENSIONS`]).
pub fn is_repo_input(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    REPO_INPUT_NAMES.contains(&name)
        || name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| REPO_INPUT_EXTENSIONS.contains(&extension))
}

/// Whether `service` is carried or analysed.
///
/// `stored` is its stored generation, `config_json` this run's carrick.json
/// entry for it serialized as a blob stores it, `changed` the paths changed
/// since the stored commit (`Err` when git could not compare), and `inputs`
/// computes what it reads, asked only when nothing cheaper decides.
/// `version_only` says whether a changed manifest moved nothing but its
/// top-level `version` ([`only_version_differs`]); it is asked only of a
/// repo input, and such a manifest is read only by the services that import
/// its package, not by the one whose manifest it is (carrick#2566).
/// `gitignore_moves` says whether an edit to a `.gitignore` can move what the
/// service reads ([`gitignore_moves`]); a `.gitignore` is judged by it alone,
/// not as a repo input or a path under a root (carrick#3113).
/// `keeps_imports` says whether an edited test source still loads what it
/// loaded ([`test_edit_keeps_imports`]); it is asked only of a modified test
/// source the service reads by a root or a directory (carrick#3114).
/// `dependencies` says whether the service loads a package the other repo
/// input changes moved, when they are a dependency-only move
/// ([`crate::dependency_move`], carrick#3115); it is asked only when such a
/// change is listed, and those manifests and lockfiles then count like a
/// version-only manifest.
///
/// A run that carries under that rule, or under the dependency-only rule,
/// and uploads would write a stored manifest projection (`packages`,
/// `package_json`, `package_json_hash`) older than HEAD's (under the
/// dependency rule, older dependency versions); `refreshed_carried` does not refresh it today because
/// no uploading run carries (pinned by
/// `a_carried_write_keeps_the_stored_manifest_projection`).
///
/// A run that asks no model carries a blob another scanner release wrote in
/// this cache format, the test `current_hosted_blobs` reads a hosted blob in
/// with: such a run uploads nothing, and the release CI scanned with is
/// rarely the CLI on a laptop (carrick#2840). A run that asks the model
/// analyses it, so what it writes is its own release's.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    stored: Option<&CloudRepoData>,
    this_run: ThisRun<'_>,
    config_json: Option<&str>,
    changed: Result<&[ChangedPath], &str>,
    mut version_only: impl FnMut(&ChangedPath) -> bool,
    mut gitignore_moves: impl FnMut(&str, &ServiceInputs) -> bool,
    mut keeps_imports: impl FnMut(&ChangedPath) -> bool,
    dependencies: impl FnOnce(&ServiceInputs) -> DependencyReach,
    inputs: impl FnOnce() -> ServiceInputs,
) -> Scope {
    let Some(stored) = stored else {
        return Scope::analysed(Reason::NoStored, "no stored index for this service");
    };
    let other_release =
        this_run.asks_model && stored.scanner_version.as_deref() != Some(this_run.scanner_version);
    if other_release || stored.cache_version != Some(this_run.cache_version) {
        return Scope::analysed(
            Reason::OtherScanner,
            format!(
                "stored by Carrick {} with cache {}, this run is Carrick {} with cache {}",
                stored
                    .scanner_version
                    .as_deref()
                    .unwrap_or("of unknown version"),
                stored
                    .cache_version
                    .map_or_else(|| "unknown".to_string(), |v| v.to_string()),
                this_run.scanner_version,
                this_run.cache_version
            ),
        );
    }
    if stored.dirty == Some(true) {
        return Scope::analysed(
            Reason::StoredDirty,
            "the stored index was read from uncommitted changes",
        );
    }
    if let Some(owed) = owed_work(stored, this_run) {
        return Scope::analysed(Reason::StoredIncomplete, owed);
    }
    if stored.config_json.as_deref() != config_json {
        return Scope::analysed(
            Reason::ConfigChanged,
            "its carrick.json entry differs from the stored one",
        );
    }
    let changed = match changed {
        Ok(changed) => changed,
        Err(reason) => {
            return Scope::analysed(
                Reason::CommitMissing,
                format!(
                    "git could not compare {}: {}",
                    short(&stored.commit_hash),
                    reason.trim()
                ),
            );
        }
    };
    let inputs = inputs();
    for unresolved in &inputs.unresolved {
        if let Some(change) = changed
            .iter()
            .find(|change| unresolved.could_be.names(&change.path))
        {
            return Scope::analysed(
                Reason::UnresolvedImport,
                format!("{}, which could be {}", unresolved.at, change.path),
            );
        }
    }
    // The same rule for an `extends` nothing could follow: it keeps the
    // service from being carried only when a changed file is one it could
    // name. The specifier is all a config's `extends` leaves, so the file
    // name is what is compared.
    let blocking: Vec<&str> = inputs
        .unfollowed_extends
        .iter()
        .map(String::as_str)
        .filter(|spec| {
            changed
                .iter()
                .any(|change| extends_could_name(spec, &change.path))
        })
        .collect();
    if !blocking.is_empty() {
        return Scope::analysed(
            Reason::ExtendsUnfollowed,
            format!(
                "tsconfig extends {} could not be followed",
                blocking.join(", ")
            ),
        );
    }
    let others: Vec<&ChangedPath> = changed
        .iter()
        .filter(|change| !is_gitignore(&change.path))
        .collect();
    let version_only: Vec<bool> = others
        .iter()
        .map(|change| is_repo_input(&change.path) && version_only(change))
        .collect();
    // Each changed `.gitignore` is judged once: the verdict decides this
    // plan, and the ones that move nothing are kept for type reach.
    let moving_gitignores: Vec<(&str, bool)> = changed
        .iter()
        .filter(|change| is_gitignore(&change.path))
        .map(|change| (change.path.as_str(), gitignore_moves(&change.path, &inputs)))
        .collect();
    let ruled_out = |dependencies: bool| RuledOut {
        dependencies,
        gitignores: moving_gitignores
            .iter()
            .filter(|(_, moves)| !moves)
            .map(|(path, _)| path.to_string())
            .collect(),
    };
    let mut moved_dependencies = false;
    if let Some((change, _)) = others
        .iter()
        .zip(&version_only)
        .find(|(change, only)| is_repo_input(&change.path) && !**only)
    {
        match dependencies(&inputs) {
            DependencyReach::Unread => {
                return Scope::analysed(Reason::RepoInputChanged, change.path.clone());
            }
            DependencyReach::Reaches { file, package } => {
                return Scope::analysed_ruling(
                    Reason::InputsChanged,
                    format!("{file}: {package}"),
                    ruled_out(false),
                );
            }
            DependencyReach::Nothing => moved_dependencies = true,
        }
    }
    if let Some((change, _)) = others.iter().zip(&version_only).find(|(change, only)| {
        if **only || (moved_dependencies && is_repo_input(&change.path)) {
            inputs.reads_outside_roots(change, &mut keeps_imports)
        } else {
            inputs.reads(change, &mut keeps_imports)
        }
    }) {
        return Scope::analysed_ruling(
            Reason::InputsChanged,
            change.path.clone(),
            ruled_out(moved_dependencies),
        );
    }
    if let Some((path, _)) = moving_gitignores.iter().find(|(_, moves)| *moves) {
        return Scope::analysed_ruling(
            Reason::InputsChanged,
            path.to_string(),
            ruled_out(moved_dependencies),
        );
    }
    Scope::Carried {
        commit: stored.commit_hash.clone(),
    }
}

/// Whether a config `extends` nothing could follow could name `path`: the
/// file its last segment names, with `.json` when it states no extension. A
/// package that changed shows as a `repo_input_changed` too.
fn extends_could_name(spec: &str, path: &str) -> bool {
    let last = spec.rsplit('/').next().unwrap_or(spec);
    if last.is_empty() || last == "." || last == ".." {
        return true;
    }
    let file = if last.contains('.') {
        last.to_string()
    } else {
        format!("{last}.json")
    };
    path.rsplit('/').next() == Some(file.as_str())
}

/// What a stored blob says it still owed when it was written: degraded types,
/// an extraction config that never arrived, files the model never answered,
/// candidates no model was asked about.
fn owed_work(stored: &CloudRepoData, this_run: ThisRun<'_>) -> Option<String> {
    if let Some(degraded) = &stored.types_degraded {
        return Some(format!(
            "the stored index lost its types ({})",
            degraded.stage
        ));
    }
    // The extraction config was asked for and did not arrive, so the blob's
    // types kept their machinery wrappers and are what a contract check would
    // compare. A blob keeps the config it was analysed with, an empty rule
    // set included, so a recorded status beside no config is the one case an
    // analysis repairs: it asks for the config again (carrick#2301). A status
    // beside a config (a capture that produced no stub) is not: nothing an
    // analysis asks for would change it. A run that asks no model cannot ask
    // for the config either.
    if this_run.asks_model
        && stored.type_extraction_status.is_some()
        && stored.cached_extraction_config.is_none()
    {
        return Some("the stored index was written without its extraction config".to_string());
    }
    // What remains is model work: files and candidates no model answered. A
    // run that asks no model would leave the same owed, so it is no reason to
    // analyse the service again (`check --changed`, carrick#2826).
    if !this_run.asks_model {
        return None;
    }
    let boundary = stored.boundary.as_ref()?;
    // A file whose answer was final is lost the same way by every scan of
    // its bytes, so it is not owed (carrick#2488): a changed file is
    // analysed afresh through the changed-file path, not through this one.
    let final_losses = boundary.final_losses.as_ref().map_or(0, |f| f.total);
    let owed_files = boundary.files_lost.total.saturating_sub(final_losses);
    if owed_files > 0 {
        return Some(format!(
            "the stored index owes {owed_files} file(s) model analysis"
        ));
    }
    match boundary.candidates_awaiting_model {
        Some(waiting) if waiting > 0 => Some(format!(
            "the stored index has {waiting} candidate(s) no model was asked about"
        )),
        _ => None,
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// The paths changed since each stored commit, listed once per commit for the
/// whole run: [`plan`] reads them, and so does a pull request scan's type-reach
/// decision (carrick#3102), so git is asked once.
///
/// An untracked `carrick.json` the runner wrote is no changed path when it
/// names exactly the services this repository has a stored index for
/// ([`scaffold_config_is_the_stored_one`]).
pub struct ChangedSince {
    repo_path: String,
    scaffold_config: Option<String>,
    listed: std::sync::Mutex<HashMap<String, Result<Vec<ChangedPath>, String>>>,
    moved: std::sync::Mutex<HashMap<String, Option<std::sync::Arc<DependencyMove>>>>,
}

impl ChangedSince {
    /// `stored_services` names every service this repository has a stored
    /// index for.
    pub fn new(repo_path: &str, services: &[Config], stored_services: &[Option<String>]) -> Self {
        Self {
            repo_path: repo_path.to_string(),
            scaffold_config: scaffold_config_is_the_stored_one(services, stored_services)
                .then(|| crate::git_state::untracked_root_config(repo_path))
                .flatten(),
            listed: Default::default(),
            moved: Default::default(),
        }
    }

    /// What the manifests and lockfiles changed since `commit` moved, read
    /// once per commit and shared by [`plan`] and a pull request's
    /// type-reach decision (carrick#3126). `None` when the change is not
    /// dependency-only or git could not compare.
    pub fn dependency_move(
        &self,
        commit: &str,
        top: &Path,
    ) -> Option<std::sync::Arc<DependencyMove>> {
        let mut moved = self.moved.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(held) = moved.get(commit) {
            return held.clone();
        }
        let paths = self.since(commit).ok()?;
        let read =
            dependency_move(&self.repo_path, top, commit, None, &paths).map(std::sync::Arc::new);
        moved.insert(commit.to_string(), read.clone());
        read
    }

    /// Every path that differs between `commit` and the working tree,
    /// relative to the repository's top level ([`crate::git_state::changed_paths`]),
    /// without the runner's own `carrick.json`. `Err` when git could not
    /// compare.
    pub fn since(&self, commit: &str) -> Result<Vec<ChangedPath>, String> {
        self.listed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(commit.to_string())
            .or_insert_with(|| {
                let mut paths = crate::git_state::changed_paths(&self.repo_path, commit, None);
                if let (Ok(paths), Some(config)) = (&mut paths, &self.scaffold_config) {
                    paths.retain(|change| !(change.status == 'A' && &change.path == config));
                }
                paths
            })
            .clone()
    }
}

/// Each service's scope, index for index with `services`, logged one line
/// per service with a summary and the closure walk's wall time. `changes`
/// lists what changed since each stored commit.
pub fn plan<'a>(
    repo_path: &str,
    services: &[Config],
    changes: &ChangedSince,
    stored: impl Fn(&Config) -> Option<&'a CloudRepoData>,
    this_run: ThisRun<'_>,
) -> Vec<Scope> {
    let top = crate::git_state::toplevel(repo_path).ok();
    let mut version_only: HashMap<(String, String), bool> = HashMap::new();
    // Listed once, and only when a `.gitignore` changed (carrick#3113).
    let untracked: std::cell::OnceCell<Option<Vec<String>>> = std::cell::OnceCell::new();
    let mut stops: HashMap<PathBuf, bool> = HashMap::new();
    let mut keeps_imports_of: HashMap<(String, String), bool> = HashMap::new();
    let mut closure_took = std::time::Duration::ZERO;
    let mut walked = 0usize;
    // Services in one repository share files (a contracts package every
    // service imports), so what a file reads is read once for all of them.
    let reads = ClosureReads::default();
    let scopes: Vec<Scope> = services
        .iter()
        .map(|service| {
            let stored = stored(service);
            let config_json = serde_json::to_string(service).ok();
            let changed = stored.map(|blob| changes.since(&blob.commit_hash));
            let changed = match &changed {
                Some(Ok(paths)) => Ok(paths.as_slice()),
                Some(Err(reason)) => Err(reason.as_str()),
                None => Ok(&[][..]),
            };
            let version_only = |change: &ChangedPath| {
                let (Some(blob), Some(top)) = (stored, &top) else {
                    return false;
                };
                *version_only
                    .entry((blob.commit_hash.clone(), change.path.clone()))
                    .or_insert_with(|| {
                        manifest_moves_only_version(repo_path, top, &blob.commit_hash, change)
                    })
            };
            let keeps_imports = |change: &ChangedPath| {
                let (Some(blob), Some(top)) = (stored, &top) else {
                    return false;
                };
                *keeps_imports_of
                    .entry((blob.commit_hash.clone(), change.path.clone()))
                    .or_insert_with(|| {
                        test_edit_keeps_imports(
                            &reads,
                            &change.path,
                            crate::git_state::file_at_commit(
                                repo_path,
                                &blob.commit_hash,
                                &change.path,
                            ),
                            std::fs::read_to_string(top.join(&change.path)).ok(),
                        )
                    })
            };
            let dependencies = |inputs: &ServiceInputs| {
                let (Some(blob), Some(top), Ok(_)) = (stored, &top, changed) else {
                    return DependencyReach::Unread;
                };
                match changes.dependency_move(&blob.commit_hash, top) {
                    Some(moved) => moved.reach(inputs, |moved| {
                        loaded_packages(repo_path, top, service, inputs, &reads, moved)
                    }),
                    None => DependencyReach::Unread,
                }
            };
            let scope = decide(
                stored,
                this_run,
                config_json.as_deref(),
                changed,
                version_only,
                |gitignore, inputs| {
                    let untracked = untracked
                        .get_or_init(|| crate::git_state::untracked_paths(repo_path).ok())
                        .as_deref();
                    let Some(top) = &top else {
                        return true;
                    };
                    let skip = crate::engine::service_ignore_patterns(service);
                    gitignore_moves(gitignore, inputs, untracked, |base, path, is_dir| {
                        walk_could_enter(top, &skip, base, path, is_dir, &mut stops)
                    })
                },
                keeps_imports,
                dependencies,
                || {
                    let started = Instant::now();
                    walked += 1;
                    let inputs = match &top {
                        Some(top) => service_inputs(repo_path, top, service, &reads),
                        // No top level, no paths to compare: nothing vouches.
                        None => ServiceInputs {
                            unresolved: vec![UnresolvedImport {
                                at: "git named no top level for this checkout".to_string(),
                                could_be: CouldBe::Anything,
                            }],
                            ..ServiceInputs::default()
                        },
                    };
                    closure_took += started.elapsed();
                    inputs
                },
            );
            let label = service.service_name.as_deref().unwrap_or("(root)");
            match &scope {
                Scope::Carried { commit } => info!(
                    "scope: {label} carried (inputs unchanged since {})",
                    short(commit)
                ),
                // A stored index that lacks something is a gap in the index: the
                // person reading the scan is not told why, the debug log is.
                Scope::Analysed {
                    reason: Reason::StoredIncomplete,
                    detail,
                    ..
                } => {
                    info!("scope: {label} analysed");
                    debug!(
                        "scope: {label} analysed ({}: {detail})",
                        Reason::StoredIncomplete.as_str()
                    );
                }
                Scope::Analysed { reason, detail, .. } => {
                    info!("scope: {label} analysed ({}: {detail})", reason.as_str())
                }
            }
            scope
        })
        .collect();
    let carried = scopes.iter().filter(|scope| scope.is_carried()).count();
    info!(
        "scope: {} analysed, {carried} carried, of {} service(s)",
        scopes.len() - carried,
        scopes.len()
    );
    info!(
        "scope: the closure walk took {:.2}s over {walked} service(s)",
        closure_took.as_secs_f64()
    );
    scopes
}

/// Whether the `carrick.json` an untracked-config run found names the
/// services the repository has stored: the same set, so each entry is judged
/// by `ConfigChanged` alone. A different list changes which directories nest
/// in which, and the stored `config_json` does not show that
/// (`Config::nested_directories`).
fn scaffold_config_is_the_stored_one(
    services: &[Config],
    stored_services: &[Option<String>],
) -> bool {
    let named: BTreeSet<&Option<String>> = services.iter().map(|s| &s.service_name).collect();
    let stored: BTreeSet<&Option<String>> = stored_services.iter().collect();
    !services.is_empty() && named == stored
}

/// Whether a modified `package.json` differs from the stored commit's copy in
/// its top-level `version` alone. Anything else (another file, an added or
/// deleted manifest, a copy that cannot be read or parsed) is `false`.
fn manifest_moves_only_version(
    repo_path: &str,
    top: &Path,
    commit: &str,
    change: &ChangedPath,
) -> bool {
    if change.status != 'M' || change.path.rsplit('/').next() != Some("package.json") {
        return false;
    }
    let Some(stored) = crate::git_state::file_at_commit(repo_path, commit, &change.path) else {
        return false;
    };
    let Ok(head) = std::fs::read_to_string(top.join(&change.path)) else {
        return false;
    };
    only_version_differs(&stored, &head)
}

/// A manifest's text split into its top-level `version` and every other key,
/// or `None` unless it parses as an object that states `version`. A version
/// is no type input (carrick#3126), so what a manifest means for the scan is
/// the rest.
pub(crate) fn split_version(
    text: &str,
) -> Option<(
    serde_json::Value,
    serde_json::Map<String, serde_json::Value>,
)> {
    let serde_json::Value::Object(mut object) = serde_json::from_str(text).ok()? else {
        return None;
    };
    let version = object.remove("version")?;
    Some((version, object))
}

/// Whether two manifests parse as objects that both state a top-level
/// `version`, differ in it, and are equal in every other key.
pub(crate) fn only_version_differs(stored: &str, head: &str) -> bool {
    match (split_version(stored), split_version(head)) {
        (Some((before, stored)), Some((after, head))) => before != after && stored == head,
        _ => false,
    }
}

/// What the import-closure walks of one tree read from a file, kept for every
/// service the tree's walks run for: a file's specifiers, a path relative to
/// the top level, a path's canonical form. Each is a property of the file on
/// disk alone, never of the service whose walk reached it.
pub struct ClosureReads {
    reader: std::cell::RefCell<crate::parser::ModuleReader>,
    specifiers: std::cell::RefCell<HashMap<PathBuf, Vec<String>>>,
    relative: std::cell::RefCell<HashMap<(PathBuf, PathBuf), Option<String>>>,
    canonical: std::cell::RefCell<HashMap<PathBuf, PathBuf>>,
    /// How many files were parsed: what sharing the reads saves.
    #[cfg(test)]
    parsed: std::cell::Cell<usize>,
}

impl Default for ClosureReads {
    fn default() -> Self {
        Self {
            // A file that does not parse is reported by the pass that reads
            // it for analysis, not by this walk.
            reader: std::cell::RefCell::new(crate::parser::ModuleReader::quiet()),
            specifiers: Default::default(),
            relative: Default::default(),
            canonical: Default::default(),
            #[cfg(test)]
            parsed: Default::default(),
        }
    }
}

impl ClosureReads {
    /// Keyed by the file's real path: one walk reaches a file by the path it
    /// walked, another through a link or by the resolver's real path.
    pub(crate) fn loaded_specifiers(&self, file: &Path) -> Vec<String> {
        let key = self.canonical(file);
        if let Some(known) = self.specifiers.borrow().get(&key) {
            return known.clone();
        }
        #[cfg(test)]
        self.parsed.set(self.parsed.get() + 1);
        // Includes types named through `import('./x').T` or a `/// <reference>`.
        let loaded = self.reader.borrow_mut().reach_specifiers(file);
        self.specifiers.borrow_mut().insert(key, loaded.clone());
        loaded
    }

    fn top_relative(&self, top: &Path, path: &Path) -> Option<String> {
        let key = (top.to_path_buf(), path.to_path_buf());
        if let Some(known) = self.relative.borrow().get(&key) {
            return known.clone();
        }
        let relative = top_relative(top, path);
        self.relative.borrow_mut().insert(key, relative.clone());
        relative
    }

    fn canonical(&self, path: &Path) -> PathBuf {
        if let Some(known) = self.canonical.borrow().get(path) {
            return known.clone();
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.canonical
            .borrow_mut()
            .insert(path.to_path_buf(), canonical.clone());
        canonical
    }
}

/// What `service` reads on the tree at `repo_path`, relative to the
/// repository's top level `top`. `reads` keeps what one file read gives, for
/// the next service whose walk reaches the same file.
pub fn service_inputs(
    repo_path: &str,
    top: &Path,
    service: &Config,
    reads: &ClosureReads,
) -> ServiceInputs {
    let root = Path::new(repo_path);
    let top = top.canonicalize().unwrap_or_else(|_| top.to_path_buf());
    let relative = |path: &Path| -> Option<String> { reads.top_relative(&top, path) };
    let mut roots: Vec<Root> = Vec::new();
    let mut unresolved: Vec<UnresolvedImport> = Vec::new();

    // Its roots, as the walk reads them.
    let service_root = match &service.directory {
        Some(dir) => root.join(dir),
        None => root.to_path_buf(),
    };
    let own = crate::config::directory_segments(service.directory.as_deref());
    let left_out: Vec<String> = service
        .nested_directories
        .iter()
        .filter(|nested| {
            let nested = crate::config::directory_segments(Some(nested));
            nested.len() > own.len() && nested.starts_with(&own)
        })
        .filter_map(|nested| relative(&root.join(nested)))
        .collect();
    roots.extend(relative(&service_root).map(|dir| Root { dir, left_out }));
    for include in &service.include {
        roots.extend(relative(&root.join(include)).map(|dir| Root {
            dir,
            left_out: Vec::new(),
        }));
    }
    let scanned_in_a_root = |path: &str| {
        roots.iter().any(|root| {
            under(path, &root.dir)
                && !root.left_out.iter().any(|dir| under(path, dir))
                && crate::file_finder::is_scanned_source(Path::new(path), Path::new(&root.dir))
        })
    };

    // Its import closure: every walked file's imports, and the imports of
    // every file they reach, resolved by the scanner's own resolver.
    let walk =
        crate::file_finder::walk_service(repo_path, service, &crate::packages::MANIFEST_SKIP_DIRS);
    let service_tsconfig = service.alias_tsconfig();
    let index = crate::workspace_resolver::WorkspaceIndex::build_with_aliases(
        root,
        service_tsconfig
            .as_ref()
            .map(|(directory, config)| (directory.as_path(), config.as_path())),
    );
    let canonical = |path: &Path| reads.canonical(path);
    let mut seen: BTreeSet<PathBuf> = walk.files.iter().map(|file| canonical(file)).collect();
    let mut queue: Vec<PathBuf> = walk.files.clone();
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut lookup_dirs: BTreeSet<String> = BTreeSet::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    while let Some(importer) = queue.pop() {
        for specifier in reads.loaded_specifiers(&importer) {
            let relative_specifier = specifier.starts_with("./") || specifier.starts_with("../");
            if relative_specifier && let Some(parent) = importer.parent() {
                // Where the lookup happens: a file appearing beside the one
                // it found can take its place.
                let looked_up = crate::workspace_resolver::normalize(&parent.join(&specifier));
                if let Some(dir) = looked_up.parent().and_then(relative) {
                    lookup_dirs.insert(dir);
                }
            }
            if let Some(package) = index.package_dir_of(&importer, &specifier)
                && let Some(dir) = relative(&root.join(package))
            {
                // The resolver may pick another file than the compiler does
                // (an `exports` map), so the whole package is the safe unit.
                dirs.insert(dir);
            }
            let Some(target) = index.resolve_module_path(&importer, &specifier) else {
                // A file the resolver does not read as source (a JSON file, a
                // stylesheet) is still read.
                let asset = importer
                    .parent()
                    .filter(|_| relative_specifier)
                    .map(|parent| parent.join(specifier.split(['?', '#']).next().unwrap_or("")))
                    .filter(|file| file.is_file());
                if let Some(asset) = asset {
                    files.extend(relative(&asset));
                } else if index.names_an_unknown_module(&importer, &specifier)
                    && !index.declares_types_for(&specifier)
                {
                    let bare = specifier.split(['?', '#']).next().unwrap_or("");
                    let could_be = if relative_specifier {
                        // `./x.js` names `./x.ts` too, so the base drops it.
                        let base = [".js", ".jsx", ".mjs", ".cjs"]
                            .iter()
                            .find_map(|ext| bare.strip_suffix(ext))
                            .unwrap_or(bare);
                        importer
                            .parent()
                            .and_then(|parent| relative(&parent.join(base)))
                            .map_or(CouldBe::Anything, CouldBe::Path)
                    } else {
                        match index.resolve(&importer, bare) {
                            crate::workspace_resolver::Resolution::Internal(path) => {
                                relative(&root.join(path)).map_or(CouldBe::Anything, CouldBe::Path)
                            }
                            crate::workspace_resolver::Resolution::AliasTargetMissing(missing) => {
                                relative(&root.join(&missing.target_root))
                                    .map_or(CouldBe::Anything, CouldBe::Under)
                            }
                            _ => CouldBe::Named(bare.to_string()),
                        }
                    };
                    unresolved.push(UnresolvedImport {
                        at: format!(
                            "{} imports {specifier}",
                            relative(&importer).unwrap_or_else(|| importer.display().to_string())
                        ),
                        could_be,
                    });
                }
                continue;
            };
            let Some(path) = relative(&target) else {
                continue;
            };
            if let Some(dir) = Path::new(&path).parent() {
                lookup_dirs.insert(dir.to_string_lossy().replace('\\', "/"));
            }
            // A target a scan reads is read by its root; one it does not (a
            // test source, an asset) is read only because this file imports
            // it, so it is a closure file (carrick#3114).
            if !scanned_in_a_root(&path) {
                files.insert(path);
            }
            if seen.insert(canonical(&target)) {
                queue.push(target);
            }
        }
    }
    let mut unfollowed_extends = index.unfollowed_extends();

    // Its config chain, and what that config includes beyond the roots.
    if let Some(tsconfig) = service_tsconfig_path(root, &service_root, service) {
        let (chain, unfollowed) =
            crate::module_aliases::tsconfig_chain(root, &tsconfig, &index.internal_package_dirs());
        for spec in unfollowed {
            if !unfollowed_extends.contains(&spec) {
                unfollowed_extends.push(spec);
            }
        }
        for (key, is_files) in [("include", false), ("files", true)] {
            // The nearest config that states the key is the one that counts.
            let Some((config, entries)) = chain.iter().find_map(|(config, json)| {
                json.get(key)
                    .and_then(|value| value.as_array())
                    .map(|entries| (config, entries))
            }) else {
                continue;
            };
            let base = root.join(config.parent().unwrap_or(Path::new("")));
            for entry in entries.iter().filter_map(|entry| entry.as_str()) {
                let target = base.join(static_prefix(entry));
                let Some(path) = relative(&target) else {
                    continue;
                };
                if is_files || target.is_file() {
                    files.insert(path);
                } else {
                    dirs.insert(path);
                }
            }
        }
        for (config, _) in &chain {
            files.extend(relative(&root.join(config)));
        }
    }
    files.extend(walk.manifest.as_deref().and_then(relative));
    files.extend(
        crate::graphql::resolve_declared_schemas(root, &service.graphql_schemas)
            .files
            .iter()
            .filter_map(|file| relative(file)),
    );

    ServiceInputs {
        roots,
        files,
        dirs,
        lookup_dirs,
        unresolved,
        unfollowed_extends,
    }
}

/// The tsconfig that governs `service`: the one carrick.json names, else the
/// nearest `tsconfig.json` at or above its directory. Repo-relative.
pub(crate) fn service_tsconfig_path(
    root: &Path,
    service_root: &Path,
    service: &Config,
) -> Option<PathBuf> {
    let found = match &service.tsconfig {
        Some(named) => Some(service_root.join(named)).filter(|path| path.is_file()),
        None => service_root
            .ancestors()
            .take_while(|dir| dir.starts_with(root))
            .map(|dir| dir.join("tsconfig.json"))
            .find(|path| path.is_file()),
    }?;
    found.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// The part of a tsconfig `include` entry before its first wildcard.
pub(crate) fn static_prefix(entry: &str) -> PathBuf {
    entry
        .trim_start_matches("./")
        .split('/')
        .take_while(|segment| !segment.contains(['*', '?', '{', '[']))
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// `path` relative to the canonical top level `top`, with `/` separators.
/// `None` for a path outside it. A path that is not on disk is placed by its
/// nearest existing ancestor.
fn top_relative(top: &Path, path: &Path) -> Option<String> {
    let normalized = crate::workspace_resolver::normalize(path);
    let (existing, rest): (PathBuf, PathBuf) = {
        let mut existing = normalized.clone();
        let mut rest = Vec::new();
        while !existing.exists() {
            rest.push(existing.file_name()?.to_os_string());
            existing = existing.parent()?.to_path_buf();
        }
        (existing, rest.into_iter().rev().collect())
    };
    let canonical = existing.canonicalize().ok()?.join(rest);
    let relative = canonical.strip_prefix(top).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Whether a stored copy of a service differs from the pull request's base,
/// judged by what that service reads (carrick#2228).
///
/// A pull-request run compares its findings with main's stored copy, and a
/// copy stored at an older commit than the base is still main's when nothing
/// its service reads changed in between. A service stored alone by a scoped
/// push leaves the others at older commits, so a whole-tree comparison would
/// call the copy stale on every pull request.
///
/// A copy whose services all sit at one commit was written by one scan of the
/// whole repository, and is judged by the whole tree as it always was.
pub struct CopyJudge<'a> {
    repo_path: &'a str,
    services: &'a [Config],
    /// Main's copy sits at more than one commit.
    scoped: bool,
    top: Option<PathBuf>,
    /// The pull request's own changes, base to the scanned tree.
    pr_changes: std::cell::OnceCell<Result<Vec<ChangedPath>, String>>,
    /// What the services' closure walks read, shared between the copies.
    reads: ClosureReads,
    /// What each (copy commit, base) pair's dependency-only move moved.
    moved: std::cell::RefCell<MovedByPair>,
}

type MovedByPair = HashMap<(String, String), Option<std::rc::Rc<DependencyMove>>>;

impl<'a> CopyJudge<'a> {
    pub fn new(repo_path: &'a str, services: &'a [Config], main_self: &[CloudRepoData]) -> Self {
        let commits: BTreeSet<&str> = main_self
            .iter()
            .map(|copy| copy.commit_hash.as_str())
            .collect();
        Self {
            repo_path,
            services,
            scoped: commits.len() > 1,
            top: crate::git_state::toplevel(repo_path).ok(),
            pr_changes: std::cell::OnceCell::new(),
            reads: ClosureReads::default(),
            moved: Default::default(),
        }
    }

    /// Whether `copy` differs from `base` in anything its service reads.
    /// `None` when git cannot list the paths changed between them.
    ///
    /// Its inputs are computed on the scanned tree, which can differ from the
    /// base's where the pull request itself edits a file outside the
    /// service's roots that the service reads: then, and for a copy whose
    /// service this tree does not configure or whose inputs cannot be read
    /// whole, the whole trees are compared as before. A changed manifest or
    /// lockfile counts as in [`decide`]: a dependency-only move counts where
    /// the service loads a moved package.
    pub fn differs(&self, copy: &CloudRepoData, base: &str) -> Option<bool> {
        if copy.commit_hash == base {
            return Some(false);
        }
        let changed =
            crate::git_state::changed_paths(self.repo_path, &copy.commit_hash, Some(base)).ok()?;
        let whole_tree =
            || crate::git_state::differs_under(self.repo_path, &copy.commit_hash, base).ok();
        if !self.scoped {
            return whole_tree();
        }
        let (Some(service), Some(top)) = (
            self.services
                .iter()
                .find(|service| service.service_name == copy.service_name),
            self.top.as_deref(),
        ) else {
            return whole_tree();
        };
        let inputs = service_inputs(self.repo_path, top, service, &self.reads);
        if !inputs.unresolved.is_empty() || !inputs.unfollowed_extends.is_empty() {
            return whole_tree();
        }
        let pr_changes = self
            .pr_changes
            .get_or_init(|| crate::git_state::changed_paths(self.repo_path, base, None));
        let pr_edits_an_outside_input = match pr_changes {
            Ok(pr_changes) => pr_changes
                .iter()
                .any(|change| inputs.reads_outside_roots(change, &mut |_| false)),
            Err(_) => true,
        };
        if pr_edits_an_outside_input {
            return whole_tree();
        }
        // A dependency-only move counts only where the service loads a
        // moved package (carrick#3115). What it loads is read on this tree,
        // so a pull request that edits anything it reads, or a repository
        // input, leaves the move unread.
        let mut moved_dependencies = false;
        if changed
            .iter()
            .any(|change| is_repo_input(&change.path) && !is_gitignore(&change.path))
        {
            let pr_edits_what_it_loads = match pr_changes {
                Ok(pr_changes) => pr_changes.iter().any(|change| {
                    is_repo_input(&change.path) || inputs.reads(change, &mut |_| false)
                }),
                Err(_) => true,
            };
            let moved = (!pr_edits_what_it_loads)
                .then(|| self.moved(&copy.commit_hash, base, &changed, top))
                .flatten();
            let reach = moved.map_or(DependencyReach::Unread, |moved| {
                moved.reach(&inputs, |moved| {
                    loaded_packages(self.repo_path, top, service, &inputs, &self.reads, moved)
                })
            });
            if reach != DependencyReach::Nothing {
                return Some(true);
            }
            moved_dependencies = true;
        }
        Some(changed.iter().any(|change| {
            if is_gitignore(&change.path) {
                inputs.reads_the_dir_of(&change.path)
            } else if moved_dependencies && is_repo_input(&change.path) {
                inputs.reads_outside_roots(change, &mut |_| false)
            } else {
                is_repo_input(&change.path)
                    || inputs.reads(change, &mut |change| {
                        test_edit_keeps_imports(
                            &self.reads,
                            &change.path,
                            crate::git_state::file_at_commit(
                                self.repo_path,
                                &copy.commit_hash,
                                &change.path,
                            ),
                            crate::git_state::file_at_commit(self.repo_path, base, &change.path),
                        )
                    })
            }
        }))
    }

    /// What the manifests and lockfiles moved between `from` and `base`,
    /// read once for every copy stored at `from`.
    fn moved(
        &self,
        from: &str,
        base: &str,
        changed: &[ChangedPath],
        top: &Path,
    ) -> Option<std::rc::Rc<DependencyMove>> {
        self.moved
            .borrow_mut()
            .entry((from.to_string(), base.to_string()))
            .or_insert_with(|| {
                dependency_move(self.repo_path, top, from, Some(base), changed)
                    .map(std::rc::Rc::new)
            })
            .clone()
    }
}

/// Whether a carried service's join fields differ from its stored blob's: its
/// type verdicts and pair outcomes, its SDK edges and what they left
/// unresolved, and its boundary (carrick#2227, carrick#3104). Read on the payload as the upload would send it,
/// before it is stamped. An absent list and an empty one are the same answer,
/// and order is not a difference.
pub fn join_fields_changed(payload: &CloudRepoData, stored: &CloudRepoData) -> bool {
    fn rows<T: serde::Serialize>(value: &Option<Vec<T>>) -> Vec<String> {
        let mut rows: Vec<String> = value
            .iter()
            .flatten()
            .map(|row| serde_json::to_string(row).unwrap_or_default())
            .collect();
        rows.sort();
        rows
    }
    // The stored pair outcomes by their rows, not their commit and build
    // stamps: those move on every run (carrick#3104).
    let outcomes = |data: &CloudRepoData| {
        rows(
            &data
                .pair_outcomes
                .as_ref()
                .map(|record| record.outcomes.clone()),
        )
    };
    rows(&payload.compat_verdicts) != rows(&stored.compat_verdicts)
        || outcomes(payload) != outcomes(stored)
        || rows(&payload.sdk_edges) != rows(&stored.sdk_edges)
        || rows(&payload.sdk_unresolved) != rows(&stored.sdk_unresolved)
        || payload.boundary != stored.boundary
}

/// A carried service's payload, when this run's join changed what its stored
/// blob says: `payload` is the stored blob with this run's verdicts and SDK
/// edges attached, `stored` the blob itself. `None` when nothing changed, so
/// nothing is written.
///
/// Its boundary is the stored one with this run's SDK join folded in: the
/// rest of it states the run that analysed the service, which this run did
/// not repeat.
pub fn refreshed_carried(
    mut payload: CloudRepoData,
    stored: &CloudRepoData,
    sdk_unresolved: &[crate::cloud_storage::SdkUnresolved],
    head: &str,
    dirty: bool,
) -> Option<CloudRepoData> {
    payload.boundary = stored.boundary.clone().map(|mut boundary| {
        boundary.fold_sdk_unresolved(sdk_unresolved);
        boundary
    });
    if !join_fields_changed(&payload, stored) {
        return None;
    }
    stamp_carried(&mut payload, head, dirty);
    Some(payload)
}

/// Stamp a carried payload whose join fields changed as this run's: its
/// inputs are unchanged since its stored commit, so its answers describe
/// `head` too, and a new commit keeps the cloud's (commit, scanner) write
/// dedupe from discarding it. It sent no file to the analyzer.
pub fn stamp_carried(payload: &mut CloudRepoData, head: &str, dirty: bool) {
    payload.commit_hash = head.to_string();
    payload.dirty = dirty.then_some(true);
    payload.last_updated = chrono::Utc::now();
    payload.scanner_build = crate::cloud_storage::ScannerBuild::current();
    if let Some(boundary) = payload.boundary.as_mut() {
        boundary.commit_hash = head.to_string();
        boundary.files_attempted = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THIS: ThisRun<'static> = ThisRun {
        scanner_version: "1.0.0",
        cache_version: 7,
        asks_model: true,
    };

    fn stored(commit: &str) -> CloudRepoData {
        let mut blob: CloudRepoData = serde_json::from_value(serde_json::json!({
            "repo_name": "mono",
            "service_name": "api",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": "{\"serviceName\":\"api\"}",
            "package_json": null,
            "packages": null,
            "last_updated": "2026-10-01T00:00:00Z",
            "commit_hash": commit,
        }))
        .unwrap();
        blob.scanner_version = Some("1.0.0".to_string());
        blob.cache_version = Some(7);
        blob
    }

    fn changed(entries: &[(char, &str)]) -> Vec<ChangedPath> {
        entries
            .iter()
            .map(|(status, path)| ChangedPath {
                status: *status,
                path: path.to_string(),
            })
            .collect()
    }

    fn api_inputs() -> ServiceInputs {
        ServiceInputs {
            roots: vec![Root {
                dir: "apps/api".to_string(),
                left_out: vec!["apps/api/nested".to_string()],
            }],
            files: ["packages/types/x.ts".to_string()].into(),
            dirs: ["packages/shared".to_string()].into(),
            lookup_dirs: ["libs/util".to_string()].into(),
            ..ServiceInputs::default()
        }
    }

    fn decide_with(
        blob: Option<&CloudRepoData>,
        changes: Result<&[ChangedPath], &str>,
        inputs: ServiceInputs,
    ) -> Scope {
        decide(
            blob,
            THIS,
            Some("{\"serviceName\":\"api\"}"),
            changes,
            |_| false,
            |gitignore, inputs| gitignore_moves(gitignore, inputs, Some(&[]), |_, _, _| true),
            |_| false,
            |_| DependencyReach::Unread,
            || inputs,
        )
    }

    fn reason(scope: &Scope) -> Option<Reason> {
        match scope {
            Scope::Analysed { reason, .. } => Some(*reason),
            Scope::Carried { .. } => None,
        }
    }

    #[test]
    fn a_service_whose_inputs_did_not_change_is_carried() {
        let blob = stored("abcdef1234");
        let changes = changed(&[('M', "README.md"), ('M', "apps/web/src/a.ts")]);
        assert_eq!(
            decide_with(Some(&blob), Ok(&changes), api_inputs()),
            Scope::Carried {
                commit: "abcdef1234".to_string()
            }
        );
    }

    #[test]
    fn each_input_kind_analyses_the_service() {
        let blob = stored("abcdef1234");
        for (change, expected) in [
            (('M', "apps/api/src/server.ts"), Some(Reason::InputsChanged)),
            (('M', "apps/api/nested/src/a.ts"), None),
            (('M', "packages/types/x.ts"), Some(Reason::InputsChanged)),
            (
                ('M', "packages/shared/src/deep/y.ts"),
                Some(Reason::InputsChanged),
            ),
            (('A', "libs/util/index.tsx"), Some(Reason::InputsChanged)),
            (('M', "libs/util/other.ts"), None),
            (('D', "libs/util/other.ts"), Some(Reason::InputsChanged)),
            (('M', "apps/api2/x.ts"), None),
            (('M', "yarn.lock"), Some(Reason::RepoInputChanged)),
            (
                ('M', "apps/web/package.json"),
                Some(Reason::RepoInputChanged),
            ),
            (('M', "schema/a.graphqls"), Some(Reason::RepoInputChanged)),
            (('M', "apps/web/.gitignore"), None),
            (('M', "carrick.json"), Some(Reason::RepoInputChanged)),
            (('M', ".npmrc"), Some(Reason::RepoInputChanged)),
            (
                ('M', "apps/web/.yarnrc.yml"),
                Some(Reason::RepoInputChanged),
            ),
            (('M', ".yarnrc"), Some(Reason::RepoInputChanged)),
            (('M', "bunfig.toml"), Some(Reason::RepoInputChanged)),
            (('M', ".gitmodules"), Some(Reason::RepoInputChanged)),
            (('M', ".gitattributes"), Some(Reason::RepoInputChanged)),
            // CONFIG_EXTENSIONS is not a repo input.
            (('M', "packages/x/tsconfig.base.json"), None),
        ] {
            let changes = changed(&[change]);
            assert_eq!(
                reason(&decide_with(Some(&blob), Ok(&changes), api_inputs())),
                expected,
                "{change:?}"
            );
        }
    }

    /// The npm manifest publishes the input rule so carrick-cloud's push
    /// filter reads it rather than restating it (carrick#2234). It must equal
    /// the constants. On failure the message prints the whole expected field.
    #[test]
    fn npm_manifest_inputs_equal_the_constants() {
        let expected = serde_json::json!({
            "rule": INPUT_RULE,
            "source_extensions": crate::file_finder::SOURCE_EXTENSIONS,
            "repo_input_names": REPO_INPUT_NAMES,
            "repo_input_extensions": REPO_INPUT_EXTENSIONS,
            "config_extensions": CONFIG_EXTENSIONS,
            "derive_names": crate::service_derivation::DEPLOY_CONFIG,
        });
        assert_eq!(
            published_inputs(),
            expected,
            "npm/carrick/package.json carrickInputs differs from the constants; set it to:\n{}",
            serde_json::to_string_pretty(&expected).unwrap()
        );
    }

    fn published_inputs() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/npm/carrick/package.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read npm manifest"))
                .expect("parse npm manifest");
        manifest["carrickInputs"].clone()
    }

    /// Rule 1 (see [`INPUT_RULE`]) over the parsed manifest field, which is
    /// what the cloud reads.
    fn rule_reads(field: &serde_json::Value, path: &str) -> bool {
        let list = |key: &str| -> Vec<String> {
            field[key]
                .as_array()
                .unwrap_or_else(|| panic!("{key} is not a list"))
                .iter()
                .map(|v| v.as_str().unwrap().to_lowercase())
                .collect()
        };
        let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        let extension = name
            .rsplit_once('.')
            .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
            .map(|(_, ext)| ext);
        let named = list("repo_input_names")
            .into_iter()
            .chain(list("derive_names"))
            .any(|n| n == name);
        let by_extension = extension.is_some_and(|ext| {
            [
                "source_extensions",
                "repo_input_extensions",
                "config_extensions",
            ]
            .iter()
            .any(|key| list(key).iter().any(|e| e == ext))
        });
        named || by_extension || extension.is_none()
    }

    #[test]
    fn the_published_rule_reads_what_the_scanner_reads() {
        let field = published_inputs();
        let read = [
            "apps/api/src/server.ts",
            "SRC/A.TS",
            "src/__tests__/a.test.ts",
            "tests/fixtures/seed.ts",
            "e2e/y.ts",
            "src/a.stories.tsx",
            "packages/x/tsconfig.build.json",
            "deno/import_map.json",
            "src/data/fixture.json",
            "config.jsonc",
            "schema/a.graphqls",
            "deep/.gitignore",
            ".npmrc",
            "apps/web/.yarnrc.yml",
            ".yarnrc",
            "bunfig.toml",
            ".gitmodules",
            ".gitattributes",
            "services/api/Dockerfile",
            "vendor/submodule",
            "bin/cli",
        ];
        let not_read = [
            "docs/a.md",
            "img.png",
            "src/style.css",
            "src/lib.rs",
            "README.md",
            ".github/workflows/ci.yml",
        ];
        for p in read {
            assert!(rule_reads(&field, p), "{p} should be read");
            assert!(read_by_input_rule(p), "{p} should be read");
            // The scanner never reads less than the rule says.
            let scanner = is_repo_input(p)
                || crate::file_finder::is_scanned_source(Path::new(p), Path::new(""));
            assert!(!scanner || rule_reads(&field, p), "{p}");
        }
        for p in not_read {
            assert!(!rule_reads(&field, p), "{p} should not be read");
            assert!(!read_by_input_rule(p), "{p} should not be read");
            assert!(
                !is_repo_input(p)
                    && !crate::file_finder::is_scanned_source(Path::new(p), Path::new("")),
                "{p}"
            );
        }
    }

    #[test]
    fn the_stored_blob_decides_before_any_path_is_read() {
        let changes: Vec<ChangedPath> = Vec::new();
        assert_eq!(
            reason(&decide_with(None, Ok(&changes), api_inputs())),
            Some(Reason::NoStored)
        );
        let mut other = stored("abcdef1234");
        other.scanner_version = Some("0.9.0".to_string());
        assert_eq!(
            reason(&decide_with(Some(&other), Ok(&changes), api_inputs())),
            Some(Reason::OtherScanner)
        );
        let mut other_cache = stored("abcdef1234");
        other_cache.cache_version = Some(6);
        assert_eq!(
            reason(&decide_with(Some(&other_cache), Ok(&changes), api_inputs())),
            Some(Reason::OtherScanner)
        );
        let mut dirty = stored("abcdef1234");
        dirty.dirty = Some(true);
        assert_eq!(
            reason(&decide_with(Some(&dirty), Ok(&changes), api_inputs())),
            Some(Reason::StoredDirty)
        );
        let mut degraded = stored("abcdef1234");
        degraded.types_degraded = Some(crate::cloud_storage::TypeDegradation {
            stage: "capture".to_string(),
            detail: "died".to_string(),
        });
        assert_eq!(
            reason(&decide_with(Some(&degraded), Ok(&changes), api_inputs())),
            Some(Reason::StoredIncomplete)
        );
        let mut wrapped = stored("abcdef1234");
        wrapped.type_extraction_status = Some("machinery unwrapping disabled this run".to_string());
        assert_eq!(
            reason(&decide_with(Some(&wrapped), Ok(&changes), api_inputs())),
            Some(Reason::StoredIncomplete)
        );
        // A run that asks no model cannot ask for the config.
        let no_model = ThisRun {
            asks_model: false,
            ..THIS
        };
        let carried = decide(
            Some(&wrapped),
            no_model,
            Some("{\"serviceName\":\"api\"}"),
            Ok(&changes),
            |_| false,
            |_, _| false,
            |_| false,
            |_| DependencyReach::Unread,
            api_inputs,
        );
        assert_eq!(reason(&carried), None);
        // Candidates no model answered are owed to a run that asks one; a
        // run that asks none would owe them too (carrick#2826).
        let mut awaiting = stored("abcdef1234");
        awaiting.boundary = Some(crate::boundary::ServiceBoundary {
            candidates_awaiting_model: Some(2),
            ..Default::default()
        });
        assert_eq!(
            reason(&decide_with(Some(&awaiting), Ok(&changes), api_inputs())),
            Some(Reason::StoredIncomplete)
        );
        let carried = decide(
            Some(&awaiting),
            no_model,
            Some("{\"serviceName\":\"api\"}"),
            Ok(&changes),
            |_| false,
            |_, _| false,
            |_| false,
            |_| DependencyReach::Unread,
            api_inputs,
        );
        assert_eq!(reason(&carried), None);
        // A status beside a config is not this rule's: the config arrived,
        // and analysing again would ask for nothing new.
        let mut no_stub = stored("abcdef1234");
        no_stub.type_extraction_status =
            Some("v2 type capture degraded: no stub package was produced".to_string());
        no_stub.cached_extraction_config = Some(Default::default());
        assert_eq!(
            reason(&decide_with(Some(&no_stub), Ok(&changes), api_inputs())),
            None
        );
        let mut lost = stored("abcdef1234");
        lost.boundary = Some(crate::boundary::ServiceBoundary {
            files_lost: crate::boundary::Counted::new(2, Vec::new()),
            ..Default::default()
        });
        assert_eq!(
            reason(&decide_with(Some(&lost), Ok(&changes), api_inputs())),
            Some(Reason::StoredIncomplete)
        );
        // carrick#2488: a file lost for good is not owed, so a blob whose
        // only losses are final is complete; one owed file beside them is not.
        let mut lost_for_good = stored("abcdef1234");
        lost_for_good.boundary = Some(crate::boundary::ServiceBoundary {
            files_lost: crate::boundary::Counted::new(2, Vec::new()),
            final_losses: Some(crate::boundary::Counted::new(2, Vec::new())),
            ..Default::default()
        });
        assert_eq!(
            reason(&decide_with(
                Some(&lost_for_good),
                Ok(&changes),
                api_inputs()
            )),
            None
        );
        let mut one_owed = stored("abcdef1234");
        one_owed.boundary = Some(crate::boundary::ServiceBoundary {
            files_lost: crate::boundary::Counted::new(2, Vec::new()),
            final_losses: Some(crate::boundary::Counted::new(1, Vec::new())),
            ..Default::default()
        });
        assert_eq!(
            reason(&decide_with(Some(&one_owed), Ok(&changes), api_inputs())),
            Some(Reason::StoredIncomplete)
        );
        let mut reconfigured = stored("abcdef1234");
        reconfigured.config_json = Some("{\"serviceName\":\"api\",\"include\":[\"x\"]}".into());
        assert_eq!(
            reason(&decide_with(
                Some(&reconfigured),
                Ok(&changes),
                api_inputs()
            )),
            Some(Reason::ConfigChanged)
        );
        let blob = stored("abcdef1234");
        assert_eq!(
            reason(&decide_with(Some(&blob), Err("bad object"), api_inputs())),
            Some(Reason::CommitMissing)
        );
    }

    /// A laptop CLI is almost never the release CI scanned with. A run that
    /// asks no model and uploads nothing carries a blob another release wrote
    /// in this cache format, as `current_hosted_blobs` reads one in
    /// (carrick#2840); a run that asks the model still refuses it.
    #[test]
    fn a_run_without_the_model_carries_a_blob_another_release_wrote() {
        let changes: Vec<ChangedPath> = Vec::new();
        let no_model = ThisRun {
            asks_model: false,
            ..THIS
        };
        let decide_no_model = |blob: &CloudRepoData| {
            decide(
                Some(blob),
                no_model,
                Some("{\"serviceName\":\"api\"}"),
                Ok(&changes),
                |_| false,
                |_, _| false,
                |_| false,
                |_| DependencyReach::Unread,
                api_inputs,
            )
        };
        let mut other_release = stored("abcdef1234");
        other_release.scanner_version = Some("0.9.0".to_string());
        assert_eq!(
            decide_no_model(&other_release),
            Scope::Carried {
                commit: "abcdef1234".to_string()
            }
        );
        assert_eq!(
            reason(&decide_with(
                Some(&other_release),
                Ok(&changes),
                api_inputs()
            )),
            Some(Reason::OtherScanner)
        );
        let mut other_cache = stored("abcdef1234");
        other_cache.scanner_version = Some("0.9.0".to_string());
        other_cache.cache_version = Some(6);
        assert_eq!(
            reason(&decide_no_model(&other_cache)),
            Some(Reason::OtherScanner)
        );
    }

    #[test]
    fn inputs_that_cannot_be_read_whole_analyse_the_service() {
        let blob = stored("abcdef1234");
        let changes = changed(&[('M', "tools/other/x.ts")]);
        let unresolved = ServiceInputs {
            unresolved: vec![UnresolvedImport {
                at: "apps/api/src/a.ts imports nowhere".to_string(),
                could_be: CouldBe::Anything,
            }],
            ..api_inputs()
        };
        assert_eq!(
            reason(&decide_with(Some(&blob), Ok(&changes), unresolved)),
            Some(Reason::UnresolvedImport)
        );
        let unfollowed = || ServiceInputs {
            unfollowed_extends: vec![
                "@tsconfig/missing".to_string(),
                "../../tsconfig.base.json".to_string(),
            ],
            ..api_inputs()
        };
        // carrick#2826: only when a changed file is one it could name.
        for path in ["tools/missing.json", "tsconfig.base.json"] {
            assert_eq!(
                reason(&decide_with(
                    Some(&blob),
                    Ok(&changed(&[('A', path)])),
                    unfollowed()
                )),
                Some(Reason::ExtendsUnfollowed),
                "{path}"
            );
        }
        assert_eq!(
            reason(&decide_with(
                Some(&blob),
                Ok(&changed(&[('M', "tools/other/x.ts")])),
                unfollowed()
            )),
            None
        );
    }

    /// carrick#2826: an import nothing resolves keeps a service from being
    /// carried only when a changed path is one it could name.
    #[test]
    fn an_unresolved_import_analyses_only_when_it_could_name_a_change() {
        let blob = stored("abcdef1234");
        let inputs = |could_be: CouldBe| ServiceInputs {
            unresolved: vec![UnresolvedImport {
                at: "apps/api/src/a.ts imports ./shared/log".to_string(),
                could_be,
            }],
            ..api_inputs()
        };
        let elsewhere = changed(&[('M', "tools/other/log.ts")]);
        // Outside the roots, so only this rule can analyse the service.
        let copy = || CouldBe::Path("build/shared/log".to_string());
        assert_eq!(
            reason(&decide_with(Some(&blob), Ok(&elsewhere), inputs(copy()))),
            None,
            "a build-time copy that is not on disk names no changed file"
        );
        for path in ["build/shared/log.ts", "build/shared/log/index.ts"] {
            assert_eq!(
                reason(&decide_with(
                    Some(&blob),
                    Ok(&changed(&[('A', path)])),
                    inputs(copy())
                )),
                Some(Reason::UnresolvedImport),
                "{path} is what the import would name"
            );
        }
        assert!(!copy().names("build/shared/logger.ts"));
        assert_eq!(
            reason(&decide_with(
                Some(&blob),
                Ok(&elsewhere),
                inputs(CouldBe::Named("vite".to_string()))
            )),
            None,
            "a package that is not installed changes through its manifest"
        );
        assert_eq!(
            reason(&decide_with(
                Some(&blob),
                Ok(&changed(&[('M', "tools/other/components/Button.tsx")])),
                inputs(CouldBe::Named("components/Button".to_string()))
            )),
            Some(Reason::UnresolvedImport),
            "a bare specifier a baseUrl could map onto a changed file"
        );
        assert_eq!(
            reason(&decide_with(
                Some(&blob),
                Ok(&changed(&[('A', "tools/gen/client/index.ts")])),
                inputs(CouldBe::Under("tools/gen/client/".to_string()))
            )),
            Some(Reason::UnresolvedImport),
            "a missing alias target that has appeared"
        );
    }

    #[test]
    fn an_include_entry_is_read_up_to_its_first_wildcard() {
        assert_eq!(static_prefix("./types/**/*.d.ts"), PathBuf::from("types"));
        assert_eq!(static_prefix("../../types"), PathBuf::from("../../types"));
        assert_eq!(static_prefix("src/*"), PathBuf::from("src"));
        assert_eq!(static_prefix("**/*"), PathBuf::new());
    }

    #[test]
    fn join_fields_compare_as_sets_and_absent_is_empty() {
        let stored = stored("abcdef1234");
        let mut payload = stored.clone();
        payload.compat_verdicts = Some(Vec::new());
        payload.sdk_edges = Some(Vec::new());
        assert!(!join_fields_changed(&payload, &stored));
        payload.boundary = Some(crate::boundary::ServiceBoundary::default());
        assert!(join_fields_changed(&payload, &stored));
    }

    /// carrick#3104: a carried consumer is rewritten when its stored pair
    /// outcomes' rows differ, and not when only their stamps do.
    #[test]
    fn stored_pair_outcomes_compare_by_their_rows() {
        use crate::cloud_storage::{StoredPairOutcome, StoredPairOutcomes};
        let row = |hash: &str| StoredPairOutcome {
            pair_key: "api/P~web/C".to_string(),
            spec_hash: hash.to_string(),
            judged: true,
            bucket: crate::services::type_sidecar::VerdictBucket::Compatible,
            gate: None,
            diagnostic: None,
            resolved: true,
            unresolved_reason: None,
            notes: Vec::new(),
            consumer_reads: Vec::new(),
            producer_surface_hash: None,
            consumer_surface_hash: None,
            left_to_consumer: false,
        };
        let record = |commit: &str, hash: &str| StoredPairOutcomes {
            version: crate::cloud_storage::PAIR_OUTCOMES_VERSION,
            commit: commit.to_string(),
            scanner_build: None,
            outcomes: vec![row(hash)],
        };
        let mut stored = stored("abcdef1234");
        stored.pair_outcomes = Some(record("abcdef1234", "aa"));
        let mut payload = stored.clone();
        payload.pair_outcomes = Some(record("fedcba4321", "aa"));
        assert!(!join_fields_changed(&payload, &stored), "stamps only");
        payload.pair_outcomes = Some(record("fedcba4321", "bb"));
        assert!(join_fields_changed(&payload, &stored), "a moved spec_hash");
        payload.pair_outcomes = None;
        assert!(join_fields_changed(&payload, &stored), "no record any more");
    }

    #[test]
    fn a_stamped_carried_payload_names_this_run() {
        let mut payload = stored("abcdef1234");
        payload.boundary = Some(crate::boundary::ServiceBoundary {
            commit_hash: "abcdef1234".to_string(),
            files_attempted: 3,
            ..Default::default()
        });
        stamp_carried(&mut payload, "1234567fff", false);
        assert_eq!(payload.commit_hash, "1234567fff");
        assert_eq!(payload.dirty, None);
        let boundary = payload.boundary.unwrap();
        assert_eq!(boundary.commit_hash, "1234567fff");
        assert_eq!(boundary.files_attempted, 0);
    }

    // --- carrick#2228: main's copy judged by each service's inputs ---

    use crate::git_state::tests::{committed_repo, git_in, partial_clone_without_remote};
    use crate::pr_baseline::{MainCopy, main_copy};

    /// Three services: `web` imports a file of `api` by a relative path, and
    /// `worker` imports a workspace package by name.
    fn workspace() -> (tempfile::TempDir, String, Vec<Config>) {
        let (repo, a) = committed_repo(&[
            (
                "package.json",
                r#"{ "name": "mono", "private": true, "workspaces": ["apps/*", "packages/*"] }"#,
            ),
            (
                "packages/shared/package.json",
                r#"{ "name": "@fixture/shared", "main": "src/x.ts" }"#,
            ),
            ("packages/shared/src/x.ts", "export const x = 1;\n"),
            ("apps/api/package.json", r#"{ "name": "api" }"#),
            ("apps/api/src/server.ts", "export const server = 1;\n"),
            (
                "apps/api/src/types.ts",
                "export interface Order { id: string }\n",
            ),
            ("apps/web/package.json", r#"{ "name": "web" }"#),
            (
                "apps/web/src/checkout.ts",
                "import type { Order } from \"../../api/src/types\";\nexport const o: Order | null = null;\n",
            ),
            (
                "apps/worker/package.json",
                r#"{ "name": "worker", "dependencies": { "@fixture/shared": "*" } }"#,
            ),
            (
                "apps/worker/src/job.ts",
                "import { x } from \"@fixture/shared\";\nexport const job = x;\n",
            ),
            ("README.md", "readme\n"),
        ]);
        let services = ["api", "web", "worker"]
            .iter()
            .map(|name| {
                serde_json::from_value(serde_json::json!({
                    "name": name,
                    "directory": format!("apps/{name}"),
                }))
                .unwrap()
            })
            .collect();
        (repo, a, services)
    }

    /// A type named only through `import('./x').T` or a triple-slash
    /// reference is still read by the service, so a change to that file
    /// reaches it.
    #[test]
    fn a_type_position_import_is_an_edge_of_the_closure() {
        let (repo, _) = committed_repo(&[
            ("package.json", r#"{ "name": "mono", "private": true }"#),
            ("apps/api/package.json", r#"{ "name": "api" }"#),
            ("apps/api/src/server.ts", "export const server = 1;\n"),
            (
                "apps/api/src/types.ts",
                "export interface Order { id: string }\n",
            ),
            (
                "apps/api/src/globals.d.ts",
                "interface Extra { n: number }\n",
            ),
            ("apps/web/package.json", r#"{ "name": "web" }"#),
            (
                "apps/web/src/checkout.ts",
                "/// <reference path=\"../../api/src/globals.d.ts\" />\nexport type T = import('../../api/src/types').Order;\n",
            ),
        ]);
        let repo_path = repo.path().to_string_lossy().to_string();
        let top = crate::git_state::toplevel(&repo_path).unwrap();
        let web: Config = serde_json::from_value(serde_json::json!({
            "name": "web",
            "directory": "apps/web",
        }))
        .unwrap();
        let inputs = service_inputs(&repo_path, &top, &web, &ClosureReads::default());
        for file in ["apps/api/src/types.ts", "apps/api/src/globals.d.ts"] {
            assert!(inputs.files.contains(file), "{file} in {:?}", inputs.files);
        }
    }

    /// Services that reach the same file share one read of it: the closure
    /// walk of one tree parses each file once, whichever services reach it,
    /// and each service's inputs are what a walk of its own would give.
    #[test]
    fn services_that_reach_one_file_parse_it_once() {
        let (repo, _, services) = workspace();
        let repo_path = repo.path().to_string_lossy().to_string();
        let top = crate::git_state::toplevel(&repo_path).unwrap();
        let shared = ClosureReads::default();
        let mut parsed_alone = 0;
        for service in &services {
            let alone = ClosureReads::default();
            assert_eq!(
                service_inputs(&repo_path, &top, service, &shared),
                service_inputs(&repo_path, &top, service, &alone),
                "{:?}",
                service.service_name
            );
            parsed_alone += alone.parsed.get();
        }
        // `web` reaches `api`'s types file, which `api` parsed already.
        assert!(
            shared.parsed.get() < parsed_alone,
            "{} parsed shared, {parsed_alone} alone",
            shared.parsed.get()
        );
        let before = shared.parsed.get();
        for service in &services {
            service_inputs(&repo_path, &top, service, &shared);
        }
        assert_eq!(shared.parsed.get(), before, "a second walk parses nothing");
    }

    fn commit_edit(repo: &Path, path: &str, contents: &str) -> String {
        std::fs::write(repo.join(path), contents).unwrap();
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", path]);
        git_in(repo, &["rev-parse", "HEAD"])
    }

    fn copy_at(service: &str, commit: &str) -> CloudRepoData {
        let mut copy = stored(commit);
        copy.service_name = Some(service.to_string());
        copy.scanner_version = Some(env!("CARGO_PKG_VERSION").to_string());
        copy
    }

    fn judged(
        repo: &Path,
        services: &[Config],
        main_self: &[CloudRepoData],
        base: &str,
    ) -> MainCopy {
        let repo_path = repo.to_string_lossy();
        let judge = CopyJudge::new(&repo_path, services, main_self);
        main_copy(
            main_self,
            Some(base),
            |copy, base| judge.differs(copy, base),
            || panic!("this scanner wrote every copy"),
        )
    }

    /// Main moved past services it does not touch: `api` was re-stored at B,
    /// `web` and `worker` stay at A, and nothing they read changed between A
    /// and B. A PR from B that edits `worker` compares with main's copy.
    /// The whole-tree comparison called it stale.
    #[test]
    fn a_copy_stored_before_a_change_it_does_not_read_is_current() {
        let (repo, a, services) = workspace();
        let b = commit_edit(
            repo.path(),
            "apps/api/src/server.ts",
            "export const server = 2;\n",
        );
        let main_self = [
            copy_at("api", &b),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        std::fs::write(
            repo.path().join("apps/worker/src/job.ts"),
            "import { x } from \"@fixture/shared\";\nexport const job = x + 1;\n",
        )
        .unwrap();
        assert_eq!(
            judged(repo.path(), &services, &main_self, &b),
            MainCopy::Current
        );
        // What the whole trees say, for the same copy.
        let whole = main_copy(
            &main_self,
            Some(&b),
            |copy, base| {
                crate::git_state::differs_under(
                    &repo.path().to_string_lossy(),
                    &copy.commit_hash,
                    base,
                )
                .ok()
            },
            || false,
        );
        assert_eq!(whole, MainCopy::Stale);
    }

    /// A change on main to a file `web` imports from `api`'s root leaves
    /// `web`'s copy at A stale.
    #[test]
    fn a_copy_stored_before_a_change_it_reads_is_stale() {
        let (repo, a, services) = workspace();
        let b = commit_edit(
            repo.path(),
            "apps/api/src/types.ts",
            "export interface Order { id: string; total: number }\n",
        );
        let main_self = [
            copy_at("api", &b),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        assert_eq!(
            judged(repo.path(), &services, &main_self, &b),
            MainCopy::Stale
        );
    }

    /// A PR that edits a file outside `worker`'s roots that `worker` reads:
    /// its inputs on this tree may not be the base's, so the whole trees are
    /// compared, as before.
    #[test]
    fn a_pr_editing_an_outside_input_falls_back_to_the_whole_tree() {
        let (repo, a, services) = workspace();
        let b = commit_edit(
            repo.path(),
            "apps/api/src/server.ts",
            "export const server = 2;\n",
        );
        let main_self = [
            copy_at("api", &b),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        std::fs::write(
            repo.path().join("packages/shared/src/x.ts"),
            "export const x = 2;\n",
        )
        .unwrap();
        assert_eq!(
            judged(repo.path(), &services, &main_self, &b),
            MainCopy::Stale
        );
    }

    /// A base this clone does not hold, and a copy whose services all sit at
    /// one commit, get the answer they always got.
    #[test]
    fn an_unknown_base_and_a_one_commit_copy_are_judged_as_before() {
        let (repo, a, services) = workspace();
        let b = commit_edit(repo.path(), "README.md", "changed\n");
        let mixed = [
            copy_at("api", &b),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        assert_eq!(
            judged(repo.path(), &services, &mixed, &"f".repeat(40)),
            MainCopy::Current
        );
        let one_commit = [
            copy_at("api", &a),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        assert_eq!(
            judged(repo.path(), &services, &one_commit, &b),
            MainCopy::Stale,
            "a README change is a whole-tree difference"
        );
    }

    /// A blobless clone holds both commits' trees but not the stored commit's
    /// copy of a file that changed since, and no remote answers. Whether the
    /// trees differ is a question about trees: it is answered offline, and a
    /// copy stored before that change is stale (carrick#2365). Some git
    /// versions read file contents for `git diff --quiet`, fail to fetch them
    /// and leave the copy counted as current. A full clone answers the same.
    #[test]
    fn a_blobless_clone_compares_trees_without_fetching_contents() {
        for filter in [true, false] {
            let (_root, clone, base) = partial_clone_without_remote(filter);
            let head = git_in(&clone, &["rev-parse", "HEAD"]);
            assert_eq!(
                crate::git_state::differs_under(&clone.to_string_lossy(), &base, &head),
                Ok(true),
                "blobless: {filter}"
            );
            assert_eq!(
                judged(&clone, &[], &[copy_at("api", &base)], &head),
                MainCopy::Stale,
                "blobless: {filter}"
            );
        }
    }

    // --- carrick#2565 / carrick#2566: what a carried service ignores ---

    /// The scan's own `ThisRun` for blobs `copy_at` writes at the crate's
    /// version.
    fn this_scanner() -> ThisRun<'static> {
        ThisRun {
            scanner_version: env!("CARGO_PKG_VERSION"),
            cache_version: 7,
            asks_model: true,
        }
    }

    /// Each service stored at `commit` with the config this run reads for it.
    fn stored_at(services: &[Config], commit: &str) -> Vec<CloudRepoData> {
        services
            .iter()
            .map(|service| {
                let mut copy = copy_at(service.service_name.as_deref().unwrap(), commit);
                copy.config_json = serde_json::to_string(service).ok();
                copy
            })
            .collect()
    }

    fn scopes_of(
        repo: &Path,
        services: &[Config],
        blobs: &[CloudRepoData],
        stored_names: &[Option<String>],
    ) -> Vec<Scope> {
        let repo = repo.to_string_lossy();
        plan(
            &repo,
            services,
            &ChangedSince::new(&repo, services, stored_names),
            |service| {
                blobs
                    .iter()
                    .find(|blob| blob.service_name == service.service_name)
            },
            this_scanner(),
        )
    }

    fn names_of(services: &[Config]) -> Vec<Option<String>> {
        services.iter().map(|s| s.service_name.clone()).collect()
    }

    fn carried(scopes: &[Scope]) -> Vec<bool> {
        scopes.iter().map(Scope::is_carried).collect()
    }

    fn reasons(scopes: &[Scope]) -> Vec<Option<Reason>> {
        scopes.iter().map(reason).collect()
    }

    fn write_config(repo: &Path) {
        std::fs::write(repo.join("carrick.json"), "{ \"services\": [] }").unwrap();
    }

    #[test]
    fn an_untracked_config_naming_the_stored_services_is_no_changed_input() {
        let (repo, a, services) = workspace();
        let blobs = stored_at(&services, &a);
        write_config(repo.path());
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(carried(&scopes), [true, true, true], "{scopes:?}");
    }

    #[test]
    fn an_untracked_config_with_another_service_list_stays_an_input() {
        let (repo, a, services) = workspace();
        let blobs = stored_at(&services, &a);
        write_config(repo.path());
        // A service the stored index lacks.
        let mut longer = services.clone();
        longer.push(
            serde_json::from_value(serde_json::json!({
                "name": "extra", "directory": "apps/extra",
            }))
            .unwrap(),
        );
        let scopes = scopes_of(repo.path(), &longer, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [
                Some(Reason::RepoInputChanged),
                Some(Reason::RepoInputChanged),
                Some(Reason::RepoInputChanged),
                Some(Reason::NoStored)
            ]
        );
        // A stored service the config no longer names.
        let mut stored_names = names_of(&services);
        stored_names.push(Some("gone".to_string()));
        let scopes = scopes_of(repo.path(), &services, &blobs, &stored_names);
        assert_eq!(carried(&scopes), [false, false, false]);
    }

    #[test]
    fn an_untracked_config_changing_one_entry_analyses_that_service_alone() {
        let (repo, a, services) = workspace();
        let mut blobs = stored_at(&services, &a);
        write_config(repo.path());
        blobs[0].config_json = Some("{\"serviceName\":\"api\",\"include\":[\"x\"]}".into());
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::ConfigChanged), None, None],
            "{scopes:?}"
        );
    }

    #[test]
    fn a_tracked_config_edit_still_analyses_every_service() {
        let (repo, _, services) = workspace();
        let a = commit_edit(repo.path(), "carrick.json", "{ \"services\": [] }");
        let blobs = stored_at(&services, &a);
        // Committed at the stored commit, then edited in the tree.
        std::fs::write(repo.path().join("carrick.json"), "{ \"services\": [1] }").unwrap();
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::RepoInputChanged); 3],
            "{scopes:?}"
        );
        // And one a commit added since the stored commit.
        let (repo, a, services) = workspace();
        let blobs = stored_at(&services, &a);
        commit_edit(repo.path(), "carrick.json", "{ \"services\": [] }");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(reasons(&scopes), [Some(Reason::RepoInputChanged); 3]);
    }

    fn set_version(repo: &Path, manifest: &str, version: &str) {
        let path = repo.join(manifest);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
        json["version"] = serde_json::json!(version);
        std::fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    }

    fn commit_all(repo: &Path, message: &str) -> String {
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", message]);
        git_in(repo, &["rev-parse", "HEAD"])
    }

    /// The stored index sits at A, where every manifest states a version.
    fn versioned_workspace() -> (tempfile::TempDir, String, Vec<Config>) {
        let (repo, _, services) = workspace();
        for manifest in [
            "package.json",
            "apps/api/package.json",
            "apps/web/package.json",
            "apps/worker/package.json",
            "packages/shared/package.json",
        ] {
            set_version(repo.path(), manifest, "1.0.0");
        }
        let a = commit_all(repo.path(), "versions");
        (repo, a, services)
    }

    #[test]
    fn a_version_only_manifest_change_carries_every_service() {
        let (repo, a, services) = versioned_workspace();
        let blobs = stored_at(&services, &a);
        set_version(repo.path(), "package.json", "1.0.1");
        set_version(repo.path(), "apps/api/package.json", "1.0.1");
        commit_all(repo.path(), "bump");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(carried(&scopes), [true, true, true], "{scopes:?}");
    }

    #[test]
    fn a_manifest_change_beyond_the_version_analyses_every_service() {
        let (repo, a, services) = versioned_workspace();
        let blobs = stored_at(&services, &a);
        set_version(repo.path(), "package.json", "1.0.1");
        let path = repo.path().join("package.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["dependencies"] = serde_json::json!({ "left-pad": "1.0.0" });
        std::fs::write(&path, json.to_string()).unwrap();
        commit_all(repo.path(), "dependency");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(reasons(&scopes), [Some(Reason::RepoInputChanged); 3]);
    }

    #[test]
    fn a_version_bump_of_an_imported_package_analyses_its_importers_only() {
        let (repo, a, services) = versioned_workspace();
        let blobs = stored_at(&services, &a);
        set_version(repo.path(), "packages/shared/package.json", "1.0.1");
        commit_all(repo.path(), "bump shared");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        // api, web, worker: only worker imports `@fixture/shared`.
        assert_eq!(
            reasons(&scopes),
            [None, None, Some(Reason::InputsChanged)],
            "{scopes:?}"
        );
    }

    #[test]
    fn a_manifest_added_or_unreadable_stays_a_repo_input() {
        let (repo, a, services) = versioned_workspace();
        let blobs = stored_at(&services, &a);
        // A stored commit the clone does not hold.
        let missing = stored_at(&services, &"f".repeat(40));
        let scopes = scopes_of(repo.path(), &services, &missing, &names_of(&services));
        assert_eq!(reasons(&scopes), [Some(Reason::CommitMissing); 3]);
        // A manifest added since the stored commit.
        std::fs::create_dir_all(repo.path().join("libs/x")).unwrap();
        std::fs::write(
            repo.path().join("libs/x/package.json"),
            "{\"version\":\"1\"}",
        )
        .unwrap();
        commit_all(repo.path(), "added");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(reasons(&scopes), [Some(Reason::RepoInputChanged); 3]);
    }

    #[test]
    fn a_manifest_the_head_copy_cannot_parse_stays_a_repo_input() {
        let (repo, a, services) = versioned_workspace();
        let blobs = stored_at(&services, &a);
        std::fs::write(repo.path().join("package.json"), "{ not json").unwrap();
        commit_all(repo.path(), "broken");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(reasons(&scopes), [Some(Reason::RepoInputChanged); 3]);
    }

    #[test]
    fn only_a_top_level_version_value_is_ignored() {
        let base = r#"{"name":"a","version":"1.0.0","dependencies":{"x":"1"}}"#;
        assert!(only_version_differs(
            base,
            r#"{"version":"1.0.1","dependencies":{"x":"1"},"name":"a"}"#
        ));
        for head in [
            base,
            r#"{"name":"a","version":"1.0.1","dependencies":{"x":"2"}}"#,
            r#"{"name":"a","dependencies":{"x":"1"}}"#,
            r#"{"name":"b","version":"1.0.1","dependencies":{"x":"1"}}"#,
            "{ broken",
            "[]",
        ] {
            assert!(!only_version_differs(base, head), "{head}");
        }
        assert!(!only_version_differs("{ broken", base));
    }

    /// No run that uploads carries under the version-only rule today. If one
    /// does, the carried write must refresh the stored manifest projection
    /// from HEAD first (carrick#2566); until then this pins that
    /// `refreshed_carried` leaves the projection as stored, so the refresh is
    /// built before that run exists.
    #[test]
    fn a_carried_write_keeps_the_stored_manifest_projection() {
        let mut stored = stored("abcdef1234");
        stored.package_json = Some("{\"version\":\"1.0.0\"}".to_string());
        stored.package_json_hash = Some("hash-at-stored-commit".to_string());
        let mut payload = stored.clone();
        payload.compat_verdicts = Some(vec![
            serde_json::from_value(serde_json::json!({
                "producer_repo": "p", "producer_key": "http|GET|/x",
                "consumer_repo": "c", "consumer_key": "http|GET|/x", "scanner_version": "1.0.0",
            }))
            .unwrap(),
        ]);
        let written = refreshed_carried(payload, &stored, &[], "1234567fff", false).unwrap();
        assert_eq!(written.package_json, stored.package_json);
        assert_eq!(written.package_json_hash, stored.package_json_hash);
    }

    /// The stored index sits at A, where a root `.gitignore` exists.
    fn ignoring_workspace() -> (tempfile::TempDir, String, Vec<Config>) {
        let (repo, _, services) = workspace();
        for dir in ["apps/web", "packages/shared"] {
            std::fs::write(repo.path().join(dir).join(".gitignore"), "out/\n").unwrap();
        }
        std::fs::write(
            repo.path().join(".gitignore"),
            "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n",
        )
        .unwrap();
        let a = commit_all(repo.path(), "ignores");
        (repo, a, services)
    }

    fn write_untracked(repo: &Path, path: &str) {
        let full = repo.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, "export const out = 1;\n").unwrap();
    }

    /// Scopes of `api`, `web`, `worker` after `edit` is committed at B.
    fn after_ignore_edit(edit: impl FnOnce(&Path), untracked: &[&str]) -> Vec<Option<Reason>> {
        let (repo, a, services) = ignoring_workspace();
        let blobs = stored_at(&services, &a);
        edit(repo.path());
        commit_all(repo.path(), "edit");
        for path in untracked {
            write_untracked(repo.path(), path);
        }
        reasons(&scopes_of(
            repo.path(),
            &services,
            &blobs,
            &names_of(&services),
        ))
    }

    fn edit_file(path: &'static str, text: &'static str) -> impl FnOnce(&Path) {
        move |repo| std::fs::write(repo.join(path), text).unwrap()
    }

    #[test]
    fn a_gitignore_edit_with_no_untracked_file_carries_every_service() {
        assert_eq!(
            after_ignore_edit(
                edit_file(
                    ".gitignore",
                    "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n# edited\n"
                ),
                &[]
            ),
            [None, None, None]
        );
    }

    #[test]
    fn a_gitignore_edit_analyses_the_service_that_reads_an_untracked_file() {
        let reasons = after_ignore_edit(
            edit_file(
                ".gitignore",
                "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n# edited\n",
            ),
            &["apps/api/gen/out.ts"],
        );
        assert_eq!(reasons, [Some(Reason::InputsChanged), None, None]);
    }

    #[test]
    fn untracked_files_the_walk_cannot_enter_move_nothing() {
        let reasons = after_ignore_edit(
            edit_file(
                ".gitignore",
                "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n# edited\n",
            ),
            &[
                "apps/web/node_modules/pkg/index.ts",
                "apps/api/dist/out.ts",
                "apps/api/.cache/out.ts",
            ],
        );
        assert_eq!(reasons, [None, None, None]);
    }

    #[test]
    fn a_gitignore_edit_reaches_only_the_folder_it_governs() {
        // web's own `.gitignore` is edited; the untracked file sits in api.
        let reasons = after_ignore_edit(
            edit_file("apps/web/.gitignore", "out/\n# edited\n"),
            &["apps/api/gen/out.ts"],
        );
        assert_eq!(reasons, [None, None, None]);
    }

    #[test]
    fn a_workspace_package_gitignore_reaches_the_services_that_enter_it() {
        let reasons = after_ignore_edit(
            edit_file("packages/shared/.gitignore", "out/\n# edited\n"),
            &[],
        );
        assert_eq!(reasons, [None, None, Some(Reason::InputsChanged)]);
    }

    #[test]
    fn a_gitignore_edit_and_a_source_edit_each_count_for_their_own_service() {
        let reasons = after_ignore_edit(
            |repo| {
                edit_file(
                    ".gitignore",
                    "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n# edited\n",
                )(repo);
                edit_file(
                    "apps/worker/src/job.ts",
                    "import { x } from \"@fixture/shared\";\nexport const job = x + 1;\n",
                )(repo);
            },
            &[],
        );
        assert_eq!(reasons, [None, None, Some(Reason::InputsChanged)]);
    }

    #[test]
    fn a_gitignore_edit_counts_for_every_service_when_git_cannot_list_untracked_files() {
        assert!(gitignore_moves(
            ".gitignore",
            &api_inputs(),
            None,
            |_, _, _| false
        ));
        let none: [String; 0] = [];
        assert!(!gitignore_moves(
            ".gitignore",
            &api_inputs(),
            Some(&none),
            |_, _, _| true
        ));
    }

    /// Both sides of the main-copy verdict are commits, so only a workspace
    /// package's own `.gitignore` can move what a service reads.
    #[test]
    fn a_copy_stored_before_a_gitignore_edit_is_current() {
        let (repo, a, services) = ignoring_workspace();
        let b = commit_edit(repo.path(), ".gitignore", "coverage/\n# edited\n");
        let main_self = [
            copy_at("api", &b),
            copy_at("web", &a),
            copy_at("worker", &a),
        ];
        assert_eq!(
            judged(repo.path(), &services, &main_self, &b),
            MainCopy::Current
        );
        let c = commit_edit(
            repo.path(),
            "packages/shared/.gitignore",
            "out/\n# edited\n",
        );
        let main_self = [
            copy_at("api", &c),
            copy_at("web", &b),
            copy_at("worker", &b),
        ];
        assert_eq!(
            judged(repo.path(), &services, &main_self, &c),
            MainCopy::Stale
        );
    }
    // --- carrick#3114: files no pass reads, and test edits that keep their imports ---

    const DOC_FILES: [(&str, &str); 9] = [
        ("apps/api/README.md", "api\n"),
        ("apps/api/docs/flow.svg", "<svg/>\n"),
        ("packages/shared/CHANGELOG.md", "# changes\n"),
        (
            "apps/api/src/__tests__/server.test.ts",
            "import { server } from \"../server\";\ntest(\"a\", () => server);\n",
        ),
        (
            "apps/api/test/setup.ts",
            "declare global { var flag: boolean }\nexport {};\n",
        ),
        ("apps/api/test/globals.ts", "var shared = 1;\n"),
        ("apps/api/Dockerfile", "FROM node\n"),
        ("apps/api/src/data.json", "{ \"a\": 1 }\n"),
        ("apps/api/src/fixtures/seed.ts", "export const seed = 1;\n"),
    ];

    /// The stored index sits at A, which holds the files above; `server.ts`
    /// imports the seed fixture when `seeded`.
    fn documented_workspace(seeded: bool) -> (tempfile::TempDir, String, Vec<Config>) {
        let (repo, _, services) = workspace();
        for (path, text) in DOC_FILES {
            write_file(repo.path(), path, text);
        }
        if seeded {
            write_file(
                repo.path(),
                "apps/api/src/server.ts",
                "import { seed } from \"./fixtures/seed\";\nexport const server = seed;\n",
            );
        }
        let a = commit_all(repo.path(), "documented");
        (repo, a, services)
    }

    fn write_file(repo: &Path, path: &str, text: &str) {
        let full = repo.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    /// Scopes of `api`, `web`, `worker` after `edit` is committed at B.
    fn after_edit(seeded: bool, edit: impl FnOnce(&Path)) -> Vec<Option<Reason>> {
        let (repo, a, services) = documented_workspace(seeded);
        let blobs = stored_at(&services, &a);
        edit(repo.path());
        commit_all(repo.path(), "edit");
        reasons(&scopes_of(
            repo.path(),
            &services,
            &blobs,
            &names_of(&services),
        ))
    }

    const API_ONLY: [Option<Reason>; 3] = [Some(Reason::InputsChanged), None, None];

    #[test]
    fn docs_and_images_under_a_root_or_a_package_move_nothing() {
        assert_eq!(
            after_edit(false, |repo| {
                write_file(repo, "apps/api/README.md", "api, edited\n");
                write_file(repo, "apps/api/docs/flow.svg", "<svg id=\"b\"/>\n");
            }),
            [None, None, None]
        );
        assert_eq!(
            after_edit(false, |repo| write_file(
                repo,
                "packages/shared/CHANGELOG.md",
                "# changes\n- more\n"
            )),
            [None, None, None]
        );
    }

    #[test]
    fn a_test_edit_that_keeps_its_imports_moves_nothing() {
        assert_eq!(
            after_edit(false, |repo| write_file(
                repo,
                "apps/api/src/__tests__/server.test.ts",
                "import { server } from \"../server\";\ntest(\"b\", () => expect(server).toBe(1));\n",
            )),
            [None, None, None]
        );
    }

    #[test]
    fn a_test_edit_that_changes_its_imports_or_its_globals_analyses_the_service() {
        for (path, text) in [
            (
                "apps/api/src/__tests__/server.test.ts",
                "import { server } from \"../server\";\nimport \"../types\";\ntest(\"a\", () => server);\n",
            ),
            ("apps/api/src/__tests__/b.test.ts", "export const b = 1;\n"),
            (
                "apps/api/test/setup.ts",
                "declare global { var flag: boolean; var other: number }\nexport {};\n",
            ),
            ("apps/api/test/globals.ts", "var shared = 2;\n"),
        ] {
            assert_eq!(
                after_edit(false, |repo| write_file(repo, path, text)),
                API_ONLY,
                "{path}"
            );
        }
    }

    #[test]
    fn a_deleted_test_file_analyses_the_service() {
        assert_eq!(
            after_edit(false, |repo| std::fs::remove_file(
                repo.join("apps/api/src/__tests__/server.test.ts")
            )
            .unwrap()),
            API_ONLY
        );
    }

    #[test]
    fn config_and_extensionless_files_under_a_root_still_count() {
        for (path, text) in [
            ("apps/api/Dockerfile", "FROM node:22\n"),
            ("apps/api/src/data.json", "{ \"a\": 2 }\n"),
        ] {
            assert_eq!(
                after_edit(false, |repo| write_file(repo, path, text)),
                API_ONLY,
                "{path}"
            );
        }
    }

    #[test]
    fn a_test_path_file_that_production_code_imports_is_analysed_like_any_source() {
        assert_eq!(
            after_edit(true, |repo| write_file(
                repo,
                "apps/api/src/fixtures/seed.ts",
                "export const seed = 2;\n"
            )),
            API_ONLY
        );
        // Unimported, the same edit is a test edit that keeps its imports.
        assert_eq!(
            after_edit(false, |repo| write_file(
                repo,
                "apps/api/src/fixtures/seed.ts",
                "export const seed = 2;\n"
            )),
            [None, None, None]
        );
    }

    #[test]
    fn a_test_edit_whose_main_copy_cannot_be_read_counts() {
        let reads = ClosureReads::default();
        let text = Some("import { a } from \"./a\";\nexport {};\n".to_string());
        let path = "apps/api/src/__tests__/x.test.ts";
        assert!(test_edit_keeps_imports(
            &reads,
            path,
            text.clone(),
            text.clone()
        ));
        assert!(!test_edit_keeps_imports(&reads, path, None, text.clone()));
        assert!(!test_edit_keeps_imports(&reads, path, text.clone(), None));
        // A CommonJS test is a module only in JavaScript.
        let cjs = Some("const a = require(\"./a\");\n".to_string());
        assert!(test_edit_keeps_imports(
            &reads,
            "t/x.test.js",
            cjs.clone(),
            cjs.clone()
        ));
        assert!(!test_edit_keeps_imports(
            &reads,
            "t/x.test.ts",
            cjs.clone(),
            cjs
        ));
    }

    #[test]
    fn a_copy_stored_before_a_doc_edit_is_current() {
        let (repo, a, services) = documented_workspace(false);
        let b = commit_edit(repo.path(), "apps/api/README.md", "api, edited\n");
        // `api`'s own copy sits at A, before the README edit.
        let main_self = [
            copy_at("api", &a),
            copy_at("web", &b),
            copy_at("worker", &b),
        ];
        assert_eq!(
            judged(repo.path(), &services, &main_self, &b),
            MainCopy::Current
        );
        let c = commit_edit(repo.path(), "apps/api/src/data.json", "{ \"a\": 3 }\n");
        let main_self = [
            copy_at("api", &c),
            copy_at("web", &b),
            copy_at("worker", &b),
        ];
        // `web` imports a file of `api`, not its data file: current. A copy
        // of `api` itself stored before the data edit is stale.
        let stale = [
            copy_at("api", &b),
            copy_at("web", &c),
            copy_at("worker", &c),
        ];
        assert_eq!(
            judged(repo.path(), &services, &main_self, &c),
            MainCopy::Current
        );
        assert_eq!(judged(repo.path(), &services, &stale, &c), MainCopy::Stale);
    }

    // --- carrick#3115: a dependency-only change reaches the services that
    // load a moved package ---

    use std::collections::BTreeMap;

    #[derive(Clone, Copy)]
    enum Manager {
        Npm,
        Pnpm,
    }

    /// Each manifest: its directory, its name, the map it declares in and
    /// what it declares. `api` loads `lib-a` (which depends on `lib-c`),
    /// `@fixture/shared` loads `lib-b`, `worker` loads `@fixture/shared`.
    /// The root declares `lib-d`, which nothing loads, `lib-e`, which a test
    /// of `web` loads, `lib-f`, which `web`'s tsconfig names, and
    /// `@types/node`.
    const LAYOUT: [(&str, &str, &str, &[&str]); 5] = [
        (
            "",
            "mono",
            "devDependencies",
            &["@types/node", "lib-d", "lib-e", "lib-f"],
        ),
        ("apps/api", "api", "dependencies", &["lib-a"]),
        ("apps/web", "web", "dependencies", &[]),
        (
            "apps/worker",
            "worker",
            "dependencies",
            &["@fixture/shared"],
        ),
        (
            "packages/shared",
            "@fixture/shared",
            "dependencies",
            &["lib-b"],
        ),
    ];

    /// Every external package at its first version.
    fn first_versions() -> BTreeMap<String, String> {
        [
            "@types/node",
            "lib-a",
            "lib-b",
            "lib-c",
            "lib-d",
            "lib-e",
            "lib-f",
        ]
        .iter()
        .map(|name| (name.to_string(), "1.0.0".to_string()))
        .collect()
    }

    fn workspace_spec(manager: Manager) -> &'static str {
        match manager {
            Manager::Npm => "*",
            Manager::Pnpm => "workspace:*",
        }
    }

    /// Every manifest, declaring each external at `declared`'s spec, and the
    /// lockfile, installing each at `installed`'s version.
    fn write_dependencies(
        repo: &Path,
        manager: Manager,
        declared: &BTreeMap<String, String>,
        installed: &BTreeMap<String, String>,
    ) {
        let spec = |name: &str| match name {
            "@fixture/shared" => workspace_spec(manager).to_string(),
            name => declared[name].clone(),
        };
        let declarations = |deps: &[&str]| -> serde_json::Value {
            deps.iter()
                .map(|dep| (dep.to_string(), serde_json::Value::from(spec(dep))))
                .collect::<serde_json::Map<_, _>>()
                .into()
        };
        for (dir, name, section, deps) in LAYOUT {
            let mut manifest = serde_json::json!({ "name": name });
            if dir.is_empty() {
                manifest["private"] = serde_json::json!(true);
                if let Manager::Npm = manager {
                    manifest["workspaces"] = serde_json::json!(["apps/*", "packages/*"]);
                }
            }
            if name == "@fixture/shared" {
                manifest["main"] = serde_json::json!("src/x.ts");
            }
            if !deps.is_empty() {
                manifest[section] = declarations(deps);
            }
            let path = Path::new(dir).join("package.json");
            std::fs::write(
                repo.join(path),
                serde_json::to_string_pretty(&manifest).unwrap(),
            )
            .unwrap();
        }
        let depends_on = |package: &str| -> Vec<&str> {
            if package == "lib-a" {
                vec!["lib-c"]
            } else {
                vec![]
            }
        };
        match manager {
            Manager::Npm => {
                let mut packages = serde_json::Map::new();
                for (dir, name, section, deps) in LAYOUT {
                    let mut entry = serde_json::json!({ "name": name });
                    if !deps.is_empty() {
                        entry[section] = declarations(deps);
                    }
                    if !dir.is_empty() {
                        packages.insert(
                            format!("node_modules/{name}"),
                            serde_json::json!({ "resolved": dir, "link": true }),
                        );
                    }
                    packages.insert(dir.to_string(), entry);
                }
                for (package, version) in installed {
                    let mut entry = serde_json::json!({
                        "version": version,
                        "resolved": format!("https://registry.npmjs.org/{package}/-/{package}-{version}.tgz"),
                        "integrity": format!("sha512-{package}-{version}"),
                    });
                    for dep in depends_on(package) {
                        entry["dependencies"][dep] = serde_json::json!("^1.0.0");
                    }
                    packages.insert(format!("node_modules/{package}"), entry);
                }
                let lock = serde_json::json!({
                    "name": "mono",
                    "lockfileVersion": 3,
                    "requires": true,
                    "packages": packages,
                });
                std::fs::write(
                    repo.join("package-lock.json"),
                    serde_json::to_string_pretty(&lock).unwrap(),
                )
                .unwrap();
            }
            Manager::Pnpm => {
                std::fs::write(
                    repo.join("pnpm-workspace.yaml"),
                    "packages:\n  - 'apps/*'\n  - 'packages/*'\n",
                )
                .unwrap();
                let mut importers = serde_json::Map::new();
                for (dir, _, section, deps) in LAYOUT {
                    let mut entry = serde_json::json!({});
                    for dep in deps {
                        let version = match *dep {
                            "@fixture/shared" => "link:../../packages/shared".to_string(),
                            dep => installed[dep].clone(),
                        };
                        entry[section][dep] =
                            serde_json::json!({ "specifier": spec(dep), "version": version });
                    }
                    let key = if dir.is_empty() { "." } else { dir };
                    importers.insert(key.to_string(), entry);
                }
                let mut packages = serde_json::Map::new();
                let mut snapshots = serde_json::Map::new();
                for (package, version) in installed {
                    let key = format!("{package}@{version}");
                    packages.insert(
                        key.clone(),
                        serde_json::json!({ "resolution": { "integrity": format!("sha512-{key}") } }),
                    );
                    let mut snapshot = serde_json::json!({});
                    for dep in depends_on(package) {
                        snapshot["dependencies"][dep] = serde_json::json!(installed[dep]);
                    }
                    snapshots.insert(key, snapshot);
                }
                let lock = serde_json::json!({
                    "lockfileVersion": "9.0",
                    "settings": { "autoInstallPeers": true, "excludeLinksFromLockfile": false },
                    "importers": importers,
                    "packages": packages,
                    "snapshots": snapshots,
                });
                std::fs::write(
                    repo.join("pnpm-lock.yaml"),
                    serde_yaml_ng::to_string(&lock).unwrap(),
                )
                .unwrap();
            }
        }
    }

    /// [`workspace`] with the dependencies of [`LAYOUT`], each source file
    /// importing what its manifest declares, a test of `web` importing
    /// `lib-e` and `web`'s tsconfig naming `lib-f`. Every service is stored
    /// at the returned commit.
    fn dependency_workspace(manager: Manager) -> (tempfile::TempDir, String, Vec<Config>) {
        let (repo, _, services) = workspace();
        let versions = first_versions();
        write_dependencies(repo.path(), manager, &versions, &versions);
        for (path, contents) in [
            (
                "apps/api/src/server.ts",
                "import { a } from \"lib-a\";\nexport const server = a;\n",
            ),
            (
                "packages/shared/src/x.ts",
                "import { b } from \"lib-b\";\nexport const x = b;\n",
            ),
            (
                "apps/web/src/checkout.test.ts",
                "import { e } from \"lib-e\";\nexport const t = e;\n",
            ),
            (
                "apps/web/tsconfig.json",
                r#"{ "compilerOptions": { "types": ["lib-f"] } }"#,
            ),
        ] {
            std::fs::write(repo.path().join(path), contents).unwrap();
        }
        let a = commit_all(repo.path(), "dependencies");
        (repo, a, services)
    }

    /// `versions` with `package` at `version`.
    fn moved(
        versions: &BTreeMap<String, String>,
        package: &str,
        version: &str,
    ) -> BTreeMap<String, String> {
        let mut moved = versions.clone();
        moved.insert(package.to_string(), version.to_string());
        moved
    }

    /// The scopes of every service after the commit `declared` and
    /// `installed` write, the services stored at the fixture's commit.
    fn scopes_after(
        manager: Manager,
        declared: &BTreeMap<String, String>,
        installed: &BTreeMap<String, String>,
    ) -> Vec<Scope> {
        let (repo, a, services) = dependency_workspace(manager);
        let blobs = stored_at(&services, &a);
        write_dependencies(repo.path(), manager, declared, installed);
        commit_all(repo.path(), "move");
        scopes_of(repo.path(), &services, &blobs, &names_of(&services))
    }

    /// The fixture's services, in order: api, web, worker.
    #[test]
    fn a_bump_of_a_package_one_service_loads_analyses_that_service_alone() {
        let bumped = moved(&first_versions(), "lib-b", "1.1.0");
        for manager in [Manager::Npm, Manager::Pnpm] {
            let scopes = scopes_after(manager, &bumped, &bumped);
            assert_eq!(
                reasons(&scopes),
                [None, None, Some(Reason::InputsChanged)],
                "{scopes:?}"
            );
        }
    }

    /// `lib-c` moved in the lockfile alone: no manifest names it, and `api`
    /// loads it through `lib-a`.
    #[test]
    fn a_lockfile_only_move_reaches_the_services_that_load_it_through_another_package() {
        let versions = first_versions();
        let scopes = scopes_after(Manager::Npm, &versions, &moved(&versions, "lib-c", "1.0.1"));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::InputsChanged), None, None],
            "{scopes:?}"
        );
        let Scope::Analysed { detail, .. } = &scopes[0] else {
            unreachable!()
        };
        assert_eq!(detail, "package-lock.json: lib-c");
    }

    /// [`scopes_after`] with `edit` also written before the commit.
    fn scopes_after_editing(
        manager: Manager,
        declared: &BTreeMap<String, String>,
        installed: &BTreeMap<String, String>,
        edit: impl FnOnce(&Path),
    ) -> Vec<Scope> {
        let (repo, a, services) = dependency_workspace(manager);
        let blobs = stored_at(&services, &a);
        write_dependencies(repo.path(), manager, declared, installed);
        edit(repo.path());
        commit_all(repo.path(), "move");
        scopes_of(repo.path(), &services, &blobs, &names_of(&services))
    }

    fn edit_api_source(repo: &Path) {
        std::fs::write(
            repo.join("apps/api/src/server.ts"),
            "import { a } from \"lib-a\";\nexport const server = a + 1;\n",
        )
        .unwrap();
    }

    fn ruled_out_dependencies(scope: &Scope) -> bool {
        match scope {
            Scope::Analysed { ruled_out, .. } => ruled_out.dependencies,
            Scope::Carried { .. } => panic!("carried: {scope:?}"),
        }
    }

    /// carrick#3126: the plan records that a dependency-only move touched
    /// nothing the service loads, so type reach need not judge it again.
    #[test]
    fn a_bump_a_service_does_not_load_is_ruled_out_for_that_service_alone() {
        let bumped = moved(&first_versions(), "lib-b", "1.1.0");
        for manager in [Manager::Npm, Manager::Pnpm] {
            let scopes = scopes_after_editing(manager, &bumped, &bumped, edit_api_source);
            assert_eq!(
                reasons(&scopes),
                [
                    Some(Reason::InputsChanged),
                    None,
                    Some(Reason::InputsChanged)
                ],
                "{scopes:?}"
            );
            assert!(ruled_out_dependencies(&scopes[0]), "api: {scopes:?}");
            assert!(!ruled_out_dependencies(&scopes[2]), "worker: {scopes:?}");
        }
    }

    #[test]
    fn a_bump_of_the_compilers_own_types_is_ruled_out_for_no_service() {
        let bumped = moved(&first_versions(), "@types/node", "20.1.0");
        let scopes = scopes_after_editing(Manager::Npm, &bumped, &bumped, edit_api_source);
        assert!(!ruled_out_dependencies(&scopes[0]), "{scopes:?}");
    }

    fn ignored_by(scopes: &[Scope], index: usize) -> BTreeSet<String> {
        match &scopes[index] {
            Scope::Analysed { ruled_out, .. } => ruled_out.gitignores.clone(),
            Scope::Carried { .. } => panic!("carried: {scopes:?}"),
        }
    }

    fn edit_ignore_and_api(repo: &Path) {
        edit_file(
            ".gitignore",
            "coverage/\ngen/\nnode_modules/\ndist/\n.cache/\n# edited\n",
        )(repo);
        edit_api_source(repo);
    }

    #[test]
    fn a_gitignore_edit_that_moves_no_file_a_service_reads_is_ruled_out() {
        let (repo, a, services) = ignoring_workspace();
        let blobs = stored_at(&services, &a);
        edit_ignore_and_api(repo.path());
        commit_all(repo.path(), "edit");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            ignored_by(&scopes, 0),
            BTreeSet::from([".gitignore".to_string()]),
            "{scopes:?}"
        );
    }

    #[test]
    fn a_gitignore_edit_that_moves_an_untracked_file_the_service_reads_is_not_ruled_out() {
        let (repo, a, services) = ignoring_workspace();
        let blobs = stored_at(&services, &a);
        edit_ignore_and_api(repo.path());
        commit_all(repo.path(), "edit");
        write_untracked(repo.path(), "apps/api/gen/out.ts");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert!(ignored_by(&scopes, 0).is_empty(), "{scopes:?}");
    }

    #[test]
    fn a_bump_of_a_package_nothing_loads_carries_every_service() {
        let bumped = moved(&first_versions(), "lib-d", "1.1.0");
        let scopes = scopes_after(Manager::Npm, &bumped, &bumped);
        assert_eq!(carried(&scopes), [true, true, true], "{scopes:?}");
    }

    #[test]
    fn a_move_of_the_compilers_own_types_analyses_every_service() {
        let bumped = moved(&first_versions(), "@types/node", "20.1.0");
        let scopes = scopes_after(Manager::Npm, &bumped, &bumped);
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::InputsChanged); 3],
            "{scopes:?}"
        );
    }

    /// The root manifest alone declares `lib-e`; a test of `web` loads it.
    #[test]
    fn a_package_a_test_loads_is_loaded_by_its_service() {
        let bumped = moved(&first_versions(), "lib-e", "1.1.0");
        let scopes = scopes_after(Manager::Npm, &bumped, &bumped);
        assert_eq!(
            reasons(&scopes),
            [None, Some(Reason::InputsChanged), None],
            "{scopes:?}"
        );
    }

    #[test]
    fn a_package_the_tsconfig_names_is_loaded_by_its_service() {
        let bumped = moved(&first_versions(), "lib-f", "1.1.0");
        let scopes = scopes_after(Manager::Npm, &bumped, &bumped);
        assert_eq!(
            reasons(&scopes),
            [None, Some(Reason::InputsChanged), None],
            "{scopes:?}"
        );
    }

    /// The install is not frozen to a lockfile that did not move with its
    /// manifest.
    #[test]
    fn a_manifest_move_without_its_lockfile_analyses_every_service() {
        let (repo, a, services) = dependency_workspace(Manager::Npm);
        let blobs = stored_at(&services, &a);
        let bumped = moved(&first_versions(), "lib-b", "1.1.0");
        write_dependencies(repo.path(), Manager::Npm, &bumped, &bumped);
        git_in(
            repo.path(),
            &["checkout", "HEAD", "--", "package-lock.json"],
        );
        commit_all(repo.path(), "move");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::RepoInputChanged); 3],
            "{scopes:?}"
        );
    }

    #[test]
    fn a_moved_workspace_package_analyses_every_service() {
        let (repo, a, services) = dependency_workspace(Manager::Npm);
        let blobs = stored_at(&services, &a);
        for path in ["apps/worker/package.json", "package-lock.json"] {
            let path = repo.path().join(path);
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                &path,
                text.replace(
                    "\"@fixture/shared\": \"*\"",
                    "\"@fixture/shared\": \"^1.0.0\"",
                ),
            )
            .unwrap();
        }
        commit_all(repo.path(), "move");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::RepoInputChanged); 3],
            "{scopes:?}"
        );
    }

    #[test]
    fn a_lockfile_this_rule_does_not_read_analyses_every_service() {
        let (repo, _, services) = dependency_workspace(Manager::Npm);
        std::fs::create_dir_all(repo.path().join("tools")).unwrap();
        std::fs::write(repo.path().join("tools/yarn.lock"), "# yarn lockfile v1\n").unwrap();
        let a = commit_all(repo.path(), "yarn");
        let blobs = stored_at(&services, &a);
        std::fs::write(
            repo.path().join("tools/yarn.lock"),
            "# yarn lockfile v1\n\nlib-x@1.0.0:\n  version \"1.0.0\"\n",
        )
        .unwrap();
        commit_all(repo.path(), "move");
        let scopes = scopes_of(repo.path(), &services, &blobs, &names_of(&services));
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::RepoInputChanged); 3],
            "{scopes:?}"
        );
    }

    /// A blobless clone with no remote holds the commit's tree but not its
    /// copy of the lockfile, so nothing says what moved.
    #[test]
    fn a_stored_lockfile_the_clone_cannot_read_analyses_every_service() {
        let (server, a, services) = dependency_workspace(Manager::Npm);
        let bumped = moved(&first_versions(), "lib-d", "1.1.0");
        write_dependencies(server.path(), Manager::Npm, &bumped, &bumped);
        commit_all(server.path(), "move");
        let clone = crate::git_state::tests::clone_without_remote(server.path(), &a, true);
        let blobs = stored_at(&services, &a);
        let scopes = scopes_of(
            &clone.path().join("clone"),
            &services,
            &blobs,
            &names_of(&services),
        );
        assert_eq!(
            reasons(&scopes),
            [Some(Reason::RepoInputChanged); 3],
            "{scopes:?}"
        );
    }

    /// Main's copy of `api` sits at A, before a bump of `lib-b` that `api`
    /// does not load: it is still main's at the base. `worker` loads it.
    #[test]
    fn a_copy_stored_before_a_dependency_move_it_does_not_load_is_current() {
        let (repo, a, services) = dependency_workspace(Manager::Npm);
        let bumped = moved(&first_versions(), "lib-b", "1.1.0");
        write_dependencies(repo.path(), Manager::Npm, &bumped, &bumped);
        let b = commit_all(repo.path(), "move");
        let main_self = [
            copy_at("api", &a),
            copy_at("web", &b),
            copy_at("worker", &b),
        ];
        let repo_path = repo.path().to_string_lossy();
        let judge = CopyJudge::new(&repo_path, &services, &main_self);
        assert_eq!(judge.differs(&main_self[0], &b), Some(false));
        assert_eq!(judge.differs(&copy_at("worker", &a), &b), Some(true));
    }
}
