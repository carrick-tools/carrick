//! Authenticated, read-only hosted index input. Only index/refresh call this
//! network reader; queries validate local credential identity before reading
//! the resulting local read model.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::workspace::Workspace;
use crate::cloud_storage::CloudRepoData;

use crate::credentials::{API_BASE, Credential};
use crate::git_state::{origin, valid_repo_name};

const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceIdentity {
    slug: String,
    billing_tier: BillingTier,
    installed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BillingTier {
    Free,
    CrossRepo,
    Paid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostedService {
    service: String,
    hash: Option<String>,
    updated_at: Option<String>,
    scanner_version: Option<String>,
    /// Where this row came from: `ci` or `laptop`, server-derived from the
    /// credential that wrote it. `None` on a row stored before the field
    /// existed, which was a CI row — but the surfaces say nothing rather than
    /// asserting that. Additive under the same `carrick.resolve-repos/0` tag
    /// (carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §6.1).
    #[serde(default)]
    source: Option<String>,
    /// The GitHub login of whoever uploaded it. Never the user id.
    #[serde(default)]
    uploaded_by: Option<String>,
    /// The tree that produced it did not match its commit. A claim the
    /// surfaces display and nothing ranks on.
    #[serde(default)]
    dirty: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolvedRepo {
    full_name: String,
    connected: bool,
    project_id: Option<String>,
    project_slug: Option<String>,
    services: Option<Vec<HostedService>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectRepos {
    project_slug: String,
    repos: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resolution {
    schema: String,
    workspace: WorkspaceIdentity,
    allowance_sentence: Option<String>,
    repos: Vec<ResolvedRepo>,
    project_repos: Vec<ProjectRepos>,
}

impl Resolution {
    fn parse(value: serde_json::Value) -> Result<Self, String> {
        // Nullable is required on the wire; missing allowance must not erase
        // a server failure and masquerade as a successful read.
        if !value
            .get("allowance_sentence")
            .is_some_and(|v| v.is_null() || v.is_string())
        {
            return Err("Invalid resolve-repos allowance field".into());
        }
        if let Some(repos) = value.get("repos").and_then(|v| v.as_array()) {
            for repo in repos {
                if let Some(services) = repo.get("services").and_then(|v| v.as_array()) {
                    for service in services {
                        if ["hash", "updated_at", "scanner_version"].iter().any(|key| {
                            !service
                                .get(key)
                                .is_some_and(|v| v.is_null() || v.is_string())
                        }) {
                            return Err("Invalid resolve-repos service metadata".into());
                        }
                        // The three provenance fields are additive: absent is
                        // the pre-field row and must read as "nothing to say",
                        // never as a parse failure that would cost the whole
                        // response. Present, they must be the right type.
                        if ["source", "uploaded_by"].iter().any(|key| {
                            service
                                .get(key)
                                .is_some_and(|v| !(v.is_null() || v.is_string()))
                        }) || service
                            .get("dirty")
                            .is_some_and(|v| !(v.is_null() || v.is_boolean()))
                        {
                            return Err("Invalid resolve-repos service provenance".into());
                        }
                    }
                }
            }
        }
        let parsed: Self =
            serde_json::from_value(value).map_err(|_| "Invalid resolve-repos response")?;
        if parsed.schema != "carrick.resolve-repos/0" || parsed.workspace.slug.is_empty() {
            return Err("Unsupported resolve-repos schema".into());
        }
        let mut names = std::collections::HashSet::new();
        for repo in &parsed.repos {
            if !names.insert(repo.full_name.to_ascii_lowercase()) {
                return Err("Duplicate resolve-repos repository".into());
            }
            if !valid_repo_name(&repo.full_name)
                || (repo.connected
                    && (repo.project_id.as_ref().is_none_or(String::is_empty)
                        || repo.project_slug.as_ref().is_none_or(String::is_empty)
                        || repo.services.is_none()))
            {
                return Err("Invalid resolve-repos repository".into());
            }
        }
        if parsed
            .project_repos
            .iter()
            .any(|p| p.project_slug.is_empty() || p.repos.iter().any(|r| !valid_repo_name(r)))
        {
            return Err("Invalid resolve-repos project membership".into());
        }
        Ok(parsed)
    }
}

/// Provenance in the additive local check/status contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostedProvenance {
    pub commit: String,
    pub indexed_at: String,
    pub scanner_version: Option<String>,
    pub project: String,
    /// `ci`, `laptop`, or `None` on a row stored before the field existed.
    /// Server-derived from the credential that wrote the row, so nothing a
    /// client said can change it.
    ///
    /// Skipped when absent, like every other addition to this contract: an
    /// index written before the field existed round-trips unchanged, and a
    /// reader of `carrick.check/0` sees no key rather than a null it has to
    /// interpret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The GitHub login that uploaded it, when the row records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_by: Option<String>,
    /// The tree that produced it carried uncommitted changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedState {
    Enriched,
    NoIndexYet,
    NotConnected,
    /// This machine could not read `owner/repo` from the repo's git remote, so
    /// it never asked the cloud about it. Distinct from `NotConnected`, which
    /// is the cloud's answer about a repo it was asked about (carrick#1056).
    RemoteUnnamed,
    #[default]
    NotSignedIn,
    VersionMismatch,
    CommitMissing,
    ReadFailed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceEnrichment {
    pub hosted: Option<HostedProvenance>,
    pub hosted_state: HostedState,
    pub remote: Option<String>,
    pub failure: Option<String>,
    pub allowance_sentence: Option<String>,
    pub hosted_cache_version: Option<u32>,
    /// This service's candidates were classified by the model in THIS run.
    ///
    /// Every other field here describes the hosted row. This one describes the
    /// scan that wrote the index, and it is what keeps `carrick index` from
    /// ending on `refresh`'s sentence: a paid scan whose hosted row cannot be
    /// replayed afterwards (a dirty tree is the ordinary case on a first run)
    /// still ran the model on this machine, and the index it wrote carries
    /// those rows (carrick#1023 item 14).
    #[serde(default)]
    pub classified_here: bool,
    /// This run's upload was not stored: the cloud already held a row for this
    /// commit from this scanner release, so it kept it (carrick#1255).
    ///
    /// The one state in which a scan changes nothing an agent reads and the
    /// run has no reason to look wrong: installing dependencies, resolving a
    /// type or fixing a setting moves no source file, so the commit does not
    /// move, so the write is refused and the stored row — with everything the
    /// fix was meant to clear still in it — is what is served. Said out loud
    /// here rather than left to a reader comparing two timestamps.
    #[serde(default)]
    pub write_refused_as_current: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    identity: String,
    checked_at: String,
    /// The `resolve-repos` answer of the LAST read. It is rewritten on every
    /// refresh, including one that reused a project's blobs, so it says
    /// nothing about which generation those blobs are; `stored_rows` does.
    resolution: Resolution,
    projects: BTreeMap<String, Vec<CloudRepoData>>,
    /// Per project, the stored row each cached blob was downloaded as. Written
    /// only when a project is downloaded, and carried unchanged when it is
    /// reused or its download fails, because it describes the blobs and not
    /// the read (carrick#1673).
    ///
    /// Absent on a snapshot written before the field existed, which reads as
    /// "never compared": every project in it downloads once.
    #[serde(default)]
    stored_rows: BTreeMap<String, Vec<StoredRow>>,
}

/// The stored row a cached blob was downloaded as, in the same four fields a
/// `resolve-repos` service row names.
///
/// `updated_at` is the row's `updatedAt` exactly as the cloud stores it:
/// `get-cross-repo-data` answers it beside each blob as `lastUpdated`, and
/// `resolve-repos` answers the same attribute as `updated_at`. Every write of
/// an index blob is followed by a row write with a fresh `updatedAt` (carrick-cloud
/// `lambdas/check-or-upload/cross_repo_staged.js`), so a row whose `updatedAt`
/// has not moved still has the same blob behind it. The two strings are
/// compared for equality only, never parsed or ordered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct StoredRow {
    repo: String,
    service: String,
    hash: String,
    updated_at: Option<String>,
}

/// One project as `get-cross-repo-data` answered it: the blobs, and the stored
/// row each was answered as.
#[derive(Debug)]
struct DownloadedProject {
    blobs: Vec<CloudRepoData>,
    rows: Vec<StoredRow>,
}

/// What one hosted read did about downloading, counted in LOCAL SERVICE ROWS —
/// the `services` entries `resolve-repos` answered for the repos on this
/// machine. Transient: it describes this run, so it is never persisted in the
/// snapshot.
///
/// Three outcomes, and the reader has to be able to tell them apart, so all
/// three are counted and the line states every one that happened (carrick#1012
/// item 2). A skipped download and a failed one both leave the cached blobs in
/// place; only the counts say which happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Downloads {
    /// Rows in a project this run downloaded.
    pub fetched: usize,
    /// Rows in a project whose every hash matched the cached blobs, so the
    /// project was not downloaded at all.
    pub reused: usize,
    /// Rows in a project whose download failed. Their cached blobs may still
    /// be serving, but nothing about them was re-read.
    pub unread: usize,
}

impl Downloads {
    /// The one line a build prints about the hosted download. Every local
    /// service row is in exactly one of the three counts, so a reader can add
    /// them up and see nothing was hidden.
    pub(super) fn line(&self) -> String {
        let mut line = format!(
            "hosted index: downloaded {} service(s), reused {} unchanged",
            self.fetched, self.reused
        );
        if self.unread > 0 {
            line.push_str(&format!(
                ", {} not re-read (the download failed)",
                self.unread
            ));
        }
        line
    }
}

#[derive(Default)]
pub(super) struct HostedInput {
    snapshot: Option<Snapshot>,
    failure: Option<String>,
    remotes: BTreeMap<PathBuf, String>,
    /// The repos of this workspace whose `origin` names no `owner/repo`, with
    /// the remote they carry — `None` when there is no origin at all. They are
    /// held apart from `remotes` because nothing can be asked of the cloud for
    /// them, and a surface that said "not connected" about them was blaming
    /// the cloud for a name this machine could not read (carrick#1056).
    unnamed: BTreeMap<PathBuf, Option<String>>,
    downloads: Option<Downloads>,
}

struct Reader {
    client: reqwest::Client,
    endpoint: String,
    #[cfg(test)]
    allow_http_staging: bool,
}

impl Reader {
    fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .gzip(true)
            .build()
            .map_err(|_| "Could not construct hosted reader")?;
        Ok(Self {
            client,
            endpoint: format!("{API_BASE}/types/check-or-upload"),
            #[cfg(test)]
            allow_http_staging: false,
        })
    }

    async fn body(response: reqwest::Response) -> Result<serde_json::Value, String> {
        let status = response.status();
        if !status.is_success() {
            return Err(if status == reqwest::StatusCode::UNAUTHORIZED {
                "Hosted read returned 401. Run carrick login.".into()
            } else {
                format!("Hosted read returned HTTP {}", status.as_u16())
            });
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Could not decode hosted response")?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err("Hosted response exceeds the local read limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        // How much came down, for the line a person is watching. Decoded
        // bytes, which is what the reader holds: the wire is gzipped and the
        // ratio is not this layer's to state (carrick#1365).
        crate::progress::hosted_bytes(bytes.len());
        serde_json::from_slice(&bytes).map_err(|_| "Invalid hosted JSON response".into())
    }

    async fn post(
        &self,
        credential: &Credential,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        // Authorization is attached to this single API request. The client
        // has no default bearer header and never follows redirects.
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&credential.token)
            .header(crate::invoker::HEADER, crate::invoker::current())
            .json(&body)
            .send()
            .await
            .map_err(|_| "Could not reach the hosted index")?;
        Self::body(response).await
    }

    async fn project(
        &self,
        credential: &Credential,
        id: &str,
    ) -> Result<DownloadedProject, String> {
        let mut value = self
            .post(
                credential,
                serde_json::json!({"action":"get-cross-repo-data", "project_id":id}),
            )
            .await?;
        if value.get("staged") == Some(&serde_json::Value::Bool(true)) {
            let url = value
                .get("staged_url")
                .and_then(|v| v.as_str())
                .ok_or("Missing staged URL")?;
            let url = reqwest::Url::parse(url).map_err(|_| "Invalid staged URL")?;
            let secure = url.scheme() == "https";
            #[cfg(test)]
            let secure = secure
                || (self.allow_http_staging
                    && url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1"));
            if !secure
                || !url.username().is_empty()
                || url.password().is_some()
                || url.host_str().is_none()
            {
                return Err("Invalid staged URL".into());
            }
            value = Self::body(
                self.client
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| "Could not read the staged index")?,
            )
            .await?;
        }
        if value
            .get("processing_errors")
            .and_then(|v| v.as_array())
            .is_some_and(|v| !v.is_empty())
        {
            return Err("Hosted project read omitted repository data".into());
        }
        #[derive(Deserialize)]
        struct Entry {
            /// Read one blob at a time below, so a blob written by a later
            /// release says to upgrade instead of reading as a broken
            /// response (carrick#1740).
            metadata: serde_json::Value,
            /// The stored row's `updatedAt`. Optional in the cloud's own type,
            /// and a blob answered without it, or with anything but a string,
            /// is one the next read cannot compare, so it downloads again
            /// (carrick#1673). It never costs the blob itself.
            #[serde(default, rename = "lastUpdated")]
            last_updated: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct Response {
            repos: Vec<Entry>,
        }
        let response: Response =
            serde_json::from_value(value).map_err(|_| "Invalid hosted project schema")?;
        let mut blobs = Vec::with_capacity(response.repos.len());
        let mut rows = Vec::with_capacity(response.repos.len());
        for entry in response.repos {
            // Only the upgrade sentence is printed: it names the sibling and
            // two versions, and never echoes the parse error's text.
            let blob = crate::cloud_storage::read_peer_blob_value(entry.metadata).map_err(|e| {
                e.upgrade_sentence()
                    .unwrap_or_else(|| "Invalid hosted project schema".to_string())
            })?;
            rows.push(StoredRow {
                repo: blob.repo_name.to_ascii_lowercase(),
                service: service_of(&blob),
                hash: blob.commit_hash.clone(),
                updated_at: entry
                    .last_updated
                    .as_ref()
                    .and_then(|at| at.as_str())
                    .map(str::to_string),
            });
            blobs.push(blob);
        }
        Ok(DownloadedProject { blobs, rows })
    }

    async fn refresh(
        &self,
        credential: &Credential,
        repos: Vec<String>,
        old: Option<Snapshot>,
    ) -> Result<(Snapshot, Option<String>, Downloads), String> {
        let resolution = self.resolve(credential, &repos).await?;
        Ok(self.assemble(credential, &repos, resolution, old).await)
    }

    /// The `resolve-repos` answer for `repos`: which are connected, to which
    /// project, and the commit and stored time of every service row. Metadata
    /// only; no blob is downloaded.
    async fn resolve(
        &self,
        credential: &Credential,
        repos: &[String],
    ) -> Result<Resolution, String> {
        if repos.len() > 200 {
            return Err("Hosted reads support at most 200 local repositories per workspace".into());
        }
        let requested: std::collections::BTreeSet<String> =
            repos.iter().map(|r| r.to_ascii_lowercase()).collect();
        let value = self
            .post(
                credential,
                serde_json::json!({"action":"resolve-repos", "repos":repos}),
            )
            .await?;
        let resolution = Resolution::parse(value)?;
        let answered: std::collections::BTreeSet<String> = resolution
            .repos
            .iter()
            .map(|r| r.full_name.to_ascii_lowercase())
            .collect();
        if answered != requested {
            return Err("Hosted metadata did not answer the requested repositories".into());
        }
        if credential
            .workspace_slug
            .as_ref()
            .is_some_and(|s| *s != resolution.workspace.slug)
        {
            return Err("Carrick credential workspace changed. Run carrick login.".into());
        }
        Ok(resolution)
    }

    /// The snapshot `resolution` describes: each connected project's blobs,
    /// downloaded unless `old` already holds them as stored
    /// ([`project_is_unchanged`]).
    async fn assemble(
        &self,
        credential: &Credential,
        repos: &[String],
        resolution: Resolution,
        old: Option<Snapshot>,
    ) -> (Snapshot, Option<String>, Downloads) {
        let requested: std::collections::BTreeSet<String> =
            repos.iter().map(|r| r.to_ascii_lowercase()).collect();
        let old = old.filter(|s| s.resolution.workspace.slug == resolution.workspace.slug);
        let ids: std::collections::BTreeSet<_> = resolution
            .repos
            .iter()
            .filter(|r| r.connected)
            .filter_map(|r| r.project_id.clone())
            .collect();
        let mut projects = BTreeMap::new();
        let mut stored_rows = BTreeMap::new();
        let mut failure = None;
        let mut downloads = Downloads::default();
        for id in ids {
            let rows = service_rows(&resolution, &id).count();
            // The blobs held for this project and the rows they were
            // downloaded as travel together: kept, they keep their rows.
            let held = old.as_ref().and_then(|s| {
                Some((
                    s.projects.get(&id)?,
                    s.stored_rows.get(&id).map(Vec::as_slice).unwrap_or(&[]),
                ))
            });
            // `get-cross-repo-data` answers with every repo under one project
            // and takes no filter, so a project is the smallest thing that can
            // be skipped. The count stays in service rows because that is what
            // the user recognises and what `resolve-repos` gave hashes for.
            if let Some((cached, read_as)) = held
                && project_is_unchanged(&resolution, &id, cached, read_as, &requested)
            {
                projects.insert(id.clone(), cached.clone());
                stored_rows.insert(id, read_as.to_vec());
                downloads.reused += rows;
                continue;
            }
            match self.project(credential, &id).await {
                Ok(downloaded) => {
                    projects.insert(id.clone(), downloaded.blobs);
                    stored_rows.insert(id, downloaded.rows);
                    downloads.fetched += rows;
                }
                Err(error) => {
                    failure = Some(error);
                    downloads.unread += rows;
                    if let Some((blobs, read_as)) = held {
                        projects.insert(id.clone(), blobs.clone());
                        stored_rows.insert(id, read_as.to_vec());
                    }
                }
            }
        }
        (
            Snapshot {
                identity: credential.identity(),
                checked_at: chrono::Utc::now().to_rfc3339(),
                resolution,
                projects,
                stored_rows,
            },
            failure,
            downloads,
        )
    }

    /// Move `old` to the hosted index the cloud holds now, when that is the
    /// same index further along this checkout's own history.
    ///
    /// `check --changed` re-reads a service when a file it reads changed since
    /// the commit the snapshot's blobs were scanned at, and a manifest or
    /// lockfile counts for every service. Nothing else moves that commit, so
    /// on a busy main branch every check re-read the whole repository. Each
    /// repo is decided alone ([`may_move`]), and only forward along HEAD's
    /// history: the newest of its hosted commits ([`newest_commit`]) must hold
    /// the newest held one and be held by HEAD. Each service's blob keeps its
    /// own commit. A branch behind main keeps the base it has, and so does a
    /// checkout that has not fetched the new commit, while every other repo
    /// of the workspace moves ([`hold_back`]).
    ///
    /// Asks `resolve-repos` first and downloads only when some repo moved, so
    /// a refusal costs one metadata request. Returns the snapshot to persist
    /// with [`Forward::Moved`], and none otherwise, and, whenever the cloud
    /// answered for this workspace, where each repo's hosted index and this
    /// machine's copy stand ([`synced_repos`]), with each kept repo's reason.
    async fn forward(
        &self,
        credential: &Credential,
        remotes: &BTreeMap<PathBuf, String>,
        old: Snapshot,
    ) -> (Forward, Option<Snapshot>, Option<Vec<SyncedRepo>>) {
        let repos: Vec<String> = remotes.values().cloned().collect();
        let resolution = match self.resolve(credential, &repos).await {
            Ok(resolution) => resolution,
            Err(reason) => return (Forward::Kept(reason), None, None),
        };
        if resolution.workspace.slug != old.resolution.workspace.slug {
            return (
                Forward::Kept(
                    "the hosted workspace is not the one this index was read from".into(),
                ),
                None,
                None,
            );
        }
        let before = HostedInput {
            snapshot: Some(old.clone()),
            remotes: remotes.clone(),
            ..Default::default()
        };
        let mut synced = synced_repos(&resolution, &before);
        let (outcome, snapshot, kept) = self
            .forward_from(credential, remotes, &repos, resolution, &before, old)
            .await;
        stamp_kept(&mut synced, remotes, &kept);
        (outcome, snapshot, Some(synced))
    }

    /// [`Reader::forward`] once the cloud has answered for this workspace.
    /// Also returns every repo [`may_move`] refused, with its reason, whatever
    /// the outcome.
    async fn forward_from(
        &self,
        credential: &Credential,
        remotes: &BTreeMap<PathBuf, String>,
        repos: &[String],
        resolution: Resolution,
        before: &HostedInput,
        old: Snapshot,
    ) -> (Forward, Option<Snapshot>, Vec<(PathBuf, String)>) {
        let mut approved: BTreeMap<PathBuf, std::collections::BTreeSet<String>> = BTreeMap::new();
        let mut moved: Vec<(PathBuf, String)> = Vec::new();
        let mut kept: Vec<(PathBuf, String)> = Vec::new();
        for (path, name) in remotes {
            let now: std::collections::BTreeSet<String> = resolution
                .repos
                .iter()
                .filter(|repo| repo.connected && repo.full_name.eq_ignore_ascii_case(name))
                .flat_map(|repo| repo.services.iter().flatten())
                .filter_map(|service| service.hash.clone())
                .collect();
            let held: std::collections::BTreeSet<String> = before
                .local_blobs(path)
                .into_iter()
                .map(|blob| blob.commit_hash)
                .collect();
            if now.is_empty() && held.is_empty() {
                continue;
            }
            match may_move(path, name, &held, &now) {
                // This repo keeps its copy; the others are decided alone.
                Err(reason) => kept.push((path.clone(), reason)),
                // The same commits: the copy still moves when the cloud
                // stores a newer generation of them (a rescan at one commit).
                Ok(None) if rescanned_in_place(&resolution, &old, name) => {
                    match rescan_may_replace(path, name, &now) {
                        Err(reason) => kept.push((path.clone(), reason)),
                        Ok(to) => {
                            approved.insert(path.clone(), now);
                            moved.push((path.clone(), to));
                        }
                    }
                }
                Ok(None) => {
                    approved.insert(path.clone(), now);
                }
                Ok(Some(to)) => {
                    approved.insert(path.clone(), now);
                    moved.push((path.clone(), to));
                }
            }
        }
        if moved.is_empty() {
            let outcome = if kept.is_empty() {
                Forward::Current
            } else {
                Forward::Kept(joined_reasons(&kept))
            };
            return (outcome, None, kept);
        }
        let held_names: Vec<String> = kept
            .iter()
            .filter_map(|(path, _)| remotes.get(path).cloned())
            .collect();
        // The kept repos are asked for as `old` holds them, so a project that
        // only they changed is reused rather than downloaded.
        let mut asked = resolution.clone();
        hold_entries(&mut asked, &old.resolution, &held_names);
        let (mut snapshot, failure, _) = self.assemble(credential, repos, asked, Some(old)).await;
        if let Some(failure) = failure {
            return (Forward::Kept(failure), None, kept);
        }
        // `before` holds the same snapshot `old` was.
        if let Some(held) = before.snapshot.as_ref() {
            hold_back(&mut snapshot, held, &resolution, &held_names);
        }
        let after = HostedInput {
            snapshot: Some(snapshot),
            remotes: remotes.clone(),
            ..Default::default()
        };
        // What came down must be what was checked: main may have moved again
        // between the two requests, and a blob this build would not carry
        // (another cache format, a dirty tree) would only re-read everything.
        for (path, checked) in &approved {
            let blobs = after.local_blobs(path);
            let is_moved = moved.iter().any(|(moved, _)| moved == path);
            let came: std::collections::BTreeSet<&String> =
                blobs.iter().map(|blob| &blob.commit_hash).collect();
            if blobs.is_empty()
                || came != checked.iter().collect()
                || (is_moved && !carried_as_is(&blobs))
            {
                return (
                    Forward::Kept(
                        "the downloaded hosted index is not the one that was checked".into(),
                    ),
                    None,
                    kept,
                );
            }
        }
        (
            Forward::Moved {
                moved,
                kept: kept.clone(),
            },
            after.snapshot,
            kept,
        )
    }
}

/// Whether a moved repo's blobs can be read in as they are: this build's cache
/// format and a clean tree. The one test of what a move may carry, for the
/// network move and for [`heal`] (a snapshot `hosted::refresh` wrote was never
/// put to it).
fn carried_as_is(blobs: &[CloudRepoData]) -> bool {
    !blobs.is_empty()
        && blobs.iter().all(|blob| {
            blob.cache_version == Some(crate::engine::CACHE_VERSION) && blob.dirty != Some(true)
        })
}

/// Whether one repo's copy may move from the commits it holds (`held`) to
/// `now`: the one rule every move of this machine's copy is decided by
/// ([`Reader::forward`], and [`heal`] for a local index left behind its
/// snapshot). `Ok(None)` when the two are the same, `Ok(Some(commit))` with
/// the commit it moves to, and `Err` with the reason it may not.
///
/// A push scan rescans only the services the push reached, so one repo's
/// services can sit at several commits of one line: the newest stands for
/// the repo when it holds every other. The move is only forward along HEAD's
/// history: the newest of `now` must hold the newest of `held` and be held by
/// HEAD.
fn may_move(
    path: &Path,
    name: &str,
    held: &std::collections::BTreeSet<String>,
    now: &std::collections::BTreeSet<String>,
) -> Result<Option<String>, String> {
    let repo = path.to_string_lossy();
    let (Some(to), Some(from)) = (newest_commit(&repo, now), newest_commit(&repo, held)) else {
        return Err(format!(
            "{name}: the hosted index or this machine's copy spans commits that are not one \
             line of history"
        ));
    };
    if now == held {
        return Ok(None);
    }
    held_by_head(&repo, name, &to)?;
    if !crate::git_state::is_ancestor(&repo, &from, &to) {
        return Err(format!(
            "{name}: the hosted index at {} does not descend from {}",
            short(&to),
            short(&from)
        ));
    }
    Ok(Some(to))
}

/// The one "never ahead of HEAD" test of a move: `to` must be held by the
/// checkout's HEAD. [`may_move`] applies it to a move to a newer commit and
/// [`rescan_may_replace`] to a rescan at the commit already held.
fn held_by_head(repo: &str, name: &str, to: &str) -> Result<(), String> {
    let head = crate::git_state::head_commit(repo).unwrap_or_default();
    if crate::git_state::is_ancestor(repo, to, &head) {
        return Ok(());
    }
    Err(format!(
        "{name}: the hosted index is at {}, which this checkout's HEAD does not hold",
        short(to)
    ))
}

/// Whether the cloud stores a different generation of repo `name`'s index than
/// the one `old` downloaded, at the commits both hold: a rescan at the same
/// commit (a new release, a late label, another machine) stores new rows under
/// the same hash, and only the row's `updated_at` says so (carrick#1673). The
/// rows compared are [`project_is_unchanged`]'s fourth rule, for this repo
/// alone. A row with no `updated_at` cannot be compared and is never "newer".
fn rescanned_in_place(resolution: &Resolution, old: &Snapshot, name: &str) -> bool {
    let Some(project) = resolution
        .repos
        .iter()
        .find(|repo| repo.connected && repo.full_name.eq_ignore_ascii_case(name))
        .and_then(|repo| repo.project_id.clone())
    else {
        return false;
    };
    let basename = name
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut named: Vec<StoredRow> = Vec::new();
    for (repo, service) in service_rows(resolution, &project) {
        if repo != basename {
            continue;
        }
        let (Some(hash), Some(updated_at)) = (service.hash.clone(), service.updated_at.clone())
        else {
            return false;
        };
        named.push(StoredRow {
            repo,
            service: service.service.clone(),
            hash,
            updated_at: Some(updated_at),
        });
    }
    named.sort();
    let mut held: Vec<StoredRow> = old
        .stored_rows
        .get(&project)
        .into_iter()
        .flatten()
        .filter(|row| row.repo == basename)
        .cloned()
        .collect();
    held.sort();
    !named.is_empty() && named != held
}

/// The commit a rescan at the commits already held replaces the copy at, or
/// the reason it may not: the same HEAD test as any move ([`held_by_head`]).
fn rescan_may_replace(
    path: &Path,
    name: &str,
    now: &std::collections::BTreeSet<String>,
) -> Result<String, String> {
    let repo = path.to_string_lossy();
    let to = newest_commit(&repo, now).ok_or_else(|| {
        format!("{name}: the hosted index spans commits that are not one line of history")
    })?;
    held_by_head(&repo, name, &to)?;
    Ok(to)
}

/// The commit of `commits` that holds every other one in its history, or
/// `None` when there is none (the set is empty, or spans two lines). The rule
/// the release kit's preflight reads a multi-commit hosted index by.
fn newest_commit(repo: &str, commits: &std::collections::BTreeSet<String>) -> Option<String> {
    commits
        .iter()
        .find(|candidate| {
            commits.iter().all(|other| {
                other == *candidate || crate::git_state::is_ancestor(repo, other, candidate)
            })
        })
        .cloned()
}

/// The first seven characters of a commit id, as a sentence names it.
fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// What one [`forward`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Forward {
    /// The snapshot on disk now holds the hosted index at a newer commit for
    /// each repo in `moved` (path, commit). Each repo in `kept` (path,
    /// reason) may not move yet and holds the copy it had.
    Moved {
        moved: Vec<(PathBuf, String)>,
        kept: Vec<(PathBuf, String)>,
    },
    /// The hosted index is at the commits this machine already holds.
    Current,
    /// Nothing moved, and why.
    Kept(String),
}

/// The reasons of every kept repo, as one sentence per repo.
fn joined_reasons(kept: &[(PathBuf, String)]) -> String {
    kept.iter()
        .map(|(_, reason)| reason.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Write each kept repo's reason and its sentence onto its row of the
/// record, where the row has none yet. A repo with no row (its hosted or held
/// commits are not one line of history, so [`synced_repos`] skipped it) gets
/// a row with no commits, so every kept repo has one place its sentence is
/// read from.
fn stamp_kept(
    synced: &mut Vec<SyncedRepo>,
    remotes: &BTreeMap<PathBuf, String>,
    kept: &[(PathBuf, String)],
) {
    for (path, reason) in kept {
        let Some(name) = remotes.get(path) else {
            continue;
        };
        if !synced.iter().any(|row| row.repo == *name) {
            synced.push(SyncedRepo {
                repo: name.clone(),
                path: path.to_string_lossy().into_owned(),
                hosted_commit: String::new(),
                hosted_scanned_at: None,
                copy_commit: String::new(),
                copy_scanned_at: None,
                head_holds_hosted: false,
                fetched: false,
                behind: None,
                head_detached: false,
                on_default_branch: None,
                kept_reason: None,
                kept_line: None,
                kept_short: None,
            });
        }
        for row in synced
            .iter_mut()
            .filter(|row| row.repo == *name && row.kept_reason.is_none())
        {
            row.kept_reason = Some(reason.clone());
            let (line, short) = kept_sentence(row);
            row.kept_line = Some(line);
            row.kept_short = Some(short);
        }
    }
}

/// What to tell a person about a repo whose copy is kept back: the sentence
/// (what is true and what to do) and its short clause. Built from the facts
/// the background forward already holds, so no hook asks git.
fn kept_sentence(row: &SyncedRepo) -> (String, String) {
    let name = &row.repo;
    if row.hosted_commit.is_empty() {
        return (
            format!(
                "{name}'s local index could not move to main's latest because local and hosted \
                 history diverged. Run `carrick refresh` to rebuild it from this checkout."
            ),
            "history diverged from main".into(),
        );
    }
    let index = match row.copy_scanned_at.as_deref().and_then(|at| at.get(..10)) {
        Some(day) => format!("{name}'s local index is from {day}"),
        None => format!("{name}'s local index is out of date"),
    };
    if row.head_holds_hosted {
        return (
            format!(
                "{index} and could not be moved to main's latest. Run `carrick refresh` to \
                 rebuild it."
            ),
            "its copy could not be moved".into(),
        );
    }
    if !row.fetched {
        return (
            format!(
                "{index} because this clone has not fetched main's latest commit. Run `git \
                 fetch`, then `git pull` on main."
            ),
            "latest main commit not fetched".into(),
        );
    }
    let behind = match row.behind {
        Some(1) => " (1 commit behind)".to_string(),
        Some(n) => format!(" ({n} commits behind)"),
        None => String::new(),
    };
    if row.head_detached {
        return (
            format!(
                "{index} because HEAD is detached and this checkout is missing main's latest \
                 commit{behind}. Check out main and run `git pull`, or run `carrick refresh` to \
                 rebuild from this checkout."
            ),
            "detached HEAD behind main".into(),
        );
    }
    let fix = if row.on_default_branch == Some(true) {
        "Run `git pull` to update."
    } else {
        "Run `git pull` on main, or `carrick refresh` on this branch."
    };
    (
        format!("{index} because this checkout is missing main's latest commit{behind}. {fix}"),
        "checkout is behind main".into(),
    )
}

/// Hold each repo named in `kept` (`owner/repo`) at what `old` held for it,
/// in `snapshot` the forward assembled: its `resolution.repos` entry (so
/// `local_blobs` filters by the service list its blobs were read under), its
/// blobs and its `stored_rows` entries, matched by repo basename within its
/// project as `local_blobs` matches them. A repo `old` held nothing for holds
/// nothing. Every other repo keeps what came down, so the snapshot holds each
/// repo at its own commit, and every reader of the snapshot reads that.
///
/// `fresh` is the cloud's answer, whose project for a repo is where its new
/// blobs came down.
fn hold_back(snapshot: &mut Snapshot, old: &Snapshot, fresh: &Resolution, kept: &[String]) {
    hold_entries(&mut snapshot.resolution, &old.resolution, kept);
    let entry = |resolution: &Resolution, name: &str| -> Option<String> {
        resolution
            .repos
            .iter()
            .find(|repo| repo.full_name.eq_ignore_ascii_case(name))
            .and_then(|repo| repo.project_id.clone())
    };
    for name in kept {
        let basename = name
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let held_in = entry(&old.resolution, name);
        let ids: std::collections::BTreeSet<String> = [entry(fresh, name), held_in.clone()]
            .into_iter()
            .flatten()
            .collect();
        for id in &ids {
            if let Some(blobs) = snapshot.projects.get_mut(id) {
                blobs.retain(|blob| !blob.repo_name.eq_ignore_ascii_case(&basename));
            }
            if let Some(rows) = snapshot.stored_rows.get_mut(id) {
                rows.retain(|row| row.repo != basename);
            }
        }
        let Some(id) = held_in else {
            continue;
        };
        if let Some(blobs) = old.projects.get(&id) {
            snapshot.projects.entry(id.clone()).or_default().extend(
                blobs
                    .iter()
                    .filter(|blob| blob.repo_name.eq_ignore_ascii_case(&basename))
                    .cloned(),
            );
        }
        if let Some(rows) = old.stored_rows.get(&id) {
            snapshot
                .stored_rows
                .entry(id)
                .or_default()
                .extend(rows.iter().filter(|row| row.repo == basename).cloned());
        }
    }
}

/// Put back `old`'s `resolution.repos` entry for each repo named in `kept`.
/// A repo `old` has no entry for keeps the new one with no services, so it
/// reads as holding nothing, as it did.
fn hold_entries(resolution: &mut Resolution, old: &Resolution, kept: &[String]) {
    for name in kept {
        let held = old
            .repos
            .iter()
            .find(|repo| repo.full_name.eq_ignore_ascii_case(name));
        if let Some(entry) = resolution
            .repos
            .iter_mut()
            .find(|repo| repo.full_name.eq_ignore_ascii_case(name))
        {
            match held {
                Some(held) => *entry = held.clone(),
                None => entry.services = Some(Vec::new()),
            }
        }
    }
}

/// Move this machine's copy of the hosted index forward to the one the cloud
/// holds now, when that one is further along HEAD's history
/// ([`Reader::forward`]), and rebuild the local read model from the moved
/// copy in the same run, so every reader takes one generation: no scan, no
/// model, no second download ([`super::index::Pass::Forwarded`]). Run by
/// `carrick refresh --hosted-only`, which the hooks start in the background
/// under the re-check runner's lock.
///
/// First, with no network, it heals a local index left behind its snapshot
/// ([`heal`]).
pub(super) fn forward(workspace: &Workspace) -> Forward {
    forward_with(workspace, &|moved| {
        super::index::run(
            workspace,
            None,
            &super::index::Pass::Forwarded(moved.iter().cloned().collect()),
        )
        .map(|_| ())
    })
}

/// What a rebuild that failed records: the move did not happen.
fn not_rebuilt(reason: &str) -> Forward {
    Forward::Kept(format!(
        "the local index could not be rebuilt from the newer copy: {reason}"
    ))
}

/// [`forward`] with the rebuild handed in, so a test can make it fail.
fn forward_with(
    workspace: &Workspace,
    rebuild: &dyn Fn(&[PathBuf]) -> Result<(), String>,
) -> Forward {
    let (remotes, _) = origins_of(workspace);
    let credential = match Credential::load() {
        Ok(Some(credential)) => credential,
        Ok(None) => return Forward::Kept("not signed in".into()),
        Err(reason) => return Forward::Kept(reason),
    };
    let Some(old) = read_snapshot(workspace, &credential) else {
        return Forward::Kept("this machine holds no hosted index to move".into());
    };
    let healed = heal(workspace, &remotes, &old, rebuild);
    let asked = remotes.clone();
    let result = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Could not start hosted reader".to_string())?;
        let reader = Reader::new()?;
        Ok::<_, String>(runtime.block_on(reader.forward(&credential, &asked, old)))
    })
    .join()
    .unwrap_or_else(|_| Err("Hosted reader failed".into()));
    let (outcome, mut synced) = match result {
        Ok((Forward::Moved { moved, kept }, Some(snapshot), synced)) => (
            move_and_rebuild(
                &workspace.index_dir().join("hosted/snapshot.json"),
                &snapshot,
                moved,
                kept,
                rebuild,
            ),
            synced,
        ),
        Ok((Forward::Moved { .. }, None, synced)) => {
            (Forward::Kept("no snapshot to write".into()), synced)
        }
        Ok((outcome, _, synced)) => (outcome, synced),
        Err(reason) => (Forward::Kept(reason), None),
    };
    let outcome = settle(outcome, healed, synced.as_mut(), &remotes);
    // Only a run the cloud answered says where the hosted index stands; any
    // other keeps the last record, whose own time says how old it is.
    if let Some(synced) = synced {
        let record = SyncRecord::of(&outcome, synced, chrono::Utc::now().to_rfc3339());
        let _ = record.write(&workspace.index_dir().join(SYNC_RECORD));
    }
    outcome
}

/// Write the moved snapshot, then rebuild the read model from it. A rebuild
/// that fails puts the old snapshot bytes back, so the snapshot and the read
/// model never split: the move is kept, with the rebuild's reason. Only the
/// moved repos are rebuilt: a kept repo's read model is the one it had, as its
/// part of the snapshot is ([`hold_back`]).
fn move_and_rebuild(
    path: &Path,
    snapshot: &Snapshot,
    moved: Vec<(PathBuf, String)>,
    kept: Vec<(PathBuf, String)>,
    rebuild: &dyn Fn(&[PathBuf]) -> Result<(), String>,
) -> Forward {
    let old = std::fs::read(path).ok();
    if let Err(reason) = persist(path, snapshot) {
        return Forward::Kept(reason);
    }
    let paths: Vec<PathBuf> = moved.iter().map(|(path, _)| path.clone()).collect();
    match rebuild(&paths) {
        Ok(()) => Forward::Moved { moved, kept },
        Err(reason) => {
            if let Some(old) = old {
                let _ = put_back(path, &old);
            }
            not_rebuilt(&reason)
        }
    }
}

/// `bytes` back at `path`, whole, with the snapshot's own permissions.
fn put_back(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("Invalid hosted cache directory")?;
    let pending = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&pending).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&pending, path).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(pending);
    result
}

