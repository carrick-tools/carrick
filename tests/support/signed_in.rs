//! A test workspace signed in offline: a credential file where the test's
//! commands look (`<root>/.test-credentials`, their `XDG_CONFIG_HOME`), and a
//! hosted snapshot read under it that holds no hosted service, so nothing is
//! read off the network. Kept copies stand in for a re-scan only under the
//! snapshot they were read with, and never signed out (REVIEW-4634 B2).

#![allow(dead_code)]

use std::path::Path;

/// The token [`sign_in`] writes.
const TOKEN: &str = "synthetic-local-reader";

/// Sign `root`'s commands in under a snapshot told apart by `allowance`.
/// Written after the index, which the tests build signed out.
pub fn sign_in(root: &Path, allowance: &str) {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let config = root.join(".test-credentials/carrick");
    std::fs::create_dir_all(&config).expect("create the credential folder");
    let credential = config.join("credentials.json");
    std::fs::write(
        &credential,
        serde_json::json!({
            "api_base": "https://api.carrick.tools",
            "token": TOKEN,
            "workspace_slug": null,
            "obtained_at": "",
        })
        .to_string(),
    )
    .expect("write the credential");
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600))
        .expect("make the credential private");
    let mut identity = Sha256::new();
    identity.update(b"https://api.carrick.tools\0");
    identity.update(TOKEN.as_bytes());
    let snapshot = serde_json::json!({
        "identity": format!("{:x}", identity.finalize()),
        "checked_at": "2026-11-15T10:00:00Z",
        "resolution": {
            "schema": "carrick.resolve-repos/0",
            "workspace": {"slug": "fixture", "billing_tier": "free", "installed": true},
            "allowance_sentence": allowance,
            "repos": [],
            "project_repos": [],
        },
        "projects": {},
    });
    std::fs::create_dir_all(root.join(".carrick/hosted")).expect("create the hosted folder");
    std::fs::write(
        root.join(".carrick/hosted/snapshot.json"),
        serde_json::to_vec(&snapshot).expect("serialise the snapshot"),
    )
    .expect("write the snapshot");
}

/// Sign `root`'s commands out: the credential goes, the snapshot stays.
pub fn sign_out(root: &Path) {
    std::fs::remove_file(root.join(".test-credentials/carrick/credentials.json"))
        .expect("remove the credential");
}
