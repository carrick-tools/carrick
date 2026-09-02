use crate::oidc::OidcProvider;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Semaphore;
use tokio::time::{Duration, sleep};
use tracing::{debug, warn};

/// Process-global circuit breaker for backend LLM quota exhaustion.
///
/// A scan runs as a single process, and every lambda call it makes draws on
/// the same backend LLM quota. A quota / rate-limit error therefore does not
/// clear within a scan, and — because the cloud counts each attempt before
/// it calls the model — every retry only burns more of the exhausted budget.
///
/// The dominant failure mode without this breaker: ~20 concurrent workers
/// each independently walk the full 2→64s backoff chain against a wall that
/// will never lift, so a scan can sit dead for 20+ minutes making no progress
/// while still consuming quota. The breaker collapses that: the first worker
/// to see a quota error trips it, and every other in-flight or queued call —
/// across all phases and all `AgentService` instances — aborts immediately.
///
/// This is deliberately a process-global (like [`crate::oidc::OidcProvider`]'s
/// `global()`): there are several independently-constructed `AgentService`
/// instances across a single scan, and a quota wall hit by any of them means
/// the backend is exhausted for all of them.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// Whether the quota circuit breaker has tripped this process. Public so the
/// engine can abort before uploading a quota-degraded (partial) index.
pub fn rate_limit_tripped() -> bool {
    RATE_LIMITED.load(Ordering::Relaxed)
}

/// Trip the quota circuit breaker. Idempotent.
fn trip_rate_limit() {
    RATE_LIMITED.store(true, Ordering::Relaxed);
}

/// Whether a cloud error envelope signals backend quota / rate-limit
/// exhaustion (which backoff cannot clear within a scan), as opposed to a
/// transient overload (which it can). The cloud maps both its own per-user
/// daily cap and upstream provider quota errors to the `rate_limited` code.
fn is_quota_error(err: &AgentError) -> bool {
    err.code == "rate_limited"
}

/// Pseudo-code for the call-level failure raised once the quota breaker is
/// open. Not a cloud code: the cloud never sends it, the scanner synthesises it
/// so callers can tell "the backend is out of quota, everything downstream is
/// doomed" apart from a genuine per-call failure.
pub const QUOTA_ABORT_CODE: &str = "quota_exhausted";

/// The error returned for an individual call once the breaker is open. Scoped
/// to what's true at the call level (this call fails fast); the engine turns a
/// tripped breaker into a fatal, no-upload abort via [`rate_limit_tripped`].
fn rate_limit_abort_error() -> AgentCallError {
    AgentCallError {
        code: QUOTA_ABORT_CODE.to_string(),
        message: "Carrick Cloud LLM quota exhausted; failing fast. This is a rate/quota \
                  limit on the analysis backend, not a problem with the scanned code. The \
                  scan will stop before uploading; re-run after the quota resets."
            .to_string(),
        retriable: false,
    }
}

/// A failed lambda call, carrying the cloud's own transient/permanent verdict
/// so callers can classify a failure without pattern-matching on message text.
///
/// `retriable` is the envelope's `error.retriable` verbatim when the call
/// reached the lambda (the 429-wrapped 503 `model_error` the backend raises
/// under Vertex pressure is `retriable: true`); for failures that never
/// produced an envelope — bare network errors, unparseable gateway bodies —
/// the scanner fills in the equivalent verdict. An `Err` with `retriable: true`
/// therefore means "transient, and the backoff chain was already spent on it",
/// which is what lets a caller report failed-after-retry honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCallError {
    /// Cloud error code (`model_error`, `rate_limited`, `internal_error`), or a
    /// scanner-side pseudo-code for a failure that never reached the envelope.
    pub code: String,
    pub message: String,
    /// Whether the failure class is transient. See the type doc.
    pub retriable: bool,
}

impl AgentCallError {
    /// A permanent, non-retriable failure (server-side bug, malformed response).
    fn permanent(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
            retriable: false,
        }
    }

    /// A transient failure whose backoff chain has been spent.
    fn transient(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
            retriable: true,
        }
    }

    /// Whether this call failed because the process-global quota breaker is
    /// open rather than on its own merits. Such a call was never attempted, so
    /// counting it as a retry failure overstates the loss.
    pub fn is_quota_abort(&self) -> bool {
        self.code == QUOTA_ABORT_CODE
    }
}

impl std::fmt::Display for AgentCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Agent error '{}' (retriable={}): {}",
            self.code, self.retriable, self.message
        )
    }
}

impl std::error::Error for AgentCallError {}

/// Attempts per call: the initial try plus six backed-off retries.
const MAX_RETRIES: u32 = 7;
/// First backoff sleep; doubles each attempt.
const RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
/// Ceiling on a single backoff sleep (reached at attempt 6: 2→4→8→16→32→64s).
const RETRY_MAX_DELAY: Duration = Duration::from_secs(64);

/// Whether a failed call should consume a backoff attempt.
///
/// The cloud's `retriable` flag is the single source of truth for the
/// transient class — the scanner does not re-derive it from status codes or
/// message text. A quota abort is excluded even though quota errors are
/// nominally transient: quota does not clear inside one scan, and the breaker
/// has already decided that every remaining call fails fast.
fn should_retry(err: &AgentCallError, attempt: u32, max_retries: u32) -> bool {
    err.retriable && !err.is_quota_abort() && attempt < max_retries
}

/// Equal-jitter exponential backoff: half the exponential delay, plus a random
/// share of the other half.
///
/// Jitter is not cosmetic here. Up to `CARRICK_CONCURRENCY_LIMIT` workers hit
/// the same overloaded backend within milliseconds of each other, and an
/// unjittered `2^attempt` sleep makes them all wake in lockstep and re-fire as
/// one burst — the exact pattern that keeps a rate-limited backend rate
/// limited. Spreading each waker across half a window decorrelates them.
///
/// This is the standard equal-jitter formulation, so the previous unjittered
/// schedule (2, 4, 8, 16, 32, 64s) is now the *ceiling* of each window rather
/// than its midpoint: the worst-case chain is unchanged at ~126s, the mean
/// sleep is three quarters of what it was. Decorrelating the wakers is worth
/// more than the quarter-window of extra patience.
///
/// Pure so it can be tested: `jitter` is any value in `0..=u32::MAX`, supplied
/// by [`jitter_seed`] at the call site.
fn backoff_delay(attempt: u32, jitter: u32) -> Duration {
    let exponential = RETRY_BASE_DELAY
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(RETRY_MAX_DELAY);
    let half = exponential / 2;
    let spread = half.mul_f64(f64::from(jitter) / f64::from(u32::MAX));
    half + spread
}

/// Jitter source: the sub-second component of the wall clock. Enough entropy
/// to decorrelate wakers that are milliseconds apart, and avoids taking a
/// direct dependency on `rand` for a sleep length.
fn jitter_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Whether a response says the OIDC token was rejected.
///
/// A 401 is the direct signal. The indirect one matters just as much: the
/// gateway in front of the cloud can answer a rejected token with a 5xx and a
/// body that is not an error envelope, and that is indistinguishable from a
/// backend overload unless the body is read (#461). Bodies are only sniffed on
/// non-2xx responses, so an analysis result that quotes the code out of scanned
/// source can never be read as a rejection.
pub(crate) fn is_oidc_rejection(status: u16, body: &str) -> bool {
    status == 401 || (!(200..300).contains(&status) && body.contains("oidc_invalid"))
}

