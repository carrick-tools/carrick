//! The one Rust reader of `carrick login`'s credential file.
//!
//! Three things in this binary now hold the same token: the hosted index
//! reader (`local_mode::hosted`), the cloud upload client
//! (`cloud_storage::aws_storage`) and the prompt-lambda client
//! (`agent_service`). They must agree on where the file is, what shape it is,
//! and when it is refused, so there is one copy of that here rather than
//! three that drift.
//!
//! Shared with `npm/carrick/src/auth/credentials.ts`, which writes it. Wire
//! contract: carrick-cloud `docs/internal/reference/laptop-scan-seam.md` §1.6.
//!
//! Deliberately no `Debug`: the token must not reach a log, an error, or the
//! persisted hosted cache.
//!
//! [`CloudAuth`] is the other half: which of the two credentials this process
//! holds, decided once so every client agrees.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const API_BASE: &str = "https://api.carrick.tools";

/// The scope a credential with no `scope` field was minted under. Every
/// credential written before the `cli` kind existed is an `mcp` one, and a
/// reader that refused them would sign out every installed CLI.
pub const DEFAULT_SCOPE: &str = "mcp";

/// The consented scope that admits a credential to the upload and inference
/// actions. Written by `carrick login` from this release on.
pub const CLI_SCOPE: &str = "cli";

#[derive(Deserialize)]
pub struct Credential {
    pub api_base: String,
    pub token: String,
    pub workspace_slug: Option<String>,
    pub obtained_at: String,
    /// The OAuth scope this credential was consented under.
    ///
    /// Absent on every credential written before the `cli` kind existed, and
    /// absent when `CARRICK_TOKEN` supplies the token, so it is read through
    /// [`Credential::scope`] and never unwrapped. It is not the Bearer
    /// selector: the cloud decides what a credential may do, and a scanner
    /// that refused locally would turn "the cloud has not deployed it yet"
    /// into "your login is wrong".
    #[serde(default)]
    pub scope: Option<String>,
}

impl Credential {
    /// Read the credential the CLI wrote, or `None` when there is none.
    ///
    /// `CARRICK_TOKEN` overrides the file, as it does everywhere else.
    pub fn load() -> Result<Option<Self>, String> {
        if let Some(token) = std::env::var_os("CARRICK_TOKEN") {
            let credential = Self {
                api_base: API_BASE.into(),
                token: token
                    .into_string()
                    .map_err(|_| "CARRICK_TOKEN is malformed")?,
                workspace_slug: None,
                obtained_at: String::new(),
                scope: None,
            };
            credential.validate()?;
            return Ok(Some(credential));
        }
        Self::read_file(&Self::path()?)
    }

    /// Where the credential file lives. `$XDG_CONFIG_HOME/carrick`, falling
    /// back to `~/.config/carrick` — never `~/.carrick`, which is the index
    /// and log directory.
    fn path() -> Result<PathBuf, String> {
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
        Ok(base.join("carrick/credentials.json"))
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

    pub fn validate(&self) -> Result<(), String> {
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

    /// The consented scope, with the pre-`cli` default applied.
    pub fn scope(&self) -> &str {
        self.scope.as_deref().unwrap_or(DEFAULT_SCOPE)
    }

    /// A stable, non-reversible identity for this credential, so a cache
    /// written under one account is never read back under another.
    pub fn identity(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.api_base.as_bytes());
        hash.update([0]);
        hash.update(self.token.as_bytes());
        format!("{:x}", hash.finalize())
    }
}

/// How this process authenticates to Carrick Cloud.
///
/// The two are mutually exclusive and the choice is made once per process: a
/// GitHub Actions runner has no credential file and a laptop has no OIDC to
/// mint, so nothing about it changes inside a run. Wire contract: carrick-cloud
/// `docs/internal/reference/laptop-scan-seam.md` §1.3 and §8.2.
pub enum CloudAuth {
    /// A GitHub Actions run with `id-token: write`. The cloud derives repo
    /// identity from the signed claims.
    Oidc,
    /// A laptop holding a credential `carrick login` wrote. The token is
    /// long-lived and cannot be re-minted, so a rejection is final.
    Bearer(String),
}

impl CloudAuth {
    /// Pick the mode for this process.
    ///
    /// OIDC wins whenever the runner offers it, so a scan inside the Action
    /// behaves exactly as it did even on a machine that also has a credential
    /// file. `AwsStorage::new` used to assert OIDC outright, on the grounds
    /// that there was no other way to authenticate; that stopped being true.
    pub fn detect() -> Result<Self, String> {
        if std::env::var_os("ACTIONS_ID_TOKEN_REQUEST_URL").is_some() {
            crate::oidc::OidcProvider::global().map_err(|e| e.to_string())?;
            return Ok(Self::Oidc);
        }
        match Credential::load() {
            Ok(Some(credential)) => Ok(Self::Bearer(credential.token)),
            // A malformed or non-private credential file is the user's own
            // problem to fix, and saying so beats falling through to the OIDC
            // error, which would name a GitHub Actions permission on a laptop.
            Err(message) => Err(message),
            // Neither credential. The OIDC error names a GitHub Actions
            // permission, which is the wrong first thing to say to someone at
            // a terminal — so the laptop remedy leads and the CI one follows.
            Ok(None) => Err(
                "Carrick is not signed in on this machine and this is not a \
                             GitHub Actions run. Run carrick login, or run the scan in \
                             Actions with `permissions: id-token: write`."
                    .to_string(),
            ),
        }
    }

