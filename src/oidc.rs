//! GitHub Actions OIDC token minting for keyless cloud auth.
//!
//! When the Action runs in GitHub Actions with `id-token: write` permission,
//! the runner exposes `ACTIONS_ID_TOKEN_REQUEST_URL` and
//! `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. We exchange those for a short-lived OIDC
//! JWT scoped to the `https://api.carrick.tools` audience and send it as the
//! `X-Carrick-OIDC` header on every cloud request. The cloud derives the repo
//! identity (owner, repo, repo id) from the signed claims, so no API key is
//! needed.
//!
//! Tokens are short-lived, and a scan of a large repo outlives one. The
//! provider therefore caches a token together with the expiry its own `exp`
//! claim declares, and mints a fresh one as soon as the cached token is within
//! [`REFRESH_MARGIN`] of that expiry. Callers get the refresh for free by
//! calling [`OidcProvider::token`] before every attempt rather than once per
//! request. A 401 that still slips through (clock skew, a token revoked early)
//! is handled reactively: callers re-mint via [`OidcProvider::remint`] and
//! retry. The cloud allows ~30s clock skew.

use std::env;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Audience the cloud requires in the OIDC token's `aud` claim.
const AUDIENCE: &str = "https://api.carrick.tools";

/// Deadline for the token request — minting must be fast, and a hung GitHub
/// endpoint must not stall the scan indefinitely.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Retries after the first attempt for transient mint failures (transport
/// errors, 5xx from the GitHub token endpoint).
const MAX_FETCH_RETRIES: u32 = 2;

/// How close to its declared expiry a cached token may get before the next
/// [`OidcProvider::token`] call mints a replacement.
///
/// It has to cover the whole life of a request that is handed the token: the
/// analyzer call's own 60s timeout, plus the time the cloud spends validating
/// on arrival. 60s is one such request end to end, and GitHub's token
/// endpoint is cheap enough that erring long costs nothing but a few extra
/// mints on a long scan.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Assumed lifetime for a token whose `exp` claim could not be read (a payload
/// that is not the JWT we expect). Deliberately short: caching a token of
/// unknown lifetime for the length of a scan is the failure this module exists
/// to prevent, and an unnecessary re-mint is cheap.
const FALLBACK_LIFETIME: Duration = Duration::from_secs(240);

#[derive(Debug)]
pub enum OidcError {
    /// Not running with `id-token: write` (the request env vars are absent).
    Unavailable,
    /// The token request to GitHub failed at the transport layer.
    Request(String),
    /// GitHub returned a non-success status or an unparseable body.
    BadResponse(String),
}

impl std::fmt::Display for OidcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OidcError::Unavailable => write!(
                f,
                "GitHub Actions OIDC is not available: ACTIONS_ID_TOKEN_REQUEST_URL / \
                 ACTIONS_ID_TOKEN_REQUEST_TOKEN are not set. Add `permissions: id-token: write` \
                 to the workflow job so Carrick can authenticate to the cloud without an API key. \
                 Note: GitHub never grants OIDC credentials to pull_request runs from forks, \
                 so fork PRs cannot authenticate (when run via the Carrick GitHub Action, \
                 fork PRs are skipped instead of failing)."
            ),
            OidcError::Request(e) => write!(f, "OIDC token request failed: {}", e),
            OidcError::BadResponse(e) => write!(f, "OIDC token endpoint error: {}", e),
        }
    }
}

impl std::error::Error for OidcError {}

/// Process-wide OIDC token provider. The request URL/token and the minted JWT
/// are global to the run, so this is a singleton reached via [`OidcProvider::global`].
pub struct OidcProvider {
    client: reqwest::Client,
    request_url: String,
    request_token: String,
    cached: Mutex<Option<CachedToken>>,
}

/// A minted token and the moment it stops being usable.
#[derive(Clone, Debug)]
struct CachedToken {
    value: String,
    /// From the token's own `exp` claim, or `mint time + FALLBACK_LIFETIME`
    /// when that claim could not be read.
    expires_at: SystemTime,
}

impl CachedToken {
    /// Whether this token is close enough to expiry that a request started now
    /// might be rejected before it lands.
    fn is_expiring(&self, now: SystemTime) -> bool {
        match self.expires_at.duration_since(now) {
            Ok(remaining) => remaining <= REFRESH_MARGIN,
            // `expires_at` is already in the past.
            Err(_) => true,
        }
    }
}