/// A short, single-line excerpt of a response body, for a log line or an error
/// message. Bodies can be large and can carry newlines; neither belongs in a
/// warning.
fn body_excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_CHARS {
        let head: String = collapsed.chars().take(MAX_CHARS).collect();
        format!("{}...", head)
    } else if collapsed.is_empty() {
        "<empty body>".to_string()
    } else {
        collapsed
    }
}

/// Reusable service for making Agent API calls
#[derive(Debug, Clone)]
pub struct AgentService {
    client: Client,
    semaphore: Arc<Semaphore>,
}

impl AgentService {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        // Limit concurrent requests to avoid rate limits
        // Paid tier allows higher limits, but let's be safe with 20 concurrent requests
        let concurrency_limit = env::var("CARRICK_CONCURRENCY_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20);
        let use_system_proxy = env::var("CARRICK_USE_SYSTEM_PROXY").is_ok();
        let mut client_builder = Client::builder();
        if !use_system_proxy {
            client_builder = client_builder.no_proxy();
        }
        let client = client_builder
            .build()
            .expect("Failed to build agent HTTP client");

        Self {
            client,
            semaphore: Arc::new(Semaphore::new(concurrency_limit)),
        }
    }

    /// Per-task lambda call where the lambda just needs a user_message +
    /// schema (e.g. file-analyzer). The lambda owns the system prompt.
    /// `task_path` is the API Gateway route, e.g. "/analyze-file".
    pub async fn analyze_with_lambda(
        &self,
        task_path: &str,
        user_message: &str,
        response_schema: Option<serde_json::Value>,
    ) -> Result<String, AgentCallError> {
        let request = LambdaRequest {
            user_message: user_message.to_string(),
            response_schema,
        };
        self.post_to_lambda(task_path, &request, user_message).await
    }

    /// Lower-level per-task lambda call for arbitrary structured payloads
    /// (e.g. framework-guidance which sends task+category+frameworks).
    /// `mock_seed` is used in mock mode to pick the right canned response.
    pub async fn post_to_lambda<B: Serialize + ?Sized>(
        &self,
        task_path: &str,
        body: &B,
        mock_seed: &str,
    ) -> Result<String, AgentCallError> {
        let _permit = self.semaphore.acquire().await.map_err(|e| {
            AgentCallError::permanent(
                "semaphore_closed",
                format!("Failed to acquire semaphore permit: {}", e),
            )
        })?;

        if env::var("CARRICK_MOCK_ALL").is_ok() {
            return Ok(generate_mock_for_task(task_path, body, mock_seed));
        }

        let provider = OidcProvider::global()
            .map_err(|e| AgentCallError::permanent("oidc_unavailable", e.to_string()))?;
        self.post_with_retry(provider, env!("CARRICK_API_ENDPOINT"), task_path, body)
            .await
    }

    /// Shared HTTP + retry implementation for all lambda calls. Sends
    /// the version header, parses the structured error envelope, and
    /// only consumes a backoff attempt when the error is marked
    /// retriable=true (or on bare network failures).
    ///
    /// `provider` and `api_base` are parameters rather than the globals the
    /// public entry point reads, so the retry loop can be driven against a
    /// stub in tests.
    async fn post_with_retry<B>(
        &self,
        provider: &OidcProvider,
        api_base: &str,
        path: &str,
        body: &B,
    ) -> Result<String, AgentCallError>
    where
        B: Serialize + ?Sized,
    {
        let endpoint = format!("{}{}", api_base, path);

        // Whether this call has already answered a rejected token with a fresh
        // mint. A second rejection after that is not an expiry, so it is fatal
        // rather than something to keep retrying with the same credential.
        let mut reminted = false;

        // Retry logic for transient failures with jittered exponential
        // backoff. 7 attempts, sleeps halving-jittered around 2s, 4s, 8s, 16s,
        // 32s, 64s (see `backoff_delay`). The lambda's structured error
        // envelope (`error.retriable`) is the source of truth for
        // application-level errors. We additionally retry on transient
        // *gateway* errors (429/502/503/504) where the body may not
        // even be a parseable JSON envelope (API Gateway timeouts return
        // non-envelope responses).
        let max_retries = MAX_RETRIES;
        for attempt in 1..=max_retries {
            // A sibling call (any phase, any `AgentService`) may have already
            // hit the backend quota wall. Re-checked each attempt so a worker
            // mid-backoff aborts after its current sleep instead of firing a
            // doomed request that burns more quota.
            if rate_limit_tripped() {
                return Err(rate_limit_abort_error());
            }

            // Read the token per attempt, not once per call. A scan of a
            // large repo outlives a token, and the provider mints a fresh one
            // as soon as the cached one nears its expiry — so the retry that
            // happens ten minutes into a call chain carries a valid credential
            // instead of the one this call started with (#461).
            let token = provider
                .token()
                .await
                .map_err(|e| AgentCallError::permanent("oidc_unavailable", e.to_string()))?;

            let request_builder = self
                .client
                .post(&endpoint)
                .json(body)
                .timeout(std::time::Duration::from_secs(60))
                .header("X-Carrick-Scanner-Version", env!("CARGO_PKG_VERSION"))
                .header("X-Carrick-Run-Id", crate::logging::run_id())
                .header("X-Carrick-OIDC", &token);

            match request_builder.send().await {
                Ok(response) => {
                    let status = response.status();

                    // Read the body as text once. Going through `.json()`
                    // discarded it, so a non-envelope response was logged as
                    // "error decoding response body" with no trace of what the
                    // body said — which is how an auth rejection wrapped in a
                    // gateway status read as a plain overload (#461).
                    let response_text = match response.text().await {
                        Ok(text) => text,
                        Err(e) => {
                            // The response never arrived in full: transport, not
                            // application, so it is retriable by definition.
                            if attempt < max_retries {
                                let wait_time = backoff_delay(attempt, jitter_seed());
                                warn!(
                                    "Failed to read agent proxy response ({}): {}. Retrying in {:?} (attempt {}/{})",
                                    status, e, wait_time, attempt, max_retries
                                );
                                sleep(wait_time).await;
                                continue;
                            }
                            return Err(AgentCallError::transient(
                                "network_error",
                                format!("Failed to read agent proxy response {}: {}", status, e),
                            ));
                        }
                    };

                    // An expired token is not always a 401 at the client: the
                    // gateway can wrap the rejection in a 5xx whose body is not
                    // an envelope, which the retry loop then spends its whole
                    // budget on as if it were an overload. Read the rejection
                    // out of the body as well as the status, mint a fresh
                    // token, and retry immediately. Only non-2xx bodies are
                    // sniffed, so a successful analysis of a file that happens
                    // to mention the code is never mistaken for one.
                    if is_oidc_rejection(status.as_u16(), &response_text) {
                        if reminted {
                            return Err(AgentCallError::permanent(
                                "oidc_rejected",
                                format!(
                                    "Agent proxy rejected a freshly minted OIDC token (status {}): {}.                                      The scan cannot authenticate to Carrick Cloud.",
                                    status,
                                    body_excerpt(&response_text)
                                ),
                            ));
                        }
                        warn!(
                            "Agent proxy rejected the OIDC token (status {}: {}); re-minting and retrying",
                            status,
                            body_excerpt(&response_text)
                        );
                        provider.remint(&token).await.map_err(|e| {
                            AgentCallError::permanent("oidc_unavailable", e.to_string())
                        })?;
                        reminted = true;
                        continue;
                    }

                    let is_transient_gateway_status =
                        matches!(status.as_u16(), 429 | 502 | 503 | 504);

                    let body: AgentResponse = match serde_json::from_str(&response_text) {
                        Ok(b) => b,
                        Err(e) => {
                            // Body wasn't a parseable envelope. If the status
                            // is a known transient gateway code, retry —
                            // otherwise fail fast (server-side bug).
                            if is_transient_gateway_status && attempt < max_retries {
                                let wait_time = backoff_delay(attempt, jitter_seed());
                                warn!(
                                    "Gateway status {} with non-envelope body ({}): {}. Retrying in {:?} (attempt {}/{})",
                                    status,
                                    e,
                                    body_excerpt(&response_text),
                                    wait_time,
                                    attempt,
                                    max_retries
                                );
                                sleep(wait_time).await;
                                continue;
                            }
                            let message = format!(
                                "Agent proxy returned status {} with unparseable body ({}): {}",
                                status,
                                e,
                                body_excerpt(&response_text)
                            );
                            return Err(if is_transient_gateway_status {
                                AgentCallError::transient("gateway_error", message)
                            } else {
                                AgentCallError::permanent("bad_response", message)
                            });
                        }
                    };

                    if status.is_success() && body.success {
                        return Ok(body.text.unwrap_or_default());
                    }

                    let err = match body.error {
                        Some(err) => err,
                        None => {
                            return Err(AgentCallError::permanent(
                                "bad_response",
                                format!(
                                    "Agent proxy status {} success={} but no error envelope",
                                    status, body.success
                                ),
                            ));
                        }
                    };

                    // A quota / rate-limit error will not clear within a single
                    // scan, and each retry consumes more of the exhausted
                    // budget. Trip the process-global breaker so sibling
                    // workers abort fast instead of each grinding the full
                    // backoff chain, and fail this call now.
                    if is_quota_error(&err) {
                        trip_rate_limit();
                        warn!(
                            "Backend LLM quota exhausted ({}); tripping circuit breaker — remaining calls fail fast and the scan aborts before upload",
                            err.message
                        );
                        return Err(rate_limit_abort_error());
                    }

                    let call_err = AgentCallError {
                        code: err.code,
                        message: err.message,
                        retriable: err.retriable,
                    };

                    if should_retry(&call_err, attempt, max_retries) {
                        let wait_time = backoff_delay(attempt, jitter_seed());
                        warn!(
                            "Agent error '{}' is retriable, retrying in {:?} (attempt {}/{}): {}",
                            call_err.code, wait_time, attempt, max_retries, call_err.message
                        );
                        sleep(wait_time).await;
                        continue;
                    }

                    return Err(call_err);
                }
                Err(e) => {
                    // Bare network failure (no response received) — retriable by definition.
                    if attempt < max_retries {
                        let wait_time = backoff_delay(attempt, jitter_seed());
                        warn!(
                            "Agent proxy network error: {}, retrying in {:?} (attempt {}/{})",
                            e, wait_time, attempt, max_retries
                        );
                        sleep(wait_time).await;
                        continue;
                    }

                    return Err(AgentCallError::transient(
                        "network_error",
                        format!("Agent proxy call failed: {}", e),
                    ));
                }
            }
        }

        Err(AgentCallError::transient(
            "retries_exhausted",
            "Maximum retry attempts exceeded".to_string(),
        ))
    }
}

