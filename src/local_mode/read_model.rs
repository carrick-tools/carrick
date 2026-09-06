//! What `.carrick/index.json` holds, and how a file is found in it.
//!
//! This is a read model, not a contract: it exists so `touch` and `check`
//! answer in milliseconds without parsing source, loading index blobs, or
//! re-running a join. Everything in it was computed by `carrick index`;
//! deleting the file and re-running the command reproduces it byte for byte.
//!
//! The shape a reader outside this repo sees is `docs/local-mode-output.md`,
//! which this feeds. Nothing else reads this file, so it carries a format
//! version and no compatibility rule: a version it does not recognise is an
//! index to rebuild, not one to migrate.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::file_analyzer_agent::ResolutionSource;
use crate::boundary::ServiceBoundary;

/// Bumped whenever this file's shape changes. A mismatch makes every read-only
/// command answer `index_unreadable`, which tells the user to re-index instead
/// of showing them rows in a shape the reader half-understands.
pub const READ_MODEL_VERSION: u32 = 2;

/// A route the service serves, or a call it makes.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Route,
    Call,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Route => "route",
            ItemKind::Call => "call",
        }
    }
}

/// Which layer is answerable for a row. A local index holds only facts —
/// the model stage does not run here — but the field is stated on every row
/// rather than implied, because the same reader will one day see rows that
/// came from an index the cloud built (R1).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Fact,
    Candidate,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Fact => "fact",
            Source::Candidate => "candidate",
        }
    }

    /// A row is the model's alone when the only layer that stated it is the
    /// model. Everything else — a deterministic pass, or a row with no source
    /// recorded because its protocol has no emit/join phase — is a fact of the
    /// source.
    pub fn of(resolution_source: Option<ResolutionSource>) -> Self {
        match resolution_source {
            Some(ResolutionSource::Model) => Source::Candidate,
            _ => Source::Fact,
        }
    }
}

/// The other side of one contract.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Counterpart {
    /// `producer`, `consumer`, or `peer` when neither side serves the other.
    pub role: String,
    pub service: String,
    pub file: String,
    pub line: Option<u32>,
}

/// What the type check concluded about a row, at index time.
///
/// `state` carries the SAME three words, with the same meanings, as
/// `verdict_state` on the PR-result payload (carrick#727, carrick#731):
/// `resolved` = the compiler reached a verdict with no `any`/`unknown`/error
/// on either side; `unresolved` = a verdict was attempted and a side was not
/// resolvable; `not_checked` = no type verdict bears on this row at all. One
/// vocabulary, because one agent reads both contracts in one session.
///
/// Staleness is NOT one of them. Whether the tree has moved since the index is
/// `stale` and `changed_since_index` at the top level, which say it once for
/// the whole file instead of per row.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoredVerdict {
    pub state: String,
    /// `compatible`, `type_mismatch`, `method_mismatch` or `producer_removed`.
    /// `None` where the state is the whole statement.
    pub result: Option<String>,
    pub detail: String,
}

/// One route or call, with everything a reader needs about it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexedItem {
    pub kind: ItemKind,
    pub service: String,
    /// `OperationKey::canonical()`.
    pub key: String,
    pub method: String,
    pub path: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub source: Source,
    pub resolution_source: Option<ResolutionSource>,
    pub evidence: Option<String>,
    pub counterparts: Vec<Counterpart>,
    pub verdict: Option<StoredVerdict>,
}

/// One service of one repo, as the index recorded it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexedService {
    /// `service_name ?? repo_name` — what every counterpart names it by.
    pub name: String,
    /// The service's root inside its repo, as `carrick.json` declares it
    /// (`packages/gateway`). `None` for a single-service repo, which owns the
    /// whole tree. It is what decides which service a file with no indexed
    /// rows belongs to: without it, a monorepo answers for every such file
    /// with whichever service sorted first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    pub commit: String,
    /// RFC 3339, when this service was last indexed or refreshed.
    pub indexed_at: String,
    pub boundary: Option<ServiceBoundary>,
    pub routes: usize,
    pub calls: usize,
}

/// One repo on this machine, its services, and its files.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IndexedRepo {
    /// Absolute path on the machine that built the index.
    pub path: String,
    pub name: String,
    pub services: Vec<IndexedService>,
    /// Repo-relative path -> the rows the index holds for it, in line order.
    pub files: BTreeMap<String, Vec<IndexedItem>>,
}

