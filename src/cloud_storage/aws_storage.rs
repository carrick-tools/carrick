use crate::agent_service::is_oidc_rejection;
use crate::cloud_storage::{
    CloudRepoData, CloudStorage, RunContext, RunStart, StorageError, UnanalysedFile, UploadOutcome,
};
use crate::credentials::CloudAuth;
use crate::oidc::OidcProvider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Total per-request deadline. Generous because uploads can carry multi-MB
/// payloads over slow CI links, but bounded so a hung connection can't stall
/// the scan until the CI job timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Retries after the first attempt for transient failures (network errors,
/// 408/429/5xx). A scan's cloud calls bookend a long, expensive analysis, so
/// one Lambda cold start or load-balancer blip must not discard the run.
const MAX_TRANSIENT_RETRIES: u32 = 3;

/// Retries after the first attempt for the actions that write the cloud index.
/// A 5xx on a write is ambiguous: the gateway cuts the connection at its
/// integration timeout while the handler keeps running, so the write may
/// already have landed (carrick#536 — a `store-metadata` answered 200 after
/// 118s, long past the 30s gateway cut, and each blind retry re-ran the whole
/// embed). The cloud recognises a duplicate arriving after the row is durable
/// and answers it cheaply, so one retry is worth making. A storm of them is
/// not: on the measured incident four attempts did the same expensive work
/// four times and the scan was still reported as failed.
const MAX_WRITE_RETRIES: u32 = 1;

/// The actions that write the cloud index. Everything else is a read (or a
/// mint) and is safely repeatable, so it keeps the full retry budget.
const WRITE_ACTIONS: [&str; 2] = ["complete-upload", "store-metadata"];

fn is_write_action(action: &str) -> bool {
    WRITE_ACTIONS.contains(&action)
}

/// Retry budget for an action. Unknown actions are treated as reads.
fn max_retries_for_action(action: &str) -> u32 {
    if is_write_action(action) {
        MAX_WRITE_RETRIES
    } else {
        MAX_TRANSIENT_RETRIES
    }
}

/// The error a caller sees once the retry budget is spent. On a write action
/// the failure is ambiguous rather than final, and saying so is what stops the
/// next reader from assuming the index is stale and forcing a re-scan.
fn retry_exhausted_message(action: &str, transient_error: &str, attempts: u32) -> String {
    if is_write_action(action) {
        format!(
            "{} (after {} attempts on '{}'). This action writes the index, and a response \
             lost to a gateway timeout does not mean the write was lost — the index may \
             already be current at this commit. Check it before re-running the scan.",
            transient_error, attempts, action
        )
    } else {
        format!("{} (after {} attempts)", transient_error, attempts)
    }
}

/// Above this serialized size, CloudRepoData is PUT to a presigned S3
/// staging URL instead of being inlined in the request body (carrick#486).
/// The inline path dies at two walls: API Gateway rejects bodies over 10 MB
/// with a 413, and the Lambda event cap (6,291,556 bytes minus ~7.6% JSON
/// envelope escaping, so ~5.8 MB effective) surfaces as an unattributable
/// 500. 4 MB leaves margin under the lower wall; one class-heavy service in
/// a large monorepo measured past 10 MB after #483.
pub(crate) const INLINE_PAYLOAD_LIMIT_BYTES: usize = 4 * 1024 * 1024;

fn retry_backoff(retries_so_far: u32) -> Duration {
    // 2s, 4s, 8s
    Duration::from_secs(2u64 << retries_so_far)
}

fn is_transient_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// The error body both laptop gates and the upload actions use:
/// `{ "error": "<sentence>", "code": "<machine code>" }`. Every field is
/// optional, because a gateway can answer a non-envelope body on the same
/// route.
#[derive(Deserialize, Default)]
struct RefusalBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
    /// Echoed by the first-scan partial rule's refusal, so the user is told
    /// exactly which files the cloud would not accept.
    #[serde(default)]
    unanalysed_files: Option<Vec<UnanalysedFile>>,
}

impl RefusalBody {
    fn of(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }
}

/// What a non-2xx says, in the user's words rather than the transport's.
///
/// The gates answer 409 with a code the user can act on
/// (`laptop_scan_in_flight`, `laptop_scan_daily_limit`, `partial_refused`),
/// and "Lambda returned 409" says none of it. A body that is not the envelope
/// falls back to the raw text, which is what it did before.
fn refusal_message(status: reqwest::StatusCode, body: &str) -> String {
    let refusal = RefusalBody::of(body);
    let mut message = match (refusal.error, refusal.code) {
        (Some(error), Some(code)) => format!("{} ({}, HTTP {})", error, code, status.as_u16()),
        (Some(error), None) => format!("{} (HTTP {})", error, status.as_u16()),
        (None, Some(code)) => format!("{} (HTTP {})", code, status.as_u16()),
        (None, None) => format!("Lambda returned {}: {}", status, body),
    };
    // `partial_refused` echoes the list it would not accept. Naming the files
    // is the difference between "re-run the scan" and "re-run the scan and
    // watch these", so the refusal carries them rather than only its code.
    if let Some(files) = refusal.unanalysed_files.filter(|f| !f.is_empty()) {
        let named: Vec<&str> = files.iter().take(5).map(|f| f.path.as_str()).collect();
        message.push_str(&format!(
            ". {} file(s) had no analysis: {}{}",
            files.len(),
            named.join(", "),
            if files.len() > named.len() {
                format!(" and {} more", files.len() - named.len())
            } else {
                String::new()
            }
        ));
    }
    message
}

/// Say what the cloud accepted when it took an index that is missing files.
///
/// Only reachable on a first index of this service: once there are rows to
/// protect the same list is refused with `409 partial_refused`, which arrives
/// as an error rather than here. The echoed list is the server's, not the
/// scanner's, so the user is told what was actually stored.
fn report_partial_acceptance(response: &WriteActionResponse, data: &CloudRepoData) {
    if response.partial != Some(true) {
        return;
    }
    let files = response.unanalysed_files.as_deref().unwrap_or_default();
    let named: Vec<&str> = files.iter().take(3).map(|f| f.path.as_str()).collect();
    let and_more = if files.len() > named.len() {
        format!(" and {} more", files.len() - named.len())
    } else {
        String::new()
    };
    let service = data.service_name.as_deref().unwrap_or(&data.repo_name);
    warn!(
        "Carrick indexed {} without {} file(s) the model did not answer for ({}{}). \
         This was accepted because {} had no index yet; run the scan again to fill them in.",
        service,
        files.len(),
        named.join(", "),
        and_more,
        service
    );
}

/// Whether a refusal is about the credential rather than about the request.
///
/// A 401 always is. A 403 only when the kind gate raised it: the same status
/// carries `repo_not_authorized` and `repo_not_connected`, which a fresh
/// consent does not fix and which name their own remedy.
fn is_credential_rejection(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
        || (status == reqwest::StatusCode::FORBIDDEN
            && RefusalBody::of(body).code.as_deref() == Some("wrong_key_kind"))
}

/// What a laptop is told when the cloud refuses its credential.
///
/// A Bearer credential cannot be re-minted, so the OIDC path's "mint a fresh
/// one and retry" has no equivalent: the only thing that fixes it is a new
/// consent (§8.2). Also the shape a `403 wrong_key_kind` takes, which is what
/// an `mcp`-scoped credential gets until the user logs in again.
fn relogin_message(status: reqwest::StatusCode, body: &str) -> String {
    let mut message = format!(
        "Carrick rejected this credential: {}. Run carrick login and try again.",
        refusal_message(status, body)
    );
    if let Some(hint) = crate::credentials::relogin_hint() {
        message.push(' ');
        message.push_str(&hint);
    }
    message
}

pub struct AwsStorage {
    lambda_url: String,
    http_client: Client,
    auth: CloudAuth,
    /// The slot the cloud minted for this run, from `start-scan`. Present only
    /// on the laptop path; every prompt-lambda call and every write action of
    /// the run carries it, and it is what the money gates key on (C4).
    scan_id: std::sync::OnceLock<String>,
    /// The tree this run scanned did not match its commit. Sent to
    /// `start-scan`, stamped on every payload, and folded into
    /// `force_reindex` so a second scan at the same HEAD is not told the
    /// index is already current (C10).
    dirty: std::sync::atomic::AtomicBool,
    /// Whether the cloud advertises a service-aware index key (set from the
    /// health-check response, or from `start-scan` on the laptop path). Until the cloud key includes a service
    /// discriminator this stays false, which gates multi-service uploads so
    /// they can't clobber each other.
    multi_service: std::sync::atomic::AtomicBool,
    /// This run re-analyzed every file (`--no-cache`, which the Action sets
    /// from `full-scan`), so its answers supersede whatever the index holds
    /// for this commit. Set once from the CLI flag and sent on every write
    /// action; see `force_reindex` on [`LambdaRequest`].
    force_reindex: bool,
}