/// Request body for per-task lambda endpoints (e.g. /analyze-file).
/// The lambda owns the system prompt; Rust just sends the user payload.
#[derive(Debug, Serialize)]
struct LambdaRequest {
    user_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
}

/// Lambda response envelope. On success: `success=true, text="..."`.
/// On failure: `success=false, error=AgentError{...}`. The `retriable`
/// flag on the error is the source of truth for whether the scanner
/// should consume an exponential-backoff attempt.
#[derive(Debug, Deserialize)]
struct AgentResponse {
    success: bool,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    error: Option<AgentError>,
}

#[derive(Debug, Deserialize, Clone)]
struct AgentError {
    code: String,
    message: String,
    retriable: bool,
}

/// Mock-mode dispatch by task path. Some lambdas don't send a
/// `response_schema` (e.g. /generate-intent ships only `{name, body,
/// called_intents}`), so falling through to schema-based dispatch
/// produces the wrong shape. This wrapper handles those tasks
/// explicitly before delegating to the generic schema-based mock.
fn generate_mock_for_task<B: Serialize + ?Sized>(
    task_path: &str,
    body: &B,
    mock_seed: &str,
) -> String {
    if let Some(canned) = fixture_mock_response(task_path, mock_seed) {
        return canned;
    }
    match task_path {
        "/generate-intent" => "Mock intent: function does something.".to_string(),
        _ => {
            // Tasks that send a schema (file-analyzer, framework-guidance)
            // dispatch by inspecting the schema shape. Tasks that don't but
            // happen to want the framework-detection-shaped fallback
            // (framework-detect) also land here — that's fine because the
            // default response_schema=None branch returns exactly that.
            let schema = serde_json::to_value(body)
                .ok()
                .and_then(|v| v.get("response_schema").cloned());
            generate_mock_response(&schema, mock_seed)
        }
    }
}

/// Fixture-driven mock responses for integration tests.
///
/// When `CARRICK_MOCK_FIXTURE_DIR` is set (alongside `CARRICK_MOCK_ALL`),
/// look up a canned response at `<dir>/<task>/<file_stem>.json`, keyed by the
/// analyzed file's path parsed from the user message. This lets tests replay
/// realistic agent output — including its known imperfections — through the
/// full sanitize/validate/mount-graph pipeline. Falls back to the schema-based
/// generated mocks when no fixture exists for the task/file.
fn fixture_mock_response(task_path: &str, mock_seed: &str) -> Option<String> {
    let dir = env::var("CARRICK_MOCK_FIXTURE_DIR").ok()?;
    let task = task_path.trim_start_matches('/');
    let marker = "### FILE CONTENT (Path: ";
    let key = match mock_seed.find(marker) {
        Some(idx) => {
            let rest = &mock_seed[idx + marker.len()..];
            let path = rest.split(')').next()?;
            std::path::Path::new(path)
                .file_stem()?
                .to_string_lossy()
                .into_owned()
        }
        // Tasks without a file in the prompt (framework-guidance) seed with a
        // short category token ("mount", "extraction_config", ...); use it as
        // the fixture key so one fixture dir can serve multiple tasks.
        None if is_fixture_key_token(mock_seed) => mock_seed.to_string(),
        None => "default".to_string(),
    };
    let fixture_path = std::path::Path::new(&dir)
        .join(task)
        .join(format!("{}.json", key));
    let canned = std::fs::read_to_string(&fixture_path).ok()?;
    debug!(
        "Mock fixture hit for {}: {}",
        task_path,
        fixture_path.display()
    );
    Some(substitute_candidate_placeholders(&canned, mock_seed))
}