/// What [`heal`] did: the rebuild's outcome (`None` when no repo healed), and
/// every repo it skipped (path, reason), whether or not another repo healed.
type Healed = (
    Option<Result<Vec<(PathBuf, String)>, String>>,
    Vec<(PathBuf, String)>,
);

/// Rebuild a local index left behind the snapshot it should read, with no
/// network: where `index.json`'s commits for a repo differ from the
/// snapshot's and [`may_move`] lets the copy move from the first to the
/// second. Each repo is decided alone: one that may not move is skipped, with
/// its reason, one whose copy cannot be carried as it is is skipped with its
/// own, and the others heal. The skips are returned even when nothing healed,
/// so a copy that stays behind is never left without a reason.
///
/// Before the move rebuilt the read model, a move wrote only the snapshot, so
/// a workspace can hold an index at one commit and a snapshot at a later one.
fn heal(
    workspace: &Workspace,
    remotes: &BTreeMap<PathBuf, String>,
    snapshot: &Snapshot,
    rebuild: &dyn Fn(&[PathBuf]) -> Result<(), String>,
) -> Healed {
    let Ok(index) = super::read_model::LocalIndex::read(&workspace.index_file()) else {
        return (None, Vec::new());
    };
    let input = HostedInput {
        snapshot: Some(snapshot.clone()),
        remotes: remotes.clone(),
        ..Default::default()
    };
    let mut moved: Vec<(PathBuf, String)> = Vec::new();
    let mut kept: Vec<(PathBuf, String)> = Vec::new();
    for (path, name) in remotes {
        let built: std::collections::BTreeSet<String> = index
            .repos
            .iter()
            .filter(|repo| Path::new(&repo.path) == path)
            .flat_map(|repo| repo.services.iter().map(|service| service.commit.clone()))
            .collect();
        let blobs = input.local_blobs(path);
        let held: std::collections::BTreeSet<String> =
            blobs.iter().map(|blob| blob.commit_hash.clone()).collect();
        if built.is_empty() || held.is_empty() || built.contains("") {
            continue;
        }
        match may_move(path, name, &built, &held) {
            Err(reason) => kept.push((path.clone(), reason)),
            Ok(None) => {}
            // What the network move would refuse to carry, the heal skips,
            // with a reason of its own. The network move may still carry a
            // newer copy of it, and then it is not kept ([`settle`]).
            Ok(Some(_)) if !carried_as_is(&blobs) => kept.push((
                path.clone(),
                format!(
                    "{name}: the held copy was scanned from uncommitted changes or by another \
                     Carrick format, so it cannot be moved as it is"
                ),
            )),
            Ok(Some(to)) => moved.push((path.clone(), to)),
        }
    }
    if moved.is_empty() {
        return (None, kept);
    }
    let paths: Vec<PathBuf> = moved.iter().map(|(path, _)| path.clone()).collect();
    (Some(rebuild(&paths).map(|()| moved)), kept)
}