#[derive(Serialize)]
struct LambdaRequest {
    action: String,
    repo: String,
    /// Service discriminator for the cloud index key. Repos can declare
    /// multiple services in carrick.json; the cloud keys each upload by
    /// (repo, service) so they don't clobber each other. Must be sent on
    /// every keyed action (including the bare existence check, which carries
    /// no `cloudRepoData`), or the cloud falls back to the repo name and all
    /// services collapse onto one row.
    #[serde(rename = "service_name", skip_serializing_if = "Option::is_none")]
    service_name: Option<String>,
    hash: String,
    filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "cloudRepoData")]
    cloud_repo_data: Option<CloudRepoData>,
    #[serde(rename = "s3Url")]
    #[serde(skip_serializing_if = "Option::is_none")]
    s3_url: Option<String>,
    /// Payload staging (carrick#486): on check-or-upload, ask the cloud to
    /// mint a presigned PUT URL for the raw CloudRepoData because it exceeds
    /// [`INLINE_PAYLOAD_LIMIT_BYTES`].
    #[serde(rename = "wantsPayloadUrl")]
    #[serde(skip_serializing_if = "Option::is_none")]
    wants_payload_url: Option<bool>,
    /// Payload staging: on complete-upload / store-metadata, signal that the
    /// CloudRepoData was PUT to the staging object instead of sent inline.
    #[serde(rename = "payloadInS3")]
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_in_s3: Option<bool>,
    /// Payload staging integrity (carrick#536): lowercase-hex SHA-256 of the
    /// exact bytes PUT to the staging object. The cloud verifies the object it
    /// fetches against this before parsing it, so a truncated or swapped blob
    /// is rejected instead of indexed. Only meaningful alongside
    /// `payloadInS3`; when `cloudRepoData` rides inline the request body is
    /// itself the authenticated payload and there is no second hop to verify.
    #[serde(rename = "payloadSha256")]
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_sha256: Option<String>,
    /// Byte length of those same bytes. Checked off the cloud's HeadObject
    /// before the download, so a truncated PUT costs one head request.
    #[serde(rename = "payloadSize")]
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_size: Option<u64>,
    /// This run re-analyzed the tree from scratch and supersedes the stored
    /// generation for this (repo, service), even at the same commit and the
    /// same scanner version (carrick#885).
    ///
    /// The cloud's freshness guard reasons about the SOURCE: same hash, same
    /// release, therefore nothing to do. `--no-cache` is a statement about
    /// the ANSWERS, and on an unchanged tree the two agree that nothing needs
    /// doing — which is exactly the case the flag exists for, so the whole
    /// re-analysis was computed at model cost and discarded. Only this side
    /// knows it superseded its own generation, so only this side can say so.
    ///
    /// Sent on the two write actions only, never on the bare existence check
    /// (which indexes nothing). Omitted entirely when the run reused its
    /// cache, so an ordinary upload's body is byte-for-byte what it was, and
    /// a cloud deployed before the field existed ignores it.
    #[serde(skip_serializing_if = "Option::is_none")]
    force_reindex: Option<bool>,
    /// The slot `start-scan` minted for this run. Ties the write to the
    /// in-flight slot and to the money meters; required when the credential
    /// kind is `cli`, omitted entirely on the CI path so an Action upload's
    /// body is byte-for-byte what it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    scan_id: Option<String>,
    /// Files the model was asked about and did not answer for, on the two
    /// write actions. The cloud's first-scan partial rule reads it: a service
    /// with no hosted rows accepts the partial index and echoes the list back,
    /// one that already has rows refuses with `409 partial_refused`. Omitted
    /// when empty, and never sent on the CI path where the scanner's own gate
    /// already stopped the run (§4).
    #[serde(skip_serializing_if = "Option::is_none")]
    unanalysed_files: Option<Vec<UnanalysedFile>>,
    /// The last write action of the run, which is what releases the cloud's
    /// in-flight scan slot. A multi-service repo sends N write actions and
    /// only this one carries it; absent, the slot falls to its TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    scan_final: Option<bool>,
}

/// `start-scan`: the first thing a laptop run does, before a single model
/// call is paid for.
///
/// It decides three things the scanner cannot: may this holder write this
/// repo, is a slot free, and is this the first index. It also replaces the
/// health-check probe on this path, which a workspace-scoped credential
/// cannot reach at all (C9).
#[derive(Serialize)]
struct StartScanRequest<'a> {
    action: &'a str,
    /// The full `owner/repo`, not the basename every other action on this
    /// endpoint sends. The cloud canonicalises it for the index key itself.
    repo: &'a str,
    commit: &'a str,
    dirty: bool,
}

/// The 200 body of `start-scan`. Additive-tolerant: every field the scanner
/// does not need is ignored, and the schema tag is the handshake.
#[derive(Deserialize)]
struct StartScanResponse {
    schema: String,
    scan_id: String,
    #[allow(dead_code)]
    project_id: String,
    project_slug: String,
    #[serde(default)]
    indexed_services: Vec<String>,
    /// The cloud key carries a service discriminator. Sets the flag the
    /// health check sets on the CI path and cannot set here.
    #[serde(default)]
    multi_service: bool,
    #[serde(default)]
    allowance_sentence: Option<String>,
}

/// The tag `start-scan` must answer under. Anything else is a cloud that has
/// not deployed this action, and the scanner says so rather than reading a
/// body it does not understand.
const START_SCAN_SCHEMA: &str = "carrick.start-scan/0";

/// Integrity fields for a payload that was staged to S3 rather than inlined.
/// Computed once, from the same serialized bytes that are PUT.
#[derive(Clone)]
struct StagedPayload {
    sha256: String,
    size: u64,
}

impl StagedPayload {
    fn of(serialized: &str) -> Self {
        Self {
            sha256: format!("{:x}", Sha256::digest(serialized.as_bytes())),
            size: serialized.len() as u64,
        }
    }
}

#[derive(Deserialize)]
struct LambdaResponse {
    #[allow(dead_code)]
    exists: bool,
    #[serde(rename = "s3Url")]
    s3_url: String,
    #[serde(rename = "uploadUrl")]
    #[allow(dead_code)]
    upload_url: Option<String>,
    /// Presigned PUT URL for the payload-staging object (carrick#486). Only
    /// present when the request set `wantsPayloadUrl` AND the deployed cloud
    /// supports staging; `default` so older clouds simply omit it.
    #[serde(rename = "payloadUploadUrl")]
    #[serde(default)]
    payload_upload_url: Option<String>,
    #[allow(dead_code)]
    hash: String,
    #[serde(default)]
    #[allow(dead_code)]
    adjacent: Vec<AdjacentRepo>,
    /// Cloud capability: true once the index key includes a service
    /// discriminator, so multiple services per repo can coexist. Absent on
    /// older clouds, defaulting to false (gated).
    #[serde(default, rename = "multiService")]
    multi_service: bool,
}

/// Envelope for the `post-pr-result` action: the transport adds the action
/// tag and schema version; every other wire field comes verbatim from the
/// flattened [`crate::findings::PrResultPayload`].
#[derive(Serialize)]
struct PostPrResultRequest<'a> {
    action: &'a str,
    schema_version: u32,
    #[serde(flatten)]
    payload: &'a crate::findings::PrResultPayload,
}

/// The 200 body of either write action (`store-metadata` / `complete-upload`).
/// Both also return `success` / `message` (and complete-upload an `s3Url` +
/// `metadata`), none of which the scanner reads — only whether the cloud
/// short-circuited. Every field is defaulted so a body that omits any of them
/// still parses.
#[derive(Deserialize, Default)]
struct WriteActionResponse {
    /// True when the cloud found a stored row already carrying this commit
    /// hash AND this scanner version, so it skipped re-indexing. Absent on
    /// clouds deployed before the check existed, which reads as "it indexed".
    #[serde(default)]
    already_current: Option<bool>,
    /// The cloud accepted an index that is missing files, because this
    /// service had no hosted rows to protect. Present only in that case.
    #[serde(default)]
    partial: Option<bool>,
    /// The list it accepted, echoed back, so the surface names exactly what
    /// the server took rather than what the scanner sent.
    #[serde(default)]
    unanalysed_files: Option<Vec<UnanalysedFile>>,
    /// What this scan cost, on the write action that carried `scan_final` and
    /// on a `cli` credential only (carrick-cloud#806/#813). Absent on every
    /// other response, including every CI upload, and on a cloud deployed
    /// before the field existed — all of which read as "no figure to state"
    /// rather than as a zero (carrick#995).
    #[serde(default)]
    scan_spend: Option<crate::scan_spend::ScanSpend>,
}

impl WriteActionResponse {
    /// What this write action did, for the run that made it.
    ///
    /// A spend under a tag this scanner does not read is dropped here rather
    /// than carried: one place decides, so no surface downstream has to.
    fn outcome(self) -> UploadOutcome {
        UploadOutcome {
            already_current: self.already_current.unwrap_or(false),
            scan_spend: self
                .scan_spend
                .filter(crate::scan_spend::ScanSpend::understood),
        }
    }
}

#[derive(Deserialize)]
struct AdjacentRepo {
    repo: String,
    hash: String,
    #[serde(rename = "s3Url")]
    s3_url: String,
    #[allow(dead_code)]
    filename: String,
    metadata: Option<CloudRepoData>, // Now includes full metadata!
    #[serde(rename = "lastUpdated")]
    #[allow(dead_code)]
    last_updated: Option<String>,
}

#[derive(Deserialize)]
struct CrossRepoResponse {
    /// Absent (and defaulted empty) on a staged response — the cloud omits it
    /// deliberately so an older scanner fails loudly instead of proceeding
    /// with an empty sibling set; this scanner checks `staged_url` FIRST and
    /// only reads `repos` off the followed body.
    #[serde(default)]
    repos: Vec<AdjacentRepo>,
    /// Staged read (carrick-cloud#456): when the project aggregate outgrows
    /// the Lambda response cap, the cloud parks the real CrossRepoResponse
    /// JSON in S3 and returns `staged: true` plus this short-lived presigned
    /// GET URL instead of the body.
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    staged_url: Option<String>,
}

#[derive(Serialize)]
struct GetCrossRepoRequest {
    action: String,
}

/// The shared configuration of every cloud-call client.
///
/// Transparent gzip is not configured here — it comes from reqwest's `gzip`
/// crate feature (see Cargo.toml), which makes every client this builder
/// produces send `Accept-Encoding: gzip` and inflate a `Content-Encoding:
/// gzip` response before the body is read. That matters for
/// `get-cross-repo-data`, whose response inlines every repo's index blob and
/// breaches AWS Lambda's 6,291,556-byte synchronous-response cap on large
/// projects; the cloud gzips it, but only for callers that advertise gzip.
///
/// Exists as a builder rather than a finished `Client` so tests can add
/// `.no_proxy()` and still exercise the production configuration.
fn http_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
}

impl AwsStorage {
    /// `force_reindex` is the run's `--no-cache`: this scan re-analyzed every
    /// file, so its write actions tell the cloud to replace the stored
    /// generation instead of short-circuiting on the commit hash
    /// (carrick#885). A property of the RUN, not of any one blob, which is
    /// why it lives here and not on `CloudRepoData` — a flag inside the blob
    /// would be stored in the index and read back as next scan's cache.
    pub fn new(force_reindex: bool) -> Result<Self, StorageError> {
        let api_endpoint = env!("CARRICK_API_ENDPOINT");
        let lambda_url = format!("{}/types/check-or-upload", api_endpoint);

        // Two ways to authenticate now, and which one this process uses is
        // decided here rather than per request. A laptop has no OIDC to mint
        // and a runner has no credential file to read, so the choice never
        // changes inside a run. Wire contract: carrick-cloud
        // `docs/internal/reference/laptop-scan-seam.md` §1.3 and §8.2.
        let auth = CloudAuth::detect().map_err(StorageError::ConnectionError)?;

        let http_client = http_client_builder().build().map_err(|e| {
            StorageError::ConnectionError(format!("Failed to build HTTP client: {}", e))
        })?;

        Ok(Self::with_parts(
            lambda_url,
            http_client,
            auth,
            force_reindex,
        ))
    }