static PROVIDER: OnceLock<Option<OidcProvider>> = OnceLock::new();

impl OidcProvider {
    /// Returns the shared provider, or [`OidcError::Unavailable`] if the runner
    /// did not expose the OIDC request env vars (i.e. the job lacks
    /// `id-token: write`).
    pub fn global() -> Result<&'static OidcProvider, OidcError> {
        PROVIDER
            .get_or_init(OidcProvider::from_env)
            .as_ref()
            .ok_or(OidcError::Unavailable)
    }

    fn from_env() -> Option<OidcProvider> {
        let request_url = env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok()?;
        let request_token = env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok()?;
        Some(OidcProvider {
            client: reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .build()
                .expect("default reqwest client construction cannot fail"),
            request_url,
            request_token,
            cached: Mutex::new(None),
        })
    }

    /// Returns a token that is good for at least [`REFRESH_MARGIN`] longer,
    /// minting one on first use and whenever the cached token is close to its
    /// declared expiry.
    ///
    /// Call this before every attempt, not once per request: on a scan long
    /// enough to outlive a token, the whole point is that the second attempt
    /// carries a different token from the first. The lock is held across the
    /// mint so twenty workers that all find the token expiring produce one
    /// token request between them, not twenty.
    pub async fn token(&self) -> Result<String, OidcError> {
        let mut guard = self.cached.lock().await;
        if let Some(cached) = guard.as_ref()
            && !cached.is_expiring(SystemTime::now())
        {
            return Ok(cached.value.clone());
        }
        self.mint_into(&mut guard).await
    }

    /// Forces a fresh mint, replacing the cache. Call after a 401 when the
    /// token that was actually sent may have expired mid-run.
    ///
    /// `used` is the token the failed request carried. If the cache no longer
    /// holds it, a sibling worker has already re-minted for the same reason
    /// and its token is returned instead — twenty concurrent 401s cost one
    /// token request, not twenty.
    pub async fn remint(&self, used: &str) -> Result<String, OidcError> {
        let mut guard = self.cached.lock().await;
        if let Some(cached) = guard.as_ref()
            && cached.value != used
            && !cached.is_expiring(SystemTime::now())
        {
            return Ok(cached.value.clone());
        }
        self.mint_into(&mut guard).await
    }

    /// Mints a token and stores it in the held cache guard.
    async fn mint_into(&self, guard: &mut Option<CachedToken>) -> Result<String, OidcError> {
        let value = self.fetch().await?;
        let now = SystemTime::now();
        let expires_at = expiry_from_jwt(&value).unwrap_or_else(|| {
            warn!(
                "Minted OIDC token carries no readable `exp` claim; assuming a {}s lifetime",
                FALLBACK_LIFETIME.as_secs()
            );
            now + FALLBACK_LIFETIME
        });
        // The lifetime GitHub actually grants is not documented in a form we
        // can rely on, and it is the single number that decides whether a long
        // scan survives. Log what this run was given, so the next incident can
        // be read off the run log instead of guessed at.
        if let Ok(remaining) = expires_at.duration_since(now) {
            debug!(
                "Minted OIDC token valid for a further {}s (refreshing within {}s of expiry)",
                remaining.as_secs(),
                REFRESH_MARGIN.as_secs()
            );
        }
        *guard = Some(CachedToken {
            value: value.clone(),
            expires_at,
        });
        Ok(value)
    }

    async fn fetch(&self) -> Result<String, OidcError> {
        #[derive(serde::Deserialize)]
        struct TokenResponse {
            value: String,
        }

        let mut retries = 0u32;
        loop {
            // `.query()` merges into the URL's existing query string (the
            // request URL already carries `?api-version=...`) and
            // percent-encodes the audience, matching the official
            // @actions/core toolkit behaviour.
            let transient_error = match self
                .client
                .get(&self.request_url)
                .query(&[("audience", AUDIENCE)])
                .header("Authorization", format!("Bearer {}", self.request_token))
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        let parsed: TokenResponse = response.json().await.map_err(|e| {
                            OidcError::BadResponse(format!("failed to parse token response: {}", e))
                        })?;
                        return Ok(parsed.value);
                    }

                    let body = response.text().await.unwrap_or_default();
                    let err = OidcError::BadResponse(format!(
                        "GitHub token endpoint returned {}: {}",
                        status, body
                    ));
                    // 4xx means the request itself is bad (missing permission,
                    // bad token) — retrying can't fix it.
                    if !status.is_server_error() {
                        return Err(err);
                    }
                    err
                }
                Err(e) => OidcError::Request(e.to_string()),
            };

            if retries >= MAX_FETCH_RETRIES {
                return Err(transient_error);
            }

            let backoff = Duration::from_secs(1u64 << retries);
            warn!(
                "{}; retrying OIDC token mint in {}s ({}/{})",
                transient_error,
                backoff.as_secs(),
                retries + 1,
                MAX_FETCH_RETRIES
            );
            tokio::time::sleep(backoff).await;
            retries += 1;
        }
    }
}