/// What the run settled on once the heal and the network move have both
/// run, and what each kept repo's row of the record says.
///
/// A network move that moved something is the run's outcome, and a repo it
/// moved is not kept whatever the heal skipped. Otherwise a heal that
/// rebuilt moved the read model, whatever the cloud said, and the outcome
/// states every kept repo: the heal's skips and the repos the network move
/// refused. A heal that rebuilt nothing leaves the network's outcome, except
/// that a repo it skipped and nothing moved is kept, not current. One that
/// failed is the run's news unless the network move rebuilt after it.
fn settle(
    outcome: Forward,
    (healed, skipped): Healed,
    mut synced: Option<&mut Vec<SyncedRepo>>,
    remotes: &BTreeMap<PathBuf, String>,
) -> Forward {
    let network_moved: Vec<PathBuf> = match &outcome {
        Forward::Moved { moved, .. } => moved.iter().map(|(path, _)| path.clone()).collect(),
        _ => Vec::new(),
    };
    let healed_paths: Vec<PathBuf> = match &healed {
        Some(Ok(moved)) => moved.iter().map(|(path, _)| path.clone()).collect(),
        _ => Vec::new(),
    };
    let skipped: Vec<(PathBuf, String)> = skipped
        .into_iter()
        .filter(|(path, _)| !network_moved.contains(path) && !healed_paths.contains(path))
        .collect();
    if let Some(rows) = synced.as_deref_mut() {
        stamp_kept(rows, remotes, &skipped);
    }
    match (outcome, healed) {
        (Forward::Moved { moved, mut kept }, _) => {
            for skip in skipped {
                if !kept.iter().any(|(path, _)| *path == skip.0) {
                    kept.push(skip);
                }
            }
            Forward::Moved { moved, kept }
        }
        (_, Some(Ok(moved))) => {
            // The network's own per-repo refusals are on the rows.
            let mut kept = skipped;
            for row in synced.iter().flat_map(|rows| rows.iter()) {
                let path = PathBuf::from(&row.path);
                if let Some(reason) = &row.kept_reason
                    && !kept.iter().any(|(seen, _)| *seen == path)
                    && !moved.iter().any(|(seen, _)| *seen == path)
                {
                    kept.push((path, reason.clone()));
                }
            }
            Forward::Moved { moved, kept }
        }
        (_, Some(Err(reason))) => not_rebuilt(&reason),
        (Forward::Current, None) if !skipped.is_empty() => Forward::Kept(joined_reasons(&skipped)),
        (outcome, None) => outcome,
    }
}

/// Where [`forward`] records what it found, under the index directory. Small,
/// so the session-start hook reads it instead of the snapshot beside it.
/// Its shape is listed in `docs/local-mode-output.md` ("On-disk layout").
pub(super) const SYNC_RECORD: &str = "hosted/last-sync.json";

/// One repo in [`SyncRecord`]: the hosted index the cloud holds for it, the
/// copy this machine compares against, and whether HEAD can take the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SyncedRepo {
    repo: String,
    /// The checkout this row describes, so a reader that knows its own
    /// checkout finds its row without asking git.
    path: String,
    /// Empty for a repo whose hosted or held commits are not one line of
    /// history: it has no single commit to state, and only `kept_line` to say.
    hosted_commit: String,
    /// When the cloud stored the newest hosted row at that commit.
    hosted_scanned_at: Option<String>,
    copy_commit: String,
    copy_scanned_at: Option<String>,
    /// Whether HEAD's history holds `hosted_commit`. Where it does not, the
    /// copy cannot move until the checkout does.
    head_holds_hosted: bool,
    /// Whether this clone holds `hosted_commit` at all.
    fetched: bool,
    /// Commits `hosted_commit` has that HEAD lacks, when this clone holds it
    /// and HEAD does not.
    behind: Option<u64>,
    head_detached: bool,
    /// Whether HEAD is on the default branch; `None` when detached or when no
    /// default branch resolves.
    on_default_branch: Option<bool>,
    /// Why this repo's copy did not move in this run, when it may not:
    /// [`may_move`]'s reason. Set only on a repo that was refused, whatever
    /// the other repos did; `None` on one that moved or had nothing to move.
    kept_reason: Option<String>,
    /// The one sentence that says this repo's copy is kept back and what to
    /// do. The session start, `carrick status` and the edit-time lines print
    /// this and nothing of their own; `None` where the copy is not kept.
    kept_line: Option<String>,
    /// [`SyncedRepo::kept_line`] as a clause for the end of an edit-time
    /// line: why the copy is kept back, in a few words.
    kept_short: Option<String>,
}

/// `.carrick/hosted/last-sync.json`, rewritten by every [`forward`] the cloud
/// answered (carrick#3432, carrick#3433).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SyncRecord {
    schema: &'static str,
    at: String,
    outcome: &'static str,
    reason: Option<String>,
    repos: Vec<SyncedRepo>,
}

impl SyncRecord {
    /// The record of `outcome`. A repo the copy moved for now holds the hosted
    /// commit, so its copy is stated as that one; a kept repo's copy is the
    /// one it had. A move that kept some repos states their reasons too.
    fn of(outcome: &Forward, mut repos: Vec<SyncedRepo>, at: String) -> Self {
        let (outcome, reason) = match outcome {
            Forward::Moved { moved, kept } => {
                for repo in &mut repos {
                    if moved
                        .iter()
                        .any(|(_, commit)| *commit == repo.hosted_commit)
                    {
                        repo.copy_commit = repo.hosted_commit.clone();
                        repo.copy_scanned_at = repo.hosted_scanned_at.clone();
                    }
                }
                ("moved", (!kept.is_empty()).then(|| joined_reasons(kept)))
            }
            Forward::Current => ("current", None),
            Forward::Kept(reason) => ("kept", Some(reason.clone())),
        };
        Self {
            schema: "carrick.hosted-sync/0",
            at,
            outcome,
            reason,
            repos,
        }
    }

    fn write(&self, path: &Path) -> Result<(), String> {
        let dir = path.parent().ok_or("Invalid hosted cache directory")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let pending = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let result = std::fs::write(&pending, json)
            .and_then(|()| std::fs::rename(&pending, path))
            .map_err(|e| e.to_string());
        let _ = std::fs::remove_file(pending);
        result
    }
}

/// A repo whose copy the last run kept back, as [`SyncRecord`] states it.
/// The one reader of `kept_line` on the Rust side: `carrick status` prints
/// `line`, and a `check` answer carries `behind` and `reason_short` for the
/// edit-time lines, so no surface words it twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptRepo {
    pub repo: String,
    /// The checkout the record describes.
    pub path: String,
    /// `kept_line` of the record: what is true and what to do.
    pub line: String,
    /// `kept_short` of the record, for the end of an edit-time line.
    pub reason_short: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u64>,
}

/// The repos `last-sync.json` (under `index_dir`) says are kept back, or none
/// where there is no record, it is of another shape, or a full refresh wrote
/// the snapshot after it: that copy is gone, so what the record said of it
/// is not true now (the same rule as `lastSync` in `npm/carrick`). Reads one
/// small file and stats one other; asks git nothing.
pub fn kept_repos(index_dir: &Path) -> Vec<KeptRepo> {
    #[derive(Deserialize)]
    struct Row {
        repo: String,
        path: Option<String>,
        behind: Option<u64>,
        kept_line: Option<String>,
        kept_short: Option<String>,
    }
    #[derive(Deserialize)]
    struct Record {
        schema: String,
        repos: Vec<Row>,
    }
    let dir = index_dir.join("hosted");
    let file = dir.join("last-sync.json");
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let (Some(written), Ok(bytes)) = (modified(&file), std::fs::read(&file)) else {
        return Vec::new();
    };
    if modified(&dir.join("snapshot.json")).is_some_and(|snapshot| snapshot > written) {
        return Vec::new();
    }
    let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
        return Vec::new();
    };
    if record.schema != "carrick.hosted-sync/0" {
        return Vec::new();
    }
    record
        .repos
        .into_iter()
        .filter_map(|row| {
            Some(KeptRepo {
                repo: row.repo,
                path: row.path?,
                line: row.kept_line?,
                reason_short: row.kept_short?,
                behind: row.behind,
            })
        })
        .collect()
}

/// Where each local repo's hosted index and this machine's copy of it stand,
/// for the repos whose hosted and held commits are each one line of history.
fn synced_repos(resolution: &Resolution, before: &HostedInput) -> Vec<SyncedRepo> {
    let scanned_at = |resolution: &Resolution, name: &str, commit: &str| -> Option<String> {
        resolution
            .repos
            .iter()
            .filter(|repo| repo.connected && repo.full_name.eq_ignore_ascii_case(name))
            .flat_map(|repo| repo.services.iter().flatten())
            .filter(|service| service.hash.as_deref() == Some(commit))
            .filter_map(|service| service.updated_at.clone())
            .max()
    };
    let Some(old) = before.snapshot.as_ref() else {
        return Vec::new();
    };
    let mut synced = Vec::new();
    for (path, name) in &before.remotes {
        let repo = path.to_string_lossy();
        let now: std::collections::BTreeSet<String> = resolution
            .repos
            .iter()
            .filter(|r| r.connected && r.full_name.eq_ignore_ascii_case(name))
            .flat_map(|r| r.services.iter().flatten())
            .filter_map(|service| service.hash.clone())
            .collect();
        let held: std::collections::BTreeSet<String> = before
            .local_blobs(path)
            .into_iter()
            .map(|blob| blob.commit_hash)
            .collect();
        let (Some(hosted), Some(copy)) = (newest_commit(&repo, &now), newest_commit(&repo, &held))
        else {
            continue;
        };
        let head = crate::git_state::head_commit(&repo).unwrap_or_default();
        let fetched = crate::git_state::has_commit(&repo, &hosted);
        let head_holds_hosted = crate::git_state::is_ancestor(&repo, &hosted, &head);
        let behind = (fetched && !head_holds_hosted)
            .then(|| crate::git_state::commits_ahead(&repo, &head, &hosted).ok())
            .flatten();
        synced.push(SyncedRepo {
            repo: name.clone(),
            path: repo.to_string(),
            hosted_scanned_at: scanned_at(resolution, name, &hosted),
            copy_scanned_at: scanned_at(&old.resolution, name, &copy),
            hosted_commit: hosted,
            copy_commit: copy,
            head_holds_hosted,
            fetched,
            behind,
            head_detached: crate::git_state::head_is_detached(&repo),
            on_default_branch: crate::git_state::on_default_branch(&repo),
            kept_reason: None,
            kept_line: None,
            kept_short: None,
        });
    }
    synced
}

/// Every service row `resolve-repos` answered for one project, with the repo
/// basename it belongs to. Only repos on this machine are in it: the response
/// carries `services` for the repos the request named and for no others.
fn service_rows<'a>(
    resolution: &'a Resolution,
    id: &'a str,
) -> impl Iterator<Item = (String, &'a HostedService)> {
    resolution
        .repos
        .iter()
        .filter(move |repo| repo.connected && repo.project_id.as_deref() == Some(id))
        .flat_map(|repo| {
            let basename = repo
                .full_name
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            repo.services
                .iter()
                .flatten()
                .map(move |service| (basename.clone(), service))
        })
}

/// The key a blob's row is stored under: the slug of its `service_name`, or
/// the repo name for a single-service repo.
///
/// The cloud keys every row by `sanitizeService` (carrick-cloud
/// `lambdas/_shared/service_slug.ts`), so `resolve-repos` answers
/// `acme-orders-api` for a blob whose `service_name` is
/// `@acme/orders-api`. Compared raw, every scoped or punctuated name missed
/// its own row: no hosted blob was replayed and every read downloaded the
/// whole project again.
fn service_of(blob: &CloudRepoData) -> String {
    stored_service(blob.service_name.as_deref(), &blob.repo_name)
}

/// [`service_of`] for a blob whose `repo_name` is no longer the hosted
/// basename (`local_blobs` relabels it to the local directory).
fn stored_service(service_name: Option<&str>, repo_basename: &str) -> String {
    crate::cloud_storage::indexed_service_slug(service_name, repo_basename)
}

/// Whether the blobs already cached for one project are still the generation
/// the cloud stores for every service the hosted metadata names, so this run
/// can skip its download.
///
/// The stored row's `hash` IS the blob's `commit_hash` (the scanner uploads
/// `hash: data.commit_hash`), so the two are comparable directly. Four things
/// must all hold, and the second and fourth are the ones a commit comparison
/// does not imply:
///
/// 1. Every row carries a hash and an `updated_at`. `None` means "cannot
///    compare", never "same".
/// 2. Every repo in the project is on this machine. `resolve-repos` answers
///    `services` for the repos the request named, so a sibling repo that is
///    only in the cloud has no hash here at all — its blob could have moved
///    without anything in this response changing, and a repo that joined the
///    project since the snapshot would be invisible in a set comparison.
/// 3. The (repo, service, hash) rows equal the (repo, service, commit) blobs
///    already held, so a removed service or an extra blob also downloads.
/// 4. The (repo, service, hash, updated_at) rows equal the rows the held blobs
///    were downloaded as (`read_as`). A rescan at the same commit — a new
///    release, a CI run, another machine, or this laptop's own last scan —
///    stores a new generation under the same hash, and only `updated_at`
///    says so (carrick#1673). The row's `scanner_version` cannot stand in for
///    it: a CI write supersedes a laptop row at the same commit and release.
fn project_is_unchanged(
    resolution: &Resolution,
    id: &str,
    cached: &[CloudRepoData],
    read_as: &[StoredRow],
    requested: &std::collections::BTreeSet<String>,
) -> bool {
    let Some(slug) = resolution
        .repos
        .iter()
        .find(|repo| repo.connected && repo.project_id.as_deref() == Some(id))
        .and_then(|repo| repo.project_slug.as_ref())
    else {
        return false;
    };
    let Some(membership) = resolution
        .project_repos
        .iter()
        .find(|project| &project.project_slug == slug)
    else {
        return false;
    };
    if !membership
        .repos
        .iter()
        .all(|repo| requested.contains(&repo.to_ascii_lowercase()))
    {
        return false;
    }
    let mut named: Vec<StoredRow> = Vec::new();
    for (basename, service) in service_rows(resolution, id) {
        let (Some(hash), Some(updated_at)) = (service.hash.clone(), service.updated_at.clone())
        else {
            return false;
        };
        named.push(StoredRow {
            repo: basename,
            service: service.service.clone(),
            hash,
            updated_at: Some(updated_at),
        });
    }
    named.sort();
    // The blobs are read straight off the snapshot, never through
    // `local_blobs`, which rewrites `repo_name` to the local directory label
    // and would never match a basename from the hosted metadata.
    let mut held: Vec<(String, String, String)> = cached
        .iter()
        .map(|blob| {
            (
                blob.repo_name.to_ascii_lowercase(),
                service_of(blob),
                blob.commit_hash.clone(),
            )
        })
        .collect();
    held.sort();
    let identities: Vec<(String, String, String)> = named
        .iter()
        .map(|row| (row.repo.clone(), row.service.clone(), row.hash.clone()))
        .collect();
    if identities != held {
        return false;
    }
    let mut read_as = read_as.to_vec();
    read_as.sort();
    named == read_as
}

/// The remotes and the cached snapshot, with no network request of any kind.
///
/// What a re-check reads (carrick#1036): the hosted services it judges against
/// are the ones the last `index` or `refresh` already downloaded, so an edit
/// costs a scan of one repo and nothing on the wire. The snapshot is subject to
/// the same credential gate as every other read — a different account, or a
/// different authenticated workspace, is no snapshot at all rather than another
/// account's rows.
///
/// `failure` is left unset: not reading the cloud is this function's contract,
/// not a degradation of it, and a caller with no snapshot answers from local
/// counterparts alone.
pub(super) fn cached(workspace: &Workspace) -> HostedInput {
    let (remotes, unnamed) = origins_of(workspace);
    let mut input = HostedInput {
        remotes,
        unnamed,
        ..Default::default()
    };
    let Ok(Some(credential)) = Credential::load() else {
        return input;
    };
    input.snapshot = read_snapshot(workspace, &credential);
    input
}

/// Read only at explicit index/refresh time. A separate thread owns its Tokio
/// runtime because the CLI dispatcher itself already runs inside Tokio.
pub(super) fn refresh(workspace: &Workspace) -> HostedInput {
    let (remotes, unnamed) = origins_of(workspace);
    let mut input = HostedInput {
        remotes,
        unnamed,
        ..Default::default()
    };
    let credential = match Credential::load() {
        Ok(Some(c)) => c,
        Ok(None) => return input,
        Err(e) => {
            input.failure = Some(e);
            return input;
        }
    };
    let cache = workspace.index_dir().join("hosted/snapshot.json");
    let old = read_snapshot(workspace, &credential);
    let repos = input.remotes.values().cloned().collect();
    let fallback = old.clone();
    let result = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Could not start hosted reader".to_string())?;
        let reader = Reader::new()?;
        runtime.block_on(reader.refresh(&credential, repos, old))
    })
    .join()
    .unwrap_or_else(|_| Err("Hosted reader failed".into()));
    match result {
        Ok((snapshot, failure, downloads)) => {
            if let Err(error) = persist(&cache, &snapshot) {
                input.failure = Some(error);
            } else {
                input.failure = failure;
            }
            input.snapshot = Some(snapshot);
            input.downloads = Some(downloads);
        }
        Err(error) => {
            input.snapshot = if error.contains("credential workspace changed") {
                None
            } else {
                fallback
            };
            input.failure = Some(error);
        }
    }
    input
}

/// What each repo of the workspace is called, split by whether this machine
/// could read a name at all: the `owner/repo` names, and the repos that gave
/// none with the remote they carry.
pub(super) fn origins_of(
    workspace: &Workspace,
) -> (BTreeMap<PathBuf, String>, BTreeMap<PathBuf, Option<String>>) {
    let mut named = BTreeMap::new();
    let mut unnamed = BTreeMap::new();
    for path in &workspace.repos {
        match origin(path) {
            Some(origin) => match origin.name {
                Some(name) => {
                    named.insert(path.clone(), name);
                }
                None => {
                    unnamed.insert(path.clone(), Some(origin.url));
                }
            },
            None => {
                unnamed.insert(path.clone(), None);
            }
        }
    }
    (named, unnamed)
}

/// The snapshot on disk, if it belongs to this credential and this
/// authenticated workspace and this build can still parse its resolution.
fn read_snapshot(workspace: &Workspace, credential: &Credential) -> Option<Snapshot> {
    std::fs::read(workspace.index_dir().join("hosted/snapshot.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|s: &Snapshot| {
            s.identity == credential.identity()
                && serde_json::to_value(&s.resolution)
                    .ok()
                    .and_then(|v| Resolution::parse(v).ok())
                    .is_some()
                && credential
                    .workspace_slug
                    .as_ref()
                    .is_none_or(|w| *w == s.resolution.workspace.slug)
        })
}

fn persist(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let dir = path.parent().ok_or("Invalid hosted cache directory")?;
    std::fs::create_dir_all(dir).map_err(|_| "Could not create hosted cache")?;
    let pending = dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&pending)
            .map_err(|_| "Could not write hosted cache")?;
        let json = serde_json::to_vec(snapshot).map_err(|_| "Could not serialize hosted cache")?;
        file.write_all(&json)
            .map_err(|_| "Could not write hosted cache")?;
        std::fs::rename(&pending, path).map_err(|_| "Could not replace hosted cache")
    })();
    let _ = std::fs::remove_file(pending);
    result.map_err(str::to_string)
}

impl HostedInput {
    pub(super) fn checked_at(&self) -> Option<String> {
        self.snapshot.as_ref().map(|s| s.checked_at.clone())
    }

    /// What this run did about downloading, for the build to print. `None`
    /// when no hosted read was made at all — nobody signed in, or the metadata
    /// read itself failed, in which case no project was ever considered and a
    /// count of zero would read as "nothing had to be fetched".
    pub(super) fn download_line(&self) -> Option<String> {
        self.downloads.map(|downloads| downloads.line())
    }

    fn repo(&self, path: &Path) -> Option<&ResolvedRepo> {
        let name = self.remotes.get(path)?;
        self.snapshot
            .as_ref()?
            .resolution
            .repos
            .iter()
            .find(|r| r.full_name.eq_ignore_ascii_case(name))
    }