/// A mock seed usable directly as a fixture file stem: short and free of
/// path/glob characters. Long seeds (full user messages) fall back to
/// `default`.
fn is_fixture_key_token(seed: &str) -> bool {
    !seed.is_empty()
        && seed.len() <= 64
        && seed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Replace `"@line:<n>"` candidate-id placeholders in a canned response with
/// the real SWC candidate id for that line, parsed from the prompt's
/// candidate hints (`- Candidate span:<a>-<b>: Line <n> ...`). This mirrors
/// the real agent contract — the LLM echoes the candidate_id it sees in the
/// CANDIDATE TARGETS section — without fixtures having to hard-code byte
/// offsets. Placeholders for lines with no candidate are left as-is, so they
/// fail the candidate map exactly like a hallucinated candidate_id would.
fn substitute_candidate_placeholders(canned: &str, mock_seed: &str) -> String {
    let mut out = canned.to_string();
    for line in mock_seed.lines() {
        let Some(rest) = line.trim_start().strip_prefix("- Candidate ") else {
            continue;
        };
        let Some((id, after)) = rest.split_once(": Line ") else {
            continue;
        };
        let line_no: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if line_no.is_empty() {
            continue;
        }
        out = out.replace(&format!("\"@line:{}\"", line_no), &format!("\"{}\"", id));
    }
    out
}

/// Generate mock response based on schema type
fn generate_mock_response(schema: &Option<serde_json::Value>, prompt: &str) -> String {
    match schema {
        Some(schema_val) => {
            // Check if schema is for an array
            if schema_val.get("type").and_then(|t| t.as_str()) == Some("ARRAY") {
                // Check what kind of array based on the items schema
                if let Some(items) = schema_val.get("items")
                    && let Some(props) = items.get("properties")
                {
                    // Triage schema - has location, classification, confidence
                    if props.get("classification").is_some() {
                        return generate_mock_triage_response(prompt);
                    }
                    // Endpoint schema - has method, path, handler, node_name
                    if props.get("node_name").is_some() && props.get("path").is_some() {
                        return generate_mock_endpoint_response(prompt);
                    }
                    // Consumer schema - has library, url, method
                    if props.get("library").is_some() {
                        return generate_mock_consumer_response(prompt);
                    }
                    // Mount schema - has parent_node, child_node, mount_path
                    if props.get("parent_node").is_some() && props.get("child_node").is_some() {
                        return generate_mock_mount_response(prompt);
                    }
                    // Middleware schema - has middleware_type
                    if props.get("middleware_type").is_some() {
                        return generate_mock_middleware_response(prompt);
                    }
                }
                // Default array response
                "[]".to_string()
            } else if schema_val.get("type").and_then(|t| t.as_str()) == Some("OBJECT") {
                if let Some(props) = schema_val.get("properties") {
                    // Check for file_analysis_schema - has mounts, endpoints, data_calls arrays
                    if props.get("mounts").is_some()
                        && props.get("endpoints").is_some()
                        && props.get("data_calls").is_some()
                    {
                        return generate_mock_file_analysis_response(prompt);
                    }
                    // Check for framework guidance schema - has mount_patterns, endpoint_patterns, etc.
                    if props.get("mount_patterns").is_some()
                        && props.get("endpoint_patterns").is_some()
                        && props.get("triage_hints").is_some()
                    {
                        return generate_mock_framework_guidance_response(prompt);
                    }
                    // Check for extraction_config_schema - has a rules array.
                    // An empty rule set is a valid config (no unwrapping).
                    if props.get("rules").is_some() {
                        return r#"{"rules": []}"#.to_string();
                    }
                    // Check for pattern_list_schema - has patterns, descriptions, frameworks arrays
                    if props.get("patterns").is_some()
                        && props.get("descriptions").is_some()
                        && props.get("frameworks").is_some()
                    {
                        return generate_mock_pattern_list_response();
                    }
                    // Check for general_guidance_schema - has triage_hints and parsing_notes
                    if props.get("triage_hints").is_some()
                        && props.get("parsing_notes").is_some()
                        && props.get("mount_patterns").is_none()
                    {
                        return generate_mock_general_guidance_response();
                    }
                }
                // Framework detection or other object schema
                r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#.to_string()
            } else {
                // Framework detection or other object schema
                r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#.to_string()
            }
        }
        None => {
            // No schema - return framework detection format
            r#"{"frameworks": ["express"], "data_fetchers": ["axios"], "notes": "Mock response"}"#
                .to_string()
        }
    }
}

/// Generate mock framework guidance response - returns empty structure for testing
/// The real LLM will provide actual patterns based on detected frameworks
fn generate_mock_framework_guidance_response(_prompt: &str) -> String {
    // In mock mode, return a valid but empty structure
    // The real LLM call will populate this with framework-specific patterns
    r#"{"mount_patterns":[],"endpoint_patterns":[],"middleware_patterns":[],"data_fetching_patterns":[],"triage_hints":"Mock mode - no guidance generated","parsing_notes":"Mock mode - no parsing notes"}"#.to_string()
}

/// Generate mock pattern list response for FrameworkGuidanceAgent pattern fetching
/// Returns basic patterns for common frameworks to enable testing
fn generate_mock_pattern_list_response() -> String {
    r#"{"patterns":["app.get('/path', handler)","app.post('/path', handler)","router.get('/path', handler)","app.use('/path', router)","fetch(url)","axios.get(url)"],"descriptions":["GET endpoint","POST endpoint","Router GET endpoint","Mount router","Fetch call","Axios GET"],"frameworks":["express","express","express","express","fetch","axios"]}"#.to_string()
}

/// Generate mock general guidance response for FrameworkGuidanceAgent
/// Returns empty triage hints and parsing notes
fn generate_mock_general_guidance_response() -> String {
    r#"{"triage_hints":"Mock mode - no triage hints","parsing_notes":"Mock mode - no parsing notes"}"#.to_string()
}

/// Generate mock file analysis response for FileAnalyzerAgent
/// Parses the file content from the prompt and extracts mock findings
fn generate_mock_file_analysis_response(prompt: &str) -> String {
    // Extract file path from prompt (format: "Path: path/to/file.ts")
    let file_path = prompt
        .lines()
        .find(|line| line.contains("Path:"))
        .and_then(|line| line.split("Path:").nth(1))
        .map(|s| s.trim().trim_end_matches(')'))
        .unwrap_or("unknown.ts");

    let mut candidate_by_line: HashMap<i32, (String, Option<u32>, Option<u32>)> = HashMap::new();
    let mut candidate_snippets: Vec<(String, Option<u32>, Option<u32>, String)> = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let Some(candidate_id) = value.get("candidate_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(line_number) = value.get("line_number").and_then(|v| v.as_i64()) else {
            continue;
        };
        let span_start = value
            .get("span_start")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let span_end = value
            .get("span_end")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        if let Some(code_snippet) = value.get("code_snippet").and_then(|v| v.as_str()) {
            candidate_snippets.push((
                candidate_id.to_string(),
                span_start,
                span_end,
                code_snippet.to_string(),
            ));
        }
        candidate_by_line.insert(
            line_number as i32,
            (candidate_id.to_string(), span_start, span_end),
        );
    }

    // Look for common patterns in the file content to generate mock results
    let mut mounts = Vec::new();
    let mut endpoints = Vec::new();
    let mut data_calls = Vec::new();

    // Find where the actual FILE CONTENT section starts (after "### FILE CONTENT")
    // This avoids detecting patterns from the framework guidance examples
    let file_content_start = prompt
        .find("### FILE CONTENT")
        .or_else(|| prompt.find("FILE CONTENT"))
        .unwrap_or(0);

    let content_section = &prompt[file_content_start..];
    let content_to_analyze = if let Some(fence_start) = content_section.find("```") {
        let after_fence = &content_section[fence_start + 3..];
        if let Some(fence_end) = after_fence.find("```") {
            &after_fence[..fence_end]
        } else {
            after_fence
        }
    } else {
        content_section
    };

    let resolve_candidate = |line_number: i32, line_text: &str| {
        if let Some(entry) = candidate_by_line.get(&line_number) {
            return entry.clone();
        }
        let trimmed_line = line_text.trim();
        if !trimmed_line.is_empty()
            && let Some(entry) = candidate_snippets.iter().find(|(_, _, _, snippet)| {
                snippet.contains(trimmed_line) || trimmed_line.contains(snippet)
            })
        {
            return (entry.0.clone(), entry.1, entry.2);
        }
        (format!("line:{}", line_number), None, None)
    };

    // Simple pattern matching on prompt content for mock generation
    // Only look at lines that are likely actual code (not comments, not in strings)
    for (line_num, line) in content_to_analyze.lines().enumerate() {
        let line_number = (line_num + 1) as i32;
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.is_empty() {
            continue;
        }

        // Skip lines that are clearly not endpoint definitions
        // (e.g., interface definitions, type annotations, etc.)
        if trimmed.starts_with("interface")
            || trimmed.starts_with("type ")
            || trimmed.starts_with("export type")
        {
            continue;
        }

        // Detect .use() mounts - must have a path string argument
        if (line.contains("app.use(")
            || line.contains("Router.use(")
            || line.contains("router.use(")
            || line.contains("apiRouter.use("))
            && (line.contains("\"/") || line.contains("'/"))
        {
            // Extract parent node name
            let parent = if line.contains("app.use") {
                "app"
            } else if line.contains("apiRouter.use") {
                "apiRouter"
            } else if line.contains("v1Router.use") {
                "v1Router"
            } else {
                "router"
            };

            // Try to extract the mount path
            let mount_path = extract_path_from_line(line).unwrap_or("/".to_string());

            mounts.push(serde_json::json!({
                "line_number": line_number,
                "parent_node": parent,
                "child_node": "childRouter",
                "mount_path": mount_path,
                "import_source": null,
                "pattern_matched": ".use("
            }));
        }

        // Detect endpoint patterns - must be on app/router object and have a path string
        // More specific patterns to avoid false positives
        let is_endpoint_call = (line.contains("app.get(")
            || line.contains("router.get(")
            || line.contains("v1Router.get(")
            || line.contains("apiRouter.get(")
            || line.contains("adminRouter.get("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_endpoint_call {
            let owner = extract_owner_from_line(line, "get");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "GET",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".get(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        let is_post_call = (line.contains("app.post(")
            || line.contains("router.post(")
            || line.contains("v1Router.post(")
            || line.contains("apiRouter.post(")
            || line.contains("adminRouter.post("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_post_call {
            let owner = extract_owner_from_line(line, "post");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "POST",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".post(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect DELETE endpoints
        let is_delete_call = (line.contains("app.delete(")
            || line.contains("router.delete(")
            || line.contains("v1Router.delete(")
            || line.contains("apiRouter.delete(")
            || line.contains("adminRouter.delete("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_delete_call {
            let owner = extract_owner_from_line(line, "delete");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "DELETE",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".delete(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect PUT endpoints
        let is_put_call = (line.contains("app.put(")
            || line.contains("router.put(")
            || line.contains("v1Router.put(")
            || line.contains("apiRouter.put(")
            || line.contains("adminRouter.put("))
            && (line.contains("\"/") || line.contains("'/"));

        if is_put_call {
            let owner = extract_owner_from_line(line, "put");
            let path = extract_path_from_line(line).unwrap_or("/".to_string());
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            endpoints.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "owner_node": owner,
                "method": "PUT",
                "path": path,
                "handler_name": "anonymous",
                "pattern_matched": ".put(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "response_expression_text": null,
                "response_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect fetch calls - but not response.json() or similar
        if line.contains("fetch(") && !line.contains("response") && !line.contains("res.") {
            let target =
                extract_url_from_line(line).unwrap_or("https://api.example.com".to_string());
            let method = if line.contains("method:") && line.contains("POST") {
                "POST"
            } else {
                "GET"
            };
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            data_calls.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "target": target,
                "method": method,
                "pattern_matched": "fetch(",
                "call_expression_text": null,
                "call_expression_line": null,
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }

        // Detect axios calls
        if line.contains("axios.get")
            || line.contains("axios.post")
            || line.contains("axios.put")
            || line.contains("axios.delete")
        {
            let method = if line.contains("axios.post") {
                "POST"
            } else if line.contains("axios.put") {
                "PUT"
            } else if line.contains("axios.delete") {
                "DELETE"
            } else {
                "GET"
            };
            let (candidate_id, _span_start, _span_end) = resolve_candidate(line_number, line);
            data_calls.push(serde_json::json!({
                "candidate_id": candidate_id,
                "line_number": line_number,
                "target": "https://api.example.com",
                "method": method,
                "pattern_matched": "axios.",
                "call_expression_text": null,
                "call_expression_line": null,
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null
            }));
        }
    }

    // Log mock generation for debugging
    debug!(
        "Mock file analysis for {}: {} mounts, {} endpoints, {} data_calls",
        file_path,
        mounts.len(),
        endpoints.len(),
        data_calls.len()
    );

    serde_json::json!({
        "mounts": mounts,
        "endpoints": endpoints,
        "data_calls": data_calls
    })
    .to_string()
}

/// Helper to extract path from a line like: app.get("/users", handler)
fn extract_path_from_line(line: &str) -> Option<String> {
    // Try double quotes first
    if let Some(start) = line.find("\"")
        && let Some(end) = line[start + 1..].find("\"")
    {
        let path = &line[start + 1..start + 1 + end];
        if path.starts_with('/') {
            return Some(path.to_string());
        }
    }
    // Try single quotes
    if let Some(start) = line.find("'")
        && let Some(end) = line[start + 1..].find("'")
    {
        let path = &line[start + 1..start + 1 + end];
        if path.starts_with('/') {
            return Some(path.to_string());
        }
    }
    None
}

/// Helper to extract owner from a line like: router.get("/path", ...)
fn extract_owner_from_line(line: &str, method: &str) -> String {
    let pattern = format!(".{}(", method);
    if let Some(idx) = line.find(&pattern) {
        let before = &line[..idx];
        // Get the last word before the dot
        let words: Vec<&str> = before.split_whitespace().collect();
        if let Some(last) = words.last() {
            // Clean up any remaining characters
            let cleaned = last.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !cleaned.is_empty() {
                return cleaned.to_string();
            }
        }
    }
    "router".to_string()
}

/// Helper to extract URL from fetch call
fn extract_url_from_line(line: &str) -> Option<String> {
    // Handle template literals and string literals
    if let Some(path) = extract_path_from_line(line) {
        return Some(path);
    }
    // Handle backtick template literals
    if let Some(start) = line.find('`')
        && let Some(end) = line[start + 1..].find('`')
    {
        return Some(line[start + 1..start + 1 + end].to_string());
    }
    None
}

/// Generate mock triage responses by extracting locations from prompt
fn generate_mock_triage_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);

    let triage_results: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("");

            let args = cs.get("args").and_then(|a| a.as_array());
            let arg_count = cs
                .get("arg_count")
                .and_then(|c| c.as_u64())
                .map(|c| c as usize)
                .or_else(|| args.map(|a| a.len()))
                .unwrap_or(0);

            let has_correlated_call = cs
                .get("correlated_call")
                .map(|v| !v.is_null())
                .unwrap_or(false);

            let classification = if matches!(
                callee_property,
                "json" | "text" | "blob" | "arrayBuffer" | "formData"
            ) {
                if has_correlated_call {
                    "DataFetchingCall"
                } else {
                    "Irrelevant"
                }
            } else if callee_object == "global" && callee_property == "fetch" {
                "DataFetchingCall"
            } else if matches!(callee_property, "get" | "post" | "put" | "delete" | "patch") {
                if callee_object == "axios" || callee_object == "request" || callee_object == "http"
                {
                    "DataFetchingCall"
                } else {
                    "HttpEndpoint"
                }
            } else if callee_property == "use" {
                if arg_count >= 2 {
                    let first_is_string = cs
                        .get("first_arg_type")
                        .and_then(|t| t.as_str())
                        .map(|t| t == "StringLiteral")
                        .or_else(|| {
                            args.and_then(|a| a.first())
                                .and_then(|arg| arg.get("arg_type"))
                                .and_then(|t| t.as_str())
                                .map(|t| t == "StringLiteral")
                        })
                        .unwrap_or(false);

                    // For LeanCallSite we don't have second arg info, so we assume RouterMount
                    // if first arg is string and arg_count >= 2.
                    // For full CallSite we check second arg is Identifier.
                    let second_is_id = args
                        .and_then(|a| a.get(1))
                        .and_then(|arg| arg.get("arg_type"))
                        .and_then(|t| t.as_str())
                        == Some("Identifier");

                    if first_is_string && (args.is_none() || second_is_id) {
                        "RouterMount"
                    } else {
                        "Middleware"
                    }
                } else {
                    "Middleware"
                }
            } else if arg_count >= 2 {
                let first_is_id = args
                    .and_then(|a| a.first())
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str())
                    == Some("Identifier");

                let second_is_object = args
                    .and_then(|a| a.get(1))
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str())
                    == Some("ObjectLiteral");

                if first_is_id && second_is_object {
                    let context_slice = cs
                        .get("context_slice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if extract_path_prefix_from_context_slice(context_slice).is_some() {
                        "RouterMount"
                    } else {
                        "Irrelevant"
                    }
                } else {
                    "Irrelevant"
                }
            } else {
                "Irrelevant"
            };

            serde_json::json!({
                "location": location,
                "classification": classification,
                "confidence": 0.9
            })
        })
        .collect();

    serde_json::to_string(&triage_results).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock endpoint responses
fn generate_mock_endpoint_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let endpoints: Vec<serde_json::Value> = call_sites
        .iter()
        .filter_map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let raw_path = cs
                .get("args")
                .and_then(|args| args.as_array())
                .and_then(|arr| arr.first())
                .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                .and_then(|v| v.as_str())
                .unwrap_or("/");

            let context_slice = cs
                .get("context_slice")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let inferred_prefix = if !context_slice.is_empty()
                && context_slice.contains(callee_object)
                && context_slice.contains("prefix")
            {
                extract_path_prefix_from_context_slice(context_slice)
            } else {
                None
            };

            let path = if let Some(prefix) = inferred_prefix {
                join_path_prefix(&prefix, raw_path)
            } else {
                raw_path.to_string()
            };

            if matches!(callee_property, "get" | "post" | "put" | "delete" | "patch") {
                Some(serde_json::json!({
                    "method": callee_property.to_uppercase(),
                    "path": path,
                    "handler": "handler",
                    "node_name": callee_object,
                    "location": location,
                    "confidence": 0.9,
                    "reasoning": "Mock endpoint extraction"
                }))
            } else {
                None
            }
        })
        .collect();

    serde_json::to_string(&endpoints).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock consumer (data fetching) responses
fn generate_mock_consumer_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);

    let consumers: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let correlated = cs.get("correlated_call");
            let correlated_callee = correlated
                .and_then(|c| c.get("callee"))
                .and_then(|v| v.as_str());
            let correlated_url = correlated
                .and_then(|c| c.get("url"))
                .and_then(|v| v.as_str());
            let correlated_method = correlated
                .and_then(|c| c.get("method"))
                .and_then(|v| v.as_str());

            let args = cs.get("args").and_then(|a| a.as_array());
            let arg0_value = args
                .and_then(|a| a.first())
                .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                .and_then(|v| v.as_str());

            let url: Option<String> = correlated_url
                .or(arg0_value)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());

            let method: Option<String> =
                correlated_method
                    .map(|s| s.to_string())
                    .or_else(|| match callee_property {
                        "get" | "post" | "put" | "delete" | "patch" => {
                            Some(callee_property.to_uppercase())
                        }
                        _ => None,
                    });

            let is_decode_call = matches!(
                callee_property,
                "json" | "text" | "blob" | "arrayBuffer" | "formData"
            ) && args.map(|a| a.is_empty()).unwrap_or(false);

            let library = if is_decode_call {
                correlated_callee
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "response_parsing".to_string())
            } else if callee_object == "global" {
                callee_property.to_string()
            } else if let Some(callee) = correlated_callee {
                callee.to_string()
            } else {
                callee_object.to_string()
            };

            serde_json::json!({
                "library": library,
                "url": url,
                "method": method,
                "location": location,
                "confidence": 0.8,
                "reasoning": "Mock data fetching call"
            })
        })
        .collect();

    serde_json::to_string(&consumers).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock mount relationship responses
