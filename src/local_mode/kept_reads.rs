//! What the edit probe reads, kept on disk so it does not read every blob
//! (carrick#4034).
//!
//! A probe (`check --recheck --no-rescan`) needs two things from the index's
//! blobs: which functions each file of its repo already declares, and the
//! ranker that looks up the functions an edit added. Reading the blobs for
//! them costs more than the rest of the probe together, so `index` and the
//! full re-check keep both under `.carrick/probe/<generation>.<build>/`
//! (`reads.json`, `similar.json`) and the probe reads those. The probe never
//! writes: a cold probe, and the first probe of an agent harness whose index
//! was never built here, still read the blobs as before.
//!
//! A file is current only when its format number and its [`Inputs`] equal the
//! ones read now: the stat of every blob, of the hosted snapshot, the
//! credential, the repos' origins and the workspace's repo list. Anything
//! else, an unreadable file included, is a miss, and a miss reads the blobs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::index::repo_label;
use super::recheck::{Held, generation_name, kept_dir, took};
use super::similar::Similar;
use super::workspace::Workspace;

/// The kept files' layout. A file of another number is a miss.
const FORMAT: u32 = 1;
const KIND: &str = "probe";
const READS: &str = "reads.json";
const SIMILAR: &str = "similar.json";

type Origins = (BTreeMap<PathBuf, String>, BTreeMap<PathBuf, Option<String>>);

/// Everything the kept files were built from, read without opening a blob.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub(super) struct Inputs {
    /// File name, length and modification time (ns) of each local blob.
    blobs: Vec<(String, u64, i128)>,
    /// The cached hosted snapshot's length and modification time.
    snapshot: Option<(u64, i128)>,
    /// The credential's identity and the workspace it is signed in to.
    credential: Option<(String, Option<String>)>,
    /// The repos' origins, read only when a snapshot can be read.
    origins: Option<Origins>,
    /// The workspace's repos.
    repos: Vec<PathBuf>,
}

impl Inputs {
    /// `None` when a file the answer depends on cannot be stat'ed: nothing is
    /// then read from, or written to, the kept files.
    pub(super) fn now(workspace: &Workspace) -> Option<Self> {
        let mut blobs = Vec::new();
        match std::fs::read_dir(workspace.blobs_dir()) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.ok()?.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                        continue;
                    }
                    let (length, at) = stamp(&path)?;
                    blobs.push((path.file_name()?.to_string_lossy().into_owned(), length, at));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
        blobs.sort();
        let snapshot_file = workspace.index_dir().join("hosted/snapshot.json");
        let snapshot = match std::fs::symlink_metadata(&snapshot_file) {
            Ok(_) => Some(stamp(&snapshot_file)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let credential = crate::credentials::Credential::load()
            .ok()
            .flatten()
            .map(|credential| (credential.identity(), credential.workspace_slug.clone()));
        let origins = (credential.is_some() && snapshot.is_some())
            .then(|| super::hosted::origins_of(workspace));
        Some(Self {
            blobs,
            snapshot,
            credential,
            origins,
            repos: workspace.repos.clone(),
        })
    }
}

fn stamp(path: &Path) -> Option<(u64, i128)> {
    let meta = std::fs::metadata(path).ok()?;
    let at = match meta.modified().ok()?.duration_since(UNIX_EPOCH) {
        Ok(after) => i128::try_from(after.as_nanos()).ok()?,
        Err(before) => -i128::try_from(before.duration().as_nanos()).ok()?,
    };
    Some((meta.len(), at))
}

/// What the probe asks of one repo's blobs: the configs the scan resolves the
/// repo's services from, and the functions each file declares.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct RepoReads {
    /// The path the index records for the repo, as [`super::checkout::Tree`]'s
    /// `repo` spells it. Never canonicalised.
    path: PathBuf,
    label: String,
    configs: Vec<Option<String>>,
    /// Forward-slash file path -> the names declared in it, `<module>` left out.
    declared: BTreeMap<String, BTreeSet<String>>,
}