    fn with_parts(
        lambda_url: String,
        http_client: Client,
        auth: CloudAuth,
        force_reindex: bool,
    ) -> Self {
        Self {
            lambda_url,
            http_client,
            auth,
            scan_id: std::sync::OnceLock::new(),
            dirty: std::sync::atomic::AtomicBool::new(false),
            multi_service: std::sync::atomic::AtomicBool::new(false),
            force_reindex,
        }
    }

    /// A client pointed at a local test server, with a chosen auth mode. The
    /// production configuration otherwise, plus `no_proxy` so an ambient
    /// proxy variable cannot intercept the loopback call.
    #[cfg(test)]
    pub(crate) fn for_test(lambda_url: &str, auth: CloudAuth, force_reindex: bool) -> Self {
        Self::with_parts(
            lambda_url.to_string(),
            http_client_builder().no_proxy().build().unwrap(),
            auth,
            force_reindex,
        )
    }

    /// Whether the index this run writes describes a tree that did not match
    /// its commit. Folded into `force_reindex` because the cloud's freshness
    /// guard reasons about the commit hash, and a dirty run's hash is a claim
    /// the tree does not support: without this a second scan at the same HEAD
    /// is told the index is current and the dirty rows survive (C10).
    fn forces_reindex(&self) -> bool {
        self.force_reindex || self.dirty.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The slot this run holds, for the write actions. `None` on the CI path,
    /// where the field is omitted entirely.
    fn scan_id(&self) -> Option<String> {
        self.scan_id.get().cloned()
    }

    /// The files this run lost, for the write actions, or `None` when it lost
    /// none or is not on the laptop path.
    ///
    /// CI never sends the list: its own gate already aborted the run before
    /// the upload, so a CI body is byte-for-byte what it was, and a CI caller
    /// that set `CARRICK_ALLOW_PARTIAL_ANALYSIS` must not be answered with
    /// the laptop rule's `409 partial_refused`.
    fn unanalysed_files(&self) -> Option<Vec<UnanalysedFile>> {
        if !self.auth.is_bearer() {
            return None;
        }
        let lost = crate::scan_health::unanalysed_files();
        (!lost.is_empty()).then_some(lost)
    }

    /// POSTs a JSON body to the upload endpoint with the OIDC bearer header,
    /// returning the raw response body on success. OIDC tokens are short-lived
    /// and a large scan outlives one, so the token is read per attempt (the
    /// provider re-mints as it nears expiry) and a 401 still gets one reactive
    /// re-mint and retry.
    /// Transient failures (network errors, 408/429/5xx) are retried with
    /// exponential backoff, up to [`max_retries_for_action`] times for the
    /// named action — the full budget for reads, one retry for the actions
    /// that write the index, where a lost response does not mean a lost write.
    async fn send_lambda<B>(&self, action: &str, body: &B) -> Result<String, StorageError>
    where
        B: serde::Serialize + ?Sized,
    {
        match &self.auth {
            CloudAuth::Oidc => self.send_lambda_oidc(action, body).await,
            CloudAuth::Bearer(token) => self.send_lambda_bearer(action, body, token).await,
        }
    }

    /// The laptop path: one long-lived credential, sent as `Authorization`.
    ///
    /// Wire contract: carrick-cloud
    /// `docs/internal/reference/laptop-scan-seam.md`, §1.3 for the headers,
    /// §2.1 for the gate's refusals and §8.2 for why this branch exists.
    ///
    /// Shorter than the OIDC loop by exactly the part that cannot apply — a
    /// Bearer credential cannot be re-minted, so a rejection is "run carrick
    /// login", not a retry (§8.2). Transient statuses keep the same budget,
    /// and every gate refusal is a 409, which `is_transient_status` already
    /// excludes: a 429 would be retried with backoff, which is why the gates
    /// are not allowed to use one (C6).
    async fn send_lambda_bearer<B>(
        &self,
        action: &str,
        body: &B,
        token: &str,
    ) -> Result<String, StorageError>
    where
        B: serde::Serialize + ?Sized,
    {
        let max_retries = max_retries_for_action(action);
        let mut retries = 0u32;
        loop {
            let transient_error = match self
                .http_client
                .post(&self.lambda_url)
                .header("Authorization", format!("Bearer {}", token))
                .json(body)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    match response.text().await {
                        Ok(response_text) => {
                            if status.is_success() {
                                return Ok(response_text);
                            }
                            // Not every 403 is a credential problem. The gate
                            // answers `repo_not_authorized` and
                            // `repo_not_connected` with one too, and telling a
                            // user to log in again when the repo is simply not
                            // connected sends them to fix the one thing that
                            // is already right (§2.1). Only a 401, or the
                            // kind gate itself, is a re-login.
                            if is_credential_rejection(status, &response_text) {
                                return Err(StorageError::ConnectionError(relogin_message(
                                    status,
                                    &response_text,
                                )));
                            }
                            if !is_transient_status(status) {
                                return Err(StorageError::ConnectionError(refusal_message(
                                    status,
                                    &response_text,
                                )));
                            }
                            refusal_message(status, &response_text)
                        }
                        Err(e) => format!("Failed to read response: {}", e),
                    }
                }
                Err(e) => format!("Lambda request failed: {}", e),
            };

            if retries >= max_retries {
                return Err(StorageError::ConnectionError(retry_exhausted_message(
                    action,
                    &transient_error,
                    retries + 1,
                )));
            }

            let backoff = retry_backoff(retries);
            warn!(
                "{}; retrying in {}s ({}/{})",
                transient_error,
                backoff.as_secs(),
                retries + 1,
                max_retries
            );
            tokio::time::sleep(backoff).await;
            retries += 1;
        }
    }

    async fn send_lambda_oidc<B>(&self, action: &str, body: &B) -> Result<String, StorageError>
    where
        B: serde::Serialize + ?Sized,
    {
        let max_retries = max_retries_for_action(action);
        let provider =
            OidcProvider::global().map_err(|e| StorageError::ConnectionError(e.to_string()))?;

        let mut reminted = false;
        let mut retries = 0u32;
        loop {
            // Per attempt, not once per call: the upload is the last thing a
            // scan does, and on a long scan the token minted at the start has
            // expired by the time an upload retry goes out (#461). The provider
            // serves the cached token until it nears its own expiry, so this
            // costs a lock, not a request.
            let token = provider
                .token()
                .await
                .map_err(|e| StorageError::ConnectionError(e.to_string()))?;

            let transient_error = match self
                .http_client
                .post(&self.lambda_url)
                .header("X-Carrick-OIDC", &token)
                .json(body)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    match response.text().await {
                        Ok(response_text) => {
                            if is_oidc_rejection(status.as_u16(), &response_text) {
                                if reminted {
                                    return Err(StorageError::ConnectionError(format!(
                                        "Carrick Cloud rejected a freshly minted OIDC token \
                                         (status {}): {}",
                                        status, response_text
                                    )));
                                }
                                warn!(
                                    "Upload returned {}; the OIDC token was rejected, re-minting and retrying",
                                    status
                                );
                                provider
                                    .remint(&token)
                                    .await
                                    .map_err(|e| StorageError::ConnectionError(e.to_string()))?;
                                reminted = true;
                                continue;
                            }

                            if status.is_success() {
                                return Ok(response_text);
                            }

                            if !is_transient_status(status) {
                                return Err(StorageError::ConnectionError(format!(
                                    "Lambda returned error {}: {}",
                                    status, response_text
                                )));
                            }

                            format!("Lambda returned {}: {}", status, response_text)
                        }
                        Err(e) => format!("Failed to read response: {}", e),
                    }
                }
                Err(e) => format!("Lambda request failed: {}", e),
            };

            if retries >= max_retries {
                return Err(StorageError::ConnectionError(retry_exhausted_message(
                    action,
                    &transient_error,
                    retries + 1,
                )));
            }

            let backoff = retry_backoff(retries);
            warn!(
                "{}; retrying in {}s ({}/{})",
                transient_error,
                backoff.as_secs(),
                retries + 1,
                max_retries
            );
            tokio::time::sleep(backoff).await;
            retries += 1;
        }
    }

    async fn call_lambda<T>(&self, request: &LambdaRequest) -> Result<T, StorageError>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let response_text = self.send_lambda(&request.action, request).await?;
        serde_json::from_str(&response_text).map_err(|e| {
            StorageError::SerializationError(format!(
                "Failed to parse lambda response for action '{}': {}. Raw response: {}",
                request.action, e, response_text
            ))
        })
    }

    async fn call_lambda_generic<Req, Resp>(
        &self,
        action: &str,
        request: &Req,
    ) -> Result<Resp, StorageError>
    where
        Req: serde::Serialize,
        Resp: for<'de> serde::Deserialize<'de>,
    {
        let response_text = self.send_lambda(action, request).await?;
        serde_json::from_str(&response_text).map_err(|e| {
            StorageError::SerializationError(format!("Failed to parse lambda response: {}", e))
        })
    }

    /// PUTs content to a pre-signed S3 URL. The PUT is idempotent, so
    /// transient failures (network errors, 5xx) are retried with backoff.
    async fn upload_to_s3(&self, upload_url: &str, content: &str) -> Result<(), StorageError> {
        self.upload_to_s3_with_content_type(upload_url, content, "text/plain")
            .await
    }

    async fn upload_to_s3_with_content_type(
        &self,
        upload_url: &str,
        content: &str,
        content_type: &str,
    ) -> Result<(), StorageError> {
        let mut retries = 0u32;
        loop {
            let transient_error = match self
                .http_client
                .put(upload_url)
                .header("Content-Type", content_type)
                .body(content.to_string())
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    // Always include the response body — S3 returns the actual
                    // cause (AccessDenied, signature mismatch, missing header,
                    // etc.) in the XML error document. A bare status code is
                    // rarely actionable.
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    if !is_transient_status(status) {
                        return Err(StorageError::ConnectionError(format!(
                            "S3 upload returned {}: {}",
                            status, body
                        )));
                    }
                    format!("S3 upload returned {}: {}", status, body)
                }
                Err(e) => format!("S3 upload failed: {}", e),
            };

            if retries >= MAX_TRANSIENT_RETRIES {
                return Err(StorageError::ConnectionError(format!(
                    "{} (after {} attempts)",
                    transient_error,
                    retries + 1
                )));
            }

            let backoff = retry_backoff(retries);
            warn!(
                "{}; retrying in {}s ({}/{})",
                transient_error,
                backoff.as_secs(),
                retries + 1,
                MAX_TRANSIENT_RETRIES
            );
            tokio::time::sleep(backoff).await;
            retries += 1;
        }
    }

    async fn store_repo_metadata(
        &self,
        data: &CloudRepoData,
        s3_url: &str,
        staged: Option<&StagedPayload>,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let request = LambdaRequest {
            action: "store-metadata".to_string(),
            repo: data.repo_name.clone(),
            service_name: data.service_name.clone(),
            hash: data.commit_hash.clone(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: staged.is_none().then(|| data.clone()),
            s3_url: Some(s3_url.to_string()),
            wants_payload_url: None,
            payload_in_s3: staged.is_some().then_some(true),
            payload_sha256: staged.map(|s| s.sha256.clone()),
            payload_size: staged.map(|s| s.size),
            force_reindex: self.forces_reindex().then_some(true),
            scan_id: self.scan_id(),
            unanalysed_files: self.unanalysed_files(),
            scan_final: final_in_run.then_some(true),
        };

        let response: WriteActionResponse = self.call_lambda(&request).await?;
        debug!("Successfully stored metadata for {}", data.repo_name);
        report_partial_acceptance(&response, data);

        Ok(response.outcome())
    }

    /// Claim a scan slot and resolve the project, before any spend.
    ///
    /// The laptop path's replacement for the health-check probe (C9). It
    /// proves connectivity, that this holder may write this repo, that a slot
    /// is free, and whether this is a first index — and it answers
    /// `multi_service`, which the probe is the only other source of and which
    /// gates a multi-service upload. Every refusal is final: the gates answer
    /// 409, which is not a transient status, so nothing here is retried (C6).
    async fn start_scan(&self, run: &RunContext) -> Result<RunStart, StorageError> {
        let repo = run.repo_full_name.as_deref().ok_or_else(|| {
            StorageError::ConnectionError(
                "This directory has no github.com origin remote, so Carrick cannot tell the \
                 cloud which repository it is scanning. Add the remote, or run the scan in CI."
                    .to_string(),
            )
        })?;
        let request = StartScanRequest {
            action: "start-scan",
            repo,
            commit: &run.commit,
            dirty: run.dirty,
        };
        let response: StartScanResponse =
            self.call_lambda_generic(request.action, &request).await?;
        if response.schema != START_SCAN_SCHEMA {
            return Err(StorageError::ConnectionError(format!(
                "Carrick Cloud answered start-scan with schema '{}'; this scanner reads {}. \
                 Upgrade with npm i -g carrick@latest.",
                response.schema, START_SCAN_SCHEMA
            )));
        }
        self.multi_service
            .store(response.multi_service, std::sync::atomic::Ordering::Relaxed);
        self.dirty
            .store(run.dirty, std::sync::atomic::Ordering::Relaxed);
        // `set` rather than an assignment: one run opens one scan, and a
        // second start-scan would mean two slots for one upload. Published to
        // the process-global as well, because the four prompt-lambda clients
        // are built far from here and each of their calls must carry it.
        crate::credentials::set_scan_id(&response.scan_id);
        let _ = self.scan_id.set(response.scan_id);
        info!(
            "Scanning {} into project {} ({} service(s) already indexed)",
            repo,
            response.project_slug,
            response.indexed_services.len()
        );
        Ok(RunStart {
            allowance_sentence: response.allowance_sentence,
            indexed_services: Some(response.indexed_services),
        })
    }

    /// Stage an oversized serialized CloudRepoData to the presigned URL from
    /// check-or-upload (carrick#486). Errors clearly when the deployed cloud
    /// doesn't mint staging URLs yet, since the inline fallback is guaranteed
    /// to die at the request-size walls.
    async fn stage_payload(
        &self,
        payload_upload_url: Option<&str>,
        serialized: &str,
        repo: &str,
    ) -> Result<(), StorageError> {
        let url = payload_upload_url.ok_or_else(|| {
            StorageError::ConnectionError(format!(
                "serialized payload for {} is {} bytes (over the {} byte inline limit) \
                 but the cloud did not return payloadUploadUrl — deploy carrick-cloud \
                 with payload staging (carrick#486) first",
                repo,
                serialized.len(),
                INLINE_PAYLOAD_LIMIT_BYTES
            ))
        })?;
        debug!(
            "Staging {} byte payload for {} via presigned S3 URL",
            serialized.len(),
            repo
        );
        self.upload_to_s3_with_content_type(url, serialized, "application/json")
            .await
    }
}