    pub fn is_bearer(&self) -> bool {
        matches!(self, Self::Bearer(_))
    }
}

/// The scope the credential on disk was consented under, when there is one.
///
/// Read only to explain a refusal. Without it "the cloud refused this
/// credential" cannot be told apart from "the cloud has not deployed the
/// action yet", and both send the user to the same browser with no reason
/// given (§1.6). Every credential written before the `cli` kind existed reads
/// as [`DEFAULT_SCOPE`], which is what it is.
pub fn consented_scope() -> Option<String> {
    Credential::load()
        .ok()
        .flatten()
        .map(|credential| credential.scope().to_string())
}

/// What to add to a refusal when the credential on disk cannot do the thing
/// that was refused, because it was consented before this release existed.
pub fn relogin_hint() -> Option<String> {
    // `CARRICK_TOKEN` carries no scope, and `scope()` defaults an absent one
    // to `mcp` — which for a file is the truth and for an env token is a
    // consent that never happened. Say nothing rather than say that.
    if std::env::var_os("CARRICK_TOKEN").is_some() {
        return None;
    }
    match consented_scope() {
        Some(scope) if scope != CLI_SCOPE => Some(format!(
            "This credential was consented under scope '{scope}'; uploading and paid analysis \
             need '{CLI_SCOPE}', which a fresh carrick login requests."
        )),
        _ => None,
    }
}

/// The scan slot the cloud minted for this run, from `start-scan`.
///
/// A process-global for the same reason the quota breaker is: one scan is one
/// process, and the four prompt-lambda clients are built far from the storage
/// client that opened the run, with no handle on it. Every prompt call of a
/// laptop run carries it as `X-Carrick-Scan-Id`, and the money gates key on
/// what the cloud stored under it — never on anything the client asserts (C4).
fn scan_slot() -> &'static std::sync::OnceLock<String> {
    static SCAN_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    &SCAN_ID
}

/// Record the slot this run holds. One run opens one scan, so a second call
/// is ignored rather than replacing it.
pub fn set_scan_id(scan_id: &str) {
    let _ = scan_slot().set(scan_id.to_string());
}

/// The slot this run holds, or `None` on the CI path where none was minted.
pub fn scan_id() -> Option<&'static str> {
    scan_slot().get().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every credential on disk today was written before the field existed.
    /// Refusing them, or reading them as an unknown scope, would sign out
    /// every installed CLI on the release that adds it.
    #[test]
    fn a_credential_without_a_scope_reads_as_mcp() {
        let credential: Credential = serde_json::from_str(
            r#"{"api_base":"https://api.carrick.tools","token":"t","workspace_slug":"w",
                "obtained_at":"2026-09-11T00:00:00.000Z"}"#,
        )
        .expect("a pre-scope credential parses");
        credential.validate().expect("and is valid");
        assert_eq!(credential.scope(), DEFAULT_SCOPE);
    }

    #[test]
    fn a_cli_credential_reads_its_scope() {
        let credential: Credential = serde_json::from_str(
            r#"{"api_base":"https://api.carrick.tools","token":"t","workspace_slug":"w",
                "obtained_at":"2026-09-11T00:00:00.000Z","scope":"cli"}"#,
        )
        .unwrap();
        assert_eq!(credential.scope(), CLI_SCOPE);
    }

    /// A scope this release does not know is carried, not refused: the cloud
    /// decides what a credential may do, and a local allow-list would turn a
    /// newer server into a broken login.
    #[test]
    fn an_unknown_scope_is_carried_rather_than_refused() {
        let credential: Credential = serde_json::from_str(
            r#"{"api_base":"https://api.carrick.tools","token":"t","workspace_slug":null,
                "obtained_at":"","scope":"something-later"}"#,
        )
        .unwrap();
        credential.validate().unwrap();
        assert_eq!(credential.scope(), "something-later");
    }

    #[test]
    fn a_credential_for_another_api_base_is_refused() {
        let credential: Credential = serde_json::from_str(
            r#"{"api_base":"https://evil.invalid","token":"t","workspace_slug":null,
                "obtained_at":"","scope":"cli"}"#,
        )
        .unwrap();
        assert!(credential.validate().is_err());
    }

    /// The file is opened once and every check is made against the open
    /// descriptor, so a symlink swapped in between the stat and the read
    /// cannot widen what is read. A world- or group-readable credential is
    /// refused outright.
    #[cfg(unix)]
    #[test]
    fn credential_reader_checks_open_file_and_rejects_symlinks_and_public_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({
                "api_base": API_BASE, "token": "test",
                "workspace_slug": null, "obtained_at": "now"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Credential::read_file(&file).unwrap().is_some());
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(Credential::read_file(&link).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Credential::read_file(&file).is_err());
    }

    /// The identity is the cache key, so it must change with the token and
    /// never contain it.
    #[test]
    fn the_identity_is_a_digest_not_the_token() {
        let of = |token: &str| {
            Credential {
                api_base: API_BASE.into(),
                token: token.into(),
                workspace_slug: None,
                obtained_at: String::new(),
                scope: None,
            }
            .identity()
        };
        assert_eq!(of("a").len(), 64);
        assert_ne!(of("a"), of("b"));
        assert!(!of("secret-token").contains("secret-token"));
    }
}