fn generate_mock_mount_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let mounts: Vec<serde_json::Value> = call_sites
        .iter()
        .filter_map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            let args = cs.get("args").and_then(|a| a.as_array());

            if args.map(|a| a.len()).unwrap_or(0) >= 2 {
                let first_arg_type = args
                    .and_then(|a| a.first())
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str());

                let second_arg_type = args
                    .and_then(|a| a.get(1))
                    .and_then(|arg| arg.get("arg_type"))
                    .and_then(|t| t.as_str());

                if callee_property == "use"
                    && first_arg_type == Some("StringLiteral")
                    && second_arg_type == Some("Identifier")
                {
                    let path = args
                        .and_then(|a| a.first())
                        .and_then(|arg| arg.get("resolved_value").or_else(|| arg.get("value")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("/");
                    let child = args
                        .and_then(|a| a.get(1))
                        .and_then(|arg| arg.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("router");

                    return Some(serde_json::json!({
                        "parent_node": callee_object,
                        "child_node": child,
                        "mount_path": path,
                        "location": location,
                        "confidence": 0.9,
                        "reasoning": "Mock mount extraction"
                    }));
                }

                if first_arg_type == Some("Identifier") && second_arg_type == Some("ObjectLiteral")
                {
                    let child = args
                        .and_then(|a| a.first())
                        .and_then(|arg| arg.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("router");

                    let context_slice = cs
                        .get("context_slice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if let Some(prefix) = extract_path_prefix_from_context_slice(context_slice) {
                        return Some(serde_json::json!({
                            "parent_node": callee_object,
                            "child_node": child,
                            "mount_path": prefix,
                            "location": location,
                            "confidence": 0.9,
                            "reasoning": "Mock mount extraction"
                        }));
                    }
                }
            }

            None
        })
        .collect();

    serde_json::to_string(&mounts).unwrap_or_else(|_| "[]".to_string())
}