#[async_trait]
impl CloudStorage for AwsStorage {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let repo = &data.repo_name;

        // Payload staging decision (carrick#486): measure the serialized
        // CloudRepoData once. Over the inline limit, ask check-or-upload for
        // a presigned staging URL and keep the write-action bodies small.
        let serialized = serde_json::to_string(data).map_err(|e| {
            StorageError::SerializationError(format!("Failed to serialize repo data: {}", e))
        })?;
        let stage_payload = serialized.len() > INLINE_PAYLOAD_LIMIT_BYTES;

        // Integrity fields for the staged object, digested once over exactly
        // the bytes that get PUT (carrick#536). Not computed on the inline
        // path, where the request body is itself the authenticated payload.
        let staged = stage_payload.then(|| StagedPayload::of(&serialized));

        // Step 1: Check if we need to upload type file
        let check_request = LambdaRequest {
            action: "check-or-upload".to_string(),
            repo: repo.clone(),
            service_name: data.service_name.clone(),
            hash: data.commit_hash.clone(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: stage_payload.then_some(true),
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            // The existence check indexes nothing, so there is nothing for it
            // to supersede; the flag rides the write actions below.
            force_reindex: None,
            // The slot rides every action of the run, including this one: the
            // cloud ties the whole scan to it, not only the writes (§2.2).
            scan_id: self.scan_id(),
            // Nothing is indexed here, so there is no partial index for the
            // cloud to accept or refuse, and the slot is not released by a
            // check.
            unanalysed_files: None,
            scan_final: None,
        };

        let lambda_response: LambdaResponse = self.call_lambda(&check_request).await?;

        if stage_payload {
            self.stage_payload(
                lambda_response.payload_upload_url.as_deref(),
                &serialized,
                repo,
            )
            .await?;
        }

        // Step 2: Upload type file if needed
        if let Some(upload_url) = lambda_response.upload_url {
            if let Some(bundled_types) = data.bundled_types.as_ref() {
                debug!("Uploading bundled types to S3...");
                self.upload_to_s3(&upload_url, bundled_types).await?;

                // Step 3: Complete the upload by storing metadata
                let complete_request = LambdaRequest {
                    action: "complete-upload".to_string(),
                    repo: repo.clone(),
                    service_name: data.service_name.clone(),
                    hash: data.commit_hash.clone(),
                    filename: "types.d.ts".to_string(),
                    cloud_repo_data: (!stage_payload).then(|| data.clone()),
                    s3_url: Some(lambda_response.s3_url),
                    wants_payload_url: None,
                    payload_in_s3: stage_payload.then_some(true),
                    payload_sha256: staged.as_ref().map(|s| s.sha256.clone()),
                    payload_size: staged.as_ref().map(|s| s.size),
                    force_reindex: self.forces_reindex().then_some(true),
                    scan_id: self.scan_id(),
                    unanalysed_files: self.unanalysed_files(),
                    scan_final: final_in_run.then_some(true),
                };

                let complete_response: WriteActionResponse =
                    self.call_lambda(&complete_request).await?;
                debug!("Successfully completed upload and stored metadata");
                report_partial_acceptance(&complete_response, data);
                Ok(complete_response.outcome())
            } else {
                debug!(
                    "No bundled types available for {}; storing metadata only",
                    repo
                );
                self.store_repo_metadata(
                    data,
                    &lambda_response.s3_url,
                    staged.as_ref(),
                    final_in_run,
                )
                .await
            }
        } else {
            debug!("Type file already exists, just updating metadata");
            self.store_repo_metadata(data, &lambda_response.s3_url, staged.as_ref(), final_in_run)
                .await
        }
    }

    async fn upload_type_file(
        &self,
        repo_name: &str,
        file_name: &str,
        content: &str,
    ) -> Result<(), StorageError> {
        let commit_hash = crate::cloud_storage::get_current_commit_hash(".");

        let request = LambdaRequest {
            action: "check-or-upload".to_string(),
            repo: repo_name.to_string(),
            // upload_type_file is not service-scoped (no CloudRepoData in scope);
            // the cloud falls back to the repo name, matching legacy behaviour.
            service_name: None,
            hash: commit_hash,
            filename: file_name.to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };

        let lambda_response: LambdaResponse = self.call_lambda(&request).await?;

        if let Some(upload_url) = lambda_response.upload_url {
            self.upload_to_s3(&upload_url, content).await?;
        }

        Ok(())
    }

    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        let request = GetCrossRepoRequest {
            action: "get-cross-repo-data".to_string(),
        };

        let mut response: CrossRepoResponse =
            self.call_lambda_generic(&request.action, &request).await?;

        // Staged read (carrick-cloud#456): the aggregate outgrew the Lambda
        // response cap, so the body is behind a presigned GET. Plain request,
        // no Authorization header — the URL carries its own auth and S3
        // rejects a request presenting both.
        if response.staged {
            let url = response.staged_url.as_deref().ok_or_else(|| {
                StorageError::ConnectionError(
                    "staged cross-repo response carried no staged_url".to_string(),
                )
            })?;
            info!("Cross-repo data staged to S3 by the cloud; following presigned URL");
            let staged = self.http_client.get(url).send().await.map_err(|e| {
                StorageError::ConnectionError(format!("staged cross-repo fetch failed: {e}"))
            })?;
            if !staged.status().is_success() {
                return Err(StorageError::ConnectionError(format!(
                    "staged cross-repo fetch returned {}",
                    staged.status()
                )));
            }
            response = staged.json::<CrossRepoResponse>().await.map_err(|e| {
                StorageError::SerializationError(format!(
                    "staged cross-repo body failed to parse: {e}"
                ))
            })?;
        }

        let mut all_repo_data = Vec::new();
        let mut repo_s3_urls = HashMap::new();

