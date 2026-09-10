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

const API_BASE: &str = "https://api.carrick.tools";
const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

// Shared with npm/carrick/src/auth/credentials.ts. Never Debug: the token
// must not enter logs, errors, or the persisted hosted cache.
#[derive(Deserialize)]
struct Credential {
    api_base: String,
    token: String,
    workspace_slug: Option<String>,
    obtained_at: String,
}

impl Credential {
    fn load() -> Result<Option<Self>, String> {
        if let Some(token) = std::env::var_os("CARRICK_TOKEN") {
            let credential = Self {
                api_base: API_BASE.into(),
                token: token
                    .into_string()
                    .map_err(|_| "CARRICK_TOKEN is malformed")?,
                workspace_slug: None,
                obtained_at: String::new(),
            };
            credential.validate()?;
            return Ok(Some(credential));
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                if cfg!(windows) {
                    std::env::var_os("APPDATA").map(PathBuf::from)
                } else {
                    None
                }
            })
            .or_else(|| dirs::home_dir().map(|p| p.join(".config")))
            .ok_or("Could not locate Carrick credentials. Run carrick login.")?;
        if !base.is_absolute() {
            return Err("Carrick's configuration directory must be absolute.".into());
        }
        let path = base.join("carrick/credentials.json");
        Self::read_file(&path)
    }

    fn read_file(path: &Path) -> Result<Option<Self>, String> {
        use std::io::Read;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Could not read Carrick credentials. Run carrick login.".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "Could not inspect Carrick credentials")?;
        if !metadata.is_file() {
            return Err("Carrick credentials must be a private regular file.".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("Carrick credentials are not private. Run carrick login.".into());
            }
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| "Could not read Carrick credentials")?;
        let credential: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "Carrick credentials are invalid. Run carrick login.")?;
        credential.validate()?;
        Ok(Some(credential))
    }

    fn validate(&self) -> Result<(), String> {
        if self.api_base != API_BASE
            || self.token.is_empty()
            || self.token.chars().any(char::is_whitespace)
        {
            return Err("Carrick credentials are invalid. Run carrick login.".into());
        }
        // The server governs expiry; obtained_at records provenance only.
        let _ = &self.obtained_at;
        Ok(())
    }

    fn identity(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.api_base.as_bytes());
        hash.update([0]);
        hash.update(self.token.as_bytes());
        format!("{:x}", hash.finalize())
    }
}

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

fn valid_repo_name(name: &str) -> bool {
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

/// Provenance in the additive local check/status contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostedProvenance {
    pub commit: String,
    pub indexed_at: String,
    pub scanner_version: Option<String>,
    pub project: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedState {
    Enriched,
    NoIndexYet,
    NotConnected,
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
}

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    identity: String,
    checked_at: String,
    resolution: Resolution,
    projects: BTreeMap<String, Vec<CloudRepoData>>,
}

#[derive(Default)]
pub(super) struct HostedInput {
    snapshot: Option<Snapshot>,
    failure: Option<String>,
    remotes: BTreeMap<PathBuf, String>,
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
    ) -> Result<Vec<CloudRepoData>, String> {
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
            metadata: CloudRepoData,
        }
        #[derive(Deserialize)]
        struct Response {
            repos: Vec<Entry>,
        }
        let response: Response =
            serde_json::from_value(value).map_err(|_| "Invalid hosted project schema")?;
        Ok(response.repos.into_iter().map(|r| r.metadata).collect())
    }

    async fn refresh(
        &self,
        credential: &Credential,
        repos: Vec<String>,
        old: Option<Snapshot>,
    ) -> Result<(Snapshot, Option<String>), String> {
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
        let old = old.filter(|s| s.resolution.workspace.slug == resolution.workspace.slug);
        let ids: std::collections::BTreeSet<_> = resolution
            .repos
            .iter()
            .filter(|r| r.connected)
            .filter_map(|r| r.project_id.clone())
            .collect();
        let mut projects = BTreeMap::new();
        let mut failure = None;
        for id in ids {
            match self.project(credential, &id).await {
                Ok(blobs) => {
                    projects.insert(id, blobs);
                }
                Err(error) => {
                    failure = Some(error);
                    if let Some(blobs) = old.as_ref().and_then(|s| s.projects.get(&id)) {
                        projects.insert(id, blobs.clone());
                    }
                }
            }
        }
        Ok((
            Snapshot {
                identity: credential.identity(),
                checked_at: chrono::Utc::now().to_rfc3339(),
                resolution,
                projects,
            },
            failure,
        ))
    }
}