/// Generate mock middleware responses
fn generate_mock_middleware_response(prompt: &str) -> String {
    let call_sites = extract_call_sites_from_prompt(prompt);
    let middleware: Vec<serde_json::Value> = call_sites
        .iter()
        .map(|cs| {
            let callee_property = cs
                .get("callee_property")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let callee_object = cs
                .get("callee_object")
                .and_then(|o| o.as_str())
                .unwrap_or("app");
            let location = cs.get("location").and_then(|l| l.as_str()).unwrap_or("");

            serde_json::json!({
                "middleware_type": "custom",
                "path_prefix": null,
                "handler": callee_property,
                "node_name": callee_object,
                "location": location,
                "confidence": 0.8,
                "reasoning": "Mock middleware"
            })
        })
        .collect();

    serde_json::to_string(&middleware).unwrap_or_else(|_| "[]".to_string())
}

fn extract_path_prefix_from_context_slice(context_slice: &str) -> Option<String> {
    extract_string_literal_after_key(context_slice, "prefix")
        .or_else(|| extract_string_literal_after_key(context_slice, "basePath"))
        .or_else(|| extract_string_literal_after_key(context_slice, "base_path"))
        .or_else(|| extract_string_literal_after_key(context_slice, "pathPrefix"))
        .or_else(|| extract_string_literal_after_key(context_slice, "path_prefix"))
        .filter(|v| v.starts_with('/'))
        .map(|v| v.to_string())
}