        for adjacent in response.repos {
            if let Some(metadata) = adjacent.metadata {
                debug!("Processing repo: {} with full metadata", adjacent.repo);
                repo_s3_urls.insert(metadata.repo_name.clone(), adjacent.s3_url);
                all_repo_data.push(metadata);
            } else {
                warn!("No metadata found for repo: {}", adjacent.repo);
                let repo_data = CloudRepoData {
                    repo_name: adjacent.repo.clone(),
                    service_name: None,
                    endpoints: Vec::new(),
                    calls: Vec::new(),
                    mounts: Vec::new(),
                    apps: HashMap::new(),
                    imported_handlers: Vec::new(),
                    function_definitions: HashMap::new(),
                    config_json: None,
                    package_json: None,
                    packages: None,
                    last_updated: chrono::Utc::now(),
                    commit_hash: adjacent.hash,
                    dirty: None,
                    mount_graph: None,
                    bundled_types: None,
                    type_manifest: None,
                    file_results: None,
                    cached_detection: None,
                    cached_guidance: None,
                    cached_extraction_config: None,
                    package_json_hash: None,
                    cache_version: None,
                    type_extraction_status: None,
                    types_degraded: None,
                    compat_verdicts: None,
                    capture_stub: None,
                    external_call_candidates: None,
                    sdk_surface: None,
                    sdk_edges: None,
                    sdk_unresolved: None,
                    scanner_version: None,
                    boundary: None,
                    dispatch_tables: None,
                };
                repo_s3_urls.insert(adjacent.repo.clone(), adjacent.s3_url);
                all_repo_data.push(repo_data);
            }
        }

