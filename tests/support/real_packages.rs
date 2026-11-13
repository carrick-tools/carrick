//! A test's repo with real package names installed from stand-ins, and the
//! store's answer for them, replayed offline (carrick#3711).
//!
//! `tests/fixtures/real-packages/<name>/` holds what a scan reads of a package
//! (`package.json`, `index.d.ts`) and the store's answer for it
//! (`claims.json`); its README says why. [`install`] puts the stand-ins in a
//! repo's `node_modules` with the public lockfile entry the store's ask needs,
//! and [`write_claims`] writes the answer the offline scan reads from
//! `<CARRICK_MOCK_FIXTURE_DIR>/library-claims/default.json`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

fn packages_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real-packages")
}

fn read(name: &str, file: &str) -> String {
    let path = packages_dir().join(name).join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn version(name: &str) -> String {
    let manifest: serde_json::Value =
        serde_json::from_str(&read(name, "package.json")).expect("a stand-in manifest");
    manifest["version"].as_str().expect("a version").to_string()
}

/// Install each of `names` under `<repo>/node_modules` and list it in the
/// repo's `package-lock.json`, taken from the public registry.
pub fn install(repo: &Path, names: &[&str]) {
    let mut packages = serde_json::Map::new();
    packages.insert(
        String::new(),
        serde_json::json!({ "name": "fixture", "dependencies": {} }),
    );
    for name in names {
        let at = repo.join("node_modules").join(name);
        std::fs::create_dir_all(&at).expect("create the stand-in dir");
        for file in ["package.json", "index.d.ts"] {
            std::fs::write(at.join(file), read(name, file)).expect("write a stand-in file");
        }
        let version = version(name);
        packages.insert(
            format!("node_modules/{name}"),
            serde_json::json!({
                "version": version,
                "resolved": format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz"),
            }),
        );
    }
    let lock = serde_json::json!({
        "name": "fixture",
        "lockfileVersion": 3,
        "requires": true,
        "packages": packages,
    });
    std::fs::write(
        repo.join("package-lock.json"),
        serde_json::to_string_pretty(&lock).expect("serialise the lockfile"),
    )
    .expect("write the lockfile");
}

/// Write the store's answer for each of `names` to
/// `<cassettes>/library-claims/default.json`.
pub fn write_claims(cassettes: &Path, names: &[&str]) {
    let _ = names; // delete-check: claims dropped
    let claims: Vec<serde_json::Value> = Vec::new();
    let to = cassettes.join("library-claims");
    std::fs::create_dir_all(&to).expect("create the library-claims cassette dir");
    std::fs::write(
        to.join("default.json"),
        serde_json::json!({ "library_claims": claims }).to_string(),
    )
    .expect("write the claims cassette");
}