fn extract_string_literal_after_key(haystack: &str, key: &str) -> Option<String> {
    let hay = haystack.as_bytes();
    let key_bytes = key.as_bytes();
    let mut i = 0;

    while i + key_bytes.len() <= hay.len() {
        if &hay[i..i + key_bytes.len()] == key_bytes {
            let mut j = i + key_bytes.len();

            while j < hay.len() && hay[j].is_ascii_whitespace() {
                j += 1;
            }

            if j >= hay.len() || (hay[j] != b':' && hay[j] != b'=') {
                i += key_bytes.len();
                continue;
            }

            j += 1;
            while j < hay.len() && hay[j].is_ascii_whitespace() {
                j += 1;
            }

            if j >= hay.len() || (hay[j] != b'\'' && hay[j] != b'"') {
                i += key_bytes.len();
                continue;
            }

            let quote = hay[j];
            j += 1;
            let start_val = j;

            while j < hay.len() && hay[j] != quote {
                j += 1;
            }

            if j >= hay.len() {
                return None;
            }

            let value = String::from_utf8_lossy(&hay[start_val..j]).to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }

        i += 1;
    }

    None
}

fn join_path_prefix(prefix: &str, path: &str) -> String {
    let normalized_prefix = prefix.trim_end_matches('/');
    let normalized_path = path.trim_start_matches('/');

    if normalized_prefix.is_empty() {
        format!("/{}", normalized_path)
    } else if normalized_path.is_empty() {
        normalized_prefix.to_string()
    } else {
        format!("{}/{}", normalized_prefix, normalized_path)
    }
}

/// Helper function to extract call sites from prompt JSON
fn extract_call_sites_from_prompt(prompt: &str) -> Vec<serde_json::Value> {
    // Try multiple search patterns for compact and pretty-printed JSON
    let patterns = [
        "[{\"callee_object\"",           // Compact JSON
        "[\n  {\n    \"callee_object\"", // Pretty-printed JSON
        "[\n  {\n   \"callee_object\"",  // Alternative indentation
    ];

    for pattern in &patterns {
        if let Some(start) = prompt.find(pattern) {
            // Find matching closing bracket
            if let Some(end_offset) = find_matching_bracket(&prompt[start..]) {
                let json_str = &prompt[start..start + end_offset];
                if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                    return parsed;
                }
            }
        }
    }

    // Fallback: iterate through all JSON arrays to find one that looks like call sites
    // This handles cases where LeanCallSite serialization might differ slightly
    // and avoids picking up other arrays (like frameworks list)
    let mut current_pos = 0;
    while let Some(start) = prompt[current_pos..].find('[') {
        let abs_start = current_pos + start;
        if let Some(end_offset) = find_matching_bracket(&prompt[abs_start..]) {
            let json_str = &prompt[abs_start..abs_start + end_offset];
            if let Ok(parsed) = serde_json::from_str::<Vec<serde_json::Value>>(json_str)
                && !parsed.is_empty()
                && parsed[0].get("callee_object").is_some()
                && parsed[0].get("location").is_some()
            {
                return parsed;
            }
        }
        current_pos = abs_start + 1;
    }

    vec![]
}