        Ok((all_repo_data, repo_s3_urls))
    }

    async fn upload_logs(&self, repo: &str, log_content: &str) -> Result<(), StorageError> {
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();

        #[derive(Serialize)]
        struct UploadLogsRequest {
            action: String,
            repo: String,
            timestamp: String,
        }

        #[derive(Deserialize)]
        struct UploadLogsResponse {
            #[serde(rename = "uploadUrl")]
            upload_url: String,
        }

        let request = UploadLogsRequest {
            action: "upload-logs".to_string(),
            repo: repo.to_string(),
            timestamp,
        };

        let resp: UploadLogsResponse = self.call_lambda_generic(&request.action, &request).await?;
        self.upload_to_s3(&resp.upload_url, log_content).await?;

        Ok(())
    }

    /// A laptop's debug log stays on the laptop.
    ///
    /// `upload-logs` is deliberately outside the set of actions a `cli`
    /// credential may take (§1.2): the log is a 0644 file naming the
    /// developer's own machine and paths, and shipping it to S3 is a CI
    /// affordance. Answering here rather than letting the call 403 keeps a
    /// guaranteed failure out of every laptop run's tail.
    fn uploads_run_logs(&self) -> bool {
        !self.auth.is_bearer()
    }

    async fn post_pr_result(
        &self,
        payload: &crate::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        // Dedicated action: unlike store-metadata/complete-upload it writes no
        // index data — the cloud gates on the project's pr_comments_enabled
        // toggle and renders/upserts the marked comment + check run itself
        // from these structured findings (OIDC identity, not the payload's
        // self-reported repo, decides where they land).
        let request = PostPrResultRequest {
            action: "post-pr-result",
            schema_version: 1,
            payload,
        };

        // Best-effort by contract (caller logs and swallows), but surface the
        // transport error so the caller can log a useful message.
        self.send_lambda(request.action, &request).await?;
        debug!(
            "Posted PR result for {} (PR #{})",
            payload.repo, payload.pr_number
        );
        Ok(())
    }

    async fn begin_run(&self, run: &RunContext) -> Result<RunStart, StorageError> {
        match &self.auth {
            // The CI path, unchanged: the probe's body, its response and its
            // error handling are exactly what they were.
            CloudAuth::Oidc => self.health_check().await.map(|()| RunStart::default()),
            CloudAuth::Bearer(_) => self.start_scan(run).await,
        }
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        let request = LambdaRequest {
            action: "check-or-upload".to_string(),
            repo: "health".to_string(),
            service_name: None,
            hash: "health-check".to_string(),
            filename: "health.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };

        match self.call_lambda::<LambdaResponse>(&request).await {
            Ok(resp) => {
                // Record whether the cloud advertises a service-aware key, so
                // the multi-service upload gate can open without a scanner
                // release once the cloud deploys the key change.
                self.multi_service
                    .store(resp.multi_service, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Err(StorageError::ConnectionError(msg))
                if msg.contains("401") || msg.contains("403") =>
            {
                Ok(()) // Lambda is responding, just rejecting our health check
            }
            Err(e) => Err(e),
        }
    }

    fn supports_multi_service(&self) -> bool {
        self.multi_service
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    // Oversized payloads go to the presigned staging object rather than the
    // request body. Unconditionally true, and honest because it is: this is a
    // property of the upload path, not a cloud capability to be discovered. A
    // cloud that has not shipped staging returns no `payloadUploadUrl`, and
    // `stage_payload` fails the upload with that named as the cause instead of
    // silently truncating the payload.
    fn stages_oversized_payloads(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn transient_statuses_are_retryable() {
        assert!(is_transient_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_transient_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_transient_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_transient_status(StatusCode::BAD_GATEWAY));
        assert!(is_transient_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_transient_status(StatusCode::GATEWAY_TIMEOUT));
    }

    #[test]
    fn permanent_statuses_are_not_retryable() {
        assert!(!is_transient_status(StatusCode::BAD_REQUEST));
        assert!(!is_transient_status(StatusCode::UNAUTHORIZED));
        assert!(!is_transient_status(StatusCode::FORBIDDEN));
        assert!(!is_transient_status(StatusCode::NOT_FOUND));
        assert!(!is_transient_status(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(retry_backoff(0), Duration::from_secs(2));
        assert_eq!(retry_backoff(1), Duration::from_secs(4));
        assert_eq!(retry_backoff(2), Duration::from_secs(8));
    }

    /// Payload-staging wire contract (carrick#486): the flags serialize under
    /// the camelCase names the cloud reads, and are omitted entirely when
    /// unset so requests to older clouds are byte-identical to pre-staging
    /// scanners.
    #[test]
    fn payload_staging_flags_serialize_by_name_and_omit_when_none() {
        let bare = LambdaRequest {
            action: "check-or-upload".to_string(),
            repo: "r".to_string(),
            service_name: None,
            hash: "h".to_string(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("wantsPayloadUrl"));
        assert!(!json.contains("payloadInS3"));

        let staged = LambdaRequest {
            wants_payload_url: Some(true),
            payload_in_s3: Some(true),
            ..bare
        };
        let json = serde_json::to_string(&staged).unwrap();
        assert!(json.contains("\"wantsPayloadUrl\":true"));
        assert!(json.contains("\"payloadInS3\":true"));
    }

    /// Staged-payload integrity (carrick#536): the digest is lowercase hex of
    /// the raw serialized bytes, and the size is their byte length — not the
    /// character count of some re-encoding of the same data.
    #[test]
    fn staged_payload_digest_is_lowercase_hex_of_the_raw_bytes() {
        // Known SHA-256 vector.
        let staged = StagedPayload::of("abc");
        assert_eq!(
            staged.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(staged.size, 3);
        assert_eq!(staged.sha256.len(), 64);
        assert!(
            staged
                .sha256
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );

        // Multi-byte characters count as bytes, matching what S3 stores.
        let multibyte = StagedPayload::of("{\"note\":\"café\"}");
        assert_eq!(multibyte.size, 16);
    }

    /// The integrity fields ride the write actions under the camelCase names
    /// the cloud reads, and are omitted on every request that does not stage —
    /// the inline body is itself the authenticated payload, so there is no
    /// second hop to verify.
    /// Both write actions answer 200 either way, so `already_current` is the
    /// only thing separating "the cloud re-indexed" from "the cloud skipped".
    /// It is snake_case on the wire (matching `scanner_version` on the payload,
    /// not the camelCase request fields), and its absence — an older cloud, or
    /// a body that carries only `success`/`message` — must read as "indexed".
    #[test]
    fn write_action_response_reads_already_current_and_defaults_to_indexed() {
        let skipped: WriteActionResponse = serde_json::from_str(
            r#"{"success":true,"message":"Metadata stored successfully","already_current":true}"#,
        )
        .expect("a short-circuit body parses");
        assert_eq!(skipped.already_current, Some(true));

        let indexed: WriteActionResponse = serde_json::from_str(
            r#"{"success":true,"message":"Metadata stored successfully","already_current":false}"#,
        )
        .expect("an explicit false parses");
        assert_eq!(indexed.already_current, Some(false));

        // Absent (older cloud, or the complete-upload body with its extra
        // fields): parses, and `unwrap_or(false)` reads it as "indexed".
        let legacy: WriteActionResponse = serde_json::from_str(
            r#"{"success":true,"message":"done","s3Url":"s3://b/k","metadata":{"pk":"w","sk":"p"}}"#,
        )
        .expect("a body without the field still parses");
        assert!(legacy.already_current.is_none());
        assert!(!legacy.already_current.unwrap_or(false));

        // And a body with neither field — the write actions have carried
        // different shapes over time and none of them may be dropped.
        let bare: WriteActionResponse =
            serde_json::from_str("{}").expect("an empty body still parses");
        assert!(bare.already_current.is_none());
    }

    #[test]
    fn integrity_fields_serialize_by_name_and_omit_when_none() {
        let inline = LambdaRequest {
            action: "store-metadata".to_string(),
            repo: "r".to_string(),
            service_name: None,
            hash: "h".to_string(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };
        let json = serde_json::to_string(&inline).unwrap();
        assert!(!json.contains("payloadSha256"));
        assert!(!json.contains("payloadSize"));

        let digest = StagedPayload::of("{}");
        let staged = LambdaRequest {
            payload_in_s3: Some(true),
            payload_sha256: Some(digest.sha256.clone()),
            payload_size: Some(digest.size),
            ..inline
        };
        let v = serde_json::to_value(&staged).unwrap();
        assert_eq!(v["payloadSha256"], digest.sha256);
        assert_eq!(v["payloadSize"], 2);
    }

    /// carrick#885: `force_reindex` is the run's statement that it superseded
    /// its own generation, and the cloud's freshness guard reads it by that
    /// exact snake_case key (matching `service_name` / `scanner_version`, not
    /// the camelCase staging fields). Absent on an ordinary scan, so a cached
    /// run's body is byte-for-byte what it was before the field existed and a
    /// cloud deployed without the reader ignores it.
    #[test]
    fn force_reindex_rides_the_write_action_and_is_omitted_when_unset() {
        let cached = LambdaRequest {
            action: "store-metadata".to_string(),
            repo: "r".to_string(),
            service_name: None,
            hash: "h".to_string(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };
        let json = serde_json::to_string(&cached).unwrap();
        assert!(
            !json.contains("force_reindex"),
            "an unforced run must omit the field, not send false: {json}"
        );

        let forced = LambdaRequest {
            force_reindex: Some(true),
            ..cached
        };
        let v = serde_json::to_value(&forced).unwrap();
        assert_eq!(v["force_reindex"], true);
    }

    /// carrick#536: `complete-upload` and `store-metadata` write the index, so
    /// a 5xx on them is ambiguous rather than final — the gateway can cut the
    /// connection while the handler runs on and commits. One retry is enough
    /// to catch the duplicate-recognition path; more just repeats the work.
    /// Reads keep the full budget.
    #[test]
    fn write_actions_get_one_retry_and_reads_keep_the_full_budget() {
        assert_eq!(max_retries_for_action("complete-upload"), 1);
        assert_eq!(max_retries_for_action("store-metadata"), 1);

        assert_eq!(
            max_retries_for_action("check-or-upload"),
            MAX_TRANSIENT_RETRIES
        );
        assert_eq!(
            max_retries_for_action("get-cross-repo-data"),
            MAX_TRANSIENT_RETRIES
        );
        assert_eq!(max_retries_for_action("upload-logs"), MAX_TRANSIENT_RETRIES);
        assert_eq!(
            max_retries_for_action("post-pr-result"),
            MAX_TRANSIENT_RETRIES
        );
        // An action this file does not know is treated as a read.
        assert_eq!(max_retries_for_action("some-new-action"), 3);
    }

    /// 5xx stays retryable as a status — the write cap is what bounds it, not
    /// a status reclassification. Reclassifying would give writes zero retries
    /// and lose the one attempt that recovers a cold start.
    #[test]
    fn write_cap_bounds_retries_without_reclassifying_5xx() {
        assert!(is_transient_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(is_transient_status(StatusCode::INTERNAL_SERVER_ERROR));
        let write = max_retries_for_action("store-metadata");
        let read = max_retries_for_action("check-or-upload");
        assert_eq!(write, MAX_WRITE_RETRIES);
        assert!(write >= 1, "one retry is still made");
        assert!(write < read, "writes are capped below the read budget");
    }

    /// Exhausting the budget on a write must not read as "the scan is lost".
    /// The write may have landed before the gateway cut the response, which is
    /// exactly what happened on the incident behind carrick#536.
    #[test]
    fn write_exhaustion_message_says_the_index_may_already_be_current() {
        let msg = retry_exhausted_message("store-metadata", "Lambda returned 504: timeout", 2);
        assert!(msg.contains("Lambda returned 504: timeout"));
        assert!(msg.contains("after 2 attempts"));
        assert!(msg.contains("store-metadata"));
        assert!(msg.contains("may already be current"));

        // Reads are unambiguous — no such caveat.
        let read = retry_exhausted_message("get-cross-repo-data", "boom", 4);
        assert_eq!(read, "boom (after 4 attempts)");
    }

    /// A pre-staging cloud omits `payloadUploadUrl` entirely; a staging cloud
    /// sends it as a string or null. All three must deserialize.
    #[test]
    fn payload_upload_url_tolerates_all_cloud_generations() {
        let old_cloud = r#"{"exists":false,"s3Url":"s","uploadUrl":null,"hash":"h"}"#;
        let parsed: LambdaResponse = serde_json::from_str(old_cloud).unwrap();
        assert_eq!(parsed.payload_upload_url, None);

        let null_url =
            r#"{"exists":false,"s3Url":"s","uploadUrl":null,"hash":"h","payloadUploadUrl":null}"#;
        let parsed: LambdaResponse = serde_json::from_str(null_url).unwrap();
        assert_eq!(parsed.payload_upload_url, None);

        let minted = r#"{"exists":false,"s3Url":"s","uploadUrl":null,"hash":"h","payloadUploadUrl":"https://bucket/staging"}"#;
        let parsed: LambdaResponse = serde_json::from_str(minted).unwrap();
        assert_eq!(
            parsed.payload_upload_url.as_deref(),
            Some("https://bucket/staging")
        );
    }

    /// The transport envelope flattens the payload next to the action tag —
    /// the cloud reads `action`/`schema_version` and the payload fields from
    /// one top-level object (pr-result-pipeline.md wire shape).
    #[test]
    fn post_pr_result_request_flattens_payload_with_envelope() {
        let payload = crate::findings::PrResultPayload {
            repo: "api-server".to_string(),
            pr_number: 7,
            head_sha: None,
            run_id: None,
            topology: crate::findings::Topology {
                repo_name: "api-server".to_string(),
                local_service_count: 1,
                peer_repo_count: 0,
            },
            stats: crate::findings::ScanStats {
                endpoints: 1,
                calls: 2,
            },
            findings: vec![],
            delta: None,
            verified: vec![],
            graphql: crate::findings::GraphqlStatus {
                libraries: vec![],
                operations_indexed: false,
            },
            has_types: true,
        };
        let request = PostPrResultRequest {
            action: "post-pr-result",
            schema_version: 1,
            payload: &payload,
        };
        let v = serde_json::to_value(&request).unwrap();
        assert_eq!(v["action"], "post-pr-result");
        assert_eq!(v["schema_version"], 1);
        // Payload fields sit at the top level, not nested under "payload".
        assert_eq!(v["repo"], "api-server");
        assert_eq!(v["pr_number"], 7);
        assert_eq!(v["stats"]["calls"], 2);
        assert!(v.get("payload").is_none());
    }

    /// The cloud gzips `get-cross-repo-data` only for callers that advertise
    /// gzip, so a scanner that stays silent is served — and size-checked
    /// against — the uncompressed aggregate, and past ~5.8 MB the response
    /// breaches Lambda's synchronous cap and comes back as a 413 that
    /// `is_transient_status` correctly refuses to retry. Both halves of the
    /// fix are properties of the client, not of any call site, so pin them
    /// both here: the request must advertise gzip, and a gzipped response
    /// must deserialize as if it had never been compressed.
    ///
    /// Guards the reqwest `gzip` crate feature specifically: drop it from
    /// Cargo.toml and this test fails on the `accept-encoding` assertion.
    #[tokio::test]
    async fn cloud_client_advertises_gzip_and_inflates_the_response() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = r#"{"repos":[{"repo":"api-server","hash":"deadbeef",
                "s3Url":"https://example.invalid/api-server.json",
                "filename":"api-server.json","metadata":null,
                "lastUpdated":"2026-07-27T00:00:00Z"}]}"#;
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(body.as_bytes()).unwrap();
            let gzipped = encoder.finish().unwrap();

            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Encoding: gzip\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                gzipped.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&gzipped).unwrap();
            stream.flush().unwrap();
            request
        });

        // The production builder, plus no_proxy so CI proxy env vars can't
        // intercept the localhost call (same guard as the OIDC tests).
        let client = http_client_builder().no_proxy().build().unwrap();
        let response = client
            .post(format!("http://{}/types/check-or-upload", addr))
            .json(&GetCrossRepoRequest {
                action: "get-cross-repo-data".to_string(),
            })
            .send()
            .await
            .unwrap();

        // Inflated transparently: no manual decode, no `Content-Encoding`
        // left on the response for a caller to have to notice.
        assert!(response.headers().get("content-encoding").is_none());
        let parsed: CrossRepoResponse = response.json().await.unwrap();
        assert_eq!(parsed.repos.len(), 1);
        assert_eq!(parsed.repos[0].repo, "api-server");
        assert_eq!(parsed.repos[0].hash, "deadbeef");
        assert!(parsed.repos[0].s3_url.ends_with("api-server.json"));

        let request = server.join().unwrap();
        assert!(
            request
                .to_lowercase()
                .lines()
                .any(|l| l.starts_with("accept-encoding:") && l.contains("gzip")),
            "client did not advertise gzip; the reqwest `gzip` feature is off: {request}"
        );
    }

    /// Staged read (carrick-cloud#456): a response with `staged: true` and a
    /// `staged_url` deserializes with an empty `repos` (the cloud omits the
    /// key on purpose), and a plain response without the fields stays exactly
    /// as before. Pins the serde defaults the follow logic keys on.
    #[test]
    fn cross_repo_response_staged_shape_deserializes() {
        let staged: CrossRepoResponse = serde_json::from_str(
            r#"{"staged":true,"staged_url":"https://bucket.s3/k?sig=x","raw_bytes":25638735,"repo_count":43}"#,
        )
        .unwrap();
        assert!(staged.staged);
        assert_eq!(
            staged.staged_url.as_deref(),
            Some("https://bucket.s3/k?sig=x")
        );
        assert!(staged.repos.is_empty());

        let plain: CrossRepoResponse = serde_json::from_str(r#"{"repos":[]}"#).unwrap();
        assert!(!plain.staged);
        assert!(plain.staged_url.is_none());

        // A staged flag with no URL is the malformed case the follow logic
        // must refuse rather than treat as an empty project.
        let malformed: CrossRepoResponse = serde_json::from_str(r#"{"staged":true}"#).unwrap();
        assert!(malformed.staged);
        assert!(malformed.staged_url.is_none());
    }
    /// One request against a local server, with the laptop credential. Returns
    /// the raw request the server saw, so the headers and the body are both
    /// assertable.
    fn bearer_storage(
        responses: Vec<(u16, String)>,
    ) -> (AwsStorage, std::thread::JoinHandle<Vec<String>>) {
        let (base, server) = crate::agent_service::tests::stub_server(responses);
        let storage = AwsStorage::for_test(
            &format!("{base}/types/check-or-upload"),
            CloudAuth::Bearer("carrick_sk_live_test".to_string()),
            false,
        );
        (storage, server)
    }

    fn run_context(dirty: bool) -> RunContext {
        RunContext {
            repo_full_name: Some("example/api".to_string()),
            commit: "4f2a1c9000000000000000000000000000000000".to_string(),
            dirty,
        }
    }

    fn body_of(request: &str) -> serde_json::Value {
        let (_, body) = request
            .split_once("\r\n\r\n")
            .expect("a request with a body");
        serde_json::from_str(body).expect("a JSON body")
    }

    fn has_header(request: &str, name: &str, value: &str) -> bool {
        request.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with(&format!("{}:", name.to_ascii_lowercase()))
                && line.contains(value)
        })
    }

    /// The laptop branch of `begin_run`: `start-scan` instead of the health
    /// probe, the credential as `Authorization`, and the three answers the
    /// rest of the run needs — the slot, `multi_service`, and whether any
    /// service already has rows (§2.1, C9).
    #[tokio::test]
    async fn start_scan_opens_a_laptop_run_and_carries_its_answers() {
        let (storage, server) = bearer_storage(vec![(
            200,
            serde_json::json!({
                "schema": "carrick.start-scan/0",
                "scan_id": "scan_01J",
                "project_id": "proj_1",
                "project_slug": "payments",
                "indexed_services": ["api", "web"],
                "multi_service": true,
                "allowance_sentence": "candidates not refreshed since 2026-09-01."
            })
            .to_string(),
        )]);

        let start = storage.begin_run(&run_context(true)).await.unwrap();
        assert_eq!(
            start.allowance_sentence.as_deref(),
            Some("candidates not refreshed since 2026-09-01.")
        );
        assert_eq!(
            start.indexed_services.as_deref(),
            Some(["api".to_string(), "web".to_string()].as_slice())
        );
        // The probe is the only other source of this flag, and it is not on
        // this path; without it a multi-service repo would scan, pay, and
        // upload nothing but a warning.
        assert!(storage.supports_multi_service());
        // A dirty run supersedes its own generation, or the next scan at the
        // same HEAD is told the index is current (C10).
        assert!(storage.forces_reindex());
        assert_eq!(storage.scan_id().as_deref(), Some("scan_01J"));

        let request = &server.join().unwrap()[0];
        assert!(
            has_header(request, "authorization", "Bearer carrick_sk_live_test"),
            "{request}"
        );
        assert!(
            !request.to_ascii_lowercase().contains("x-carrick-oidc"),
            "the laptop branch must not send the OIDC header: {request}"
        );
        let body = body_of(request);
        assert_eq!(body["action"], "start-scan");
        // The full owner/repo, not the basename every other action sends.
        assert_eq!(body["repo"], "example/api");
        assert_eq!(body["commit"], "4f2a1c9000000000000000000000000000000000");
        assert_eq!(body["dirty"], true);
    }

    /// A count gate answers 409, and 409 is not a transient status — so the
    /// refusal is made once and reported with the code the user can act on.
    /// A 429 here would be retried with backoff, which is why the gates are
    /// not allowed to use one (C6).
    #[tokio::test]
    async fn a_gate_refusal_is_final_and_names_its_code() {
        let (storage, server) = bearer_storage(vec![(
            409,
            serde_json::json!({
                "error": "A scan of example/api is already running.",
                "code": "laptop_scan_in_flight"
            })
            .to_string(),
        )]);

        let error = storage.begin_run(&run_context(false)).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("already running"), "{message}");
        assert!(message.contains("laptop_scan_in_flight"), "{message}");

        assert_eq!(
            server.join().unwrap().len(),
            1,
            "a gate refusal must not be retried"
        );
    }

    /// 409 is the status every laptop gate must use, precisely because this
    /// classifier does not retry it. Pinned here so a later widening of the
    /// transient set cannot silently turn a refusal into a retry storm.
    #[test]
    fn a_gate_refusal_status_is_not_transient() {
        assert!(!is_transient_status(StatusCode::CONFLICT));
    }

    /// A credential the cloud will not accept cannot be re-minted, so the
    /// answer is a sentence, not a retry (§8.2).
    #[tokio::test]
    async fn a_rejected_credential_says_run_carrick_login_and_stops() {
        let (storage, server) = bearer_storage(vec![(
            403,
            serde_json::json!({ "error": "wrong key kind", "code": "wrong_key_kind" }).to_string(),
        )]);

        let message = storage
            .begin_run(&run_context(false))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("carrick login"), "{message}");
        assert!(message.contains("wrong_key_kind"), "{message}");
        assert_eq!(server.join().unwrap().len(), 1, "a 403 is not retried");
    }

    /// A cloud that has not deployed the action answers something else, and
    /// the scanner says so rather than reading a body it does not understand.
    #[tokio::test]
    async fn an_unknown_start_scan_schema_is_refused() {
        let (storage, server) = bearer_storage(vec![(
            200,
            serde_json::json!({
                "schema": "carrick.start-scan/1",
                "scan_id": "s", "project_id": "p", "project_slug": "q"
            })
            .to_string(),
        )]);
        let message = storage
            .begin_run(&run_context(false))
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("carrick.start-scan/0"), "{message}");
        server.join().unwrap();
    }

    /// The first-scan partial rule's two answers, as the scanner reads them.
    /// A 200 carrying `partial` is an acceptance and the run continues; a 409
    /// `partial_refused` is final and names the files, because "re-run the
    /// scan" without them is not actionable (§4).
    #[test]
    fn the_partial_upload_envelopes_parse_as_acceptance_and_refusal() {
        let accepted: WriteActionResponse = serde_json::from_str(
            r#"{"success":true,"partial":true,
                "unanalysed_files":[{"path":"src/routes/orders.ts","reason":"model_error"}]}"#,
        )
        .expect("the 200 acceptance parses");
        assert_eq!(accepted.partial, Some(true));
        assert_eq!(
            accepted.unanalysed_files.as_deref(),
            Some(
                [UnanalysedFile {
                    path: "src/routes/orders.ts".to_string(),
                    reason: "model_error".to_string(),
                }]
                .as_slice()
            )
        );

        // An ordinary 200 carries neither key, and must still parse.
        let ordinary: WriteActionResponse =
            serde_json::from_str(r#"{"success":true,"message":"done"}"#).unwrap();
        assert_eq!(ordinary.partial, None);
        assert!(ordinary.unanalysed_files.is_none());

        let refusal = refusal_message(
            StatusCode::CONFLICT,
            r#"{"error":"This service already has an index.","code":"partial_refused",
                "unanalysed_files":[{"path":"src/routes/orders.ts","reason":"model_error"},
                                    {"path":"src/routes/refunds.ts","reason":"internal_error"}]}"#,
        );
        assert!(refusal.contains("already has an index"), "{refusal}");
        assert!(refusal.contains("partial_refused"), "{refusal}");
        assert!(refusal.contains("2 file(s) had no analysis"), "{refusal}");
        assert!(refusal.contains("src/routes/refunds.ts"), "{refusal}");
    }

    /// The run-scoped envelope fields ride the write actions under the exact
    /// snake_case keys the cloud reads, and every one of them is omitted when
    /// unset — so a CI upload's body is byte-for-byte what it was before this
    /// release and a cloud deployed without the readers ignores them (§8.6).
    #[test]
    fn the_run_scoped_envelope_fields_serialize_by_name_and_omit_when_none() {
        let ci = LambdaRequest {
            action: "store-metadata".to_string(),
            repo: "r".to_string(),
            service_name: None,
            hash: "h".to_string(),
            filename: "types.d.ts".to_string(),
            cloud_repo_data: None,
            s3_url: None,
            wants_payload_url: None,
            payload_in_s3: None,
            payload_sha256: None,
            payload_size: None,
            force_reindex: None,
            scan_id: None,
            unanalysed_files: None,
            scan_final: None,
        };
        let json = serde_json::to_string(&ci).unwrap();
        for field in ["scan_id", "unanalysed_files", "scan_final"] {
            assert!(!json.contains(field), "{field} must be omitted: {json}");
        }

        let laptop = LambdaRequest {
            scan_id: Some("scan_01J".to_string()),
            unanalysed_files: Some(vec![UnanalysedFile {
                path: "src/a.ts".to_string(),
                reason: "model_error".to_string(),
            }]),
            scan_final: Some(true),
            ..ci
        };
        let v = serde_json::to_value(&laptop).unwrap();
        assert_eq!(v["scan_id"], "scan_01J");
        assert_eq!(v["scan_final"], true);
        assert_eq!(v["unanalysed_files"][0]["path"], "src/a.ts");
        assert_eq!(v["unanalysed_files"][0]["reason"], "model_error");
    }

    /// `unanalysed_files` is a laptop field. CI's own gate already aborts the
    /// run before the upload, so sending the list there would offer the cloud
    /// a decision it must not be asked to make — and would answer a CI caller
    /// that set `CARRICK_ALLOW_PARTIAL_ANALYSIS` with `409 partial_refused`.
    #[test]
    fn the_unanalysed_list_is_never_sent_on_the_ci_path() {
        let ci = AwsStorage::for_test("http://127.0.0.1:1", CloudAuth::Oidc, false);
        assert!(ci.unanalysed_files().is_none());
        assert!(ci.scan_id().is_none());
        assert!(ci.uploads_run_logs());
    }

    /// A laptop's debug log names the developer's own machine, and
    /// `upload-logs` is outside what a `cli` credential may do (§1.2).
    #[test]
    fn a_laptop_run_does_not_ship_its_debug_log() {
        let laptop =
            AwsStorage::for_test("http://127.0.0.1:1", CloudAuth::Bearer("t".into()), false);
        assert!(!laptop.uploads_run_logs());
    }

    /// `start-scan` keys on the full `owner/repo`, and a clone with no GitHub
    /// remote cannot supply one. Saying that beats sending a name the cloud
    /// would resolve to the wrong repository.
    #[tokio::test]
    async fn a_clone_with_no_github_remote_is_told_why_it_cannot_scan() {
        let storage =
            AwsStorage::for_test("http://127.0.0.1:1", CloudAuth::Bearer("t".into()), false);
        let message = storage
            .begin_run(&RunContext {
                repo_full_name: None,
                commit: "abc".to_string(),
                dirty: false,
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("origin remote"), "{message}");
    }

    /// A storage holding a scan slot, as `start-scan` would have left it.
    /// Set here rather than through the process-global, so two of these
    /// running at once cannot see each other's slot.
    fn bearer_storage_in_scan(
        responses: Vec<(u16, String)>,
        scan_id: &str,
    ) -> (AwsStorage, std::thread::JoinHandle<Vec<String>>) {
        let (storage, server) = bearer_storage(responses);
        storage.scan_id.set(scan_id.to_string()).unwrap();
        (storage, server)
    }

    fn blob() -> CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "api",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-11T00:00:00Z",
            "commit_hash": "4f2a1c9"
        }))
        .unwrap()
    }

    fn check_ok() -> (u16, String) {
        (
            200,
            serde_json::json!({
                "exists": true, "s3Url": "s3://bucket/api.json",
                "uploadUrl": null, "hash": "4f2a1c9", "multiService": true
            })
            .to_string(),
        )
    }

    /// The run-scoped fields on a real write action, not just on a struct:
    /// the slot ties the write to the meters, and `scan_final` on the last one
    /// is what releases the cloud's in-flight slot. The existence check
    /// carries the slot too but never `scan_final` — it indexes nothing
    /// (§2.2).
    #[tokio::test]
    async fn the_last_write_action_of_a_run_carries_the_slot_and_releases_it() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            "scan_01J",
        );

        storage.upload_repo_data(&blob(), true).await.unwrap();

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);

        let check = body_of(&requests[0]);
        assert_eq!(check["action"], "check-or-upload");
        assert_eq!(check["scan_id"], "scan_01J");
        assert!(
            check.get("scan_final").is_none(),
            "the existence check indexes nothing, so it releases nothing: {check}"
        );

        let write = body_of(&requests[1]);
        assert_eq!(write["action"], "store-metadata");
        assert_eq!(write["scan_id"], "scan_01J");
        assert_eq!(write["scan_final"], true);
    }

    /// C10's scanner half, on the wire. A dirty run's `hash` is HEAD's SHA
    /// even though the tree was not HEAD, so without `force_reindex` the
    /// cloud's freshness guard tells the next scan at the same commit that the
    /// index is current — and the dirty rows survive at a commit they never
    /// described. Both write actions carry it; the existence check does not,
    /// because it indexes nothing to supersede.
    #[tokio::test]
    async fn a_dirty_run_forces_the_reindex_on_its_write_action() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            "scan_01J",
        );
        // What `start-scan` leaves behind on a dirty run.
        storage
            .dirty
            .store(true, std::sync::atomic::Ordering::Relaxed);

        storage.upload_repo_data(&blob(), true).await.unwrap();

        let requests = server.join().unwrap();
        assert!(
            body_of(&requests[0]).get("force_reindex").is_none(),
            "the existence check supersedes nothing: {}",
            body_of(&requests[0])
        );
        assert_eq!(body_of(&requests[1])["force_reindex"], true);
    }

    /// And a clean run does not: the field is omitted, so an ordinary upload's
    /// body is byte-for-byte what it was.
    #[tokio::test]
    async fn a_clean_run_omits_force_reindex() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            "scan_01J",
        );

        storage.upload_repo_data(&blob(), true).await.unwrap();

        let write = body_of(&server.join().unwrap()[1]);
        assert!(write.get("force_reindex").is_none(), "{write}");
    }

    /// A service that is not the last one in the run must not release the
    /// slot: the rest of the run would then be unprotected, and a second
    /// laptop could start scanning the same repo halfway through this one.
    #[tokio::test]
    async fn a_write_that_is_not_the_last_leaves_the_slot_held() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            "scan_01J",
        );

        storage.upload_repo_data(&blob(), false).await.unwrap();

        let write = body_of(&server.join().unwrap()[1]);
        assert_eq!(write["scan_id"], "scan_01J");
        assert!(
            write.get("scan_final").is_none(),
            "only the last write action of the run releases the slot: {write}"
        );
    }

    /// The wire shape of `carrick.scan-spend/0`, as the cloud sends it on the
    /// write action that carried `scan_final` (carrick-cloud#813). Every field
    /// reaches the outcome, because the surface that prints it cannot ask
    /// again.
    #[tokio::test]
    async fn the_last_write_action_carries_what_the_scan_cost() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (
                    200,
                    serde_json::json!({
                        "success": true,
                        "scan_spend": {
                            "schema": "carrick.scan-spend/0",
                            "scan_id": "scan_01J",
                            "first_index": true,
                            "priced": true,
                            "unpriced_models": [],
                            "usd": 4.32,
                            "input_tokens": 1170432,
                            "output_tokens": 288114,
                            "cached_tokens": 0,
                            "calls": 412,
                            "first_index_ceiling_usd": 15,
                            "first_index_remaining_usd": 10.68,
                            "monthly_allowance_usd": 10,
                            "monthly_remaining_usd": 10,
                            "period": "2026-09"
                        }
                    })
                    .to_string(),
                ),
            ],
            "scan_01J",
        );

        let outcome = storage.upload_repo_data(&blob(), true).await.unwrap();
        let spend = outcome.scan_spend.expect("the figure the cloud sent");

        assert_eq!(spend.scan_id, "scan_01J");
        assert!(spend.first_index && spend.priced);
        assert_eq!(spend.usd, Some(4.32));
        assert_eq!(spend.input_tokens, 1_170_432);
        assert_eq!(spend.output_tokens, 288_114);
        assert_eq!(spend.calls, 412);
        assert_eq!(spend.first_index_ceiling_usd, Some(15.0));
        assert_eq!(spend.first_index_remaining_usd, Some(10.68));
        assert_eq!(spend.monthly_allowance_usd, Some(10.0));
        assert_eq!(spend.monthly_remaining_usd, Some(10.0));
        assert_eq!(spend.period, "2026-09");
        server.join().unwrap();
    }

    /// An unpriced run is the shipped state on day one: the price map is empty
    /// in production, so the meters are real and the dollars are null. It must
    /// parse, and it must not read as a scan that cost nothing.
    #[tokio::test]
    async fn an_unpriced_scan_still_parses_and_states_no_figure() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (
                    200,
                    serde_json::json!({
                        "success": true,
                        "scan_spend": {
                            "schema": "carrick.scan-spend/0",
                            "scan_id": "scan_01J",
                            "first_index": true,
                            "priced": false,
                            "unpriced_models": ["a-preview-model"],
                            "usd": null,
                            "input_tokens": 1170432,
                            "output_tokens": 288114,
                            "cached_tokens": 0,
                            "calls": 412,
                            "first_index_ceiling_usd": null,
                            "first_index_remaining_usd": null,
                            "monthly_allowance_usd": null,
                            "monthly_remaining_usd": null,
                            "period": "2026-09"
                        }
                    })
                    .to_string(),
                ),
            ],
            "scan_01J",
        );

        let spend = storage
            .upload_repo_data(&blob(), true)
            .await
            .unwrap()
            .scan_spend
            .expect("a body that carries the block carries it unpriced too");
        assert!(!spend.priced);
        assert_eq!(spend.usd, None);
        assert_eq!(spend.unpriced_models, vec!["a-preview-model".to_string()]);
        // The tokens are facts whatever the price map says.
        assert_eq!(spend.calls, 412);
        server.join().unwrap();
    }

    /// Every other answer carries no figure, and the absence is the statement:
    /// a CI upload is not metered this way, a write before the last one has
    /// nothing to report yet, and a cloud deployed before the field simply
    /// omits it. None of them is a scan that cost nothing.
    #[tokio::test]
    async fn a_body_without_the_block_reports_no_spend() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (200, serde_json::json!({ "success": true }).to_string()),
            ],
            "scan_01J",
        );

        let outcome = storage.upload_repo_data(&blob(), true).await.unwrap();
        assert!(outcome.scan_spend.is_none());
        server.join().unwrap();
    }

    /// A tag this scanner does not read is a cloud that has moved on. Printing
    /// dollars off a shape whose meaning may have changed is worse than
    /// printing nothing, so the block is dropped here, once, rather than
    /// guarded at every surface.
    #[tokio::test]
    async fn a_spend_under_an_unknown_tag_is_dropped() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (
                    200,
                    serde_json::json!({
                        "success": true,
                        "scan_spend": {
                            "schema": "carrick.scan-spend/1",
                            "scan_id": "scan_01J",
                            "priced": true,
                            "usd": 4.32
                        }
                    })
                    .to_string(),
                ),
            ],
            "scan_01J",
        );

        let outcome = storage.upload_repo_data(&blob(), true).await.unwrap();
        assert!(outcome.scan_spend.is_none());
        server.join().unwrap();
    }

    /// The fail-closed half of the first-scan partial rule. The cloud refuses
    /// with a 409, the scanner does not retry it — the existence check plus
    /// one write action and no more — and the error names the files, because
    /// "re-run the scan" without them is not actionable (§4, C6).
    #[tokio::test]
    async fn a_refused_partial_upload_is_final_and_names_the_files() {
        let (storage, server) = bearer_storage_in_scan(
            vec![
                check_ok(),
                (
                    409,
                    serde_json::json!({
                        "error": "This service already has an index.",
                        "code": "partial_refused",
                        "unanalysed_files": [
                            { "path": "src/routes/orders.ts", "reason": "model_error" }
                        ]
                    })
                    .to_string(),
                ),
            ],
            "scan_01J",
        );

        let message = storage
            .upload_repo_data(&blob(), true)
            .await
            .unwrap_err()
            .to_string();
        assert!(message.contains("already has an index"), "{message}");
        assert!(message.contains("partial_refused"), "{message}");
        assert!(message.contains("src/routes/orders.ts"), "{message}");

        assert_eq!(
            server.join().unwrap().len(),
            2,
            "a refusal is not retried, on a write action least of all"
        );
    }

    /// A refusal that is about the repo, not the credential, must not tell the
    /// user to log in again: a fresh consent does not connect a repo, and the
    /// sentence that does is the cloud's own (§2.1).
    #[tokio::test]
    async fn a_repo_refusal_is_not_reported_as_a_credential_problem() {
        let (storage, server) = bearer_storage(vec![(
            403,
            serde_json::json!({
                "error": "example/api is not connected to this workspace.",
                "code": "repo_not_connected"
            })
            .to_string(),
        )]);

        let message = storage
            .begin_run(&run_context(false))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("not connected to this workspace"),
            "{message}"
        );
        assert!(message.contains("repo_not_connected"), "{message}");
        assert!(
            !message.contains("carrick login"),
            "a repo that is not connected is not a credential to replace: {message}"
        );
        server.join().unwrap();
    }

    /// The kind gate IS a credential problem, and it is the one an `mcp`
    /// credential hits until its holder logs in again.
    #[test]
    fn only_a_401_or_the_kind_gate_reads_as_a_credential_rejection() {
        let kind = r#"{"error":"wrong key kind","code":"wrong_key_kind"}"#;
        assert!(is_credential_rejection(StatusCode::FORBIDDEN, kind));
        assert!(is_credential_rejection(StatusCode::UNAUTHORIZED, "{}"));
        assert!(!is_credential_rejection(
            StatusCode::FORBIDDEN,
            r#"{"code":"repo_not_authorized"}"#
        ));
        assert!(!is_credential_rejection(StatusCode::CONFLICT, kind));
    }
}