/// Reads the `exp` claim out of a JWT without verifying it.
///
/// Verification is the cloud's job — it holds GitHub's public keys. All the
/// scanner needs is the expiry, so this decodes the payload segment and reads
/// one number. Anything unexpected (wrong segment count, payload that is not
/// base64url JSON, no numeric `exp`) returns `None` and the caller falls back
/// to [`FALLBACK_LIFETIME`]; a malformed token must never be treated as
/// long-lived.
fn expiry_from_jwt(token: &str) -> Option<SystemTime> {
    use base64::Engine as _;

    let payload = token.split('.').nth(1)?;
    // JWT segments are base64url and unpadded.
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let exp = claims.get("exp")?.as_u64()?;
    Some(UNIX_EPOCH + Duration::from_secs(exp))
}

#[cfg(test)]
impl OidcProvider {
    /// Test-only constructor pointing the provider at an arbitrary endpoint.
    /// `pub(crate)` so the callers that thread a provider through their retry
    /// loop can be tested against a stub token endpoint too.
    pub(crate) fn for_test(request_url: String, request_token: String) -> Self {
        OidcProvider {
            // no_proxy so CI proxy env vars can't intercept the localhost call.
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            request_url,
            request_token,
            cached: Mutex::new(None),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Verifies the exact contract the cloud expects: the audience is appended
    /// to the runner-provided request URL (preserving its existing query),
    /// percent-encoded to decode back to `https://api.carrick.tools`, the
    /// request token is forwarded as a bearer header, and `.value` is the JWT.
    #[tokio::test]
    async fn fetch_appends_audience_and_parses_value() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();

            let body = r#"{"value":"header.payload.signature"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            request
        });

        let url = format!("http://{}/token?api-version=2.0", addr);
        let provider = OidcProvider::for_test(url, "request-token-xyz".to_string());

        let token = provider.fetch().await.unwrap();
        assert_eq!(token, "header.payload.signature");

