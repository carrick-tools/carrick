use crate::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use crate::oidc::OidcProvider;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, warn};

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

pub struct AwsStorage {
    lambda_url: String,
    http_client: Client,
    /// Whether the cloud advertises a service-aware index key (set from the
    /// health-check response). Until the cloud key includes a service
    /// discriminator this stays false, which gates multi-service uploads so
    /// they can't clobber each other.
    multi_service: std::sync::atomic::AtomicBool,
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
}

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
    repos: Vec<AdjacentRepo>,
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
    pub fn new() -> Result<Self, StorageError> {
        let api_endpoint = env!("CARRICK_API_ENDPOINT");
        let lambda_url = format!("{}/types/check-or-upload", api_endpoint);

        // Fail fast if OIDC isn't available — the cloud derives repo identity
        // from the signed OIDC claims, so there is no other way to authenticate.
        OidcProvider::global().map_err(|e| StorageError::ConnectionError(e.to_string()))?;

        let http_client = http_client_builder().build().map_err(|e| {
            StorageError::ConnectionError(format!("Failed to build HTTP client: {}", e))
        })?;

        Ok(Self {
            lambda_url,
            http_client,
            multi_service: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// POSTs a JSON body to the upload endpoint with the OIDC bearer header,
    /// returning the raw response body on success. OIDC tokens are short-lived,
    /// so on a 401 (token likely expired mid-run) we re-mint once and retry.
    /// Transient failures (network errors, 408/429/5xx) are retried with
    /// exponential backoff, up to [`max_retries_for_action`] times for the
    /// named action — the full budget for reads, one retry for the actions
    /// that write the index, where a lost response does not mean a lost write.
    async fn send_lambda<B>(&self, action: &str, body: &B) -> Result<String, StorageError>
    where
        B: serde::Serialize + ?Sized,
    {
        let max_retries = max_retries_for_action(action);
        let provider =
            OidcProvider::global().map_err(|e| StorageError::ConnectionError(e.to_string()))?;
        let mut token = provider
            .token()
            .await
            .map_err(|e| StorageError::ConnectionError(e.to_string()))?;

        let mut reminted = false;
        let mut retries = 0u32;
        loop {
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
                            if status.as_u16() == 401 && !reminted {
                                warn!(
                                    "Upload returned 401; re-minting OIDC token and retrying once"
                                );
                                token = provider
                                    .remint()
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
        };

        let response: WriteActionResponse = self.call_lambda(&request).await?;
        debug!("Successfully stored metadata for {}", data.repo_name);

        Ok(UploadOutcome {
            already_current: response.already_current.unwrap_or(false),
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
    async fn upload_repo_data(&self, data: &CloudRepoData) -> Result<UploadOutcome, StorageError> {
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
                };

                let complete_response: WriteActionResponse =
                    self.call_lambda(&complete_request).await?;
                debug!("Successfully completed upload and stored metadata");
                Ok(UploadOutcome {
                    already_current: complete_response.already_current.unwrap_or(false),
                })
            } else {
                debug!(
                    "No bundled types available for {}; storing metadata only",
                    repo
                );
                self.store_repo_metadata(data, &lambda_response.s3_url, staged.as_ref())
                    .await
            }
        } else {
            debug!("Type file already exists, just updating metadata");
            self.store_repo_metadata(data, &lambda_response.s3_url, staged.as_ref())
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

        let response: CrossRepoResponse =
            self.call_lambda_generic(&request.action, &request).await?;

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
}