    pub(super) fn local_blobs(&self, path: &Path) -> Vec<CloudRepoData> {
        let Some(repo) = self.repo(path).filter(|r| r.connected) else {
            return Vec::new();
        };
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let Some(membership) = snapshot
            .resolution
            .project_repos
            .iter()
            .find(|p| Some(&p.project_slug) == repo.project_slug.as_ref())
        else {
            return Vec::new();
        };
        let basename = repo.full_name.rsplit('/').next().unwrap_or_default();
        let owners: Vec<_> = membership
            .repos
            .iter()
            .filter(|r| {
                r.rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(basename))
            })
            .collect();
        if owners.len() != 1 || !owners[0].eq_ignore_ascii_case(&repo.full_name) {
            return Vec::new();
        }
        self.snapshot
            .as_ref()
            .and_then(|s| s.projects.get(repo.project_id.as_ref()?))
            .into_iter()
            .flatten()
            .filter(|b| {
                repo.full_name
                    .rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&b.repo_name))
            })
            .filter(|b| {
                let key = service_of(b);
                repo.services
                    .as_ref()
                    .is_some_and(|ss| ss.iter().any(|s| s.service == key))
            })
            .cloned()
            .map(|mut b| {
                b.repo_name = super::index::repo_label(path);
                b
            })
            .collect()
    }

    pub(super) fn service(&self, path: &Path, blob: &CloudRepoData) -> ServiceEnrichment {
        let mut result = ServiceEnrichment {
            remote: self.remotes.get(path).cloned(),
            failure: self.failure.clone(),
            ..Default::default()
        };
        let Some(snapshot) = &self.snapshot else {
            if self.failure.is_some() {
                result.hosted_state = HostedState::ReadFailed;
            }
            return result;
        };
        result.allowance_sentence = snapshot.resolution.allowance_sentence.clone();
        let Some(repo) = self.repo(path).filter(|r| r.connected) else {
            // Two different answers wear one sentence otherwise: the cloud
            // saying it does not hold this repo, and this machine never having
            // had a name to ask about (carrick#1056). The remote rides along
            // so the surface can name what it read.
            if let Some(remote) = self.unnamed.get(path) {
                result.hosted_state = HostedState::RemoteUnnamed;
                result.remote = remote.clone();
            } else {
                result.hosted_state = HostedState::NotConnected;
            }
            return result;
        };
        let previous = self
            .local_blobs(path)
            .into_iter()
            .find(|b| b.service_name == blob.service_name);
        let Some(previous) = previous else {
            result.hosted_state = if repo.services.as_ref().is_some_and(Vec::is_empty) {
                HostedState::NoIndexYet
            } else if self.failure.is_some() {
                HostedState::ReadFailed
            } else if repo
                .services
                .as_ref()
                .is_some_and(|services| !services.is_empty())
            {
                // Name both sides, in the names a reader wrote: the hosted
                // blobs' own `service_name`s, not the row slugs. The index is
                // keyed on the service name, so a package renamed since the
                // hosted commit, or a `serviceName` other than the one the
                // hosted scan read, matches nothing (carrick#1023 item 12).
                let local = blob.service_name.as_deref().unwrap_or(&blob.repo_name);
                let mut hosted = self
                    .local_blobs(path)
                    .iter()
                    .map(|b| {
                        b.service_name
                            .clone()
                            .unwrap_or_else(|| b.repo_name.clone())
                    })
                    .collect::<Vec<_>>();
                if hosted.is_empty() {
                    // Rows with no blob behind them: the row keys are all
                    // there is to name.
                    hosted = repo
                        .services
                        .iter()
                        .flatten()
                        .map(|service| service.service.clone())
                        .collect();
                }
                hosted.sort();
                result.failure = Some(format!(
                    "local service `{local}` has no hosted match; the hosted index holds [{}]. \
                     The index matches services by name, and this checkout names this one \
                     differently from the scan that wrote the hosted index (a renamed package, \
                     or a different `serviceName` in carrick.json)",
                    hosted.join(", ")
                ));
                HostedState::ReadFailed
            } else {
                HostedState::NoIndexYet
            };
            return result;
        };
        result.hosted_cache_version = previous.cache_version;
        // Provenance comes from the resolve-repos ROW, not from the blob: the
        // cloud derives `source` and `uploaded_by` from the credential that
        // wrote the row and never echoes what a client claimed, so the row is
        // the only place they can be read honestly (§1.4).
        // `previous` was relabelled to the local directory, so the fallback
        // key is the hosted repo's basename, as the cloud stored it.
        let key = stored_service(
            previous.service_name.as_deref(),
            repo.full_name.rsplit('/').next().unwrap_or_default(),
        );
        let row = repo
            .services
            .as_ref()
            .and_then(|services| services.iter().find(|service| service.service == key));
        result.hosted = Some(HostedProvenance {
            commit: previous.commit_hash.clone(),
            indexed_at: previous.last_updated.to_rfc3339(),
            scanner_version: previous.scanner_version.clone(),
            project: repo.project_slug.clone().unwrap_or_default(),
            source: row.and_then(|service| service.source.clone()),
            uploaded_by: row.and_then(|service| service.uploaded_by.clone()),
            dirty: row.and_then(|service| service.dirty),
        });
        result.hosted_state = if previous.cache_version != Some(crate::engine::CACHE_VERSION) {
            HostedState::VersionMismatch
        } else if super::query::changed_since(path, &previous.commit_hash).is_none() {
            HostedState::CommitMissing
        } else if previous.dirty == Some(true) || row.and_then(|s| s.dirty) == Some(true) {
            // A dirty generation deliberately carries no answers to replay
            // (§2.4), so this is a state, not a failure to explain away as a
            // broken read. Saying which one it is stops a user chasing a
            // corrupt index that is behaving exactly as designed.
            //
            // The provenance carries it too, because the sentence the reader
            // gets is chosen on that field: a generation the BLOB knows is
            // dirty must not read as clean just because the stored row was
            // written before `dirty` existed (carrick#1255).
            if let Some(hosted) = result.hosted.as_mut() {
                hosted.dirty = Some(true);
            }
            result.failure =
                Some("Hosted index was written from a tree with uncommitted changes".into());
            HostedState::ReadFailed
        } else {
            // A clean generation with no file answers asked the analyzer about
            // no file, and the scan replays it like any other (carrick#1746).
            HostedState::Enriched
        };
        result
    }

    /// The enrichment for a service THIS RUN scanned and uploaded.
    ///
    /// The snapshot every other answer here is read from was taken before the
    /// scan ran, so on a first paid run it says the repo has no hosted index —
    /// and kept saying it, in the completed run's own output, in `carrick
    /// status` and in both hooks, while the index it had just written was
    /// being served (carrick#1007 item 1). The run that uploaded is the one
    /// thing that knows better, and it knows without asking: a laptop scan
    /// that exits successfully has written this exact blob to the cloud (the
    /// tee propagates the cloud's error, so a refused upload fails the scan).
    ///
    /// Everything stated here is read off that blob or off the pre-scan
    /// snapshot; nothing is guessed. A dirty generation carries no replayable
    /// answers by design, so it keeps the sentence that says so.
    pub(super) fn uploaded(&self, path: &Path, blob: &CloudRepoData) -> ServiceEnrichment {
        /// Did the cloud refuse this write as already current, leaving the
        /// stored row in place (carrick#1214)?
        ///
        /// The commit alone does not answer it. The ingest's `isIndexCurrent`
        /// (carrick-cloud `lambdas/check-or-upload/index_freshness.js`) keeps a
        /// stored row only when the hash matches AND it was written by this
        /// scanner release AND neither side is dirty; a row from an older
        /// release, or from no release at all (a row written before the field
        /// existed), is stale and is rewritten. A laptop caller also supersedes
        /// a CI row at the same commit only through those same clauses, so
        /// matching them here is what keeps `carrick status` describing the row
        /// that is actually being served.
        ///
        /// Conservative in one direction on purpose: every clause the cloud
        /// checks and this cannot see (its own view of the stored row) can only
        /// make the write land, which is the case that stamps this run.
        fn write_was_refused(hosted: &HostedProvenance, blob: &CloudRepoData) -> bool {
            hosted.commit == blob.commit_hash
                && hosted.scanner_version.as_deref() == Some(env!("CARGO_PKG_VERSION"))
                && hosted.dirty != Some(true)
                && blob.dirty != Some(true)
        }

        // Unless the write was refused as already current, in which case the
        // row being served is the older one — possibly CI's, with a login and
        // a source of its own — and stamping "by a laptop scan" over it would
        // be a claim this run did not earn.
        let previous = self.service(path, blob);
        if previous
            .hosted
            .as_ref()
            .is_some_and(|hosted| write_was_refused(hosted, blob))
        {
            // And the run says so. This is the case carrick#1255 is about: the
            // scan ran, the local output shows whatever it improved, and the
            // index an agent reads did not move at all.
            return ServiceEnrichment {
                write_refused_as_current: true,
                ..previous
            };
        }
        let mut result = ServiceEnrichment {
            remote: self.remotes.get(path).cloned(),
            failure: self.failure.clone(),
            allowance_sentence: self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.resolution.allowance_sentence.clone()),
            hosted_cache_version: blob.cache_version,
            hosted: Some(HostedProvenance {
                commit: blob.commit_hash.clone(),
                indexed_at: blob.last_updated.to_rfc3339(),
                scanner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                project: self
                    .repo(path)
                    .and_then(|repo| repo.project_slug.clone())
                    .unwrap_or_default(),
                // Server-derived everywhere else, and here it is what this
                // process did: a laptop scan, by whoever this credential is.
                source: Some("laptop".to_string()),
                uploaded_by: None,
                dirty: blob.dirty,
            }),
            hosted_state: HostedState::Enriched,
            // This process ran the model over this service and wrote the blob,
            // whatever becomes of the hosted row below (carrick#1023 item 14).
            classified_here: true,
            // The refusal is answered above; reaching here means the write
            // landed.
            write_refused_as_current: false,
        };
        if blob.dirty == Some(true) {
            result.failure =
                Some("Hosted index was written from a tree with uncommitted changes".into());
            result.hosted_state = HostedState::ReadFailed;
        }
        result
    }

    pub(super) fn remote_blobs(&self) -> Vec<(String, CloudRepoData)> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (id, blobs) in &snapshot.projects {
            let Some(slug) = snapshot
                .resolution
                .repos
                .iter()
                .find(|r| r.connected && r.project_id.as_ref() == Some(id))
                .and_then(|r| r.project_slug.as_ref())
            else {
                continue;
            };
            let Some(membership) = snapshot
                .resolution
                .project_repos
                .iter()
                .find(|p| &p.project_slug == slug)
            else {
                continue;
            };
            for blob in blobs {
                let owners: Vec<_> = membership
                    .repos
                    .iter()
                    .filter(|r| {
                        r.rsplit('/')
                            .next()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&blob.repo_name))
                    })
                    .collect();
                // Ambiguous basenames cannot establish an owner/repo identity.
                if owners.len() != 1 {
                    continue;
                }
                let name = owners[0];
                if self
                    .remotes
                    .values()
                    .any(|local| local.eq_ignore_ascii_case(name))
                {
                    continue;
                }
                result.push((name.clone(), blob.clone()));
            }
        }
        result
    }

    /// Every blob the cached snapshot holds, borrowed, each with the repo
    /// label a reader of this workspace knows it by: the checkout's folder name
    /// ([`super::index::repo_label`]) for a repo checked out here, as
    /// [`Self::local_blobs`] labels it, and the indexed repo name otherwise.
    /// Nothing is filtered: a reader that wants every indexed function, such
    /// as the edit-time lead, reads them all.
    pub(super) fn every_blob(&self) -> Vec<(String, &CloudRepoData)> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        snapshot
            .projects
            .values()
            .flatten()
            .map(|blob| {
                let local = self.remotes.iter().find(|(_, remote)| {
                    remote
                        .rsplit('/')
                        .next()
                        .is_some_and(|n| n.eq_ignore_ascii_case(&blob.repo_name))
                });
                let label = match local {
                    Some((path, _)) => super::index::repo_label(path),
                    None => blob.repo_name.clone(),
                };
                (label, blob)
            })
            .collect()
    }
}

/// Explicit per-repo handoff to the scan subprocess.
///
/// Read only when local storage is selected and the run is one the indexer
/// drives — a facts-only pass or a laptop scan. It cannot change CI, which
/// names no cache directory and sets neither flag.
///
/// The laptop scan needs it for a different reason than the facts-only pass
/// does: its cross-repo download is the isolated local one, so without this
/// there is no previous generation at all and every laptop rescan is a cold,
/// paid one.
pub(super) const PREVIOUS_ENV: &str = "CARRICK_LOCAL_HOSTED_PREVIOUS";

/// Whether the local indexer drives this scan: local storage, and a
/// facts-only pass or a laptop scan. Such a run is the laptop's, whatever
/// its storage answers about the index, so it ends as a laptop run does
/// (carrick#3992).
pub(crate) fn driven_by_the_indexer() -> bool {
    (super::no_model() || crate::cloud_storage::laptop_scan_requested())
        && std::env::var_os(crate::cloud_storage::CACHE_DIR_ENV).is_some()
}
pub(crate) fn previous_data() -> Result<Option<Vec<CloudRepoData>>, Box<dyn std::error::Error>> {
    if !driven_by_the_indexer() {
        return Ok(None);
    }
    let Some(path) = std::env::var_os(PREVIOUS_ENV) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_slice(&std::fs::read(path)?)?))
}

impl HostedInput {
    pub(super) fn identity(&self) -> (Option<String>, Option<String>) {
        match &self.snapshot {
            Some(snapshot) => (
                Some(snapshot.identity.clone()),
                Some(snapshot.resolution.workspace.slug.clone()),
            ),
            None => (None, None),
        }
    }

    /// Changes to identity, membership, or hosted bytes require all local
    /// services to rebuild their replay. A new check timestamp alone does not.
    pub(super) fn source_key(&self) -> Option<String> {
        let snapshot = self.snapshot.as_ref()?;
        // Value canonicalises nested HashMap keys before hashing; raw struct
        // serialization would change order when a cached blob is reloaded.
        let value = serde_json::to_value((
            &snapshot.identity,
            &snapshot.resolution,
            &snapshot.projects,
            &self.remotes,
        ))
        .ok()?;
        let bytes = serde_json::to_vec(&value).ok()?;
        Some(format!("{:x}", Sha256::digest(bytes)))
    }
}