impl IndexedRepo {
    /// The service a repo-relative path belongs to: the one whose directory is
    /// the longest prefix of it, and the repo's only service when that service
    /// declares no directory.
    ///
    /// Most files in a monorepo hold no indexed row at all, so this — not the
    /// rows — is what names the service in the common case.
    pub fn service_for(&self, relative: &str) -> Option<&IndexedService> {
        let mut best: Option<&IndexedService> = None;
        for service in &self.services {
            let matches = match service.directory.as_deref() {
                Some(directory) => {
                    let directory = directory.trim_matches('/');
                    !directory.is_empty()
                        && (relative == directory || relative.starts_with(&format!("{directory}/")))
                }
                // A service with no directory owns the whole repo, and is only
                // the answer when nothing more specific claims the path.
                None => true,
            };
            if !matches {
                continue;
            }
            let longer = best.as_ref().is_none_or(|current| {
                current.directory.as_deref().map_or(0, str::len)
                    < service.directory.as_deref().map_or(0, str::len)
            });
            if longer {
                best = Some(service);
            }
        }
        best
    }
}

/// The whole read model.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LocalIndex {
    pub version: u32,
    pub scanner_version: String,
    /// RFC 3339, when the last `index` or `refresh` finished.
    pub indexed_at: String,
    pub repos: Vec<IndexedRepo>,
}

impl LocalIndex {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let index: LocalIndex =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if index.version != READ_MODEL_VERSION {
            return Err(format!(
                "{} was written by a different scanner (index format {}, this build reads {}). \
                 Re-run `carrick index`.",
                path.display(),
                index.version,
                READ_MODEL_VERSION
            ));
        }
        Ok(index)
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("failed to serialize the index: {e}")))?;
        std::fs::write(path, json)
    }

    /// The repo a file belongs to, and the file's path relative to it.
    ///
    /// Longest prefix wins, so a repo nested inside another (a workspace that
    /// lists both a monorepo and one of its packages) resolves to the nearest
    /// one rather than the first match.
    pub fn locate_file(&self, file: &Path) -> Option<(&IndexedRepo, String)> {
        let absolute = file.canonicalize().unwrap_or_else(|_| absolutize(file));
        let mut best: Option<(&IndexedRepo, String)> = None;
        for repo in &self.repos {
            let root = Path::new(&repo.path);
            let Ok(relative) = absolute.strip_prefix(root) else {
                continue;
            };
            let relative = super::normalize_relative(relative);
            let longer = best
                .as_ref()
                .is_none_or(|(current, _)| current.path.len() < repo.path.len());
            if longer {
                best = Some((repo, relative));
            }
        }
        best
    }
}

/// A best-effort absolute path for a file that does not exist — the deleted
/// producer case, where `canonicalize` fails but the index still holds rows
/// for the path.
fn absolutize(file: &Path) -> std::path::PathBuf {
    if file.is_absolute() {
        return file.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let joined = cwd.join(file);
            // `canonicalize` on the parent resolves the symlinked temp dirs
            // macOS hands out (`/var` -> `/private/var`), which is what makes
            // a path under one comparable with an indexed repo root.
            match (joined.parent(), joined.file_name()) {
                (Some(parent), Some(name)) => match parent.canonicalize() {
                    Ok(parent) => parent.join(name),
                    Err(_) => joined,
                },
                _ => joined,
            }
        }
        Err(_) => file.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &str, name: &str) -> IndexedRepo {
        IndexedRepo {
            path: path.to_string(),
            name: name.to_string(),
            services: Vec::new(),
            files: BTreeMap::new(),
        }
    }

    #[test]
    fn a_file_resolves_to_the_nearest_indexed_repo() {
        // A workspace can list a monorepo and one of its packages; the file
        // belongs to the package, not to the tree that contains it.
        let index = LocalIndex {
            version: READ_MODEL_VERSION,
            scanner_version: "test".to_string(),
            indexed_at: "2026-09-06T00:00:00Z".to_string(),
            repos: vec![repo("/w/mono", "mono"), repo("/w/mono/packages/api", "api")],
        };
        let (found, relative) = index
            .locate_file(Path::new("/w/mono/packages/api/src/routes.ts"))
            .unwrap();
        assert_eq!(found.name, "api");
        assert_eq!(relative, "src/routes.ts");
    }

    #[test]
    fn a_file_outside_every_repo_resolves_to_nothing() {
        let index = LocalIndex {
            version: READ_MODEL_VERSION,
            scanner_version: "test".to_string(),
            indexed_at: "2026-09-06T00:00:00Z".to_string(),
            repos: vec![repo("/w/api", "api")],
        };
        assert!(
            index
                .locate_file(Path::new("/elsewhere/src/x.ts"))
                .is_none()
        );
    }

    #[test]
    fn only_a_model_row_is_a_candidate() {
        assert_eq!(Source::of(Some(ResolutionSource::Model)), Source::Candidate);
        assert_eq!(
            Source::of(Some(ResolutionSource::FileBasedRoute)),
            Source::Fact
        );
        // A protocol with no emit/join phase records no source; that is not
        // the model speaking.
        assert_eq!(Source::of(None), Source::Fact);
    }
}