impl RepoReads {
    /// Both records of this repo, because neither is present on every install:
    /// the local blobs, then the hosted snapshot's.
    pub(super) fn of(held: &Held, path: &Path) -> Self {
        let label = repo_label(path);
        let hosted = held.hosted.local_blobs(path);
        let mut configs = Vec::new();
        let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for blob in held
            .local
            .iter()
            .filter(|blob| blob.repo_name == label)
            .chain(hosted.iter())
        {
            configs.push(blob.config_json.clone());
            for function in blob.function_definitions.values() {
                if let Some(file) = declared_path(function) {
                    declared
                        .entry(file)
                        .or_default()
                        .insert(function.name.clone());
                }
            }
        }
        Self {
            path: path.to_path_buf(),
            label,
            configs,
            declared,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn configs(&self) -> Vec<Option<&str>> {
        self.configs.iter().map(Option::as_deref).collect()
    }

    /// The names the index holds for `relative`.
    pub(super) fn declared_in(&self, relative: &str) -> BTreeSet<String> {
        self.declared
            .get(&relative.replace('\\', "/"))
            .cloned()
            .unwrap_or_default()
    }
}

/// The file a blob's record of a function is about, as a forward-slash path.
///
/// `<module>` is the file's own top-level call sites recorded under a
/// synthetic definition (carrick#965). It is not a function anybody wrote, so
/// a file that gains its first import would otherwise report one.
fn declared_path(function: &crate::visitor::FunctionDefinition) -> Option<String> {
    (function.name != crate::visitor::MODULE_SCOPE_KEY)
        .then(|| function.file_path.to_string_lossy().replace('\\', "/"))
}

/// The ranker over every blob `held` has, in every repo.
pub(super) fn similar_of(held: &Held) -> Similar {
    Similar::build(
        held.local
            .iter()
            .map(|blob| (blob.repo_name.clone(), blob))
            .chain(held.hosted.every_blob()),
    )
}

/// What a writer builds from `held` and keeps.
pub(super) struct Built {
    pub repos: Vec<RepoReads>,
    pub similar: Similar,
}

impl Built {
    pub(super) fn of(held: &Held, paths: &[PathBuf]) -> Self {
        Self {
            repos: paths.iter().map(|path| RepoReads::of(held, path)).collect(),
            similar: similar_of(held),
        }
    }
}

#[derive(Deserialize)]
struct ReadsIn {
    format: u32,
    inputs: Inputs,
    repos: Vec<RepoReads>,
}

#[derive(Serialize)]
struct ReadsOut<'a> {
    format: u32,
    inputs: &'a Inputs,
    repos: &'a [RepoReads],
}

#[derive(Deserialize)]
struct SimilarIn {
    format: u32,
    inputs: Inputs,
    similar: Similar,
}

#[derive(Serialize)]
struct SimilarOut<'a> {
    format: u32,
    inputs: &'a Inputs,
    similar: &'a Similar,
}

fn dir_of(workspace: &Workspace, generation: &str) -> Option<PathBuf> {
    kept_dir(workspace, KIND, generation)
}

fn load_reads(
    workspace: &Workspace,
    generation: &str,
    now: &Inputs,
    repo: &Path,
) -> Option<RepoReads> {
    let bytes = std::fs::read(dir_of(workspace, generation)?.join(READS)).ok()?;
    let file: ReadsIn = serde_json::from_slice(&bytes).ok()?;
    if file.format != FORMAT || file.inputs != *now {
        return None;
    }
    file.repos.into_iter().find(|reads| reads.path == repo)
}

/// The probe's read: what `repo`'s blobs hold, when the kept file is current.
pub(super) fn read(
    workspace: &Workspace,
    generation: &str,
    now: &Inputs,
    repo: &Path,
) -> Option<RepoReads> {
    let began = Instant::now();
    let found = load_reads(workspace, generation, now, repo)?;
    took("probe-kept-read", began);
    Some(found)
}

/// Whether a writer would only rewrite what is already kept for `repo`.
pub(super) fn is_current(
    workspace: &Workspace,
    generation: &str,
    now: &Inputs,
    repo: &Path,
) -> bool {
    load_reads(workspace, generation, now, repo).is_some()
}

/// The kept ranker, when it is current.
pub(super) fn read_similar(
    workspace: &Workspace,
    generation: &str,
    now: &Inputs,
) -> Option<Similar> {
    let began = Instant::now();
    let bytes = std::fs::read(dir_of(workspace, generation)?.join(SIMILAR)).ok()?;
    let file: SimilarIn = serde_json::from_slice(&bytes).ok()?;
    if file.format != FORMAT || file.inputs != *now {
        return None;
    }
    took("probe-kept-similar-read", began);
    Some(file.similar)
}