/// Validate persisted hosted answers against the currently selected local
/// credential without making a network request. Revocation is still checked
/// by the server at the next explicit index/refresh.
pub(super) fn can_read_index(index: &super::read_model::LocalIndex) -> bool {
    if index.hosted_source_key.is_none() {
        return true;
    }
    let Ok(Some(credential)) = Credential::load() else {
        return false;
    };
    index.hosted_identity.as_deref() == Some(credential.identity().as_str())
        && credential
            .workspace_slug
            .as_ref()
            .is_none_or(|slug| Some(slug) == index.hosted_workspace.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_state::tests::committed_repo;
    use serde_json::json;
    use std::io::{Read, Write};

    fn credential(token: &str) -> Credential {
        Credential {
            scope: None,
            api_base: API_BASE.into(),
            token: token.into(),
            workspace_slug: None,
            obtained_at: String::new(),
        }
    }

    fn resolution() -> serde_json::Value {
        json!({"schema":"carrick.resolve-repos/0", "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
            "allowance_sentence":null,"repos":[{"full_name":"example/api","connected":true,"project_id":"p","project_slug":"fixture","services":[]}],
            "project_repos":[{"project_slug":"fixture","repos":["example/api"]}]})
    }

    /// The stored generation as the cloud holds it, stamped with the release
    /// that wrote it — which is where `service()` reads the hosted provenance
    /// version from.
    fn blob_from(version: Option<&str>) -> CloudRepoData {
        let mut stored = blob();
        stored.scanner_version = version.map(str::to_string);
        stored
    }

    fn blob() -> CloudRepoData {
        serde_json::from_value(json!({"repo_name":"api", "service_name":"api", "endpoints":[],"calls":[],"mounts":[],"apps":{},"imported_handlers":[],"function_definitions":{},"last_updated":"2026-09-09T10:00:00Z","commit_hash":"abcdef", "file_results":{},"cache_version":crate::engine::CACHE_VERSION})).unwrap()
    }

    /// One bounded mock HTTP exchange, returning the captured request. The
    /// socket timeout prevents a failed assertion from leaving a worker alive.
    fn serve(
        status: &str,
        headers: &str,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(5), "no mock request");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            // On macOS an accepted socket inherits the listener's
            // non-blocking flag, and a read before the request has arrived
            // then fails with WouldBlock (carrick#1633). The read timeout
            // below is the bound.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = socket.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|n| n.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(head.as_bytes()).unwrap();
            socket.write_all(&body).unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, handle)
    }

    /// A bounded mock that answers `replies` in order and returns every
    /// captured request. One refresh posts `resolve-repos` and then one
    /// `get-cross-repo-data` per project it decides to download, all to this
    /// one endpoint, so the request LOG is what proves a download was skipped:
    /// a run that asks for more than `replies.len()` finds the listener gone
    /// and fails, rather than quietly passing on the counts alone.
    fn serve_sequence(
        replies: Vec<(&str, Vec<u8>)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let replies: Vec<(String, Vec<u8>)> = replies
            .into_iter()
            .map(|(status, body)| (status.to_string(), body))
            .collect();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in replies {
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let start = std::time::Instant::now();
                let mut socket = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(start.elapsed() < Duration::from_secs(5), "no mock request");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                // As in `serve` (carrick#1633).
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = socket.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]);
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                socket.write_all(head.as_bytes()).unwrap();
                socket.write_all(&body).unwrap();
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });
        (url, handle)
    }

    fn action(request: &str) -> serde_json::Value {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    /// When every fixture row was stored, unless a test moves one.
    const STORED_AT: &str = "2026-10-01T10:00:00.000Z";

    fn service_row(service: &str, hash: &str) -> serde_json::Value {
        json!({"service":service,"hash":hash,"updated_at":STORED_AT,"scanner_version":null})
    }

    /// A `get-cross-repo-data` body answering `blobs`, each beside the
    /// `updatedAt` of the row it is stored under.
    fn answered(blobs: &[(CloudRepoData, &str)]) -> Vec<u8> {
        serde_json::to_vec(&json!({"repos": blobs
            .iter()
            .map(|(blob, at)| json!({"repo": blob.repo_name, "hash": blob.commit_hash,
                "metadata": blob, "lastUpdated": at}))
            .collect::<Vec<_>>()}))
        .unwrap()
    }

    /// The rows `blobs` were downloaded as when every one was answered beside
    /// `at`: what a refresh records for them.
    fn read_as(blobs: &[CloudRepoData], at: &str) -> Vec<StoredRow> {
        blobs
            .iter()
            .map(|blob| StoredRow {
                repo: blob.repo_name.to_ascii_lowercase(),
                service: service_of(blob),
                hash: blob.commit_hash.clone(),
                updated_at: Some(at.to_string()),
            })
            .collect()
    }

    /// Two connected repos in two projects: `example/api` (project `pa`, two
    /// services) and `example/web` (project `pb`, one).
    fn two_projects(web_hash: &str) -> serde_json::Value {
        json!({"schema":"carrick.resolve-repos/0",
            "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
            "allowance_sentence":null,
            "repos":[
                {"full_name":"example/api","connected":true,"project_id":"pa","project_slug":"sa",
                 "services":[service_row("api","abcdef"), service_row("worker","fedcba")]},
                {"full_name":"example/web","connected":true,"project_id":"pb","project_slug":"sb",
                 "services":[service_row("web", web_hash)]}],
            "project_repos":[{"project_slug":"sa","repos":["example/api"]},
                             {"project_slug":"sb","repos":["example/web"]}]})
    }

    fn named_blob(repo: &str, service: &str, commit: &str) -> CloudRepoData {
        let mut blob = blob();
        blob.repo_name = repo.to_string();
        blob.service_name = Some(service.to_string());
        blob.commit_hash = commit.to_string();
        blob
    }

    /// A snapshot holding `projects`, every blob downloaded as a row stored at
    /// [`STORED_AT`].
    fn snapshot(
        resolution: serde_json::Value,
        projects: BTreeMap<String, Vec<CloudRepoData>>,
    ) -> Snapshot {
        Snapshot {
            identity: credential("test").identity(),
            checked_at: "old".into(),
            resolution: Resolution::parse(resolution).unwrap(),
            stored_rows: projects
                .iter()
                .map(|(id, blobs)| (id.clone(), read_as(blobs, STORED_AT)))
                .collect(),
            projects,
        }
    }

    fn both_repos() -> Vec<String> {
        vec!["example/api".into(), "example/web".into()]
    }

    fn matching_cache() -> BTreeMap<String, Vec<CloudRepoData>> {
        BTreeMap::from([
            (
                "pa".to_string(),
                vec![
                    named_blob("api", "api", "abcdef"),
                    named_blob("api", "worker", "fedcba"),
                ],
            ),
            ("pb".to_string(), vec![named_blob("web", "web", "old-web")]),
        ])
    }

    fn reader(endpoint: String) -> Reader {
        Reader {
            client: reqwest::Client::builder()
                .no_proxy()
                .gzip(true)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            endpoint,
            allow_http_staging: true,
        }
    }

    #[tokio::test]
    async fn forced_gzip_is_http_decoded_and_project_id_is_sent_with_bearer() {
        let payload = serde_json::to_vec(&json!({"repos":[{"metadata":blob()}]})).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&payload).unwrap();
        let (url, request) = serve(
            "200 OK",
            "Content-Encoding: gzip\r\n",
            encoder.finish().unwrap(),
        );
        let result = reader(url)
            .project(&credential("test-secret"), "authorised-project")
            .await
            .unwrap();
        assert_eq!(result.blobs.len(), 1);
        let request = request.join().unwrap();
        assert!(request.contains("authorization: Bearer test-secret"));
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"action":"get-cross-repo-data","project_id":"authorised-project"})
        );
    }

    /// A blob answered with no stored time, or with one that is not a string,
    /// is still read; only the record of it says "cannot compare".
    #[tokio::test]
    async fn a_stored_time_that_is_missing_or_not_a_string_still_reads_the_blob() {
        let (url, request) = serve(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"repos":[
                {"metadata": named_blob("api","api","abcdef"), "lastUpdated": 1_727_800_000},
                {"metadata": named_blob("api","worker","fedcba")}]}))
            .unwrap(),
        );
        let project = reader(url)
            .project(&credential("test-secret"), "p")
            .await
            .unwrap();
        request.join().unwrap();
        assert_eq!(project.blobs.len(), 2);
        assert!(project.rows.iter().all(|row| row.updated_at.is_none()));
    }

    /// carrick#1740: a sibling's blob carrying a value this build does not
    /// know fails the project's read. When a later release wrote it, the
    /// failure is the upgrade sentence; otherwise it is the fixed one. Neither
    /// echoes the parse error.
    #[tokio::test]
    async fn a_blob_from_a_later_release_fails_the_read_with_the_upgrade() {
        let answer = |version: Option<&str>, source: &str| {
            serde_json::to_vec(&json!({"repos":[{"metadata":
                crate::cloud_storage::sibling_blob_with_source(version, source)}]}))
            .unwrap()
        };
        let (url, request) = serve("200 OK", "", answer(Some("99.0.0"), "whole_url_env"));
        let read = reader(url).project(&credential("test-secret"), "p").await;
        request.join().unwrap();
        assert_eq!(read.unwrap().blobs.len(), 1);

        let later = "a_source_from_a_later_release";
        let (url, request) = serve("200 OK", "", answer(Some("99.0.0"), later));
        let error = reader(url)
            .project(&credential("test-secret"), "p")
            .await
            .unwrap_err();
        request.join().unwrap();
        assert!(
            error.starts_with(
                "The index of service `ledger` of `org/ledger` was written by carrick 99.0.0"
            ),
            "{error}"
        );
        assert!(
            error.contains("Upgrade carrick to 99.0.0 or later"),
            "{error}"
        );
        assert!(!error.contains(later), "{error}");

        for version in [Some(env!("CARGO_PKG_VERSION")), None] {
            let (url, request) = serve("200 OK", "", answer(version, later));
            let error = reader(url)
                .project(&credential("test-secret"), "p")
                .await
                .unwrap_err();
            request.join().unwrap();
            assert_eq!(error, "Invalid hosted project schema", "{version:?}");
        }
    }

    #[tokio::test]
    async fn staged_get_has_no_bearer_or_api_body() {
        let (staged, staged_request) = serve("200 OK", "", answered(&[(blob(), STORED_AT)]));
        let (url, api_request) = serve("200 OK", "", serde_json::to_vec(&json!({"staged":true,"staged_url":format!("{staged}/signed?signature=fixture"),"raw_bytes":1,"repo_count":1})).unwrap());
        let project = reader(url)
            .project(&credential("test-secret"), "p")
            .await
            .unwrap();
        assert_eq!(project.blobs.len(), 1);
        // The staged body is the inline body, so it carries each row's stored
        // time too, and that is what the blob is recorded as (carrick#1673).
        assert_eq!(project.rows, read_as(&project.blobs, STORED_AT));
        assert!(api_request.join().unwrap().contains("Bearer test-secret"));
        let request = staged_request.join().unwrap();
        assert!(request.starts_with("GET /signed?signature=fixture HTTP/1.1"));
        assert!(!request.to_ascii_lowercase().contains("authorization"));
        assert!(!request.contains("test-secret"));
    }

    #[tokio::test]
    async fn redirects_are_not_followed_for_api_or_staged_requests() {
        let forbidden = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        forbidden.set_nonblocking(true).unwrap();
        let target = format!("http://{}/secret", forbidden.local_addr().unwrap());
        let (url, request) = serve("302 Found", &format!("Location: {target}\r\n"), Vec::new());
        assert!(
            reader(url)
                .project(&credential("test-secret"), "p")
                .await
                .unwrap_err()
                .contains("302")
        );
        request.join().unwrap();
        assert!(forbidden.accept().is_err());
        let (staged, staged_request) = serve(
            "307 Temporary Redirect",
            &format!("Location: {target}\r\n"),
            Vec::new(),
        );
        let (url, api_request) = serve(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"staged":true,"staged_url":staged})).unwrap(),
        );
        assert!(
            reader(url)
                .project(&credential("test-secret"), "p")
                .await
                .unwrap_err()
                .contains("307")
        );
        api_request.join().unwrap();
        assert!(!staged_request.join().unwrap().contains("test-secret"));
        assert!(forbidden.accept().is_err());
    }

    #[tokio::test]
    async fn malformed_gzip_staging_and_project_schemas_fail_closed() {
        for (headers, body) in [
            ("Content-Encoding: gzip\r\n", b"not gzip".to_vec()),
            ("", br#"{"staged":true}"#.to_vec()),
            (
                "",
                br#"{"staged":true,"staged_url":"file:///private/credentials"}"#.to_vec(),
            ),
            ("", br#"{"repos":[{"metadata":null}]}"#.to_vec()),
            (
                "",
                br#"{"repos":[],"processing_errors":[{"repo":"api"}]}"#.to_vec(),
            ),
            ("", br#"{"isBase64Encoded":true,"body":"e30="}"#.to_vec()),
        ] {
            let (url, request) = serve("200 OK", headers, body);
            let error = reader(url)
                .project(&credential("test-secret"), "p")
                .await
                .unwrap_err();
            assert!(!error.contains("test-secret"));
            request.join().unwrap();
        }
    }

    #[test]
    fn resolve_schema_is_pinned_and_nullable_allowance_is_required() {
        assert!(Resolution::parse(resolution()).is_ok());
        let mut missing = resolution();
        missing
            .as_object_mut()
            .unwrap()
            .remove("allowance_sentence");
        assert!(Resolution::parse(missing).is_err());
        for (key, value) in [
            ("schema", json!("carrick.resolve-repos/1")),
            (
                "repos",
                json!([{"full_name":"example/api","connected":true}]),
            ),
        ] {
            let mut wrong = resolution();
            wrong[key] = value;
            assert!(Resolution::parse(wrong).is_err());
        }
    }

    #[tokio::test]
    async fn successful_resolution_removes_disconnected_project_cache() {
        let mut metadata = resolution();
        metadata["repos"] = json!([{"full_name":"example/api","connected":false}]);
        metadata["project_repos"] = json!([]);
        let old = Snapshot {
            identity: credential("test").identity(),
            checked_at: "old".into(),
            resolution: Resolution::parse(resolution()).unwrap(),
            projects: BTreeMap::from([("p".into(), vec![blob()])]),
            stored_rows: BTreeMap::new(),
        };
        let (url, request) = serve("200 OK", "", serde_json::to_vec(&metadata).unwrap());
        let (new, failure, _) = reader(url)
            .refresh(&credential("test"), vec!["example/api".into()], Some(old))
            .await
            .unwrap();
        assert!(new.projects.is_empty());
        assert!(failure.is_none());
        request.join().unwrap();
    }

    /// A project whose every service still carries the hash the cached blobs
    /// hold is not downloaded again (carrick#1012 item 2). The download is per
    /// project because `get-cross-repo-data` answers with a whole project and
    /// takes no filter; the counts are in service rows, which is what the
    /// hashes are per.
    #[tokio::test]
    async fn only_the_project_whose_hashes_moved_is_downloaded_again() {
        let old = snapshot(two_projects("old-web"), matching_cache());
        let cached = old.projects["pa"].clone();
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&two_projects("new-web")).unwrap(),
            ),
            (
                "200 OK",
                serde_json::to_vec(
                    &json!({"repos":[{"metadata": named_blob("web","web","new-web")}]}),
                )
                .unwrap(),
            ),
        ]);
        let (new, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(
            downloads,
            Downloads {
                fetched: 1,
                reused: 2,
                unread: 0
            }
        );
        assert_eq!(
            downloads.line(),
            "hosted index: downloaded 1 service(s), reused 2 unchanged"
        );
        let requests = requests.join().unwrap();
        assert_eq!(requests.len(), 2, "the unchanged project was downloaded");
        assert_eq!(
            action(&requests[1]),
            json!({"action":"get-cross-repo-data","project_id":"pb"})
        );
        // The skipped project keeps exactly the blobs it already had, and the
        // fetched one holds what the download answered.
        assert_eq!(
            serde_json::to_value(&new.projects["pa"]).unwrap(),
            serde_json::to_value(&cached).unwrap()
        );
        assert_eq!(new.projects["pb"][0].commit_hash, "new-web");
    }

    /// No snapshot, and a snapshot whose hashes have all moved, both download
    /// everything — the behaviour before the skip existed.
    #[tokio::test]
    async fn everything_is_downloaded_with_no_snapshot_and_when_every_hash_moved() {
        let project = |repo: &str, service: &str| {
            serde_json::to_vec(&json!({"repos":[{"metadata": named_blob(repo, service, "x")}]}))
                .unwrap()
        };
        for old in [
            None,
            Some(snapshot(
                two_projects("old-web"),
                BTreeMap::from([
                    (
                        "pa".to_string(),
                        vec![
                            named_blob("api", "api", "moved"),
                            named_blob("api", "worker", "moved"),
                        ],
                    ),
                    ("pb".to_string(), vec![named_blob("web", "web", "moved")]),
                ]),
            )),
        ] {
            let (url, requests) = serve_sequence(vec![
                (
                    "200 OK",
                    serde_json::to_vec(&two_projects("new-web")).unwrap(),
                ),
                ("200 OK", project("api", "api")),
                ("200 OK", project("web", "web")),
            ]);
            let (_, failure, downloads) = reader(url)
                .refresh(&credential("test"), both_repos(), old)
                .await
                .unwrap();
            assert!(failure.is_none());
            assert_eq!(
                downloads,
                Downloads {
                    fetched: 3,
                    reused: 0,
                    unread: 0
                }
            );
            let requests = requests.join().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(
                action(&requests[1]),
                json!({"action":"get-cross-repo-data","project_id":"pa"})
            );
        }
    }

    /// `resolve-repos` answers `services` for the repos the request named and
    /// for no others, so a project holding a repo that is not on this machine
    /// has a blob nothing here can compare. It downloads every time, even when
    /// every hash it CAN see still matches.
    #[tokio::test]
    async fn a_project_holding_a_repo_that_is_not_local_is_always_downloaded() {
        let mut metadata = two_projects("old-web");
        metadata["project_repos"][0]["repos"] = json!(["example/api", "example/sibling"]);
        let old = snapshot(metadata.clone(), matching_cache());
        let (url, requests) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&metadata).unwrap()),
            (
                "200 OK",
                serde_json::to_vec(
                    &json!({"repos":[{"metadata": named_blob("api","api","abcdef")}]}),
                )
                .unwrap(),
            ),
        ]);
        let (_, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(
            downloads,
            Downloads {
                fetched: 2,
                reused: 1,
                unread: 0
            }
        );
        let requests = requests.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            action(&requests[1]),
            json!({"action":"get-cross-repo-data","project_id":"pa"})
        );
    }

    /// A skipped download and a failed one both leave the cached blobs
    /// serving. The counts are the only thing that tells them apart, so a
    /// failed project is counted apart from both and named in the line.
    #[tokio::test]
    async fn a_failed_download_is_counted_apart_from_a_skipped_one() {
        let old = snapshot(two_projects("old-web"), matching_cache());
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&two_projects("new-web")).unwrap(),
            ),
            ("500 Internal Server Error", Vec::new()),
        ]);
        let (new, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_some_and(|failure| failure.contains("500")));
        assert_eq!(
            downloads,
            Downloads {
                fetched: 0,
                reused: 2,
                unread: 1
            }
        );
        assert_eq!(
            downloads.line(),
            "hosted index: downloaded 0 service(s), reused 2 unchanged, 1 not re-read (the \
             download failed)"
        );
        // The failed project keeps the copy it had; nothing about it was read.
        assert_eq!(new.projects["pb"][0].commit_hash, "old-web");
        assert_eq!(requests.join().unwrap().len(), 2);
    }

    /// The live rows of [`two_projects`] after the `api` service of
    /// `example/api` was stored again at the SAME commit by another writer: a
    /// CI run, another machine, or this laptop's own last scan. Its hash and
    /// its scanner version are what they were; only `updated_at` moved.
    fn api_rewritten_at_the_same_commit(at: &str) -> serde_json::Value {
        let mut metadata = two_projects("old-web");
        metadata["repos"][0]["services"][0]["updated_at"] = json!(at);
        metadata["repos"][0]["services"][0]["source"] = json!("ci");
        metadata
    }

    /// A rescan at an unchanged commit stores a new generation under the same
    /// hash, and the next read downloads it rather than keeping the first
    /// copy this machine ever downloaded there (carrick#1673). That copy is
    /// what the laptop scan reads as its previous generation, so keeping it
    /// re-asked semantics and wrote its old detection back over the newer one.
    ///
    /// The generation that moved is the one handed to the next scan as its
    /// previous one, and a second read of the same rows downloads nothing.
    #[tokio::test]
    async fn a_rewrite_at_the_same_commit_is_downloaded_and_becomes_the_previous_generation() {
        let later = "2026-10-01T18:10:50.769Z";
        let first = named_blob("api", "api", "abcdef");
        let mut rewritten = named_blob("api", "api", "abcdef");
        rewritten.last_updated = "2026-10-01T18:09:00Z".parse().unwrap();
        let worker = named_blob("api", "worker", "fedcba");
        let old = snapshot(two_projects("old-web"), matching_cache());
        assert_eq!(
            serde_json::to_value(&old.projects["pa"][0]).unwrap(),
            serde_json::to_value(&first).unwrap(),
            "the cached copy is the first one"
        );
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&api_rewritten_at_the_same_commit(later)).unwrap(),
            ),
            (
                "200 OK",
                answered(&[(rewritten.clone(), later), (worker.clone(), STORED_AT)]),
            ),
        ]);
        let (new, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(
            downloads,
            Downloads {
                fetched: 2,
                reused: 1,
                unread: 0
            }
        );
        let requests = requests.join().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "the rewritten project was not downloaded"
        );
        assert_eq!(
            action(&requests[1]),
            json!({"action":"get-cross-repo-data","project_id":"pa"})
        );
        let mut recorded = new.stored_rows["pa"].clone();
        recorded.sort();
        assert_eq!(
            recorded,
            vec![
                StoredRow {
                    repo: "api".into(),
                    service: "api".into(),
                    hash: "abcdef".into(),
                    updated_at: Some(later.into()),
                },
                StoredRow {
                    repo: "api".into(),
                    service: "worker".into(),
                    hash: "fedcba".into(),
                    updated_at: Some(STORED_AT.into()),
                },
            ],
            "the rows the new blobs were answered as are what the next read compares"
        );

        // What `index` hands the laptop scan as its previous generation.
        let input = HostedInput {
            snapshot: Some(new.clone()),
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            ..Default::default()
        };
        let previous = input
            .local_blobs(Path::new("/w/api"))
            .into_iter()
            .find(|blob| blob.service_name.as_deref() == Some("api"))
            .expect("the api service is handed over");
        assert_eq!(previous.last_updated, rewritten.last_updated);

        // Read again with nothing stored since: nothing is downloaded.
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&api_rewritten_at_the_same_commit(later)).unwrap(),
        )]);
        let (again, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(new))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(
            downloads,
            Downloads {
                fetched: 0,
                reused: 3,
                unread: 0
            }
        );
        assert_eq!(requests.join().unwrap().len(), 1);
        assert_eq!(again.projects["pa"][0].last_updated, rewritten.last_updated);
    }

    /// The indexer's progress count reads the list each scan will resolve
    /// (carrick#2221): a refresh counts the hosted configs it hands over, and
    /// a scan that asks the model counts the split derived from the tree.
    #[test]
    fn the_progress_count_reads_the_services_the_scan_resolves() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(
            repo.path().join("package.json"),
            r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        for name in ["a", "b"] {
            let dir = repo.path().join("packages").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"@scope/{name}"}}"#),
            )
            .unwrap();
        }
        let mut hosted = blob();
        hosted.config_json = Some(r#"{"serviceName":"api"}"#.into());
        let mut resolution = resolution();
        resolution["repos"][0]["services"] =
            json!([{"service":"api","hash":"abcdef","updated_at":null,"scanner_version":null}]);
        let input = HostedInput {
            snapshot: Some(snapshot(
                resolution,
                BTreeMap::from([("p".to_string(), vec![hosted])]),
            )),
            remotes: BTreeMap::from([(repo.path().to_path_buf(), "example/api".into())]),
            ..Default::default()
        };
        let repos = [repo.path().to_path_buf()];
        let counts = |pass| super::super::index::workspace_service_counts(&repos, &input, &pass);
        assert_eq!(counts(super::super::index::Pass::Facts), Some(vec![1]));
        assert_eq!(counts(super::super::index::Pass::Infer), Some(vec![2]));
    }

    /// The snapshot a laptop holds when this ships: written before blobs
    /// recorded the row they were downloaded as, with a `resolution` that a
    /// later read already rewrote to the newest rows while its blobs stayed
    /// the first ones downloaded. Nothing says which generation those blobs
    /// are, so the project downloads.
    #[tokio::test]
    async fn a_snapshot_whose_blobs_never_recorded_their_rows_is_downloaded() {
        let later = "2026-10-01T18:10:50.769Z";
        let current = api_rewritten_at_the_same_commit(later);
        let mut written =
            serde_json::to_value(snapshot(current.clone(), matching_cache())).unwrap();
        written.as_object_mut().unwrap().remove("stored_rows");
        let old: Snapshot = serde_json::from_value(written).unwrap();
        assert!(old.stored_rows.is_empty());
        let (url, requests) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&current).unwrap()),
            (
                "200 OK",
                answered(&[
                    (named_blob("api", "api", "abcdef"), later),
                    (named_blob("api", "worker", "fedcba"), STORED_AT),
                ]),
            ),
            (
                "200 OK",
                answered(&[(named_blob("web", "web", "old-web"), STORED_AT)]),
            ),
        ]);
        let (new, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(
            downloads,
            Downloads {
                fetched: 3,
                reused: 0,
                unread: 0
            }
        );
        assert_eq!(requests.join().unwrap().len(), 3);
        assert_eq!(
            new.stored_rows.len(),
            2,
            "both projects now record their rows"
        );
    }

    /// A row with no `updated_at`, or a blob answered without its row's
    /// `lastUpdated`, cannot be compared, and "cannot compare" downloads.
    #[tokio::test]
    async fn a_row_or_a_blob_without_a_stored_time_is_downloaded() {
        // `example/web`'s row carries no stored time.
        let mut rows = two_projects("old-web");
        rows["repos"][1]["services"][0]["updated_at"] = json!(null);
        // `example/api`'s blob was answered without one.
        let mut unstamped = snapshot(two_projects("old-web"), matching_cache());
        unstamped.stored_rows.get_mut("pa").unwrap()[0].updated_at = None;
        for (live, old, project, body, fetched) in [
            (
                rows,
                snapshot(two_projects("old-web"), matching_cache()),
                "pb",
                answered(&[(named_blob("web", "web", "old-web"), STORED_AT)]),
                1,
            ),
            (
                two_projects("old-web"),
                unstamped,
                "pa",
                answered(&[
                    (named_blob("api", "api", "abcdef"), STORED_AT),
                    (named_blob("api", "worker", "fedcba"), STORED_AT),
                ]),
                2,
            ),
        ] {
            let (url, requests) = serve_sequence(vec![
                ("200 OK", serde_json::to_vec(&live).unwrap()),
                ("200 OK", body),
            ]);
            let (_, failure, downloads) = reader(url)
                .refresh(&credential("test"), both_repos(), Some(old))
                .await
                .unwrap();
            assert!(failure.is_none());
            assert_eq!(
                downloads,
                Downloads {
                    fetched,
                    reused: 3 - fetched,
                    unread: 0
                }
            );
            let requests = requests.join().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(
                action(&requests[1]),
                json!({"action":"get-cross-repo-data","project_id":project})
            );
        }
    }

    /// A download that fails keeps the blobs it had AND the rows they were
    /// read as, so the next read still sees that they are behind and tries
    /// again, instead of taking the newer rows as describing the old blobs.
    #[tokio::test]
    async fn a_failed_download_keeps_the_rows_its_blobs_were_read_as() {
        let later = "2026-10-01T18:10:50.769Z";
        let old = snapshot(two_projects("old-web"), matching_cache());
        let kept = old.stored_rows["pa"].clone();
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&api_rewritten_at_the_same_commit(later)).unwrap(),
            ),
            ("500 Internal Server Error", Vec::new()),
        ]);
        let (new, failure, _) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(old))
            .await
            .unwrap();
        assert!(failure.is_some());
        assert_eq!(requests.join().unwrap().len(), 2);
        assert_eq!(new.stored_rows["pa"], kept);

        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&api_rewritten_at_the_same_commit(later)).unwrap(),
            ),
            (
                "200 OK",
                answered(&[
                    (named_blob("api", "api", "abcdef"), later),
                    (named_blob("api", "worker", "fedcba"), STORED_AT),
                ]),
            ),
        ]);
        let (_, failure, downloads) = reader(url)
            .refresh(&credential("test"), both_repos(), Some(new))
            .await
            .unwrap();
        assert!(failure.is_none());
        assert_eq!(downloads.fetched, 2, "the next read tries again");
        assert_eq!(requests.join().unwrap().len(), 2);
    }

    #[test]
    fn local_seeding_requires_unambiguous_full_repository_identity() {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] =
            json!([{"service":"api","hash":"abcdef","updated_at":null,"scanner_version":null}]);
        let mut input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([("p".into(), vec![blob()])]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        assert_eq!(input.local_blobs(Path::new("/w/api")).len(), 1);
        input.snapshot.as_mut().unwrap().resolution.project_repos[0]
            .repos
            .push("another/api".into());
        assert!(input.local_blobs(Path::new("/w/api")).is_empty());
        assert!(input.remote_blobs().is_empty());
    }

    /// A hosted input holding `stored` for `repo` (remote `example/api`) under
    /// one row whose key is `row_service`.
    fn scoped_input(repo: &Path, row_service: &str, stored: CloudRepoData) -> HostedInput {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": row_service, "hash": stored.commit_hash, "updated_at": null,
            "scanner_version": null, "source": "ci", "uploaded_by": "ci-bot", "dirty": false
        }]);
        HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([("p".into(), vec![stored])]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(repo.to_path_buf(), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        }
    }

    /// The cloud keys a row by the slug of its service name and the blob keeps
    /// the name it was scanned under, so `@acme/orders-api` is stored as
    /// `acme-orders-api`. Compared raw, no scoped name found its row and no
    /// hosted answer was replayed onto any checkout.
    #[test]
    fn a_scoped_service_name_finds_the_row_stored_under_its_slug() {
        let (dir, head) = committed_repo(&[("app.ts", "export const value = 1;")]);
        let repo = dir.path();
        let input = scoped_input(
            repo,
            "acme-orders-api",
            named_blob("api", "@acme/orders-api", &head),
        );
        assert_eq!(input.local_blobs(repo).len(), 1);
        let mut local = blob();
        local.service_name = Some("@acme/orders-api".into());
        let answer = input.service(repo, &local);
        assert_eq!(answer.hosted_state, HostedState::Enriched, "{answer:?}");
        assert_eq!(
            answer.hosted.as_ref().and_then(|h| h.source.as_deref()),
            Some("ci")
        );
    }

    /// A package renamed since the hosted scan has no hosted match, and the
    /// reason names both sides in the names a reader wrote, never the slugs.
    #[test]
    fn a_renamed_service_names_both_sides_in_their_own_names() {
        let (dir, head) = committed_repo(&[("app.ts", "export const value = 1;")]);
        let repo = dir.path();
        let input = scoped_input(
            repo,
            "acme-orders-api",
            named_blob("api", "@acme/orders-api", &head),
        );
        let mut local = blob();
        local.service_name = Some("@shop/orders-api".into());
        let answer = input.service(repo, &local);
        assert_eq!(answer.hosted_state, HostedState::ReadFailed);
        let failure = answer.failure.expect("the reason is stated");
        assert!(failure.contains("`@shop/orders-api`"), "{failure}");
        assert!(failure.contains("[@acme/orders-api]"), "{failure}");
        assert!(failure.contains("renamed package"), "{failure}");
    }

    /// The held blobs of a project whose rows are slugs are the generation
    /// the cloud stores, so the read reuses them instead of downloading the
    /// whole project again on every refresh.
    #[test]
    fn a_held_scoped_blob_is_unchanged_against_its_slugged_row() {
        let metadata = json!({"schema":"carrick.resolve-repos/0",
            "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
            "allowance_sentence":null,
            "repos":[{"full_name":"example/api","connected":true,"project_id":"pa",
                "project_slug":"sa","services":[service_row("acme-orders-api","abcdef")]}],
            "project_repos":[{"project_slug":"sa","repos":["example/api"]}]});
        let resolution = Resolution::parse(metadata).unwrap();
        let cached = vec![named_blob("api", "@acme/orders-api", "abcdef")];
        let held_as = read_as(&cached, STORED_AT);
        let requested = std::collections::BTreeSet::from(["example/api".to_string()]);
        assert!(project_is_unchanged(
            &resolution,
            "pa",
            &cached,
            &held_as,
            &requested
        ));
    }

    /// The first paid run's own output used to tell the user to go and run the
    /// scan they had just finished: the snapshot behind every other answer was
    /// read BEFORE the scan, so the repo still had no hosted services in it.
    /// The run that uploaded states what it uploaded (carrick#1007 item 1).
    #[test]
    fn the_run_that_uploaded_states_the_hosted_index_it_just_wrote() {
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                // No services: this repo had no hosted index when the run began.
                resolution: Resolution::parse(resolution()).unwrap(),
                projects: BTreeMap::new(),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let path = Path::new("/w/api");
        let blob = blob();

        let before = input.service(path, &blob);
        assert_eq!(before.hosted_state, HostedState::NoIndexYet);
        let stale = super::super::query::enrichment_note(&before, None, "api");
        assert!(stale.contains("has no hosted index yet"), "{stale}");

        let after = input.uploaded(path, &blob);
        assert_eq!(after.hosted_state, HostedState::Enriched);
        let hosted = after.hosted.as_ref().expect("the uploaded blob is hosted");
        assert_eq!(hosted.commit, "abcdef");
        assert_eq!(hosted.source.as_deref(), Some("laptop"));
        assert_eq!(hosted.project, "fixture");
        assert_eq!(
            hosted.scanner_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let note = super::super::query::enrichment_note(&after, None, "api");
        assert!(!note.contains("has no hosted index yet"), "{note}");
        assert!(note.contains("from the hosted index at abcdef"), "{note}");
        assert!(note.contains("by a laptop scan"), "{note}");
    }

    /// A scan the cloud refuses as already current writes nothing, so the row
    /// being served is the older one — possibly CI's, with a login of its own.
    /// This run did not earn the right to stamp "by a laptop scan" over it.
    ///
    /// The refusal is the ingest's, and its clauses are the commit AND the
    /// scanner release AND neither side dirty (carrick#1214): a row from
    /// another release at this commit is rewritten, and this run is what the
    /// cloud is serving.
    #[test]
    fn a_scan_that_uploaded_nothing_new_keeps_the_row_that_is_served() {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": "api", "hash": "abcdef", "updated_at": null,
            "scanner_version": env!("CARGO_PKG_VERSION"), "source": "ci",
            "uploaded_by": "ci-bot", "dirty": false
        }]);
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([(
                    "p".into(),
                    vec![blob_from(Some(env!("CARGO_PKG_VERSION")))],
                )]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let answer = input.uploaded(Path::new("/w/api"), &blob());
        let hosted = answer.hosted.as_ref().expect("the served row");
        assert_eq!(hosted.source.as_deref(), Some("ci"));
        assert_eq!(hosted.uploaded_by.as_deref(), Some("ci-bot"));

        // A scan at a NEW commit is this run's, and says so.
        let mut moved = blob();
        moved.commit_hash = "fedcba".to_string();
        let after = input.uploaded(Path::new("/w/api"), &moved);
        assert_eq!(
            after.hosted.as_ref().and_then(|h| h.source.as_deref()),
            Some("laptop")
        );
    }

    /// The same commit at a DIFFERENT scanner release is re-indexed by the
    /// cloud, so the row being served is this run's (carrick#1214). A stored
    /// row carrying no release at all is the same case: the ingest compares
    /// the field and rewrites a row that does not match.
    #[test]
    fn a_rescan_at_a_new_release_stamps_its_own_row_at_the_same_commit() {
        let row = |version: Option<&str>| {
            let mut metadata = resolution();
            metadata["repos"][0]["services"] = json!([{
                "service": "api", "hash": "abcdef", "updated_at": "2026-09-01T00:00:00Z",
                "scanner_version": version, "source": "ci", "uploaded_by": "ci-bot",
                "dirty": false
            }]);
            HostedInput {
                snapshot: Some(Snapshot {
                    identity: "test".into(),
                    checked_at: "now".into(),
                    resolution: Resolution::parse(metadata).unwrap(),
                    projects: BTreeMap::from([("p".into(), vec![blob_from(version)])]),
                    stored_rows: BTreeMap::new(),
                }),
                failure: None,
                remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
                unnamed: BTreeMap::new(),
                downloads: None,
            }
        };

        for stored in [Some("0.0.1"), None] {
            let answer = row(stored).uploaded(Path::new("/w/api"), &blob());
            let hosted = answer.hosted.as_ref().expect("this run's row");
            assert_eq!(hosted.source.as_deref(), Some("laptop"), "{stored:?}");
            assert_eq!(
                hosted.scanner_version.as_deref(),
                Some(env!("CARGO_PKG_VERSION")),
                "{stored:?}"
            );
        }
    }

    /// A dirty generation is never current for the cloud, whichever side holds
    /// it, so a rescan at the same commit and release still lands.
    #[test]
    fn a_dirty_row_or_a_dirty_upload_is_this_runs_row() {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": "api", "hash": "abcdef", "updated_at": null,
            "scanner_version": env!("CARGO_PKG_VERSION"), "source": "ci",
            "uploaded_by": "ci-bot", "dirty": true
        }]);
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([(
                    "p".into(),
                    vec![blob_from(Some(env!("CARGO_PKG_VERSION")))],
                )]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let answer = input.uploaded(Path::new("/w/api"), &blob());
        assert_eq!(
            answer.hosted.as_ref().and_then(|h| h.source.as_deref()),
            Some("laptop")
        );
    }

    /// The stored row predates the `dirty` field, and the blob it points at
    /// knows better. The provenance has to carry that, because the sentence a
    /// reader gets is chosen on it (carrick#1255).
    #[test]
    fn a_dirty_blob_under_a_pre_field_row_still_reads_as_dirty() {
        // A real repository at a real commit: the read path answers
        // `CommitMissing` for a clone that does not hold the hosted commit, and
        // that branch is reached before this one.
        let (dir, head) = committed_repo(&[("app.ts", "export const value = 1;")]);
        let repo = dir.path();

        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": "api", "hash": head, "updated_at": null,
            "scanner_version": env!("CARGO_PKG_VERSION"), "source": null,
            "uploaded_by": null, "dirty": null
        }]);
        let mut stored = blob_from(Some(env!("CARGO_PKG_VERSION")));
        stored.commit_hash = head;
        stored.dirty = Some(true);
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([("p".into(), vec![stored])]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(repo.to_path_buf(), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let answer = input.service(repo, &blob());
        assert_eq!(answer.hosted.as_ref().and_then(|h| h.dirty), Some(true));
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(
            note.contains("Written from a tree with uncommitted changes"),
            "{note}"
        );
        assert!(!note.contains("retained"), "{note}");
    }

    /// A clean generation whose scan asked the analyzer about no file stores
    /// no answers, and the next scan replays it all the same (carrick#1746):
    /// it is a hosted index this machine reads, not one it failed to read.
    #[test]
    fn a_clean_generation_with_no_file_answers_is_read() {
        let (dir, head) = committed_repo(&[("slugify.ts", "export const slug = 1;")]);
        let repo = dir.path();

        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": "api", "hash": head, "updated_at": null,
            "scanner_version": env!("CARGO_PKG_VERSION"), "source": "ci",
            "uploaded_by": null, "dirty": false
        }]);
        let mut stored = blob_from(Some(env!("CARGO_PKG_VERSION")));
        stored.commit_hash = head;
        stored.file_results = None;
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([("p".into(), vec![stored])]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(repo.to_path_buf(), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let answer = input.service(repo, &blob());
        assert_eq!(answer.hosted_state, HostedState::Enriched, "{answer:?}");
        assert_eq!(answer.failure, None);
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(!note.contains("could not refresh"), "{note}");
    }

    /// A scan whose write the cloud refuses as already current changed nothing
    /// an agent reads, and the run says so (carrick#1255). The local output is
    /// no signal at all here: it shows whatever this scan improved, at a commit
    /// whose stored row is what is actually being served.
    #[test]
    fn a_write_refused_as_already_current_is_stated() {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([{
            "service": "api", "hash": "abcdef", "updated_at": null,
            "scanner_version": env!("CARGO_PKG_VERSION"), "source": "ci",
            "uploaded_by": "ci-bot", "dirty": false
        }]);
        let input = HostedInput {
            snapshot: Some(Snapshot {
                identity: "test".into(),
                checked_at: "now".into(),
                resolution: Resolution::parse(metadata).unwrap(),
                projects: BTreeMap::from([(
                    "p".into(),
                    vec![blob_from(Some(env!("CARGO_PKG_VERSION")))],
                )]),
                stored_rows: BTreeMap::new(),
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let answer = input.uploaded(Path::new("/w/api"), &blob());
        assert!(answer.write_refused_as_current, "{answer:?}");
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(
            note.contains("Nothing this scan computed was stored"),
            "{note}"
        );
        assert!(
            note.contains("installing dependencies or changing a setting does not move the commit"),
            "{note}"
        );
    }

    /// The cloud's "I do not hold this repo" and the machine's "I never had a
    /// name to ask about" are separate answers and separate sentences
    /// (carrick#1056). Both reach the same place — no hosted rows — and only
    /// one of them is fixed by connecting the repo.
    #[test]
    fn a_repo_whose_remote_names_nothing_is_not_reported_as_unconnected() {
        let signed_in = |remotes: BTreeMap<PathBuf, String>,
                         unnamed: BTreeMap<PathBuf, Option<String>>| {
            HostedInput {
                snapshot: Some(Snapshot {
                    identity: "test".into(),
                    checked_at: "now".into(),
                    resolution: Resolution::parse(resolution()).unwrap(),
                    projects: BTreeMap::new(),
                    stored_rows: BTreeMap::new(),
                }),
                failure: None,
                remotes,
                unnamed,
                downloads: None,
            }
        };
        let path = Path::new("/w/mirror");

        // A name the workspace does not hold: the cloud answered, and this is
        // its answer.
        let named = signed_in(
            BTreeMap::from([(path.to_path_buf(), "other/mirror".into())]),
            BTreeMap::new(),
        );
        let answer = named.service(path, &blob());
        assert_eq!(answer.hosted_state, HostedState::NotConnected);
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(
            note.contains("is not connected to a Carrick project"),
            "{note}"
        );

        // No name at all: nothing was asked, so nothing was refused.
        let unnamed = signed_in(
            BTreeMap::new(),
            BTreeMap::from([(path.to_path_buf(), Some("/srv/mirrors/api".to_string()))]),
        );
        let answer = unnamed.service(path, &blob());
        assert_eq!(answer.hosted_state, HostedState::RemoteUnnamed);
        assert_eq!(answer.remote.as_deref(), Some("/srv/mirrors/api"));
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(
            note.contains("could not read owner/repo from the git remote `/srv/mirrors/api`"),
            "{note}"
        );
        assert!(!note.contains("not connected"), "{note}");

        // No origin remote at all: there is no URL to name, and the sentence
        // says the other thing rather than printing an empty one.
        let none = signed_in(
            BTreeMap::new(),
            BTreeMap::from([(path.to_path_buf(), None)]),
        );
        let answer = none.service(path, &blob());
        assert_eq!(answer.hosted_state, HostedState::RemoteUnnamed);
        assert_eq!(answer.remote, None);
        let note = super::super::query::enrichment_note(&answer, None, "api");
        assert!(note.contains("has no origin remote"), "{note}");
    }

    /// A dirty generation carries no replayable answers by design, and saying
    /// "uploaded, all good" over the top of that would be false.
    #[test]
    fn an_uploaded_dirty_generation_keeps_the_sentence_that_says_so() {
        let input = HostedInput {
            snapshot: None,
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
            unnamed: BTreeMap::new(),
            downloads: None,
        };
        let mut dirty = blob();
        dirty.dirty = Some(true);
        let after = input.uploaded(Path::new("/w/api"), &dirty);
        assert_eq!(after.hosted_state, HostedState::ReadFailed);
        assert!(
            after
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("uncommitted changes")),
            "{after:?}"
        );
        // And never as a refused upload: a dirty run forces the reindex, so
        // this write landed and the index an agent reads is this one. Reading
        // that sentence as a refusal is how carrick#1255 was first diagnosed.
        let note = super::super::query::enrichment_note(&after, None, "api");
        assert!(!note.contains("retained"), "{note}");
        assert!(
            !note.contains("Nothing this scan computed was stored"),
            "{note}"
        );
        assert!(
            note.contains("Written from a tree with uncommitted changes"),
            "{note}"
        );
        assert!(
            note.contains("Commit them; the next scan of main replaces it"),
            "{note}"
        );
    }

    /// A repo whose main branch moved on after the hosted index this machine
    /// holds: commit A, then B changing a manifest (an input of every
    /// service), then the branch's own work at C, checked out. Returns the
    /// directory and the three commits.
    fn moved_on_repo() -> (tempfile::TempDir, String, String, String) {
        moved_on_repo_named("api")
    }

    /// [`moved_on_repo`] for a repo called `name`, whose files say so, so two
    /// such repos never share a commit.
    fn moved_on_repo_named(name: &str) -> (tempfile::TempDir, String, String, String) {
        use crate::git_state::tests::git_in;
        let manifest = format!(r#"{{"name":"{name}","dependencies":{{}}}}"#);
        let (dir, a) = committed_repo(&[
            ("package.json", manifest.as_str()),
            ("src/app.ts", "export const value = 1;"),
        ]);
        std::fs::write(
            dir.path().join("package.json"),
            format!(r#"{{"name":"{name}","dependencies":{{"left-pad":"1.0.0"}}}}"#),
        )
        .unwrap();
        git_in(dir.path(), &["commit", "-qam", "main moves on"]);
        let b = git_in(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("src/app.ts"), "export const value = 2;").unwrap();
        git_in(dir.path(), &["commit", "-qam", "the branch's own work"]);
        let c = git_in(dir.path(), &["rev-parse", "HEAD"]);
        (dir, a, b, c)
    }

    /// The `resolve-repos` answer for one connected repo, `example/api`, whose
    /// one service is stored at `hash`.
    fn resolved_at(hash: &str) -> serde_json::Value {
        let mut metadata = resolution();
        metadata["repos"][0]["services"] = json!([service_row("api", hash)]);
        metadata
    }

    fn held_at(hash: &str) -> Snapshot {
        snapshot(
            resolved_at(hash),
            BTreeMap::from([("p".to_string(), vec![named_blob("api", "api", hash)])]),
        )
    }

    fn remotes_of(repo: &Path) -> BTreeMap<PathBuf, String> {
        BTreeMap::from([(repo.to_path_buf(), "example/api".to_string())])
    }

    const RESCANNED_AT: &str = "2026-10-04T10:00:00.000Z";

    /// [`resolved_at`] after the cloud rescanned the repo at the same commit:
    /// the row's `updated_at` is `at`.
    fn rescanned_at(hash: &str, at: &str) -> serde_json::Value {
        let mut metadata = resolved_at(hash);
        metadata["repos"][0]["services"][0]["updated_at"] = json!(at);
        metadata
    }

    /// T3: the cloud rescanned at the commit this machine holds. The copy is
    /// replaced by the new generation, which the readers then read.
    #[tokio::test]
    async fn a_rescan_at_the_held_commit_replaces_the_copy() {
        let (dir, _, b, _) = moved_on_repo();
        let (url, requests) = serve_sequence(vec![
            (
                "200 OK",
                serde_json::to_vec(&rescanned_at(&b, RESCANNED_AT)).unwrap(),
            ),
            (
                "200 OK",
                answered(&[(named_blob("api", "api", &b), RESCANNED_AT)]),
            ),
        ]);
        let remotes = remotes_of(dir.path());
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes, held_at(&b))
            .await;
        assert_eq!(
            outcome,
            Forward::Moved {
                moved: vec![(dir.path().to_path_buf(), b.clone())],
                kept: Vec::new()
            }
        );
        assert_eq!(requests.join().unwrap().len(), 2);
        let snapshot = snapshot.unwrap();
        assert_eq!(
            snapshot.stored_rows["p"][0].updated_at.as_deref(),
            Some(RESCANNED_AT)
        );
        let input = HostedInput {
            snapshot: Some(snapshot),
            remotes,
            ..Default::default()
        };
        assert_eq!(input.local_blobs(dir.path()).len(), 1);
    }

    /// T3: the same commit and the same scan is current: one metadata
    /// request and nothing downloaded.
    #[tokio::test]
    async fn the_same_scan_at_the_held_commit_is_current_for_one_request() {
        let (dir, _, b, _) = moved_on_repo();
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&b)).unwrap(),
        )]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&b))
            .await;
        assert_eq!(outcome, Forward::Current);
        assert!(snapshot.is_none());
        assert_eq!(requests.join().unwrap().len(), 1);
    }

    /// T3: a rescan at the held commit follows the rule of any move: a
    /// checkout whose HEAD does not hold the commit keeps its copy, with the
    /// reason, for one request.
    #[tokio::test]
    async fn a_rescan_the_checkout_does_not_hold_is_kept_with_its_reason() {
        use crate::git_state::tests::git_in;
        let (dir, a, b, _) = moved_on_repo();
        git_in(dir.path(), &["checkout", "-q", &a]);
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&rescanned_at(&b, RESCANNED_AT)).unwrap(),
        )]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&b))
            .await;
        assert_eq!(
            outcome,
            Forward::Kept(format!(
                "example/api: the hosted index is at {}, which this checkout's HEAD does not hold",
                short(&b)
            ))
        );
        assert!(snapshot.is_none());
        assert_eq!(requests.join().unwrap().len(), 1);
    }

    /// The hosted index moved from A to B, B is in HEAD's history: the
    /// snapshot moves to B, and the change base of `check --changed` with it,
    /// so the manifest main changed between A and B no longer re-reads every
    /// service.
    #[tokio::test]
    async fn a_hosted_index_further_along_heads_history_moves_the_base_forward() {
        let (dir, a, b, _) = moved_on_repo();
        let (url, requests) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&resolved_at(&b)).unwrap()),
            (
                "200 OK",
                answered(&[(named_blob("api", "api", &b), "2026-10-02T10:00:00.000Z")]),
            ),
        ]);
        let remotes = remotes_of(dir.path());
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes, held_at(&a))
            .await;
        assert_eq!(
            outcome,
            Forward::Moved {
                moved: vec![(dir.path().to_path_buf(), b.clone())],
                kept: Vec::new()
            }
        );
        assert_eq!(requests.join().unwrap().len(), 2);
        let input = HostedInput {
            snapshot,
            remotes,
            ..Default::default()
        };
        let base: Vec<String> = input
            .local_blobs(dir.path())
            .into_iter()
            .map(|blob| blob.commit_hash)
            .collect();
        assert_eq!(base, vec![b.clone()]);
        // What the change scope compares against each base: from A a
        // manifest changed (every service is re-read), from B only source.
        let repo_inputs_since = |commit: &str| -> Vec<String> {
            crate::git_state::changed_paths(&dir.path().to_string_lossy(), commit, None)
                .unwrap()
                .into_iter()
                .map(|change| change.path)
                .filter(|path| crate::change_scope::is_repo_input(path))
                .collect()
        };
        assert_eq!(repo_inputs_since(&a), vec!["package.json".to_string()]);
        assert!(repo_inputs_since(&b).is_empty());
    }

    /// A hosted commit HEAD does not hold keeps the base, and costs one
    /// metadata request: a branch behind main (HEAD at A, hosted at B), and a
    /// hosted commit on a line this checkout does not descend from.
    #[tokio::test]
    async fn a_hosted_index_outside_heads_history_keeps_the_base() {
        use crate::git_state::tests::git_in;
        let (dir, a, b, c) = moved_on_repo();
        git_in(dir.path(), &["checkout", "-q", "-b", "side", &a]);
        std::fs::write(dir.path().join("src/app.ts"), "export const value = 3;").unwrap();
        git_in(dir.path(), &["commit", "-qam", "a line HEAD will not hold"]);
        let side = git_in(dir.path(), &["rev-parse", "HEAD"]);

        // Behind main: HEAD at A, the hosted index at B.
        git_in(dir.path(), &["checkout", "-q", &a]);
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&b)).unwrap(),
        )]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        assert!(snapshot.is_none());
        assert_eq!(requests.join().unwrap().len(), 1, "it downloaded anyway");

        // Off HEAD's line: HEAD at C, the hosted index on the side branch.
        git_in(dir.path(), &["checkout", "-q", &c]);
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&side)).unwrap(),
        )]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        assert!(snapshot.is_none());
        assert_eq!(requests.join().unwrap().len(), 1, "it downloaded anyway");
    }

    /// A move writes the snapshot and then rebuilds the read model from it.
    /// A rebuild that fails puts the old snapshot bytes back and keeps the
    /// move with the rebuild's reason, so the two never read two generations
    /// (READY-A2 P4).
    #[test]
    fn a_rebuild_that_fails_puts_the_old_snapshot_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosted/snapshot.json");
        persist(&path, &held_at("a")).unwrap();
        let before = std::fs::read(&path).unwrap();
        let repo = dir.path().join("api");
        let asked = std::cell::RefCell::new(Vec::new());
        let outcome = move_and_rebuild(
            &path,
            &held_at("b"),
            vec![(repo.clone(), "b".to_string())],
            Vec::new(),
            &|paths| {
                asked.borrow_mut().push(paths.to_vec());
                Err("the join failed".into())
            },
        );
        assert_eq!(
            outcome,
            Forward::Kept(
                "the local index could not be rebuilt from the newer copy: the join failed".into()
            )
        );
        assert_eq!(asked.into_inner(), vec![vec![repo.clone()]]);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // And one that succeeds leaves the moved snapshot in place.
        let outcome = move_and_rebuild(
            &path,
            &held_at("b"),
            vec![(repo.clone(), "b".to_string())],
            Vec::new(),
            &|_| Ok(()),
        );
        assert_eq!(
            outcome,
            Forward::Moved {
                moved: vec![(repo, "b".to_string())],
                kept: Vec::new()
            }
        );
        let written: Snapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&written).unwrap(),
            serde_json::to_value(held_at("b")).unwrap()
        );
    }

    /// A checkout behind the hosted commit keeps the copy, and the answer says
    /// where both stand: the hosted commit, the copy's, and how far HEAD is
    /// behind, so the session start can name `git pull` (carrick#3433). The
    /// record written from it says `kept`, with both commits.
    #[tokio::test]
    async fn a_kept_copy_states_both_commits_and_how_far_head_is_behind() {
        use crate::git_state::tests::git_in;
        let (dir, a, b, _) = moved_on_repo();
        git_in(dir.path(), &["checkout", "-q", &a]);
        let (url, _) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&b)).unwrap(),
        )]);
        let (outcome, _, synced) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        let synced = synced.expect("the cloud answered, so the run says where things stand");
        // The sentence is built from the facts on the row, in a hook-free pass.
        let (line, short) = kept_sentence(&synced[0]);
        assert_eq!(synced[0].kept_line.as_deref(), Some(line.as_str()));
        assert_eq!(synced[0].kept_short.as_deref(), Some(short.as_str()));
        assert_eq!(
            synced,
            vec![SyncedRepo {
                repo: "example/api".into(),
                path: dir.path().to_string_lossy().into_owned(),
                on_default_branch: None,
                kept_line: Some(line),
                kept_short: Some(short),
                hosted_commit: b.clone(),
                hosted_scanned_at: Some(STORED_AT.into()),
                copy_commit: a.clone(),
                copy_scanned_at: Some(STORED_AT.into()),
                head_holds_hosted: false,
                fetched: true,
                behind: Some(1),
                head_detached: true,
                kept_reason: Some(format!(
                    "example/api: the hosted index is at {}, which this checkout's HEAD does \
                     not hold",
                    &b[..7]
                )),
            }]
        );

        let record = SyncRecord::of(&outcome, synced, "2026-11-06T10:00:00Z".into());
        let path = dir.path().join(".carrick").join(SYNC_RECORD);
        record.write(&path).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["schema"], "carrick.hosted-sync/0");
        assert_eq!(written["outcome"], "kept");
        assert_eq!(written["repos"][0]["hosted_commit"], json!(b));
        assert_eq!(written["repos"][0]["copy_commit"], json!(a));
        assert_eq!(written["repos"][0]["behind"], 1);
    }

    /// A hosted commit this clone has never fetched has no count, and says
    /// so, rather than a guessed one; a moved copy is recorded at the
    /// hosted commit it moved to; an unreachable cloud records nothing.
    #[tokio::test]
    async fn an_unfetched_hosted_commit_is_named_as_one_and_a_move_records_the_new_copy() {
        let (dir, a, b, _) = moved_on_repo();
        let unknown = "f".repeat(40);
        let (url, _) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&unknown)).unwrap(),
        )]);
        let (_, _, synced) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        let row = &synced.unwrap()[0];
        assert!(!row.fetched && !row.head_holds_hosted, "{row:?}");
        assert_eq!(row.behind, None);
        assert!(!row.head_detached, "{row:?}");

        let (url, _) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&resolved_at(&b)).unwrap()),
            (
                "200 OK",
                answered(&[(named_blob("api", "api", &b), "2026-10-02T10:00:00.000Z")]),
            ),
        ]);
        let (outcome, _, synced) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        let record = SyncRecord::of(&outcome, synced.unwrap(), "now".into());
        assert_eq!(record.outcome, "moved");
        assert_eq!(record.repos[0].copy_commit, b);

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let (_, _, synced) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(synced.is_none());
    }

    /// A push scan rescans only the services the push reached, so a hosted
    /// repo's services sit at several commits. Two on one line move to the
    /// newer one, each blob keeping its own commit; two on unrelated lines
    /// keep the base without a download.
    #[tokio::test]
    async fn services_at_several_commits_of_one_line_move_and_of_two_lines_keep() {
        use crate::git_state::tests::git_in;
        let (dir, a, b, c) = moved_on_repo();
        let two_at = |api: &str, worker: &str| {
            let mut metadata = resolution();
            metadata["repos"][0]["services"] =
                json!([service_row("api", api), service_row("worker", worker)]);
            metadata
        };
        let held = snapshot(
            two_at(&a, &a),
            BTreeMap::from([(
                "p".to_string(),
                vec![
                    named_blob("api", "api", &a),
                    named_blob("api", "worker", &a),
                ],
            )]),
        );
        let at = "2026-10-02T10:00:00.000Z";
        let (url, requests) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&two_at(&b, &a)).unwrap()),
            (
                "200 OK",
                answered(&[
                    (named_blob("api", "api", &b), at),
                    (named_blob("api", "worker", &a), at),
                ]),
            ),
        ]);
        let remotes = remotes_of(dir.path());
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes, held.clone())
            .await;
        assert_eq!(
            outcome,
            Forward::Moved {
                moved: vec![(dir.path().to_path_buf(), b.clone())],
                kept: Vec::new()
            }
        );
        assert_eq!(requests.join().unwrap().len(), 2);
        let input = HostedInput {
            snapshot,
            remotes: remotes.clone(),
            ..Default::default()
        };
        let mut commits: Vec<(Option<String>, String)> = input
            .local_blobs(dir.path())
            .into_iter()
            .map(|blob| (blob.service_name, blob.commit_hash))
            .collect();
        commits.sort();
        assert_eq!(
            commits,
            vec![
                (Some("api".to_string()), b.clone()),
                (Some("worker".to_string()), a.clone())
            ]
        );

        // A side line from A: B and `side` share A but neither holds the other.
        git_in(dir.path(), &["checkout", "-q", "-b", "side", &a]);
        std::fs::write(dir.path().join("src/app.ts"), "export const value = 3;").unwrap();
        git_in(dir.path(), &["commit", "-qam", "a line HEAD will not hold"]);
        let side = git_in(dir.path(), &["rev-parse", "HEAD"]);
        git_in(dir.path(), &["checkout", "-q", &c]);
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&two_at(&b, &side)).unwrap(),
        )]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes, held)
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        assert!(snapshot.is_none());
        assert_eq!(requests.join().unwrap().len(), 1, "it downloaded anyway");
    }

    /// No answer from the hosted side moves nothing and is no error: the
    /// check that follows runs on the base it had.
    #[tokio::test]
    async fn an_unreachable_hosted_index_keeps_the_base() {
        let (dir, a, _, _) = moved_on_repo();
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        assert!(snapshot.is_none());
    }

    /// A hosted index already at the commit held is current, with no
    /// download; one whose blob came down at another commit than the one the
    /// metadata named (main moved between the two requests) is not taken.
    #[tokio::test]
    async fn a_current_index_downloads_nothing_and_a_raced_download_is_refused() {
        let (dir, a, b, c) = moved_on_repo();
        let (url, requests) = serve_sequence(vec![(
            "200 OK",
            serde_json::to_vec(&resolved_at(&a)).unwrap(),
        )]);
        let (outcome, _, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert_eq!(outcome, Forward::Current);
        assert_eq!(requests.join().unwrap().len(), 1);

        let (url, _) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&resolved_at(&b)).unwrap()),
            (
                "200 OK",
                answered(&[(named_blob("api", "api", &c), "2026-10-02T10:00:00.000Z")]),
            ),
        ]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes_of(dir.path()), held_at(&a))
            .await;
        assert!(matches!(outcome, Forward::Kept(_)), "{outcome:?}");
        assert!(snapshot.is_none());
    }

    /// When the move's download says each blob was stored.
    const MOVED_AT: &str = "2026-10-02T10:00:00.000Z";

    /// A `resolve-repos` answer for connected repos of one project, `p`, each
    /// `(owner/repo, [(service, hash)])`.
    fn in_one_project(repos: &[(&str, Vec<(&str, &str)>)]) -> serde_json::Value {
        json!({"schema":"carrick.resolve-repos/0",
            "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
            "allowance_sentence":null,
            "repos": repos.iter().map(|(name, services)| json!({
                "full_name": name, "connected": true, "project_id": "p", "project_slug": "fixture",
                "services": services.iter().map(|(s, h)| service_row(s, h)).collect::<Vec<_>>()}))
                .collect::<Vec<_>>(),
            "project_repos":[{"project_slug":"fixture",
                "repos": repos.iter().map(|(name, _)| *name).collect::<Vec<_>>()}]})
    }

    type Repo = (tempfile::TempDir, String, String, String);

    /// Three repos of one project, each with its own history
    /// ([`moved_on_repo_named`]), checked out at their own work (C), which
    /// holds the hosted commit (B).
    struct Three {
        api: Repo,
        web: Repo,
        jobs: Repo,
    }

    impl Three {
        fn new() -> Self {
            Self {
                api: moved_on_repo_named("api"),
                web: moved_on_repo_named("web"),
                jobs: moved_on_repo_named("jobs"),
            }
        }

        fn remotes(&self) -> BTreeMap<PathBuf, String> {
            BTreeMap::from([
                (self.api.0.path().to_path_buf(), "example/api".to_string()),
                (self.web.0.path().to_path_buf(), "example/web".to_string()),
                (self.jobs.0.path().to_path_buf(), "example/jobs".to_string()),
            ])
        }

        /// What this machine holds: every repo at A, `web` with one service.
        fn held(&self) -> Snapshot {
            snapshot(
                in_one_project(&[
                    ("example/api", vec![("api", &self.api.1)]),
                    ("example/web", vec![("web", &self.web.1)]),
                    ("example/jobs", vec![("jobs", &self.jobs.1)]),
                ]),
                BTreeMap::from([(
                    "p".to_string(),
                    vec![
                        named_blob("api", "api", &self.api.1),
                        named_blob("web", "web", &self.web.1),
                        named_blob("jobs", "jobs", &self.jobs.1),
                    ],
                )]),
            )
        }

        /// The cloud now: every repo at B, where `web` gained a second
        /// service, so a kept `web` must keep the service list it had.
        fn hosted(&self) -> serde_json::Value {
            let mut hosted = in_one_project(&[
                ("example/api", vec![("api", &self.api.2)]),
                (
                    "example/web",
                    vec![("web", &self.web.2), ("web-worker", &self.web.2)],
                ),
                ("example/jobs", vec![("jobs", &self.jobs.2)]),
            ]);
            // The rows say what the download says ([`Self::downloaded`]): a
            // row stored at another time than its blob is a rescan.
            for repo in hosted["repos"].as_array_mut().unwrap() {
                for service in repo["services"].as_array_mut().unwrap() {
                    service["updated_at"] = json!(MOVED_AT);
                }
            }
            hosted
        }

        /// The download of project `p` at B, every row stored at
        /// [`MOVED_AT`].
        fn downloaded(&self) -> Vec<u8> {
            answered(&[
                (named_blob("api", "api", &self.api.2), MOVED_AT),
                (named_blob("web", "web", &self.web.2), MOVED_AT),
                (named_blob("web", "web-worker", &self.web.2), MOVED_AT),
                (named_blob("jobs", "jobs", &self.jobs.2), MOVED_AT),
            ])
        }

        /// One forward from `held` against [`Self::hosted`], and the number
        /// of requests it made.
        async fn forward(
            &self,
            held: Snapshot,
            download: bool,
        ) -> (Forward, Option<Snapshot>, Option<Vec<SyncedRepo>>, usize) {
            let mut replies = vec![("200 OK", serde_json::to_vec(&self.hosted()).unwrap())];
            if download {
                replies.push(("200 OK", self.downloaded()));
            }
            let (url, requests) = serve_sequence(replies);
            let (outcome, snapshot, synced) = reader(url)
                .forward(&credential("test"), &self.remotes(), held)
                .await;
            (outcome, snapshot, synced, requests.join().unwrap().len())
        }

        fn web_reason(&self) -> String {
            format!(
                "example/web: the hosted index is at {}, which this checkout's HEAD does not hold",
                &self.web.2[..7]
            )
        }
    }

    fn commits_of(input: &HostedInput, repo: &Path) -> Vec<(Option<String>, String)> {
        let mut commits: Vec<_> = input
            .local_blobs(repo)
            .into_iter()
            .map(|blob| (blob.service_name, blob.commit_hash))
            .collect();
        commits.sort();
        commits
    }

    /// What every reader reads of one repo from a snapshot: its blobs, the
    /// hosted state each blob is enriched with, and the blobs the edit-time
    /// lead reads under its label.
    fn read_of(input: &HostedInput, repo: &Path) -> serde_json::Value {
        let blobs = input.local_blobs(repo);
        let enriched: Vec<_> = blobs
            .iter()
            .map(|blob| serde_json::to_value(input.service(repo, blob)).unwrap())
            .collect();
        let label = super::super::index::repo_label(repo);
        let every: Vec<_> = input
            .every_blob()
            .into_iter()
            .filter(|(l, _)| *l == label)
            .map(|(_, blob)| serde_json::to_value(blob).unwrap())
            .collect();
        json!({"blobs": blobs, "enriched": enriched, "every": every})
    }

    /// One repo of three is behind its hosted commit (`detached`: a detached
    /// checkout, otherwise a branch that was not pulled). The other two move;
    /// the behind one keeps its blobs, its stored rows and its resolution
    /// entry, and is recorded as kept with its reason. Returns the moved
    /// snapshot.
    async fn one_of_three_behind(t: &Three, detached: bool) -> Snapshot {
        use crate::git_state::tests::git_in;
        let web = t.web.0.path();
        if detached {
            git_in(web, &["checkout", "-q", &t.web.1]);
        } else {
            git_in(web, &["checkout", "-q", "-b", "behind", &t.web.1]);
        }
        let (outcome, snapshot, synced, requests) = t.forward(t.held(), true).await;
        assert_eq!(requests, 2);
        let moved: Vec<(PathBuf, String)> = t
            .remotes()
            .keys()
            .filter(|path| path.as_path() != web)
            .map(|path| {
                let commit = if path == t.api.0.path() {
                    &t.api.2
                } else {
                    &t.jobs.2
                };
                (path.clone(), commit.clone())
            })
            .collect();
        assert_eq!(
            outcome,
            Forward::Moved {
                moved,
                kept: vec![(web.to_path_buf(), t.web_reason())]
            }
        );
        let snapshot = snapshot.expect("a move writes a snapshot");
        let input = HostedInput {
            snapshot: Some(snapshot.clone()),
            remotes: t.remotes(),
            ..Default::default()
        };
        assert_eq!(
            commits_of(&input, t.api.0.path()),
            vec![(Some("api".into()), t.api.2.clone())]
        );
        assert_eq!(
            commits_of(&input, t.jobs.0.path()),
            vec![(Some("jobs".into()), t.jobs.2.clone())]
        );
        // The blobs: web holds its copy at A, and nothing of B.
        assert_eq!(
            commits_of(&input, web),
            vec![(Some("web".into()), t.web.1.clone())]
        );
        assert!(
            snapshot.projects["p"]
                .iter()
                .filter(|blob| blob.repo_name == "web")
                .all(|blob| blob.commit_hash == t.web.1),
            "a blob of web at B stayed in the snapshot"
        );
        // The stored rows: web's is the row its copy was read as.
        let row_of = |repo: &str| -> Vec<StoredRow> {
            let mut rows: Vec<StoredRow> = snapshot.stored_rows["p"]
                .iter()
                .filter(|row| row.repo == repo)
                .cloned()
                .collect();
            rows.sort();
            rows
        };
        assert_eq!(
            row_of("web"),
            vec![StoredRow {
                repo: "web".into(),
                service: "web".into(),
                hash: t.web.1.clone(),
                updated_at: Some(STORED_AT.into()),
            }]
        );
        assert_eq!(row_of("api")[0].updated_at.as_deref(), Some(MOVED_AT));
        // The resolution: web's entry is the one its copy was read under.
        let web_services: Vec<(String, Option<String>)> = snapshot
            .resolution
            .repos
            .iter()
            .find(|repo| repo.full_name == "example/web")
            .and_then(|repo| repo.services.clone())
            .unwrap()
            .into_iter()
            .map(|service| (service.service, service.hash))
            .collect();
        assert_eq!(
            web_services,
            vec![("web".to_string(), Some(t.web.1.clone()))]
        );
        // The record: the moved repos at their hosted commit, web kept.
        let record = SyncRecord::of(&outcome, synced.unwrap(), "now".into());
        assert_eq!(record.outcome, "moved");
        assert_eq!(record.reason, Some(t.web_reason()));
        for row in &record.repos {
            if row.repo == "example/web" {
                assert_eq!(row.copy_commit, t.web.1);
                assert_eq!(row.kept_reason, Some(t.web_reason()));
                assert_eq!(row.head_detached, detached);
            } else {
                assert_eq!(row.copy_commit, row.hosted_commit, "{row:?}");
                assert_eq!(row.kept_reason, None, "{row:?}");
            }
        }
        snapshot
    }

    /// r1: a branch that was not pulled holds back its own repo only. The
    /// next forward over the spliced snapshot keeps it again for one
    /// metadata request.
    #[tokio::test]
    async fn one_repo_behind_does_not_hold_the_others_back() {
        let t = Three::new();
        let moved = one_of_three_behind(&t, false).await;
        let (outcome, snapshot, synced, requests) = t.forward(moved, false).await;
        assert_eq!(outcome, Forward::Kept(t.web_reason()));
        assert!(snapshot.is_none());
        assert_eq!(requests, 1, "it downloaded anyway");
        let synced = synced.unwrap();
        let web = synced.iter().find(|r| r.repo == "example/web").unwrap();
        assert_eq!(web.kept_reason, Some(t.web_reason()));
        let record = SyncRecord::of(&outcome, synced, "now".into());
        assert_eq!(record.outcome, "kept");
    }

    /// r2: a detached checkout behind holds back its own repo only.
    #[tokio::test]
    async fn a_detached_checkout_behind_holds_back_only_its_own_repo() {
        let t = Three::new();
        one_of_three_behind(&t, true).await;
    }

    /// r0: every HEAD holds its hosted commit, so every repo moves, as before.
    #[tokio::test]
    async fn every_repo_current_moves_every_repo() {
        let t = Three::new();
        let (outcome, snapshot, synced, requests) = t.forward(t.held(), true).await;
        assert_eq!(requests, 2);
        let moved: Vec<(PathBuf, String)> = t
            .remotes()
            .keys()
            .map(|path| {
                let repo = [&t.api, &t.web, &t.jobs]
                    .into_iter()
                    .find(|repo| repo.0.path() == path)
                    .unwrap();
                (path.clone(), repo.2.clone())
            })
            .collect();
        assert_eq!(
            outcome,
            Forward::Moved {
                moved,
                kept: Vec::new()
            }
        );
        let input = HostedInput {
            snapshot,
            remotes: t.remotes(),
            ..Default::default()
        };
        assert_eq!(
            commits_of(&input, t.web.0.path()),
            vec![
                (Some("web".into()), t.web.2.clone()),
                (Some("web-worker".into()), t.web.2.clone())
            ]
        );
        let record = SyncRecord::of(&outcome, synced.unwrap(), "now".into());
        assert_eq!((record.outcome, record.reason), ("moved", None));
        assert!(
            record
                .repos
                .iter()
                .all(|row| row.copy_commit == row.hosted_commit && row.kept_reason.is_none())
        );
    }

    /// Every reader of the mixed snapshot reads the two moved repos exactly
    /// as it reads them from a snapshot where every repo moved, and the kept
    /// repo exactly as it read it before the move.
    #[tokio::test]
    async fn a_mixed_snapshot_reads_the_moved_repos_as_an_all_current_one_does() {
        use crate::git_state::tests::git_in;
        let t = Three::new();
        let mixed = HostedInput {
            snapshot: Some(one_of_three_behind(&t, false).await),
            remotes: t.remotes(),
            ..Default::default()
        };
        git_in(t.web.0.path(), &["checkout", "-q", "main"]);
        let (outcome, current, _, _) = t.forward(t.held(), true).await;
        assert!(matches!(&outcome, Forward::Moved { kept, .. } if kept.is_empty()));
        let current = HostedInput {
            snapshot: current,
            remotes: t.remotes(),
            ..Default::default()
        };
        let before = HostedInput {
            snapshot: Some(t.held()),
            remotes: t.remotes(),
            ..Default::default()
        };
        for repo in [t.api.0.path(), t.jobs.0.path()] {
            assert_eq!(read_of(&mixed, repo), read_of(&current, repo));
        }
        assert_eq!(
            read_of(&mixed, t.web.0.path()),
            read_of(&before, t.web.0.path())
        );
    }

    /// A kept repo is asked for as this machine holds it, so a project only
    /// it changed is reused, not downloaded: `example/api` (project `pa`)
    /// moves, `example/web` (project `pb`) is behind and costs nothing.
    #[tokio::test]
    async fn a_project_only_a_kept_repo_changed_is_not_downloaded() {
        use crate::git_state::tests::git_in;
        let (api, api_a, api_b, _) = moved_on_repo_named("api");
        let (web, web_a, web_b, _) = moved_on_repo_named("web");
        git_in(web.path(), &["checkout", "-q", &web_a]);
        let two = |api: &str, web: &str| {
            json!({"schema":"carrick.resolve-repos/0",
                "workspace":{"slug":"fixture","billing_tier":"free","installed":true},
                "allowance_sentence":null,
                "repos":[
                    {"full_name":"example/api","connected":true,"project_id":"pa","project_slug":"sa",
                     "services":[service_row("api", api)]},
                    {"full_name":"example/web","connected":true,"project_id":"pb","project_slug":"sb",
                     "services":[service_row("web", web)]}],
                "project_repos":[{"project_slug":"sa","repos":["example/api"]},
                                 {"project_slug":"sb","repos":["example/web"]}]})
        };
        let held = snapshot(
            two(&api_a, &web_a),
            BTreeMap::from([
                ("pa".to_string(), vec![named_blob("api", "api", &api_a)]),
                ("pb".to_string(), vec![named_blob("web", "web", &web_a)]),
            ]),
        );
        // Two replies only: a download of `pb` finds the mock gone.
        let (url, requests) = serve_sequence(vec![
            ("200 OK", serde_json::to_vec(&two(&api_b, &web_b)).unwrap()),
            (
                "200 OK",
                answered(&[(named_blob("api", "api", &api_b), MOVED_AT)]),
            ),
        ]);
        let remotes = BTreeMap::from([
            (api.path().to_path_buf(), "example/api".to_string()),
            (web.path().to_path_buf(), "example/web".to_string()),
        ]);
        let (outcome, snapshot, _) = reader(url)
            .forward(&credential("test"), &remotes, held)
            .await;
        assert!(
            matches!(&outcome, Forward::Moved { moved, kept } if moved.len() == 1 && kept.len() == 1),
            "{outcome:?}"
        );
        assert_eq!(requests.join().unwrap().len(), 2);
        let input = HostedInput {
            snapshot,
            remotes,
            ..Default::default()
        };
        assert_eq!(
            commits_of(&input, api.path()),
            vec![(Some("api".into()), api_b)]
        );
        assert_eq!(
            commits_of(&input, web.path()),
            vec![(Some("web".into()), web_a)]
        );
    }

    /// The no-network repair is per repo too: a local index left behind its
    /// snapshot heals the repo that may move, skips with its reason the one
    /// whose HEAD does not hold the snapshot's commit, and skips the one whose
    /// snapshot blob was written from a dirty tree (the network move decides
    /// that one).
    #[test]
    fn heal_moves_the_repo_that_may_and_skips_the_one_that_may_not() {
        use crate::git_state::tests::git_in;
        let (api, api_a, api_b, _) = moved_on_repo_named("api");
        let (web, web_a, web_b, _) = moved_on_repo_named("web");
        let (jobs, jobs_a, jobs_b, _) = moved_on_repo_named("jobs");
        git_in(web.path(), &["checkout", "-q", &web_a]);
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace {
            root: root.path().to_path_buf(),
            repos: vec![
                api.path().to_path_buf(),
                web.path().to_path_buf(),
                jobs.path().to_path_buf(),
            ],
            missing: Vec::new(),
            repos_detected_by: String::new(),
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            parent_proposal: None,
        };
        let built = |path: &Path, name: &str, commit: &str| {
            json!({"path": path.to_string_lossy(), "name": name, "files": {},
                "services": [{"name": name, "commit": commit, "indexed_at": "then",
                    "boundary": null, "routes": 0, "calls": 0}]})
        };
        let index = json!({"version": super::super::read_model::READ_MODEL_VERSION,
            "scanner_version": "test", "indexed_at": "then",
            "repos": [built(api.path(), "api", &api_a), built(web.path(), "web", &web_a),
                built(jobs.path(), "jobs", &jobs_a)]});
        std::fs::create_dir_all(workspace.index_dir()).unwrap();
        std::fs::write(workspace.index_file(), serde_json::to_vec(&index).unwrap()).unwrap();
        // A snapshot `hosted::refresh` wrote at B for all three, with no HEAD
        // test; jobs' blob came from a tree with uncommitted changes.
        let mut dirty = named_blob("jobs", "jobs", &jobs_b);
        dirty.dirty = Some(true);
        let held = snapshot(
            in_one_project(&[
                ("example/api", vec![("api", &api_b)]),
                ("example/web", vec![("web", &web_b)]),
                ("example/jobs", vec![("jobs", &jobs_b)]),
            ]),
            BTreeMap::from([(
                "p".to_string(),
                vec![
                    named_blob("api", "api", &api_b),
                    named_blob("web", "web", &web_b),
                    dirty,
                ],
            )]),
        );
        let remotes = BTreeMap::from([
            (api.path().to_path_buf(), "example/api".to_string()),
            (web.path().to_path_buf(), "example/web".to_string()),
            (jobs.path().to_path_buf(), "example/jobs".to_string()),
        ]);
        let asked = std::cell::RefCell::new(Vec::new());
        let healed = heal(&workspace, &remotes, &held, &|paths| {
            asked.borrow_mut().push(paths.to_vec());
            Ok(())
        });
        let (healed, mut skipped) = healed;
        assert_eq!(
            healed,
            Some(Ok(vec![(api.path().to_path_buf(), api_b.clone())]))
        );
        // The dirty copy is skipped with a reason of its own, not silently.
        skipped.sort();
        let mut expected = vec![
            (
                web.path().to_path_buf(),
                format!(
                    "example/web: the hosted index is at {}, which this checkout's HEAD does \
                     not hold",
                    &web_b[..7]
                ),
            ),
            (
                jobs.path().to_path_buf(),
                "example/jobs: the held copy was scanned from uncommitted changes or by another \
                 Carrick format, so it cannot be moved as it is"
                    .to_string(),
            ),
        ];
        expected.sort();
        assert_eq!(skipped, expected);
        assert_eq!(asked.into_inner(), vec![vec![api.path().to_path_buf()]]);
    }

    /// A kept repo the old snapshot held nothing for holds nothing after the
    /// splice: no blob, no stored row, and an entry with no services, while
    /// the moved repo keeps what came down.
    #[test]
    fn a_kept_repo_that_held_nothing_holds_nothing() {
        let old = snapshot(
            in_one_project(&[("example/api", vec![("api", "a1")])]),
            BTreeMap::from([("p".to_string(), vec![named_blob("api", "api", "a1")])]),
        );
        let fresh = in_one_project(&[
            ("example/api", vec![("api", "a2")]),
            ("example/web", vec![("web", "w2")]),
        ]);
        let mut moved = snapshot(
            fresh.clone(),
            BTreeMap::from([(
                "p".to_string(),
                vec![
                    named_blob("api", "api", "a2"),
                    named_blob("web", "web", "w2"),
                ],
            )]),
        );
        let fresh = Resolution::parse(fresh).unwrap();
        hold_back(&mut moved, &old, &fresh, &["example/web".to_string()]);
        let repos: Vec<(String, String)> = moved.projects["p"]
            .iter()
            .map(|blob| (blob.repo_name.clone(), blob.commit_hash.clone()))
            .collect();
        assert_eq!(repos, vec![("api".to_string(), "a2".to_string())]);
        assert!(moved.stored_rows["p"].iter().all(|row| row.repo == "api"));
        let web = moved
            .resolution
            .repos
            .iter()
            .find(|repo| repo.full_name == "example/web")
            .unwrap();
        assert_eq!(web.services.as_ref().map(Vec::len), Some(0));
    }

    /// A row for `repo` at `path`, current: nothing kept.
    fn row_of(repo: &str, path: &Path) -> SyncedRepo {
        SyncedRepo {
            repo: repo.into(),
            path: path.to_string_lossy().into_owned(),
            hosted_commit: "b".repeat(40),
            hosted_scanned_at: Some("2026-11-12T10:00:00Z".into()),
            copy_commit: "a".repeat(40),
            copy_scanned_at: Some("2026-11-10T08:00:00Z".into()),
            head_holds_hosted: false,
            fetched: true,
            behind: Some(3),
            head_detached: false,
            on_default_branch: Some(true),
            kept_reason: None,
            kept_line: None,
            kept_short: None,
        }
    }

    /// Every way a copy is kept back says what is true and the move that
    /// fixes it, and names the repo and the copy's date.
    #[test]
    fn the_kept_sentence_names_the_repo_the_date_and_the_fix() {
        let base = row_of("example/web", Path::new("/w"));
        let case = |change: &dyn Fn(&mut SyncedRepo)| {
            let mut row = base.clone();
            change(&mut row);
            kept_sentence(&row).0
        };
        let on_main = case(&|_| {});
        let on_branch = case(&|row| row.on_default_branch = Some(false));
        let detached = case(&|row| row.head_detached = true);
        let unfetched = case(&|row| row.fetched = false);
        let unmovable = case(&|row| row.head_holds_hosted = true);
        let not_one_line = case(&|row| row.hosted_commit.clear());
        let undated = case(&|row| row.copy_scanned_at = None);
        for line in [&on_main, &on_branch, &detached, &unfetched, &unmovable] {
            assert!(line.contains("example/web"), "{line}");
        }
        for line in [&on_main, &on_branch, &detached, &unfetched, &unmovable] {
            assert!(line.contains("2026-11-10"), "{line}");
        }
        assert!(on_main.contains("`git pull`") && !on_main.contains("carrick refresh"));
        assert!(on_main.contains("3 commits behind"), "{on_main}");
        assert!(on_branch.contains("`git pull`") && on_branch.contains("`carrick refresh`"));
        assert!(detached.contains("detached") && detached.contains("`git pull`"));
        assert!(unfetched.contains("`git fetch`"));
        assert!(unmovable.contains("`carrick refresh`"));
        assert!(not_one_line.contains("history diverged"), "{not_one_line}");
        assert!(!undated.contains("2026"), "{undated}");
    }

    /// Gap (a): a repo whose hosted or held commits are not one line has no
    /// row from `synced_repos`, and its sentence still has one place to live.
    #[test]
    fn a_repo_with_no_row_still_gets_its_kept_sentence() {
        let remotes = BTreeMap::from([(PathBuf::from("/w"), "example/web".to_string())]);
        let mut synced = Vec::new();
        stamp_kept(
            &mut synced,
            &remotes,
            &[(PathBuf::from("/w"), "example/web: not one line".into())],
        );
        assert_eq!(synced.len(), 1);
        let row = &synced[0];
        assert_eq!(row.path, "/w");
        assert!(row.hosted_commit.is_empty() && row.copy_commit.is_empty());
        assert_eq!(
            row.kept_reason.as_deref(),
            Some("example/web: not one line")
        );
        assert_eq!(
            row.kept_line.as_deref(),
            Some(kept_sentence(row).0.as_str())
        );
        assert!(row.kept_line.as_deref().unwrap().contains("example/web"));
    }

    /// Gap (b): the heal moved a repo, then the network move refused another.
    /// The run's outcome and the record's reason name the refused repo too,
    /// and its row carries the sentence.
    #[test]
    fn a_heal_that_moved_and_a_network_refusal_both_reach_the_record() {
        let remotes = BTreeMap::from([
            (PathBuf::from("/api"), "example/api".to_string()),
            (PathBuf::from("/web"), "example/web".to_string()),
        ]);
        let mut synced = vec![
            row_of("example/api", Path::new("/api")),
            row_of("example/web", Path::new("/web")),
        ];
        // What the network move stamped before the heal's say.
        stamp_kept(
            &mut synced,
            &remotes,
            &[(PathBuf::from("/web"), "example/web: behind".into())],
        );
        let healed = vec![(PathBuf::from("/api"), "b".repeat(40))];
        let outcome = settle(
            Forward::Kept("example/web: behind".into()),
            (Some(Ok(healed.clone())), Vec::new()),
            Some(&mut synced),
            &remotes,
        );
        assert_eq!(
            outcome,
            Forward::Moved {
                moved: healed,
                kept: vec![(PathBuf::from("/web"), "example/web: behind".into())]
            }
        );
        let record = SyncRecord::of(&outcome, synced, "now".into());
        assert_eq!(record.outcome, "moved");
        assert_eq!(record.reason.as_deref(), Some("example/web: behind"));
        let web = record
            .repos
            .iter()
            .find(|r| r.repo == "example/web")
            .unwrap();
        assert!(web.kept_line.is_some());
        let api = record
            .repos
            .iter()
            .find(|r| r.repo == "example/api")
            .unwrap();
        assert!(api.kept_line.is_none() && api.kept_reason.is_none());
    }

    /// A copy the heal skipped and nothing else moved is kept, not current:
    /// the sentence is written even though no repo healed.
    #[test]
    fn a_lone_heal_skip_is_kept_with_its_sentence() {
        let remotes = BTreeMap::from([(PathBuf::from("/web"), "example/web".to_string())]);
        let mut synced = vec![row_of("example/web", Path::new("/web"))];
        let skip = (PathBuf::from("/web"), "example/web: dirty".to_string());
        let outcome = settle(
            Forward::Current,
            (None, vec![skip]),
            Some(&mut synced),
            &remotes,
        );
        assert_eq!(outcome, Forward::Kept("example/web: dirty".into()));
        assert_eq!(synced[0].kept_reason.as_deref(), Some("example/web: dirty"));
        assert!(synced[0].kept_line.is_some());
    }

    /// A repo the network move then moved is not kept because the heal had
    /// skipped it.
    #[test]
    fn a_heal_skip_the_network_move_carried_is_not_kept() {
        let remotes = BTreeMap::from([(PathBuf::from("/web"), "example/web".to_string())]);
        let mut synced = vec![row_of("example/web", Path::new("/web"))];
        let moved = vec![(PathBuf::from("/web"), "b".repeat(40))];
        let outcome = settle(
            Forward::Moved {
                moved: moved.clone(),
                kept: Vec::new(),
            },
            (
                None,
                vec![(PathBuf::from("/web"), "example/web: dirty".into())],
            ),
            Some(&mut synced),
            &remotes,
        );
        assert_eq!(
            outcome,
            Forward::Moved {
                moved,
                kept: Vec::new()
            }
        );
        assert!(synced[0].kept_line.is_none());
    }

    /// `kept_repos` reads one sentence per kept repo from the record, the
    /// same string the record holds, names only the kept one of three, and
    /// says nothing once a full refresh has rewritten the snapshot.
    #[test]
    fn the_reader_returns_the_records_sentence_for_the_kept_repo_only() {
        let remotes = BTreeMap::from([
            (PathBuf::from("/api"), "example/api".to_string()),
            (PathBuf::from("/web"), "example/web".to_string()),
            (PathBuf::from("/jobs"), "example/jobs".to_string()),
        ]);
        let mut synced = vec![
            row_of("example/api", Path::new("/api")),
            row_of("example/web", Path::new("/web")),
            row_of("example/jobs", Path::new("/jobs")),
        ];
        stamp_kept(
            &mut synced,
            &remotes,
            &[(PathBuf::from("/web"), "example/web: behind".into())],
        );
        let record = SyncRecord::of(
            &Forward::Kept("example/web: behind".into()),
            synced.clone(),
            "now".into(),
        );
        let dir = tempfile::tempdir().unwrap();
        record.write(&dir.path().join(SYNC_RECORD)).unwrap();
        let kept = kept_repos(dir.path());
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].repo, "example/web");
        assert_eq!(kept[0].path, "/web");
        assert_eq!(Some(&kept[0].line), synced[1].kept_line.as_ref());
        assert_eq!(Some(&kept[0].reason_short), synced[1].kept_short.as_ref());
        assert_eq!(kept[0].behind, Some(3));
        // A snapshot written after the record: that copy is gone.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(dir.path().join("hosted/snapshot.json"), b"{}").unwrap();
        assert!(kept_repos(dir.path()).is_empty());
        // No record at all.
        assert!(kept_repos(&dir.path().join("nowhere")).is_empty());
    }
}