        let request = server.join().unwrap();
        let request_line = request.lines().next().unwrap_or_default();
        // Existing query param preserved + audience appended and encoded so the
        // GitHub token service decodes it back to the exact required audience.
        assert!(
            request_line.contains("api-version=2.0"),
            "existing query dropped: {request_line}"
        );
        assert!(
            request_line.contains("audience=https%3A%2F%2Fapi.carrick.tools"),
            "audience missing/mis-encoded: {request_line}"
        );
        // Request token forwarded as bearer (header name case-insensitive).
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer request-token-xyz"),
            "bearer auth header missing: {request}"
        );
    }

    /// A transient 5xx from the token endpoint is retried; the mint succeeds
    /// on the second attempt instead of aborting the scan.
    #[tokio::test]
    async fn fetch_retries_transient_server_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let responses = [
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
                {
                    let body = r#"{"value":"retried.token.ok"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                },
            ];
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });

        let url = format!("http://{}/token?api-version=2.0", addr);
        let provider = OidcProvider::for_test(url, "request-token-xyz".to_string());

        let token = provider.fetch().await.unwrap();
        assert_eq!(token, "retried.token.ok");
        server.join().unwrap();
    }

    /// A 4xx from the token endpoint (bad permission/token) is permanent —
    /// no retry, error returned immediately.
    #[tokio::test]
    async fn fetch_does_not_retry_client_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf).unwrap();
            let response =
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            // A second connection attempt would block here and fail the test
            // via join timeout if fetch retried; instead the listener is
            // dropped right after the first response.
        });

        let url = format!("http://{}/token?api-version=2.0", addr);
        let provider = OidcProvider::for_test(url, "request-token-xyz".to_string());

        let err = provider.fetch().await.unwrap_err();
        assert!(
            matches!(&err, OidcError::BadResponse(msg) if msg.contains("403")),
            "expected permanent 403 error, got: {err}"
        );
        server.join().unwrap();
    }

    /// Builds a JWT whose payload carries a known `exp` (and `iat`), signed
    /// with nothing — the scanner only ever decodes it.
    pub(crate) fn jwt_with_exp(exp: u64) -> String {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = engine.encode(
            serde_json::json!({
                "aud": AUDIENCE,
                "iat": exp.saturating_sub(300),
                "exp": exp,
            })
            .to_string(),
        );
        format!("{header}.{payload}.not-a-real-signature")
    }

    /// Seconds since the epoch, as the `exp` claim counts them.
    fn unix_now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// The expiry is read from the token's own claims, to the second.
    #[test]
    fn expiry_is_read_from_the_exp_claim() {
        let token = jwt_with_exp(1_700_000_000);
        assert_eq!(
            expiry_from_jwt(&token),
            Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        );
    }

    /// Anything that is not a JWT with a numeric `exp` yields no expiry, so
    /// the caller applies the short fallback lifetime instead of caching a
    /// token of unknown life for the whole scan.
    #[test]
    fn malformed_tokens_yield_no_expiry() {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let no_exp = format!("h.{}.s", engine.encode(br#"{"aud":"x"}"#));
        let exp_not_a_number = format!("h.{}.s", engine.encode(br#"{"exp":"soon"}"#));

        for token in [
            "",
            "not-a-jwt",
            "only.two",
            "h.!!!not-base64!!!.s",
            &no_exp,
            &exp_not_a_number,
        ] {
            assert_eq!(expiry_from_jwt(token), None, "token: {token}");
        }
    }

    /// The margin, at its boundary: a token with more than `REFRESH_MARGIN`
    /// left is reused, one with exactly the margin or less is not.
    #[test]
    fn expiring_is_decided_by_the_refresh_margin() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let cached = |remaining: u64| CachedToken {
            value: "t".to_string(),
            expires_at: now + Duration::from_secs(remaining),
        };

        assert!(!cached(REFRESH_MARGIN.as_secs() + 1).is_expiring(now));
        assert!(cached(REFRESH_MARGIN.as_secs()).is_expiring(now));
        assert!(cached(1).is_expiring(now));
        assert!(
            CachedToken {
                value: "t".to_string(),
                expires_at: now - Duration::from_secs(1),
            }
            .is_expiring(now)
        );
    }

    /// The regression this module exists for: a token minted at the start of a
    /// long scan is replaced once it nears expiry, rather than being sent
    /// until the cloud rejects it. A token with plenty of life left is reused.
    #[tokio::test]
    async fn token_refreshes_when_the_cached_one_nears_expiry() {
        let near_expiry = jwt_with_exp(unix_now() + 30);
        let fresh = jwt_with_exp(unix_now() + 3600);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = [near_expiry.clone(), fresh.clone()];
        let server = thread::spawn(move || {
            for value in served {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf).unwrap();
                let body = serde_json::json!({ "value": value }).to_string();
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

        let url = format!("http://{}/token?api-version=2.0", addr);
        let provider = OidcProvider::for_test(url, "request-token-xyz".to_string());

        // First use mints; it expires inside the margin, so the next use mints
        // again rather than handing back the same token.
        assert_eq!(provider.token().await.unwrap(), near_expiry);
        assert_eq!(provider.token().await.unwrap(), fresh);
        // The fresh one has an hour left, so it is served from cache. The stub
        // has no third response to give, so a third mint would not return this.
        assert_eq!(provider.token().await.unwrap(), fresh);

        server.join().unwrap();
    }

    /// A worker whose request was rejected re-mints, but only if no sibling
    /// already did: the token in the cache is not the one it sent, so it takes
    /// that instead of asking GitHub again. The stub answers exactly once, so
    /// a second mint would hang the test's first assertion.
    #[tokio::test]
    async fn remint_reuses_a_token_a_sibling_already_minted() {
        let fresh = jwt_with_exp(unix_now() + 3600);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = fresh.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf).unwrap();
            let body = serde_json::json!({ "value": served }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let url = format!("http://{}/token?api-version=2.0", addr);
        let provider = OidcProvider::for_test(url, "request-token-xyz".to_string());

        // The sibling's mint.
        assert_eq!(provider.token().await.unwrap(), fresh);
        // This worker sent the older token and got a 401; the cache already
        // holds a newer one, so it is handed that without a second mint.
        assert_eq!(provider.remint("stale.token.value").await.unwrap(), fresh);

        server.join().unwrap();
    }
}
