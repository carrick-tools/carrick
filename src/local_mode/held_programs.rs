//! `carrick held-programs --json`: how each indexed service's program is
//! loaded, printed for a reader that keeps a compiler open on it.
//!
//! A hidden subcommand: it is not in `LOCAL_COMMANDS` or `--help`, and it
//! reads only `.carrick/index.json` (the repos) and each checkout's own
//! config. It makes no network call and writes nothing.
//!
//! The values are the scan's own, never copies of its rules: the service list
//! is [`crate::service_derivation::resolve_for_scan`] (what the preflight and
//! the engine read), the root is the engine's `service_sidecar_root`, the
//! tsconfig is `Config::tsconfig`, and the scan root is the canonical repo
//! path the scan hands the compiler engine (`main.rs`, `set_scan_root`).

use std::path::Path;

use serde::Serialize;

/// The schema this subcommand prints.
pub const SCHEMA: &str = "carrick.held-programs/0";

/// One service's program init.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct HeldProgram {
    pub repo: String,
    pub service: String,
    pub repo_path: String,
    pub sidecar_root: String,
    pub tsconfig: Option<String>,
    pub scan_root: String,
}

/// Every service of every repo in the workspace's index.
pub fn document(workspace_root: &Path) -> Result<serde_json::Value, String> {
    let index = super::query::read_index(workspace_root)
        .map_err(|failure| failure.message().to_string())?;
    let mut services = Vec::new();
    for repo in &index.repos {
        services.extend(repo_programs(Path::new(&repo.path), &repo.name)?);
    }
    Ok(serde_json::json!({ "schema": SCHEMA, "services": services }))
}

/// The services of one repo, as a scan of it would load them.
pub fn repo_programs(repo_path: &Path, repo_name: &str) -> Result<Vec<HeldProgram>, String> {
    let _ = (repo_path, repo_name);
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_mode::read_model::{IndexedRepo, LocalIndex, READ_MODEL_VERSION};
    use std::collections::BTreeMap;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn indexed(path: &Path, name: &str) -> IndexedRepo {
        IndexedRepo {
            path: path.display().to_string(),
            name: name.to_string(),
            services: Vec::new(),
            files: BTreeMap::new(),
            uses: None,
            unread: BTreeMap::new(),
            read_from_hosted: false,
        }
    }

    /// Two repos: a monorepo whose declared service sits in a directory with
    /// its own tsconfig, and a single-service repo that declares neither.
    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mono = dir.path().join("mono");
        write(
            &mono.join("carrick.json"),
            r#"{"services":[{"serviceName":"orders","directory":"packages/orders","tsconfig":"tsconfig.build.json"}]}"#,
        );
        write(&mono.join("packages/orders/src/routes.ts"), "export {};\n");
        let solo = dir.path().join("solo");
        write(&solo.join("carrick.json"), r#"{"serviceName":"billing"}"#);
        write(&solo.join("src/app.ts"), "export {};\n");
        let index = LocalIndex {
            hosted_identity: None,
            hosted_workspace: None,
            repos_detected_by: None,
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            hosted_checked_at: None,
            hosted_source_key: None,
            version: READ_MODEL_VERSION,
            scanner_version: "test".to_string(),
            scanner_build: None,
            indexed_at: "2026-11-14T00:00:00Z".to_string(),
            repos: vec![indexed(&mono, "mono"), indexed(&solo, "solo")],
            type_check: None,
        };
        index
            .write(&dir.path().join(".carrick/index.json"))
            .unwrap();
        dir
    }

    fn row(
        repo: &Path,
        name: &str,
        service: &str,
        dir: Option<&str>,
        ts: Option<&str>,
    ) -> serde_json::Value {
        let canonical = repo.canonicalize().unwrap();
        serde_json::json!({
            "repo": name,
            "service": service,
            "repo_path": canonical.display().to_string(),
            "sidecar_root": match dir {
                Some(dir) => canonical.join(dir),
                None => canonical.clone(),
            }.display().to_string(),
            "tsconfig": ts,
            "scan_root": canonical.display().to_string(),
        })
    }

    /// Both rows, exactly: the directory service roots its program at its
    /// directory and carries its tsconfig, the flat one roots at the repo and
    /// has no tsconfig. The scan root is the repo in both.
    #[test]
    fn each_services_program_init_is_the_scans_own() {
        let dir = workspace();
        let document = document(dir.path()).unwrap();
        let expected = serde_json::json!({
            "schema": "carrick.held-programs/0",
            "services": [
                row(&dir.path().join("mono"), "mono", "orders", Some("packages/orders"), Some("tsconfig.build.json")),
                row(&dir.path().join("solo"), "solo", "billing", None, None),
            ],
        });
        assert_eq!(document, expected);
    }
}