/// Write the kept files for `generation`, built from blobs read while the
/// inputs were `before`. When an index rewrote any of them since, nothing is
/// written: the files would describe blobs that are gone. Errors are ignored:
/// a lost write costs the next probe one blob read.
pub(super) fn keep(
    workspace: &Workspace,
    generation: &str,
    before: &Inputs,
    repos: &[RepoReads],
    similar: &Similar,
) {
    let began = Instant::now();
    let Some(dir) = dir_of(workspace, generation) else {
        return;
    };
    if Inputs::now(workspace).as_ref() != Some(before) {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // The ranker first: a reads file that is there says the ranker was kept.
    let _ = put(
        &dir.join(SIMILAR),
        &SimilarOut {
            format: FORMAT,
            inputs: before,
            similar,
        },
    );
    let _ = put(
        &dir.join(READS),
        &ReadsOut {
            format: FORMAT,
            inputs: before,
            repos,
        },
    );
    if let Some(parent) = dir.parent() {
        prune_other_generations(parent, &generation_name(generation));
    }
    took("probe-kept-write", began);
}

/// What `index` keeps once it has written its blobs and its index: the reads
/// of every repo it holds, and the ranker.
pub(super) fn keep_index(workspace: &Workspace, generation: &str, paths: &[PathBuf]) {
    let Some(before) = Inputs::now(workspace) else {
        return;
    };
    let Ok(held) = Held::read(workspace) else {
        return;
    };
    let built = Built::of(&held, paths);
    keep(workspace, generation, &before, &built.repos, &built.similar);
}

/// Write `value` whole: to a name of its own, then renamed over `path`, so a
/// reader never sees half a file.
fn put<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}-{nanos}", std::process::id()));
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Remove every entry of `.carrick/probe/` that is not a build of `generation`:
/// a new index replaces what the old one left, and another build's folder of
/// this generation stays.
fn prune_other_generations(parent: &Path, generation: &str) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let keep = format!("{generation}.");
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_name().to_string_lossy().starts_with(&keep) {
            let path = entry.path();
            let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_in(root: &Path) -> Workspace {
        std::fs::create_dir_all(root.join(".carrick/repos")).unwrap();
        Workspace {
            root: root.to_path_buf(),
            repos: vec![root.join("shop")],
            missing: Vec::new(),
            repos_detected_by: String::new(),
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            parent_proposal: None,
        }
    }

    /// The fingerprint moves with each thing the kept files were built from.
    #[test]
    #[serial_test::serial]
    fn the_inputs_move_with_a_blob_the_snapshot_and_the_credential() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = workspace_in(dir.path());
        let before_env = std::env::var_os("XDG_CONFIG_HOME");
        let config = dir.path().join("config");
        // SAFETY: serialised with the other tests that read the environment.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &config) };
        let none = Inputs::now(&workspace).expect("a stat-able workspace");

        std::fs::write(workspace.blobs_dir().join("0.json"), "{}").unwrap();
        let blob = Inputs::now(&workspace).unwrap();
        assert_ne!(none, blob, "a new blob moves the inputs");
        std::fs::write(workspace.blobs_dir().join("0.json"), "{\"a\":1}").unwrap();
        assert_ne!(blob, Inputs::now(&workspace).unwrap(), "a rewritten blob");

        let before_snapshot = Inputs::now(&workspace).unwrap();
        std::fs::create_dir_all(workspace.index_dir().join("hosted")).unwrap();
        std::fs::write(workspace.index_dir().join("hosted/snapshot.json"), "{}").unwrap();
        let snapshot = Inputs::now(&workspace).unwrap();
        assert_ne!(before_snapshot, snapshot, "a snapshot that appears");

        let credential = |token: &str| {
            serde_json::json!({
                "api_base": crate::credentials::API_BASE,
                "token": token,
                "workspace_slug": "w",
                "obtained_at": "2026-11-14T00:00:00Z",
            })
            .to_string()
        };
        let file = config.join("carrick/credentials.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let write = |token: &str| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&file, credential(token)).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        write("one");
        let one = Inputs::now(&workspace).unwrap();
        assert_ne!(snapshot, one, "a credential that appears");
        write("two");
        assert_ne!(one, Inputs::now(&workspace).unwrap(), "another credential");

        match before_env {
            Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}