/// Find the matching closing bracket for a JSON array
fn find_matching_bracket(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn err_with_code(code: &str) -> AgentError {
        AgentError {
            code: code.to_string(),
            message: "boom".to_string(),
            retriable: true,
        }
    }

    #[test]
    fn quota_error_classified_by_code() {
        // Only the `rate_limited` code trips the breaker; transient overloads
        // keep their normal backoff path.
        assert!(is_quota_error(&err_with_code("rate_limited")));
        assert!(!is_quota_error(&err_with_code("overloaded")));
        assert!(!is_quota_error(&err_with_code("model_error")));
    }

    #[test]
    #[serial]
    fn breaker_trips_and_is_idempotent() {
        // Process-global state — reset around the assertions so neither this
        // run nor a sibling test leaks a tripped breaker.
        RATE_LIMITED.store(false, Ordering::Relaxed);
        assert!(!rate_limit_tripped());

        trip_rate_limit();
        assert!(rate_limit_tripped());
        trip_rate_limit();
        assert!(rate_limit_tripped());

        RATE_LIMITED.store(false, Ordering::Relaxed);
        assert!(!rate_limit_tripped());
    }

    #[test]
    fn abort_error_names_the_quota() {
        // The message must read as a backend capacity limit, not a code fault.
        let msg = rate_limit_abort_error().to_string().to_lowercase();
        assert!(msg.contains("quota"));
    }

    #[test]
    fn transient_errors_retry_and_permanent_ones_do_not() {
        // The cloud's own `retriable` flag decides — the scanner never
        // re-derives the class from message text. The 429-wrapped 503 the
        // backend raises under Vertex pressure (#460) arrives as
        // `model_error, retriable: true` and must consume a backoff attempt.
        let transient = AgentCallError::transient("model_error", "Gemini overloaded".to_string());
        assert!(should_retry(&transient, 1, MAX_RETRIES));
        assert!(should_retry(&transient, MAX_RETRIES - 1, MAX_RETRIES));
        // ...but only while attempts remain.
        assert!(!should_retry(&transient, MAX_RETRIES, MAX_RETRIES));

        // A permanent failure is never retried, at any attempt.
        let permanent = AgentCallError::permanent("internal_error", "boom".to_string());
        assert!(!should_retry(&permanent, 1, MAX_RETRIES));

        // A quota abort is not a per-call failure: the breaker is open, so
        // retrying only burns more of an exhausted budget.
        assert!(rate_limit_abort_error().is_quota_abort());
        assert!(!should_retry(&rate_limit_abort_error(), 1, MAX_RETRIES));
        assert!(!transient.is_quota_abort());
    }

    #[test]
    fn backoff_is_jittered_exponential_under_a_cap() {
        // Zero jitter is the floor (half the exponential), max jitter the
        // ceiling (the full exponential) — so every waker lands somewhere in
        // the back half of its window instead of all on the same instant.
        for attempt in 1..=MAX_RETRIES {
            let low = backoff_delay(attempt, 0);
            let high = backoff_delay(attempt, u32::MAX);
            assert!(low <= high, "attempt {attempt}: jitter inverted the range");
            assert!(
                high <= RETRY_MAX_DELAY,
                "attempt {attempt}: {high:?} exceeded the {RETRY_MAX_DELAY:?} cap"
            );
            assert!(low >= RETRY_BASE_DELAY / 2);
        }

        // Unjittered schedule: 2, 4, 8, 16, 32, 64 seconds, then held at the cap.
        assert_eq!(backoff_delay(1, u32::MAX), Duration::from_secs(2));
        assert_eq!(backoff_delay(2, u32::MAX), Duration::from_secs(4));
        assert_eq!(backoff_delay(6, u32::MAX), RETRY_MAX_DELAY);
        assert_eq!(backoff_delay(7, u32::MAX), RETRY_MAX_DELAY);
        // Doubling is real, not an artefact of the cap.
        assert!(backoff_delay(3, 0) > backoff_delay(2, 0));
        // A large attempt number cannot overflow into a tiny (or huge) sleep.
        assert_eq!(backoff_delay(64, u32::MAX), RETRY_MAX_DELAY);
    }

    /// A single-shot HTTP stub: answers each connection with the next canned
    /// response and records the request it received. Reads the whole request
    /// (headers plus `Content-Length` body) before replying, because reqwest
    /// treats a response that arrives mid-upload as a transport failure.
    fn stub_server(
        responses: Vec<(u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    if n == 0 {
                        break;
                    }
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(header_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let content_length = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&raw).to_string());
                let response = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            seen
        });
        (format!("http://{}", addr), handle)
    }

    /// A stub GitHub token endpoint that hands out each token in turn.
    fn stub_token_endpoint(tokens: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for token in tokens {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf).unwrap();
                let body = serde_json::json!({ "value": token }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        (format!("http://{}/token?api-version=2.0", addr), handle)
    }

    /// The header value the request carried, for asserting which token was sent.
    fn oidc_header(request: &str) -> String {
        request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("x-carrick-oidc")
                    .then(|| value.trim().to_string())
            })
            .unwrap_or_default()
    }

    /// A 401 is a rejection whatever the body says; a gateway 5xx is one only
    /// when the body names the code; a 200 never is, so an analysis result
    /// that quotes the code out of scanned source is not mistaken for one.
    #[test]
    fn oidc_rejection_reads_status_and_body() {
        assert!(is_oidc_rejection(401, ""));
        assert!(is_oidc_rejection(
            503,
            r#"{"code":"oidc_invalid","reason":"token expired"}"#
        ));
        assert!(!is_oidc_rejection(503, "upstream connect error"));
        assert!(!is_oidc_rejection(
            200,
            r#"{"success":true,"text":"the file handles oidc_invalid errors"}"#
        ));
    }

    #[test]
    fn body_excerpt_is_short_and_single_line() {
        assert_eq!(body_excerpt("  a\n  b  "), "a b");
        assert_eq!(body_excerpt("   "), "<empty body>");
        let long = "x".repeat(500);
        let excerpt = body_excerpt(&long);
        assert_eq!(excerpt.chars().count(), 203);
        assert!(excerpt.ends_with("..."));
    }

    /// The #461 path end to end: the cloud rejects the token behind a gateway
    /// 503 whose body is not an envelope, which the retry loop used to spend
    /// its whole budget on. Now the rejection is read out of the body, a fresh
    /// token is minted, and the retry carries it.
    #[tokio::test]
    async fn a_rejected_token_is_reminted_and_the_retry_carries_the_new_one() {
        // Distinct expiries so the two tokens are distinguishable on the wire.
        let (token_url, token_server) = stub_token_endpoint(vec![
            crate::oidc::tests::jwt_with_exp(unix_now() + 3600),
            crate::oidc::tests::jwt_with_exp(unix_now() + 7200),
        ]);
        let provider = OidcProvider::for_test(token_url, "request-token".to_string());

        let (api_base, api_server) = stub_server(vec![
            (
                503,
                r#"{"code":"oidc_invalid","reason":"token expired"}"#.to_string(),
            ),
            (200, r#"{"success":true,"text":"analysed"}"#.to_string()),
        ]);

        let service = AgentService::new();
        let result = service
            .post_with_retry(
                &provider,
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(result.unwrap(), "analysed");

        let requests = api_server.join().unwrap();
        token_server.join().unwrap();
        assert_eq!(requests.len(), 2, "expected exactly one retry");
        let first = oidc_header(&requests[0]);
        let second = oidc_header(&requests[1]);
        assert!(!first.is_empty(), "first request carried no OIDC header");
        assert_ne!(
            first, second,
            "the retry re-sent the token that was just rejected"
        );
    }

    /// A rejection that survives a fresh mint is not an expiry, so it fails
    /// loudly and permanently instead of burning the retry budget on a
    /// credential the cloud will keep refusing.
    #[tokio::test]
    async fn a_rejection_after_reminting_is_permanent() {
        // Distinct expiries so the two tokens are distinguishable on the wire.
        let (token_url, token_server) = stub_token_endpoint(vec![
            crate::oidc::tests::jwt_with_exp(unix_now() + 3600),
            crate::oidc::tests::jwt_with_exp(unix_now() + 7200),
        ]);
        let provider = OidcProvider::for_test(token_url, "request-token".to_string());

        let (api_base, api_server) = stub_server(vec![
            (401, r#"{"code":"oidc_invalid"}"#.to_string()),
            (401, r#"{"code":"oidc_invalid"}"#.to_string()),
        ]);

        let service = AgentService::new();
        let err = service
            .post_with_retry(
                &provider,
                &api_base,
                "/analyze-file",
                &serde_json::json!({}),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "oidc_rejected");
        assert!(!err.retriable, "a rejected fresh token is not transient");
        assert_eq!(api_server.join().unwrap().len(), 2);
        token_server.join().unwrap();
    }

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}