/// Read only at explicit index/refresh time. A separate thread owns its Tokio
/// runtime because the CLI dispatcher itself already runs inside Tokio.
pub(super) fn refresh(workspace: &Workspace) -> HostedInput {
    let remotes = workspace
        .repos
        .iter()
        .filter_map(|path| remote_name(path).map(|name| (path.clone(), name)))
        .collect();
    let mut input = HostedInput {
        remotes,
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
    let old: Option<Snapshot> = std::fs::read(&cache)
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
        });
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
        Ok((snapshot, failure)) => {
            if let Err(error) = persist(&cache, &snapshot) {
                input.failure = Some(error);
            } else {
                input.failure = failure;
            }
            input.snapshot = Some(snapshot);
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

fn remote_name(repo: &Path) -> Option<String> {
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

fn parse_remote(remote: &str) -> Option<String> {
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

impl HostedInput {
    pub(super) fn checked_at(&self) -> Option<String> {
        self.snapshot.as_ref().map(|s| s.checked_at.clone())
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
                repo.services.as_ref().is_some_and(|ss| {
                    ss.iter()
                        .any(|s| s.service == b.service_name.as_deref().unwrap_or(&b.repo_name))
                })
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
            result.hosted_state = HostedState::NotConnected;
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
                result.failure =
                    Some("No unambiguous hosted blob matched this local service".into());
                HostedState::ReadFailed
            } else {
                HostedState::NoIndexYet
            };
            return result;
        };
        result.hosted_cache_version = previous.cache_version;
        result.hosted = Some(HostedProvenance {
            commit: previous.commit_hash.clone(),
            indexed_at: previous.last_updated.to_rfc3339(),
            scanner_version: previous.scanner_version.clone(),
            project: repo.project_slug.clone().unwrap_or_default(),
        });
        result.hosted_state = if previous.cache_version != Some(crate::engine::CACHE_VERSION) {
            HostedState::VersionMismatch
        } else if super::query::changed_since(path, &previous.commit_hash).is_none() {
            HostedState::CommitMissing
        } else if previous.file_results.is_none() {
            result.failure = Some("Hosted index has no reusable model answers".into());
            HostedState::ReadFailed
        } else {
            HostedState::Enriched
        };
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
}

/// Explicit per-repo handoff to the scan subprocess. It is read only when
/// local storage and no-model mode are both selected; it cannot change CI.
pub(super) const PREVIOUS_ENV: &str = "CARRICK_LOCAL_HOSTED_PREVIOUS";
pub(crate) fn previous_data() -> Result<Option<Vec<CloudRepoData>>, Box<dyn std::error::Error>> {
    if !super::no_model() || std::env::var_os(crate::cloud_storage::CACHE_DIR_ENV).is_none() {
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
    use serde_json::json;
    use std::io::{Read, Write};

    fn credential(token: &str) -> Credential {
        Credential {
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
        assert_eq!(result.len(), 1);
        let request = request.join().unwrap();
        assert!(request.contains("authorization: Bearer test-secret"));
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"action":"get-cross-repo-data","project_id":"authorised-project"})
        );
    }

    #[tokio::test]
    async fn staged_get_has_no_bearer_or_api_body() {
        let (staged, staged_request) = serve(
            "200 OK",
            "",
            serde_json::to_vec(&json!({"repos":[{"metadata":blob()}]})).unwrap(),
        );
        let (url, api_request) = serve("200 OK", "", serde_json::to_vec(&json!({"staged":true,"staged_url":format!("{staged}/signed?signature=fixture"),"raw_bytes":1,"repo_count":1})).unwrap());
        assert_eq!(
            reader(url)
                .project(&credential("test-secret"), "p")
                .await
                .unwrap()
                .len(),
            1
        );
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
        };
        let (url, request) = serve("200 OK", "", serde_json::to_vec(&metadata).unwrap());
        let (new, failure) = reader(url)
            .refresh(&credential("test"), vec!["example/api".into()], Some(old))
            .await
            .unwrap();
        assert!(new.projects.is_empty());
        assert!(failure.is_none());
        request.join().unwrap();
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
            }),
            failure: None,
            remotes: BTreeMap::from([(PathBuf::from("/w/api"), "example/api".into())]),
        };
        assert_eq!(input.local_blobs(Path::new("/w/api")).len(), 1);
        input.snapshot.as_mut().unwrap().resolution.project_repos[0]
            .repos
            .push("another/api".into());
        assert!(input.local_blobs(Path::new("/w/api")).is_empty());
        assert!(input.remote_blobs().is_empty());
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

    #[cfg(unix)]
    #[test]
    fn credential_reader_checks_open_file_and_rejects_symlinks_and_public_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        std::fs::write(&file,serde_json::to_vec(&json!({"api_base":API_BASE,"token":"test","workspace_slug":null,"obtained_at":"now"})).unwrap()).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Credential::read_file(&file).unwrap().is_some());
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(Credential::read_file(&link).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Credential::read_file(&file).is_err());
    }
}
