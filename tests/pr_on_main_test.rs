//! carrick-cloud#1369: a PR run marks each finding with whether main already
//! had it, so the PR check fails only on what the PR introduced.
//!
//! Two repos from `tests/fixtures/local-mode-workspace`: a flat-route
//! producer (`GET /api/v1/widgets/:widgetId`) and a client consumer whose
//! verb or response type is edited to make a finding. Every row is
//! deterministic, `CARRICK_MOCK_ALL` keeps the run offline, and the storage is
//! in memory, handing each copy back from its bytes. The type cases run the
//! real sidecar.

use async_trait::async_trait;
use carrick::cloud_storage::{
    CloudRepoData, CloudStorage, StorageError, TypeReachObject, TypeReachRecord, UploadOutcome,
    decode_type_reach,
};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::findings::PrResultPayload;
use carrick::pr_baseline::{
    MainSideFault, inject_mock_main_side_fault, main_side_checks, main_side_runs,
};
use carrick::services::type_sidecar::TypeSidecar;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The tests in this binary share the process environment and the injected
/// fault, so they run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Each service's stored type-reach record (carrick#3101), gzipped, by repo
/// and service.
type ReachObjects = HashMap<(String, Option<String>), Vec<u8>>;

/// In-memory index plus every PR result the run posted.
#[derive(Default, Clone)]
struct Store {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
    pr_results: Arc<Mutex<Vec<PrResultPayload>>>,
    type_reach: Arc<Mutex<ReachObjects>>,
}

#[async_trait]
impl CloudStorage for Store {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let mut repos = self.repos.lock().unwrap();
        repos.retain(|stored| {
            !(stored.repo_name == data.repo_name && stored.service_name == data.service_name)
        });
        repos.push(data.clone());
        Ok(UploadOutcome::default())
    }
    // The record is kept beside the blob and the blob names it, as the
    // cloud stamps it (carrick#3101), so a PR run reads main's.
    async fn upload_repo_data_with_reach(
        &self,
        data: &CloudRepoData,
        type_reach: Option<&TypeReachObject>,
        final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let Some(object) = type_reach else {
            return self.upload_repo_data(data, final_in_run).await;
        };
        self.type_reach.lock().unwrap().insert(
            (data.repo_name.clone(), data.service_name.clone()),
            object.gz().to_vec(),
        );
        let mut stamped = data.clone();
        stamped.type_reach = Some(object.reference().clone());
        self.upload_repo_data(&stamped, final_in_run).await
    }
    async fn type_reach_records(
        &self,
        stored: &[CloudRepoData],
    ) -> HashMap<String, TypeReachRecord> {
        let objects = self.type_reach.lock().unwrap();
        stored
            .iter()
            .filter_map(|data| {
                let object = objects.get(&(data.repo_name.clone(), data.service_name.clone()))?;
                let record = decode_type_reach(object, data.type_reach.as_ref()?)?;
                let id = data.service_name.clone().unwrap_or(data.repo_name.clone());
                Some((id, record))
            })
            .collect()
    }
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        Ok(self.repos.lock().unwrap().iter().any(|stored| {
            stored.repo_name == data.repo_name
                && stored.service_name == data.service_name
                && stored.commit_hash == data.commit_hash
        }))
    }
    // Every copy is read back from its bytes, as a CI run reads the cloud's
    // download: a field the blob reader does not keep reaches the PR run as
    // it would on a runner (carrick#2037).
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        let repos = self
            .repos
            .lock()
            .unwrap()
            .iter()
            .map(|stored| {
                let bytes = serde_json::to_string(stored).expect("a stored copy serializes");
                serde_json::from_str(&bytes).expect("a stored copy reads back")
            })
            .collect();
        Ok((repos, HashMap::new()))
    }
    async fn upload_type_file(
        &self,
        _repo_name: &str,
        _file_name: &str,
        _content: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        Ok(())
    }
    // The index keys each service of a repo apart, as the cloud does, so the
    // two-service fixture uploads both.
    fn supports_multi_service(&self) -> bool {
        true
    }
    async fn upload_logs(&self, _repo: &str, _log_content: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn post_pr_result(&self, payload: &PrResultPayload) -> Result<(), StorageError> {
        self.pr_results.lock().unwrap().push(payload.clone());
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {:?} failed", args);
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// `root` with no link in it. macOS reaches its temp dir through one (`/var`
/// is `/private/var`), and a scan run through a link uploads its package
/// paths with the root still on them for the upload boundary to rewrite
/// (carrick#2060), which a runner's checkout does not. The skip tests need
/// the payload a runner uploads.
fn unlinked(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap()
}

fn fixture_repo(root: &Path, name: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/local-mode-workspace")
        .join(name);
    let repo = unlinked(root).join(name);
    copy_dir(&src, &repo);
    git(&repo, &["init", "-q"]);
    commit(&repo, "init");
    repo
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// Rewrite the consumer's verb, with `padding` comment lines above the call
/// to move its line the way an unrelated edit would.
fn set_consumer_verb(repo: &Path, verb: &str, padding: usize) {
    let client = repo.join("src/client.ts");
    let source = std::fs::read_to_string(&client).unwrap();
    let body = source
        .lines()
        .filter(|line| !line.starts_with("// pad"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = body
        .replace("method: \"GET\"", &format!("method: \"{verb}\""))
        .replace("method: \"PUT\"", &format!("method: \"{verb}\""));
    let pad: String = (0..padding).map(|i| format!("// pad {i}\n")).collect();
    std::fs::write(&client, format!("{pad}{body}\n")).unwrap();
}

/// Rewrite the consumer's response type for `activeCount`, which the
/// producer returns as a number.
fn set_consumer_count_type(repo: &Path, ty: &str) {
    let client = repo.join("src/client.ts");
    let source = std::fs::read_to_string(&client).unwrap();
    let source = source
        .replace("activeCount: number;", &format!("activeCount: {ty};"))
        .replace("activeCount: string;", &format!("activeCount: {ty};"));
    std::fs::write(&client, source).unwrap();
}

fn sidecar_for(repo: &Path) -> Option<TypeSidecar> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !path.exists() {
        return None;
    }
    let sidecar = TypeSidecar::spawn(&path).ok()?;
    sidecar.start_init(repo, None);
    sidecar.wait_ready(Duration::from_secs(120)).ok()?;
    Some(sidecar)
}

async fn scan_with(store: &Store, repo: &Path, typed: bool) {
    let sidecar = if typed { sidecar_for(repo) } else { None };
    if typed {
        assert!(sidecar.is_some(), "the type cases need the built sidecar");
    }
    run_analysis_engine_with_sidecar(
        store.clone(),
        repo.to_str().unwrap(),
        sidecar.as_ref(),
        false,
    )
    .await
    .expect("scan failed");
}

async fn scan(store: &Store, repo: &Path) {
    scan_with(store, repo, false).await;
}

/// Scan `repo` as PR #7 and return the whole result it posted, which is what
/// the cloud renders the PR comment and check from.
async fn pr_payload_with(store: &Store, repo: &Path, typed: bool) -> serde_json::Value {
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe {
        std::env::set_var("GITHUB_REF", "refs/pull/7/merge");
        std::env::set_var("GITHUB_EVENT_NAME", "pull_request");
    }
    scan_with(store, repo, typed).await;
    unsafe {
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
    }
    let payload = store
        .pr_results
        .lock()
        .unwrap()
        .pop()
        .expect("a PR run posts its result");
    serde_json::to_value(&payload).unwrap()
}

/// The findings of a posted PR result.
fn findings_of(payload: &serde_json::Value) -> Vec<serde_json::Value> {
    payload["findings"].as_array().unwrap().clone()
}

/// Scan `repo` as PR #7 and return every finding it posted.
async fn pr_scan_with(store: &Store, repo: &Path, typed: bool) -> Vec<serde_json::Value> {
    findings_of(&pr_payload_with(store, repo, typed).await)
}

async fn pr_scan(store: &Store, repo: &Path) -> Vec<serde_json::Value> {
    pr_scan_with(store, repo, false).await
}

async fn pr_payload(store: &Store, repo: &Path) -> serde_json::Value {
    pr_payload_with(store, repo, false).await
}

fn wrong_verb(findings: &[serde_json::Value]) -> serde_json::Value {
    findings
        .iter()
        .find(|f| f["kind"] == "method_mismatch" && f["method"] == "PUT")
        .cloned()
        .unwrap_or_else(|| panic!("no PUT method_mismatch in {findings:#?}"))
}

fn type_mismatch(findings: &[serde_json::Value]) -> serde_json::Value {
    findings
        .iter()
        .find(|f| f["kind"] == "type_mismatch")
        .cloned()
        .unwrap_or_else(|| panic!("no type_mismatch in {findings:#?}"))
}

/// The findings with every `on_main_unknown` checked to be `reason` and then
/// removed, so two runs that could not compare for different reasons can be
/// compared byte for byte.
fn reasons_as(findings: &[serde_json::Value], reason: &str) -> Vec<serde_json::Value> {
    findings
        .iter()
        .map(|finding| {
            let mut finding = finding.clone();
            if let Some(object) = finding.as_object_mut()
                && let Some(said) = object.remove("on_main_unknown")
            {
                assert_eq!(said, serde_json::json!(reason), "{finding:#}");
            }
            finding
        })
        .collect()
}

/// Point the run at a `pull_request` event whose base is `base`, as a runner
/// does.
fn event_with_base(dir: &Path, base: &str) -> PathBuf {
    let path = dir.join("event.json");
    let event = serde_json::json!({ "pull_request": { "base": { "sha": base } } });
    std::fs::write(&path, event.to_string()).unwrap();
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe { std::env::set_var("GITHUB_EVENT_PATH", &path) };
    path
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git failed to spawn");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
        // The repo name comes from GITHUB_REPOSITORY before the directory, so
        // on a runner both fixtures would be one repo named for this one.
        std::env::remove_var("GITHUB_REPOSITORY");
        // A runner's own event would name a base commit these fixtures do
        // not have.
        std::env::remove_var("GITHUB_EVENT_PATH");
    }
    let tmp = tempfile::tempdir().unwrap();
    let producer = fixture_repo(tmp.path(), "catalog-web");
    let consumer = fixture_repo(tmp.path(), "inventory-svc");
    (tmp, producer, consumer)
}

#[tokio::test]
async fn a_pr_run_marks_each_finding_with_whether_main_had_it() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();

    // No prior index of the consumer: the run cannot tell, and says why
    // (carrick-cloud#1408).
    let store = Store::default();
    scan(&store, &producer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "put");
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    assert!(
        finding.get("on_main").is_none(),
        "no baseline, no verdict: {finding:#}"
    );
    assert_eq!(
        finding["on_main_unknown"],
        serde_json::json!("no_main_index"),
        "{finding:#}"
    );

    // Main calls with the right verb; the PR introduces the wrong one.
    set_consumer_verb(&consumer, "GET", 0);
    commit(&consumer, "main: get");
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "pr: put");
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");

    // Main already calls with the wrong verb. The PR only edits the file
    // above the call, so the line moves and the pairing does not.
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 3);
    commit(&consumer, "pr: unrelated edit above the call");
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
}

/// Declare `name` at `spec` in the repo's manifest.
fn set_dependency(repo: &Path, name: &str, spec: &str) {
    let manifest = repo.join("package.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    json["dependencies"][name] = serde_json::json!(spec);
    std::fs::write(&manifest, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

fn conflict_on(findings: &[serde_json::Value], package: &str) -> serde_json::Value {
    findings
        .iter()
        .find(|f| f["kind"] == "dependency_conflict" && f["package_name"] == package)
        .cloned()
        .unwrap_or_else(|| panic!("no dependency_conflict on {package} in {findings:#?}"))
}

/// The written specifier each service's entry of a conflict carries.
fn specs_of(finding: &serde_json::Value) -> Vec<(String, String)> {
    let mut specs: Vec<(String, String)> = finding["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["repo"].as_str().unwrap().to_string(),
                entry["spec"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    specs.sort();
    specs
}

/// carrick#2283: a PR run says of each dependency conflict whether main has
/// it and which service's specifier the PR changed. No call here is wrong,
/// so the run has no mismatch and main's analysis never runs: the conflict is
/// marked from the manifests alone.
#[tokio::test]
async fn a_pr_run_marks_a_dependency_conflict_without_a_mismatch_to_compare() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    set_dependency(&producer, "zod", "^3.22.0");
    commit(&producer, "zod 3");
    scan(&store, &producer).await;

    // No index of the consumer on main: the run cannot tell, and says why.
    set_dependency(&consumer, "zod", "^4.0.0");
    commit(&consumer, "zod 4");
    let findings = pr_scan(&store, &consumer).await;
    assert!(
        findings
            .iter()
            .all(|f| f["kind"] != "method_mismatch" && f["kind"] != "type_mismatch"),
        "this run is meant to have no mismatch: {findings:#?}"
    );
    let finding = conflict_on(&findings, "zod");
    assert_eq!(finding["tier"], serde_json::json!("major"), "{finding:#}");
    assert_eq!(
        finding["on_main_unknown"],
        serde_json::json!("no_main_index"),
        "{finding:#}"
    );
    assert!(finding.get("on_main").is_none(), "{finding:#}");
    assert!(finding.get("changed_by_pr").is_none(), "{finding:#}");

    // Main agrees with the producer; the PR moves the consumer a major on.
    set_dependency(&consumer, "zod", "^3.22.0");
    commit(&consumer, "main: zod 3");
    scan(&store, &consumer).await;
    set_dependency(&consumer, "zod", "^4.0.0");
    commit(&consumer, "pr: zod 4");
    let runs = main_side_runs();
    let findings = pr_scan(&store, &consumer).await;
    assert_eq!(main_side_runs(), runs, "nothing here needs main's analysis");
    let finding = conflict_on(&findings, "zod");
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");
    assert!(finding.get("on_main_unknown").is_none(), "{finding:#}");
    assert_eq!(
        finding["changed_by_pr"],
        serde_json::json!([{ "service": "inventory-svc", "from": "^3.22.0", "to": "^4.0.0" }]),
        "{finding:#}"
    );
    assert_eq!(
        specs_of(&finding),
        vec![
            ("catalog-web".to_string(), "^3.22.0".to_string()),
            ("inventory-svc".to_string(), "^4.0.0".to_string()),
        ],
        "{finding:#}"
    );

    // Main has the conflict already; the PR edits a source file and leaves
    // the manifest alone.
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "GET", 3);
    commit(&consumer, "pr: unrelated edit");
    let finding = conflict_on(&pr_scan(&store, &consumer).await, "zod");
    assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
    assert!(finding.get("changed_by_pr").is_none(), "{finding:#}");
}

/// The value a run states for its peer indexes: sha256 over one
/// `repo|service|commit` line per peer, sorted.
fn peer_value(lines: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut lines = lines.to_vec();
    lines.sort();
    format!("{:x}", Sha256::digest(lines.concat().as_bytes()))
}

/// carrick#2284: a PR result names the index commits the run compared with
/// and counts what it checked. The run here is untyped, so every pair it
/// matched is one it did not compare.
#[tokio::test]
async fn a_pr_result_names_the_index_it_compared_with_and_counts_what_it_checked() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, consumer) = setup();
    let store = Store::default();
    set_dependency(&producer, "zod", "^3.22.0");
    commit(&producer, "zod 3");
    scan(&store, &producer).await;
    let peers = serde_json::json!({
        "count": 1,
        "sha256": peer_value(&[format!("catalog-web||{}\n", head(&producer))]),
    });

    // No index of the consumer on main.
    set_dependency(&consumer, "zod", "^3.22.0");
    commit(&consumer, "zod 3");
    let payload = pr_payload(&store, &consumer).await;
    assert_eq!(
        payload["baseline"],
        serde_json::json!({
            "base_sha": null,
            "copy": "none",
            "main_index": [],
            "peers": peers,
        }),
        "{payload:#}"
    );
    assert_eq!(
        payload["checked"]["dependencies"],
        serde_json::json!({ "shared": 1 }),
        "{payload:#}"
    );

    // Main's copy, and a PR based on it that moves the shared package.
    scan(&store, &consumer).await;
    let main_at = head(&consumer);
    set_dependency(&consumer, "zod", "^4.0.0");
    commit(&consumer, "pr: zod 4");
    event_with_base(tmp.path(), &main_at);
    let payload = pr_payload(&store, &consumer).await;
    assert_eq!(
        payload["baseline"],
        serde_json::json!({
            "base_sha": main_at,
            "copy": "current",
            "main_index": [{
                "service": null,
                "commit": main_at,
                "scanner_version": env!("CARGO_PKG_VERSION"),
            }],
            "peers": peers,
        }),
        "{payload:#}"
    );
    assert_eq!(
        payload["checked"],
        serde_json::json!({
            "pairs": { "compatible": 0, "unverifiable": 0, "incompatible": 0, "not_compared": 3 },
            "dependencies": { "shared": 1, "changed": 1 },
        }),
        "{payload:#}"
    );
}

/// A repository of its own, written file by file.
fn written_repo(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let repo = unlinked(root).join(name);
    for (file, source) in files {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }
    git(&repo, &["init", "-q"]);
    commit(&repo, "init");
    repo
}

/// The `(file_path, name)` of every function row the store holds for `repo`:
/// the key the cloud joins a PR's function to main's row by.
fn stored_function_keys(store: &Store, repo: &str) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = store
        .repos
        .lock()
        .unwrap()
        .iter()
        .filter(|stored| stored.repo_name == repo)
        .flat_map(|stored| stored.function_definitions.values())
        .map(|row| {
            (
                row.file_path.to_string_lossy().into_owned(),
                row.name.clone(),
            )
        })
        .collect();
    keys.sort();
    keys
}

fn function_named<'a>(block: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    block["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {block:#}"))
}

/// carrick#2285: a PR result names the functions the PR added, edited or
/// removed, from the base commit's tree. `m` calls `f` and is not edited:
/// its description depends on `f`'s, so its intent hash moves in this run,
/// and it is not listed. A function is named by the file path and the name
/// main's uploaded row carries, which is what the cloud joins the two by.
#[tokio::test]
async fn a_pr_result_names_the_functions_the_pr_changed() {
    let _serial = SERIAL.lock().await;
    let (tmp, _, _) = setup();
    let repo = written_repo(
        tmp.path(),
        "toolkit",
        &[
            ("package.json", r#"{ "name": "toolkit", "private": true }"#),
            (
                "src/a.ts",
                "export function f(x: number): number {\n  return x + 1;\n}\n\n\
                 export function g(): string {\n  return \"g\";\n}\n\n\
                 function h(): void {}\n",
            ),
            (
                "src/b.ts",
                "export function p(): number {\n  return 1;\n}\n\n\
                 export const q = (): number => 2;\n",
            ),
            (
                "src/d.ts",
                "import { f } from \"./a\";\n\nexport function m(): number {\n  return f(1);\n}\n",
            ),
            (
                "src/shelf.ts",
                "export class Shelf {\n  count(): number {\n    return 1;\n  }\n}\n",
            ),
        ],
    );
    let store = Store::default();
    scan(&store, &repo).await;
    let main_at = head(&repo);
    let main_keys = stored_function_keys(&store, "toolkit");
    assert!(
        main_keys.contains(&("src/a.ts".to_string(), "f".to_string())),
        "{main_keys:?}"
    );

    // The PR: `f` edited, a comment above `g`, `h` deleted, `k` new; `b.ts`
    // renamed untouched; a method edited; `e.ts` new, calling `k`.
    std::fs::write(
        repo.join("src/a.ts"),
        "export function f(x: number): number {\n  return x + 2;\n}\n\n\
         // A note about g,\n// two lines long.\n\
         export function g(): string {\n  return \"g\";\n}\n\n\
         export function k(): number {\n  return 3;\n}\n",
    )
    .unwrap();
    git(&repo, &["mv", "src/b.ts", "src/c.ts"]);
    std::fs::write(
        repo.join("src/shelf.ts"),
        "export class Shelf {\n  count(): number {\n    return 2;\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/e.ts"),
        "import { k } from \"./a\";\n\nexport function n(): number {\n  return k();\n}\n",
    )
    .unwrap();
    commit(&repo, "pr");
    event_with_base(tmp.path(), &main_at);
    let payload = pr_payload(&store, &repo).await;
    let block = &payload["functions"];

    let mut listed: Vec<(String, String, String)> = block["changed"]
        .as_array()
        .unwrap_or_else(|| panic!("no functions.changed in {payload:#}"))
        .iter()
        .map(|entry| {
            (
                entry["file"].as_str().unwrap().to_string(),
                entry["name"].as_str().unwrap().to_string(),
                entry["change"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    listed.sort();
    let entry = |file: &str, name: &str, change: &str| {
        (file.to_string(), name.to_string(), change.to_string())
    };
    assert_eq!(
        listed,
        vec![
            entry("src/a.ts", "f", "edited"),
            entry("src/a.ts", "k", "added"),
            entry("src/e.ts", "n", "added"),
            entry("src/shelf.ts", "Shelf.count", "edited"),
        ],
        "g only moved, m was not touched, c.ts is b.ts renamed: {block:#}"
    );
    assert_eq!(
        block["removed"],
        serde_json::json!([{ "file": "src/a.ts", "name": "h" }]),
        "{block:#}"
    );
    assert_eq!(block["counts"]["added"], serde_json::json!(2), "{block:#}");
    assert_eq!(block["counts"]["edited"], serde_json::json!(2), "{block:#}");
    assert_eq!(
        block["counts"]["removed"],
        serde_json::json!(1),
        "{block:#}"
    );

    // The call edges, by the same file and name.
    let reference = |file: &str, name: &str| serde_json::json!([{ "file": file, "name": name }]);
    assert_eq!(
        function_named(block, "f")["called_by"],
        reference("src/d.ts", "m"),
        "{block:#}"
    );
    assert_eq!(
        function_named(block, "n")["calls"],
        reference("src/a.ts", "k"),
        "{block:#}"
    );
    assert_eq!(
        function_named(block, "k")["called_by"],
        reference("src/e.ts", "n"),
        "{block:#}"
    );

    // Every edited function is named as main's row names it, a method
    // included: the cloud finds main's copy by this key and no other.
    for edited in ["f", "Shelf.count"] {
        let named = function_named(block, edited);
        let key = (
            named["file"].as_str().unwrap().to_string(),
            named["name"].as_str().unwrap().to_string(),
        );
        assert!(
            main_keys.contains(&key),
            "{key:?} is not a key main's index holds: {main_keys:?}"
        );
    }

    // With no base commit the run says so and names nothing.
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe { std::env::remove_var("GITHUB_EVENT_PATH") };
    let payload = pr_payload(&store, &repo).await;
    assert_eq!(
        payload["functions"],
        serde_json::json!({ "unknown": "no_base_commit" }),
        "{payload:#}"
    );
}

/// Run the built binary over `repo` as the GitHub Action does on a
/// `pull_request` event: from inside the checkout, with the path it is given,
/// offline. Returns the result the run posted.
fn action_pr_result(repo: &Path, path: &str, scratch: &Path) -> serde_json::Value {
    let store = scratch.join("store");
    let out = scratch.join("pr-result.json");
    let _ = std::fs::remove_dir_all(&store);
    let _ = std::fs::remove_file(&out);
    std::fs::create_dir_all(&store).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(path)
        .current_dir(repo)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", &store)
        .env("CARRICK_PR_RESULT_OUT", &out)
        .env("GITHUB_EVENT_NAME", "pull_request")
        .env("GITHUB_REF", "refs/pull/7/merge");
    // A runner's own values would name this repo and a base commit the
    // fixture does not have.
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_PATH",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let run = cmd.output().expect("failed to spawn carrick");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success(), "{output}");
    let posted = std::fs::read_to_string(&out)
        .unwrap_or_else(|error| panic!("the PR run posted no result ({error}):\n{output}"));
    serde_json::from_str(&posted).unwrap()
}

/// A PR scan names the functions the PR changed when it runs as the GitHub
/// Action runs it: the path is `.`, the checkout is the PR's merge commit,
/// and the config has a service whose directory is `.` beside one in a
/// subdirectory. The walk of that first service lists `././src/label.ts`,
/// which is the file git names `src/label.ts`. The block is the same when the
/// same checkout is scanned by its absolute path.
#[tokio::test]
async fn an_action_run_names_the_functions_of_a_service_at_the_repo_root() {
    let _serial = SERIAL.lock().await;
    let (tmp, _, _) = setup();
    let repo = written_repo(
        tmp.path(),
        "portal",
        &[
            (
                "carrick.json",
                r#"{ "services": [
                    { "name": "web", "directory": "." },
                    { "name": "edge", "directory": "workers/edge" }
                ] }"#,
            ),
            ("package.json", r#"{ "name": "portal", "private": true }"#),
            (
                "src/label.ts",
                "export function title(name: string): string {\n  return name.trim();\n}\n",
            ),
            (
                "src/old.ts",
                "export function retired(): number {\n  return 0;\n}\n",
            ),
            (
                "workers/edge/package.json",
                r#"{ "name": "edge", "private": true }"#,
            ),
            (
                "workers/edge/src/index.ts",
                "export function route(path: string): string {\n  return path;\n}\n",
            ),
        ],
    );
    git(&repo, &["branch", "-M", "main"]);

    // The PR: one function appended to a file of the root service, one file
    // of it deleted, one function of the nested service edited.
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        repo.join("src/label.ts"),
        "export function title(name: string): string {\n  return name.trim();\n}\n\n\
         export function minutes(words: number): number {\n  return Math.ceil(words / 200);\n}\n",
    )
    .unwrap();
    std::fs::remove_file(repo.join("src/old.ts")).unwrap();
    std::fs::write(
        repo.join("workers/edge/src/index.ts"),
        "export function route(path: string): string {\n  return path.toLowerCase();\n}\n",
    )
    .unwrap();
    commit(&repo, "pr");

    // Main moves on, and the checkout is the merge of the two, as
    // `refs/pull/<n>/merge` is.
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("NOTES.md"), "main moved\n").unwrap();
    commit(&repo, "main moves");
    let main_at = head(&repo);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "merge", "feature"]);

    let scratch = tmp.path().join("action");
    let relative = action_pr_result(&repo, ".", &scratch);
    assert_eq!(relative["baseline"]["base_sha"], main_at.as_str());
    let block = &relative["functions"];
    let mut listed: Vec<(String, String, String)> = block["changed"]
        .as_array()
        .unwrap_or_else(|| panic!("no functions.changed in {relative:#}"))
        .iter()
        .map(|entry| {
            (
                entry["file"].as_str().unwrap().to_string(),
                entry["name"].as_str().unwrap().to_string(),
                entry["change"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    listed.sort();
    let entry = |file: &str, name: &str, change: &str| {
        (file.to_string(), name.to_string(), change.to_string())
    };
    assert_eq!(
        listed,
        vec![
            entry("src/label.ts", "minutes", "added"),
            entry("workers/edge/src/index.ts", "route", "edited"),
        ],
        "{block:#}"
    );
    assert_eq!(
        block["removed"],
        serde_json::json!([{ "file": "src/old.ts", "name": "retired" }]),
        "{block:#}"
    );

    let absolute = action_pr_result(&repo, repo.to_str().unwrap(), &scratch);
    assert_eq!(
        absolute["functions"], relative["functions"],
        "the path the scan is given does not change what the PR changed"
    );
}

/// Main's side failing in any way costs the run nothing but the comparison:
/// the findings it posts are byte for byte the ones a run with no prior index
/// posts, apart from why neither could compare. (The payload's `delta`
/// differs, as it must: a prior index is what gives the run one.)
#[tokio::test]
async fn a_failing_main_side_posts_the_findings_of_a_run_without_a_baseline() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();

    let fresh = Store::default();
    scan(&fresh, &producer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "put");
    let without_baseline = serde_json::to_string(&reasons_as(
        &pr_scan(&fresh, &consumer).await,
        "no_main_index",
    ))
    .unwrap();

    // Main calls with the right verb, so the comparison has work to do.
    let store = Store::default();
    scan(&store, &producer).await;
    set_consumer_verb(&consumer, "GET", 0);
    commit(&consumer, "main: get");
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "pr: put");

    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe { std::env::set_var("CARRICK_MAIN_SIDE_TIMEOUT_SECS", "1") };
    for fault in [
        MainSideFault::Error,
        MainSideFault::Panic,
        MainSideFault::Hang,
    ] {
        let runs = main_side_runs();
        inject_mock_main_side_fault(fault);
        let posted = serde_json::to_string(&reasons_as(
            &pr_scan(&store, &consumer).await,
            "main_side_failed",
        ))
        .unwrap();
        assert_eq!(main_side_runs(), runs + 1, "{fault:?}: main's side ran");
        assert_eq!(posted, without_baseline, "{fault:?}");
    }
    unsafe { std::env::remove_var("CARRICK_MAIN_SIDE_TIMEOUT_SECS") };
}

/// A PR with no mismatch to mark: main's side never needs to run. (A PR that
/// leaves every scanned service as main has it is the next two tests'.)
#[tokio::test]
async fn main_side_is_skipped_when_it_has_nothing_to_add() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    scan(&store, &producer).await;

    // Main and the PR both call with the right verb, and the PR changes the
    // call's line so the surfaces differ.
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "GET", 2);
    commit(&consumer, "pr: pad");
    let runs = main_side_runs();
    let findings = pr_scan(&store, &consumer).await;
    assert_eq!(main_side_runs(), runs, "no mismatch, nothing run");
    assert!(
        findings.iter().all(|f| f.get("on_main").is_none()),
        "{findings:#?}"
    );
}

/// Give one function in main's stored copy of `repo` (of `service`, in a
/// multi-service repo) the kind `kind`, and return its name. The function is
/// one whose stored kind is a real one other than `kind`. `Placeholder` is
/// what a release before carrick#2036 stored for every function of a service
/// whose payload the upload boundary rewrote.
fn set_one_function_kind(store: &Store, repo: &str, service: Option<&str>, kind: &str) -> String {
    let mut changed = None;
    rewrite_main_copy(store, repo, |stored| {
        if stored.service_name.as_deref() != service {
            return;
        }
        let mut value = serde_json::to_value(&*stored).unwrap();
        let definitions = value["function_definitions"].as_object_mut().unwrap();
        let name = definitions
            .iter()
            .filter(|(_, definition)| {
                definition["node_type"] != kind && definition["node_type"] != "Placeholder"
            })
            .map(|(name, _)| name.clone())
            .min()
            .unwrap_or_else(|| panic!("main's copy has a function that is not {kind}"));
        definitions[&name]["node_type"] = serde_json::json!(kind);
        *stored = serde_json::from_value(value).unwrap();
        changed = Some(name);
    });
    changed.unwrap_or_else(|| panic!("main has a copy of {service:?}"))
}

/// Main's stored copies went up as scanned: the upload boundary found no
/// machine path to rewrite. Its rewrite reads the payload back through the
/// same reader as the download, so a copy it rewrote would match this run's
/// rewritten one even with a reader that loses fields, and the skip would
/// fire without proving the read.
fn assert_not_rewritten(store: &Store) {
    let stored = serde_json::to_string(&*store.repos.lock().unwrap()).unwrap();
    for placeholder in ["<checkout>", "<home>"] {
        assert!(
            !stored.contains(placeholder),
            "main's copy went through the upload boundary's rewrite ({placeholder})"
        );
    }
}

/// The two PR results, compared as the bytes the cloud receives, so a
/// failure prints the line where they part.
fn assert_same_result(compared: &serde_json::Value, skipped: &serde_json::Value, why: &str) {
    assert_eq!(
        serde_json::to_string_pretty(compared).unwrap(),
        serde_json::to_string_pretty(skipped).unwrap(),
        "{why}: the full comparison posted another result than the skip"
    );
}

/// carrick#2037. A PR that leaves every scanned service as main has it skips
/// main's side of the comparison, and posts what the full comparison posts.
///
/// Main's copy reaches the run through its bytes, as a CI run downloads it.
/// Before carrick#2036 that read turned every function's kind into
/// `Placeholder`, so the copy never matched the run's own scan and the skip
/// never ran. The full comparison is the same PR against the same copy with
/// one function's kind changed: the surface compares kinds and no finding
/// reads them, so main's side runs and must reach the skip's result.
#[tokio::test]
async fn a_pr_that_leaves_main_as_it_is_posts_what_the_full_comparison_posts() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    scan(&store, &producer).await;

    // Main already calls with the wrong verb, and the PR touches only a file
    // nothing scans.
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "main: put");
    scan(&store, &consumer).await;
    std::fs::write(consumer.join("NOTES.txt"), "unscanned\n").unwrap();
    commit(&consumer, "pr: notes");
    let main_copy = store.repos.lock().unwrap().clone();
    assert_not_rewritten(&store);

    let runs = main_side_runs();
    let skipped = pr_payload(&store, &consumer).await;
    assert_eq!(main_side_runs(), runs, "same surface, main's side skipped");
    // A finding with a pairing, so it is the skip, and not a comparison with
    // nothing to mark, that kept main's side from running.
    let finding = wrong_verb(&findings_of(&skipped));
    assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");

    for kind in ["ArrowFunction", "Placeholder"] {
        *store.repos.lock().unwrap() = main_copy.clone();
        let changed = set_one_function_kind(&store, "inventory-svc", None, kind);
        let why = format!("{changed} is {kind} in main's copy");
        let runs = main_side_runs();
        let compared = pr_payload(&store, &consumer).await;
        assert_eq!(main_side_runs(), runs + 1, "{why}: main's side ran");
        assert_same_result(&compared, &skipped, &why);
    }
}

/// carrick#2037 on the two-service repo whose breaks only the retype check
/// finds (carrick#1491). Main's side cannot retype, so the full comparison
/// reads those breaks from main's stored verdicts; the skip must reach the
/// same answer without them.
#[tokio::test]
async fn a_two_service_pr_that_leaves_main_as_it_is_posts_what_the_full_comparison_posts() {
    let _serial = SERIAL.lock().await;
    let (tmp, _, _) = setup();
    let (repo, cassettes) = retype_repo(tmp.path());
    let _cassettes = Cassettes::from(&cassettes);

    let store = Store::default();
    scan_with(&store, &repo, true).await;
    assert_not_rewritten(&store);
    std::fs::write(repo.join("NOTES.txt"), "unscanned\n").unwrap();
    commit(&repo, "pr: notes");

    let runs = main_side_runs();
    let skipped = pr_payload_with(&store, &repo, true).await;
    assert_eq!(main_side_runs(), runs, "same surface, main's side skipped");
    for line in [6, 11] {
        let finding = call_at(&findings_of(&skipped), line);
        assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
    }

    let changed = set_one_function_kind(&store, "retype-http-client", Some("web"), "ArrowFunction");
    let why = format!("{changed} is ArrowFunction in main's copy of web");
    let runs = main_side_runs();
    let compared = pr_payload_with(&store, &repo, true).await;
    assert_eq!(main_side_runs(), runs + 1, "{why}: main's side ran");
    assert_same_result(&compared, &skipped, &why);
}

/// A type mismatch is judged on main's side from main's stored type surface,
/// with the real sidecar: an existing one reads as on main after an edit that
/// moves its line, and one the PR introduces does not.
#[tokio::test]
async fn a_type_mismatch_is_checked_against_mains_stored_surface() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    scan_with(&store, &producer, true).await;

    // Main reads the count as a string; the producer returns a number. The
    // PR only edits the file above the call.
    set_consumer_count_type(&consumer, "string");
    commit(&consumer, "main: string count");
    scan_with(&store, &consumer, true).await;
    set_consumer_verb(&consumer, "GET", 3);
    commit(&consumer, "pr: unrelated edit above the call");
    let runs = main_side_runs();
    let finding = type_mismatch(&pr_scan_with(&store, &consumer, true).await);
    assert_eq!(main_side_runs(), runs + 1, "main's side ran its type check");
    assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");

    // Main agrees with the producer; the PR breaks the type.
    set_consumer_count_type(&consumer, "number");
    set_consumer_verb(&consumer, "GET", 0);
    commit(&consumer, "main: number count");
    scan_with(&store, &consumer, true).await;
    set_consumer_count_type(&consumer, "string");
    commit(&consumer, "pr: string count");
    let finding = type_mismatch(&pr_scan_with(&store, &consumer, true).await);
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");
}

/// carrick#2284, with the real sidecar: the pairs the type check reached are
/// counted under what it answered, and a type the PR breaks moves its pairs
/// from compatible to incompatible.
#[tokio::test]
async fn a_typed_pr_result_counts_each_compared_pair_under_its_verdict() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    scan_with(&store, &producer, true).await;
    scan_with(&store, &consumer, true).await;

    set_consumer_verb(&consumer, "GET", 3);
    commit(&consumer, "pr: unrelated edit above the call");
    let agreeing = pr_payload_with(&store, &consumer, true).await["checked"]["pairs"].clone();

    set_consumer_count_type(&consumer, "string");
    commit(&consumer, "pr: string count");
    let broken = pr_payload_with(&store, &consumer, true).await["checked"]["pairs"].clone();

    // The consumer's three calls share the response type the PR breaks.
    assert_eq!(
        agreeing,
        serde_json::json!({ "compatible": 3, "unverifiable": 0, "incompatible": 0, "not_compared": 0 })
    );
    assert_eq!(
        broken,
        serde_json::json!({ "compatible": 0, "unverifiable": 0, "incompatible": 3, "not_compared": 0 })
    );
}

/// The two-service repo whose consumer reads a response field off calls that
/// publish no comparable type, so only the retype check (carrick#1491) can
/// judge them: `web/src/checkout.ts` reads `response.data.x` after the calls
/// on lines 6 and 11, and `api` answers with `{ y }`. Returned with its mocked
/// model answers.
fn retype_repo(root: &Path) -> (PathBuf, PathBuf) {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retype-http-client");
    let repo = unlinked(root).join("retype-http-client");
    copy_dir(&fixture, &repo);
    git(&repo, &["init", "-q"]);
    commit(&repo, "init");
    (repo, fixture.join("__llm__"))
}

/// Answer the model from `cassettes` until the returned guard drops.
struct Cassettes;

impl Cassettes {
    fn from(cassettes: &Path) -> Self {
        // SAFETY: the tests in this binary run one at a time (SERIAL).
        unsafe {
            std::env::set_var(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", cassettes.display()),
            )
        };
        Cassettes
    }
}

impl Drop for Cassettes {
    fn drop(&mut self) {
        // SAFETY: the tests in this binary run one at a time (SERIAL).
        unsafe { std::env::remove_var("CARRICK_MOCK_FIXTURE_DIR") };
    }
}

/// The finding at the call on `line` of the consumer file, from a PR run's
/// findings.
fn call_at(findings: &[serde_json::Value], line: u32) -> serde_json::Value {
    let site = format!("web/src/checkout.ts:{line}");
    findings
        .iter()
        .find(|f| f["kind"] == "type_mismatch" && f["call_sites"][0] == site.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("no type_mismatch at {site} in {findings:#?}"))
}

/// carrick-cloud#1408, the smoke test's shape. The PR run finds the broken
/// reads by retyping the consumer's calls, which needs the consumer's source
/// on disk. Main's side is recomputed from main's stored copy, which has none,
/// so the recomputation cannot judge them. Main's own scan did judge them,
/// with main's sources, and stored the answer: that answer is main's.
///
/// The PR adds an exported function after the calls, so every call keeps its
/// line and the scanned surface gains a function definition. That difference,
/// not the unordered sets the surface also holds (carrick#786), is what makes
/// main's side run, and the test checks it is there.
#[tokio::test]
async fn a_break_main_stored_from_its_own_retype_is_on_main() {
    let _serial = SERIAL.lock().await;
    let (tmp, _, _) = setup();
    let (repo, cassettes) = retype_repo(tmp.path());
    let _cassettes = Cassettes::from(&cassettes);

    let store = Store::default();
    scan_with(&store, &repo, true).await;
    {
        let repos = store.repos.lock().unwrap();
        let stored: Vec<String> = repos
            .iter()
            .flat_map(|repo| repo.compat_verdicts.iter().flatten())
            .flat_map(|row| row.sites.iter())
            .filter(|site| {
                site.response.as_ref().is_some_and(|answer| {
                    answer.verdict == carrick::operation::TypeVerdict::Incompatible
                })
            })
            .map(|site| site.consumer_location.clone())
            .collect();
        for line in [6, 11] {
            let site = format!("web/src/checkout.ts:{line}");
            assert!(
                stored.contains(&site),
                "main's scan stored its own retype of the call at {site}: {stored:?}"
            );
        }
    }

    let checkout = repo.join("web/src/checkout.ts");
    let mut source = std::fs::read_to_string(&checkout).unwrap();
    source.push_str(
        "\nexport function checkoutPath(orderId: string): string {\n  return `/checkout/${orderId}`;\n}\n",
    );
    std::fs::write(&checkout, source).unwrap();
    commit(&repo, "pr: a function after the calls");

    // The edit is in the surface the PR run compares with main's copy: a
    // main-branch scan of the PR's tree defines a function main's copy does
    // not. Checked here so the run count below never rests on chance.
    let web_functions = |store: &Store| -> std::collections::BTreeSet<String> {
        store
            .repos
            .lock()
            .unwrap()
            .iter()
            .filter(|repo| repo.service_name.as_deref() == Some("web"))
            .flat_map(|repo| repo.function_definitions.keys().cloned())
            .collect()
    };
    let pr_tree = Store::default();
    scan(&pr_tree, &repo).await;
    let added: Vec<String> = web_functions(&pr_tree)
        .difference(&web_functions(&store))
        .cloned()
        .collect();
    assert!(
        added.iter().any(|name| name.contains("checkoutPath")),
        "the PR's function is in the scanned surface: added {added:?}"
    );

    let runs = main_side_runs();
    let findings = pr_scan_with(&store, &repo, true).await;
    assert_eq!(main_side_runs(), runs + 1, "main's side ran");
    for line in [6, 11] {
        let finding = call_at(&findings, line);
        assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
    }
}

/// carrick#3127. Main's side takes main's stored outcome for each pair of
/// main's own copy and sends no check for it, and the PR's result is the one
/// it posts when main's side judges every pair again (the off arm).
#[tokio::test]
async fn main_side_takes_mains_stored_outcomes_and_sends_no_check() {
    let _serial = SERIAL.lock().await;
    let (tmp, _, _) = setup();
    let (repo, cassettes) = retype_repo(tmp.path());
    let _cassettes = Cassettes::from(&cassettes);

    let store = Store::default();
    scan_with(&store, &repo, true).await;
    {
        let repos = store.repos.lock().unwrap();
        let web = repos
            .iter()
            .find(|repo| repo.service_name.as_deref() == Some("web"))
            .expect("main stored web");
        let record = web
            .pair_outcomes
            .as_ref()
            .expect("main stored web's pair outcomes");
        assert!(record.outcomes.len() >= 2, "{:?}", record.outcomes);
        assert!(
            record.outcomes.iter().all(|outcome| outcome.judged),
            "{:?}",
            record.outcomes
        );
    }

    let checkout = repo.join("web/src/checkout.ts");
    let mut source = std::fs::read_to_string(&checkout).unwrap();
    source.push_str(
        "\nexport function checkoutPath(orderId: string): string {\n  return `/checkout/${orderId}`;\n}\n",
    );
    std::fs::write(&checkout, source).unwrap();
    commit(&repo, "pr: a function after the calls");

    let (runs, checks) = (main_side_runs(), main_side_checks());
    let on = pr_payload_with(&store, &repo, true).await;
    assert_eq!(main_side_runs(), runs + 1, "main's side ran");
    assert_eq!(
        main_side_checks(),
        checks,
        "main's side sent no check: every pair took main's outcome"
    );

    let (runs, checks) = (main_side_runs(), main_side_checks());
    let off = {
        let _off = EnvVar::set("CARRICK_TYPE_REACH", "off");
        pr_payload_with(&store, &repo, true).await
    };
    assert_eq!(main_side_runs(), runs + 1, "main's side ran");
    assert_eq!(
        main_side_checks(),
        checks + 1,
        "with the decision off, main's side judges its pairs"
    );

    for field in ["findings", "delta", "verified", "checked", "baseline"] {
        assert_eq!(on[field], off[field], "{field}");
    }
    assert!(!findings_of(&on).is_empty());
    for line in [6, 11] {
        let finding = call_at(&findings_of(&on), line);
        assert_eq!(finding["on_main"], serde_json::json!(true), "{finding:#}");
    }
}

/// Review of carrick#1525 (F1). Main's stored mismatch was judged against the
/// producer as it was when main's consumer last scanned. The producer has
/// fixed the contract since, and main's recomputation, judged against the
/// producer as it is now, resolves the call as compatible. A PR that breaks
/// the call again introduces the break, whatever main's stored row says.
#[tokio::test]
async fn a_pr_that_breaks_a_call_the_producer_fixed_introduces_it() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, consumer) = setup();
    let store = Store::default();
    scan_with(&store, &producer, true).await;

    // Main reads the count as a string; the producer returns a number, and
    // main's scan stores the mismatch.
    set_consumer_count_type(&consumer, "string");
    commit(&consumer, "main: string count");
    scan_with(&store, &consumer, true).await;

    // The producer now returns a string, and re-indexes. Main's consumer is
    // not rescanned, so its stored row still says incompatible.
    let route = producer.join("app/routes/api.v1.widgets.$widgetId.ts");
    let source = std::fs::read_to_string(&route).unwrap();
    let fixed = source
        .replace("activeCount: number;", "activeCount: string;")
        .replace("activeCount: 3", "activeCount: \"3\"");
    assert_ne!(source, fixed, "the producer's count type moved");
    std::fs::write(&route, fixed).unwrap();
    commit(&producer, "producer: string count");
    scan_with(&store, &producer, true).await;

    // The PR reads the count as a number again.
    set_consumer_count_type(&consumer, "number");
    commit(&consumer, "pr: number count");
    let finding = type_mismatch(&pr_scan_with(&store, &consumer, true).await);
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");
}

/// carrick-cloud#1408. When this clone shows main's copy to be at a commit
/// that differs from the PR's base, main moved after its last index, and a
/// finding the copy lacks may be main's: the run cannot call it introduced.
#[tokio::test]
async fn a_copy_behind_the_prs_base_never_calls_a_finding_introduced() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, consumer) = setup();
    let store = Store::default();
    scan(&store, &producer).await;

    // Main's last index calls with the right verb. Main then moves on without
    // a new index, and the PR, based on the moved main, sends the wrong verb.
    scan(&store, &consumer).await;
    set_consumer_verb(&consumer, "GET", 1);
    commit(&consumer, "main: moved after its last index");
    let base = head(&consumer);
    set_consumer_verb(&consumer, "PUT", 1);
    commit(&consumer, "pr: put");

    event_with_base(tmp.path(), &base);
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    unsafe { std::env::remove_var("GITHUB_EVENT_PATH") };
    assert!(finding.get("on_main").is_none(), "{finding:#}");
    assert_eq!(
        finding["on_main_unknown"],
        serde_json::json!("main_index_stale"),
        "{finding:#}"
    );

    // The same PR against a copy at its base is compared.
    let fresh = Store::default();
    scan(&fresh, &producer).await;
    set_consumer_verb(&consumer, "GET", 1);
    commit(&consumer, "main: indexed");
    let base = head(&consumer);
    scan(&fresh, &consumer).await;
    set_consumer_verb(&consumer, "PUT", 1);
    commit(&consumer, "pr: put again");
    event_with_base(tmp.path(), &base);
    let finding = wrong_verb(&pr_scan(&fresh, &consumer).await);
    unsafe { std::env::remove_var("GITHUB_EVENT_PATH") };
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");
}

/// A second client in the consumer, beside the one the tests edit: a member
/// that calls the producer's health route, and a caller that imports it. The
/// PR runs below never touch these files, so their rows are what an earlier
/// scanner's copy of main is judged on (carrick#1530).
fn add_health_client(repo: &Path) {
    std::fs::write(
        repo.join("src/health-client.ts"),
        r#"import { send } from "./send.js";

export type Health = {
  status: string;
};

export class HealthClient {
  constructor(private readonly baseUrl: string) {}

  readHealth(): Promise<Health> {
    return send<Health>(`${this.baseUrl}/api/v1/health`, {
      method: "GET",
    });
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("src/status.ts"),
        r#"import type { HealthClient } from "./health-client.js";

export async function isUp(client: HealthClient): Promise<boolean> {
  const health = await client.readHealth();
  return health.status === "ok";
}
"#,
    )
    .unwrap();
}

/// Change main's stored copy of `repo` in place, as another scanner would
/// have written it.
fn rewrite_main_copy(store: &Store, repo: &str, mut rewrite: impl FnMut(&mut CloudRepoData)) {
    let mut repos = store.repos.lock().unwrap();
    let mut rewritten = 0;
    for stored in repos.iter_mut().filter(|stored| stored.repo_name == repo) {
        rewrite(stored);
        rewritten += 1;
    }
    assert!(rewritten > 0, "main has a copy of {repo}");
}

/// The health client's row sits at its caller.
fn in_status(location: &str) -> bool {
    location.starts_with("src/status.ts")
}

/// carrick#1530. Main's copy was written by another scanner version, as it is
/// on every repo from a release until main is scanned again. The other
/// scanner read every file the PR left alone as this run does, so its copy
/// counts, and a break the PR introduces reads as introduced. Where it read
/// one of those files differently, the run cannot tell, as before.
#[tokio::test]
async fn another_scanners_copy_counts_when_it_reads_the_untouched_files_alike() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, consumer) = setup();
    let store = Store::default();
    scan(&store, &producer).await;

    // Main calls with the right verb, and has a second client the PRs leave
    // alone. Its row is in main's copy, so there is something to compare.
    add_health_client(&consumer);
    commit(&consumer, "main: get, and a health client");
    let base = head(&consumer);
    scan(&store, &consumer).await;
    {
        let repos = store.repos.lock().unwrap();
        let copy = repos
            .iter()
            .find(|stored| stored.repo_name == "inventory-svc")
            .expect("main's copy of the consumer");
        assert!(
            copy.calls
                .iter()
                .any(|call| in_status(&call.file_path.to_string_lossy())),
            "main's copy has a row for a file the PRs leave alone: {:#?}",
            copy.calls
        );
        assert_eq!(
            copy.scanner_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
    rewrite_main_copy(&store, "inventory-svc", |copy| {
        copy.scanner_version = Some("0.0.1".to_string());
    });
    event_with_base(tmp.path(), &base);

    // The PR changes the client's verb and nothing else. The rows sit at the
    // callers, which the PR did not touch, and they moved with the client:
    // the run cannot tell that from a scanner that reads the callers
    // differently, so it does not compare.
    set_consumer_verb(&consumer, "PUT", 0);
    commit(&consumer, "pr: put");
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    assert_eq!(
        finding["on_main_unknown"],
        serde_json::json!("main_index_other_scanner"),
        "{finding:#}"
    );

    // The PR also edits the callers. Every file it left alone reads as main's
    // copy reads it, so the copy counts and the wrong verb is introduced.
    let callers = consumer.join("src/inventory.ts");
    let mut source = std::fs::read_to_string(&callers).unwrap();
    source.push_str("\n// Counts come from the catalog.\n");
    std::fs::write(&callers, source).unwrap();
    commit(&consumer, "pr: and a note in the callers");
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    assert_eq!(finding["on_main"], serde_json::json!(false), "{finding:#}");

    // The other scanner never saw the health client's call.
    rewrite_main_copy(&store, "inventory-svc", |copy| {
        copy.calls
            .retain(|call| !in_status(&call.file_path.to_string_lossy()));
        if let Some(graph) = copy.mount_graph.as_mut() {
            graph
                .data_calls
                .retain(|call| !in_status(&call.file_location));
        }
        if let Some(manifest) = copy.type_manifest.as_mut() {
            manifest.retain(|entry| !in_status(&entry.file_path));
        }
    });
    let finding = wrong_verb(&pr_scan(&store, &consumer).await);
    unsafe { std::env::remove_var("GITHUB_EVENT_PATH") };
    assert!(finding.get("on_main").is_none(), "{finding:#}");
    assert_eq!(
        finding["on_main_unknown"],
        serde_json::json!("main_index_other_scanner"),
        "{finding:#}"
    );
}

/// Scan `repo` as PR #7 and return the endpoint delta it posted.
async fn pr_delta(store: &Store, repo: &Path) -> serde_json::Value {
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe {
        std::env::set_var("GITHUB_REF", "refs/pull/7/merge");
        std::env::set_var("GITHUB_EVENT_NAME", "pull_request");
    }
    scan_with(store, repo, false).await;
    unsafe {
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
    }
    let payload = store
        .pr_results
        .lock()
        .unwrap()
        .pop()
        .expect("a PR run posts its result");
    serde_json::to_value(&payload).unwrap()["delta"].clone()
}

/// Main's stored copies of `repo_name` as another scanner release would have
/// written them: by `version`, and without the endpoint that release did not
/// state.
fn as_written_by(store: &Store, repo_name: &str, version: Option<&str>) {
    for stored in store
        .repos
        .lock()
        .unwrap()
        .iter_mut()
        .filter(|stored| stored.repo_name == repo_name)
    {
        stored.scanner_version = version.map(str::to_string);
        stored.endpoints.clear();
    }
}

/// Main's stored copies of `repo_name` without their HTTP endpoint `path`,
/// everything else as this release wrote it: an earlier reading of the same
/// tree that did not state the route.
fn forget_stored_route(store: &Store, repo_name: &str, path: &str) {
    for stored in store
        .repos
        .lock()
        .unwrap()
        .iter_mut()
        .filter(|stored| stored.repo_name == repo_name)
    {
        stored.endpoints.retain(|endpoint| {
            !matches!(
                &endpoint.key,
                carrick::operation::OperationKey::Http { path: stored, .. } if stored == path
            )
        });
    }
}

/// The `method path` of every row on one side of a posted delta.
fn delta_rows(delta: &serde_json::Value, side: &str) -> Vec<String> {
    delta[side]
        .as_array()
        .map(|refs| {
            refs.iter()
                .map(|r| {
                    format!(
                        "{} {}",
                        r["method"].as_str().unwrap_or_default(),
                        r["path"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

const WIDGET_ROUTE: &str = "/api/v1/widgets/:widgetId";
const WIDGETS_FILE: &str = "app/routes/api.v1.widgets.$widgetId.ts";
const HEALTH_FILE: &str = "app/routes/api.v1.health.ts";

/// Append `text` to `file` under `repo`.
fn append_to(repo: &Path, file: &str, text: &str) {
    let path = repo.join(file);
    let mut source = std::fs::read_to_string(&path).unwrap();
    source.push_str(text);
    std::fs::write(path, source).unwrap();
}

/// Replace the first line of `file` under `repo` with `line`.
fn replace_line_two(repo: &Path, file: &str, line: &str) {
    let path = repo.join(file);
    let source = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<&str> = source.lines().collect();
    lines[1] = line;
    std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

/// Main's stored HTTP endpoint paths of `repo_name` with `from` replaced by
/// `to`, the scanner version kept: the same index as an earlier reading of
/// this release spelled them.
fn respell_stored_paths(store: &Store, repo_name: &str, from: &str, to: &str) {
    for stored in store
        .repos
        .lock()
        .unwrap()
        .iter_mut()
        .filter(|stored| stored.repo_name == repo_name)
    {
        for endpoint in stored.endpoints.iter_mut() {
            if let carrick::operation::OperationKey::Http { path, .. } = &mut endpoint.key {
                *path = path.replace(from, to);
            }
        }
    }
}

/// carrick#3079: a route spelled differently in main's index and in the PR's
/// scan is not new and not removed; a different route still is both.
#[tokio::test]
async fn a_pr_delta_ignores_how_a_route_parameter_is_spelled() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();
    let paths = |delta: &serde_json::Value, side: &str| -> Vec<String> {
        delta[side]
            .as_array()
            .map(|refs| {
                refs.iter()
                    .map(|r| r["path"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    for respelled in [
        "/api/v1/widgets/{widgetId}",
        "/api/v1/widgets/${widgetId}",
        "/api/v1/widgets/:id",
    ] {
        let store = Store::default();
        scan(&store, &producer).await;
        respell_stored_paths(&store, &repo_name, "/api/v1/widgets/:widgetId", respelled);
        event_with_base(_tmp.path(), &head(&producer));
        let delta = pr_delta(&store, &producer).await;
        assert!(!delta.is_null(), "{respelled}: a delta is posted");
        assert!(
            paths(&delta, "new_endpoints").is_empty(),
            "{respelled}: {delta:#}"
        );
        assert!(
            paths(&delta, "removed_endpoints").is_empty(),
            "{respelled}: {delta:#}"
        );
    }

    let store = Store::default();
    scan(&store, &producer).await;
    respell_stored_paths(&store, &repo_name, "/widgets/", "/gadgets/");
    event_with_base(_tmp.path(), &head(&producer));
    let delta = pr_delta(&store, &producer).await;
    assert_eq!(
        paths(&delta, "new_endpoints"),
        vec!["/api/v1/widgets/:widgetId"]
    );
    assert_eq!(
        paths(&delta, "removed_endpoints"),
        vec!["/api/v1/gadgets/:widgetId"]
    );
}

/// carrick#1712: a row the PR did not add never shows as new. Main's index
/// written by this release lacking an endpoint the PR's scan states is a real
/// difference, and the delta lists it; the same index written by another
/// release, or by one that did not say which, is no baseline for the delta,
/// so the PR posts none.
#[tokio::test]
async fn a_pr_shows_no_endpoint_delta_against_an_index_another_release_wrote() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();
    let new_paths = |delta: &serde_json::Value| -> Vec<String> {
        delta["new_endpoints"]
            .as_array()
            .map(|refs| {
                refs.iter()
                    .map(|r| r["path"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    let store = Store::default();
    scan(&store, &producer).await;
    as_written_by(&store, &repo_name, Some(env!("CARGO_PKG_VERSION")));
    // The PR adds a route file, so the rows its scan states there are the
    // PR's own, and this release's index lacks them (carrick#3080).
    let base = head(&producer);
    std::fs::write(
        producer.join("app/routes/api.v1.gizmos.ts"),
        "export async function loader(): Promise<{ ok: boolean }> {\n  return { ok: true };\n}\n",
    )
    .unwrap();
    commit(&producer, "pr: a gizmos route");
    event_with_base(_tmp.path(), &base);
    let delta = pr_delta(&store, &producer).await;
    assert_eq!(
        new_paths(&delta),
        vec!["/api/v1/gizmos"],
        "this release's index lacks the endpoint, so the PR lists it: {delta:#}"
    );

    for other in [Some("0.0.1"), None] {
        let store = Store::default();
        scan(&store, &producer).await;
        as_written_by(&store, &repo_name, other);
        event_with_base(_tmp.path(), &head(&producer));
        let delta = pr_delta(&store, &producer).await;
        assert!(delta.is_null(), "{other:?}: {delta:#}");
    }
}

/// carrick#3080: the delta lists a row only when the PR's own diff places it.
/// A route main's reading lacked, in a file the PR edits on other lines or
/// does not touch, is a reading that moved, not the PR's.
#[tokio::test]
async fn a_pr_delta_lists_only_rows_the_prs_diff_places() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();

    // Edited file: the PR changes a comment on line 2 of the widgets file.
    // Main's reading lacked the route that file has always held.
    let store = Store::default();
    scan(&store, &producer).await;
    forget_stored_route(&store, &repo_name, WIDGET_ROUTE);
    let base = head(&producer);
    replace_line_two(
        &producer,
        WIDGETS_FILE,
        "// GET /api/v1/widgets/:widgetId, edited.",
    );
    commit(&producer, "pr: a comment");
    event_with_base(tmp.path(), &base);
    let delta = pr_delta(&store, &producer).await;
    assert!(!delta.is_null(), "a delta is posted: {delta:#}");
    assert_eq!(
        delta_rows(&delta, "new_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );
    assert_eq!(
        delta_rows(&delta, "removed_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );

    // Edited file with a new route: the same PR adds an `action` export, and
    // only the route it adds is listed.
    append_to(
        &producer,
        WIDGETS_FILE,
        "\nexport async function action(): Promise<{ ok: boolean }> {\n  return { ok: true };\n}\n",
    );
    commit(&producer, "pr: an action");
    event_with_base(tmp.path(), &base);
    let delta = pr_delta(&store, &producer).await;
    assert_eq!(
        delta_rows(&delta, "new_endpoints"),
        vec![format!("POST {WIDGET_ROUTE}")],
        "{delta:#}"
    );
}

#[tokio::test]
async fn a_pr_delta_leaves_out_a_row_in_a_file_the_pr_did_not_touch() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();
    let store = Store::default();
    scan(&store, &producer).await;
    forget_stored_route(&store, &repo_name, WIDGET_ROUTE);
    let base = head(&producer);
    append_to(&producer, HEALTH_FILE, "\n// A note.\n");
    commit(&producer, "pr: health note");
    event_with_base(tmp.path(), &base);
    let delta = pr_delta(&store, &producer).await;
    assert!(!delta.is_null(), "a delta is posted: {delta:#}");
    assert_eq!(
        delta_rows(&delta, "new_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );
    assert_eq!(
        delta_rows(&delta, "removed_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );
}

/// Main gained a route file after the PR branched, and its index was scanned
/// there: the route is main's, not one the PR removed.
#[tokio::test]
async fn a_pr_delta_does_not_list_a_route_main_gained_after_the_pr_branched() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, _consumer) = setup();
    let base = head(&producer);

    git(&producer, &["checkout", "-q", "-b", "main-moves"]);
    std::fs::write(
        producer.join("app/routes/api.v1.gadgets.ts"),
        "export async function loader(): Promise<{ ok: boolean }> {\n  return { ok: true };\n}\n",
    )
    .unwrap();
    commit(&producer, "main: a gadgets route");
    let moved = head(&producer);
    let store = Store::default();
    scan(&store, &producer).await;

    git(&producer, &["checkout", "-q", "-b", "pr", &base]);
    append_to(&producer, HEALTH_FILE, "\n// A note.\n");
    commit(&producer, "pr: health note");
    event_with_base(tmp.path(), &moved);
    let delta = pr_delta(&store, &producer).await;
    assert!(!delta.is_null(), "a delta is posted: {delta:#}");
    assert_eq!(
        delta_rows(&delta, "removed_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );
    assert_eq!(
        delta_rows(&delta, "new_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );
}

/// Controls: a route file the PR adds is new, and one it deletes is removed.
#[tokio::test]
async fn a_pr_delta_lists_a_route_file_the_pr_adds_or_deletes() {
    let _serial = SERIAL.lock().await;
    let (tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();
    let store = Store::default();
    scan(&store, &producer).await;
    let base = head(&producer);

    std::fs::write(
        producer.join("app/routes/api.v1.gizmos.ts"),
        "export async function loader(): Promise<{ ok: boolean }> {\n  return { ok: true };\n}\n",
    )
    .unwrap();
    commit(&producer, "pr: a gizmos route");
    event_with_base(tmp.path(), &base);
    let delta = pr_delta(&store, &producer).await;
    assert_eq!(
        delta_rows(&delta, "new_endpoints"),
        vec!["GET /api/v1/gizmos"],
        "{delta:#}"
    );
    assert_eq!(
        delta_rows(&delta, "removed_endpoints"),
        Vec::<String>::new(),
        "{delta:#}"
    );

    git(&producer, &["rm", "-q", HEALTH_FILE]);
    commit(&producer, "pr: drop the health route");
    let delta = pr_delta(&store, &producer).await;
    assert_eq!(
        delta_rows(&delta, "removed_endpoints"),
        vec!["GET /api/v1/health"],
        "{repo_name}: {delta:#}"
    );
}

/// Without a base commit the run cannot tell what the PR changed, so it
/// shows no delta.
#[tokio::test]
async fn a_pr_with_no_base_commit_shows_no_endpoint_delta() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, _consumer) = setup();
    let repo_name = producer.file_name().unwrap().to_str().unwrap().to_string();
    let store = Store::default();
    scan(&store, &producer).await;
    forget_stored_route(&store, &repo_name, WIDGET_ROUTE);
    let delta = pr_delta(&store, &producer).await;
    assert!(delta.is_null(), "{delta:#}");
}

// carrick#3095: one seeded change per finding kind and per protocol, run as a
// real PR scan against main's stored index. The fixture and its answer key are
// `tests/fixtures/pr-seeds/README.md`. Every seed edits `orders-api` (or
// `admin-api`) and leaves `storefront` as main has it, so each break is a
// consumer the PR did not update. A seed whose payload is dark today holds its
// full assertion under `#[ignore]`, naming the issue that fixes it; that issue
// removes the ignore as its failing-first test.

/// Set the five switches the cloud runner sets for every workspace
/// (carrick-cloud `runner/image/src/proc.ts`) until the guard drops.
struct FactLayer(Vec<(&'static str, Option<String>)>);

impl FactLayer {
    const SWITCHES: [&'static str; 5] = [
        "CARRICK_SITE_QUESTIONS",
        "CARRICK_POSITION_ROWS",
        "CARRICK_RECEIVER_TRACE",
        "CARRICK_FACT_FILES",
        "CARRICK_SITE_PAYLOADS",
    ];

    fn on() -> Self {
        let before = Self::SWITCHES
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();
        for name in Self::SWITCHES {
            // SAFETY: the tests in this binary run one at a time (SERIAL).
            unsafe { std::env::set_var(name, "1") };
        }
        FactLayer(before)
    }
}

impl Drop for FactLayer {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            // SAFETY: the tests in this binary run one at a time (SERIAL).
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// The process environment a seed scan starts from: the model replayed from
/// cassettes, and no runner event.
fn seed_env() {
    // SAFETY: the tests in this binary run one at a time (SERIAL).
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REPOSITORY");
        std::env::remove_var("GITHUB_EVENT_PATH");
    }
}

/// A fresh git repo holding a copy of `tests/fixtures/pr-seeds/<name>`.
fn seed_repo(root: &Path, name: &str) -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pr-seeds")
        .join(name);
    let repo = unlinked(root).join(name);
    copy_dir(&src, &repo);
    git(&repo, &["init", "-q"]);
    commit(&repo, "init");
    repo
}

/// Replace each `from` in `file` of `repo` with its `to`. A `from` the file
/// lacks panics, so a seed that stops applying fails loudly.
fn edit(repo: &Path, file: &str, replacements: &[(&str, &str)]) {
    let path = repo.join(file);
    let mut source = std::fs::read_to_string(&path).unwrap();
    for (from, to) in replacements {
        assert!(
            source.contains(from),
            "seed edit: {from:?} is not in {file}"
        );
        source = source.replace(from, to);
    }
    std::fs::write(&path, source).unwrap();
}

/// Main's index of the seed fixture, built once per run of this binary: the
/// stored rows of the `orders-api` and `storefront` store, of the `admin-api`
/// store (its router stub pins a package no registry has, which would fail
/// every cross-repo type check it joined, trap 1), and the trees they came
/// from. Each seed copies what it edits.
struct MainSnapshot {
    trees: PathBuf,
    shared_rows: Vec<CloudRepoData>,
    shared_reach: ReachObjects,
    admin_rows: Vec<CloudRepoData>,
    admin_reach: ReachObjects,
}

static MAIN: tokio::sync::OnceCell<MainSnapshot> = tokio::sync::OnceCell::const_new();

/// Scan `repo` typed, with the model answering from its own `__llm__`.
async fn scan_seed_repo(store: &Store, repo: &Path) {
    let _cassettes = Cassettes::from(&repo.join("__llm__"));
    scan_with(store, repo, true).await;
}

async fn main_snapshot() -> &'static MainSnapshot {
    MAIN.get_or_init(|| async {
        seed_env();
        let _fact_layer = FactLayer::on();
        // The trees outlive the run: a static is never dropped, and each seed
        // copies from them for as long as the binary runs.
        let trees = tempfile::tempdir().unwrap().keep();
        let orders = seed_repo(&trees, "orders-api");
        let storefront = seed_repo(&trees, "storefront");
        let admin = seed_repo(&trees, "admin-api");
        let shared = Store::default();
        scan_seed_repo(&shared, &orders).await;
        scan_seed_repo(&shared, &storefront).await;
        scan_seed_repo(&shared, &orders).await;
        let admin_store = Store::default();
        scan_seed_repo(&admin_store, &admin).await;
        MainSnapshot {
            trees: unlinked(&trees),
            shared_rows: shared.repos.lock().unwrap().clone(),
            shared_reach: shared.type_reach.lock().unwrap().clone(),
            admin_rows: admin_store.repos.lock().unwrap().clone(),
            admin_reach: admin_store.type_reach.lock().unwrap().clone(),
        }
    })
    .await
}

/// The result of scanning `repo_name` of the fixture as a PR that applies
/// `change` on top of main.
///
/// Every seed is also the A/B of carrick#3103: the PR is scanned with the
/// type-reach decision off (every service re-typed whole) and then on, and
/// the two posted results must be the same. The on run is last, so a dump
/// (`CARRICK_TYPE_REACH_DUMP`) reads its decision. A run that turned the
/// decision off itself is scanned once.
async fn seed_payload(repo_name: &str, change: impl Fn(&Path)) -> serde_json::Value {
    if std::env::var("CARRICK_TYPE_REACH").is_ok_and(|value| value.trim() == "off") {
        return seed_payload_once(repo_name, &change).await;
    }
    let off = {
        let _off = EnvVar::set("CARRICK_TYPE_REACH", "off");
        seed_payload_once(repo_name, &change).await
    };
    let on = seed_payload_once(repo_name, &change).await;
    assert_eq!(
        on, off,
        "the PR result of {repo_name} moved with the type-reach decision on"
    );
    on
}

async fn seed_payload_once(repo_name: &str, change: &dyn Fn(&Path)) -> serde_json::Value {
    let main = main_snapshot().await;
    seed_env();
    let _fact_layer = FactLayer::on();
    let tmp = tempfile::tempdir().unwrap();
    let repo = unlinked(tmp.path()).join(repo_name);
    copy_dir(&main.trees.join(repo_name), &repo);
    let base = head(&repo);
    git(&repo, &["checkout", "-q", "-b", "pr"]);
    change(&repo);
    commit(&repo, "pr: seed");
    event_with_base(tmp.path(), &base);
    let (rows, reach) = if repo_name == "admin-api" {
        (&main.admin_rows, &main.admin_reach)
    } else {
        (&main.shared_rows, &main.shared_reach)
    };
    let store = Store {
        repos: Arc::new(Mutex::new(rows.clone())),
        pr_results: Arc::default(),
        type_reach: Arc::new(Mutex::new(reach.clone())),
    };
    let _cassettes = Cassettes::from(&repo.join("__llm__"));
    pr_payload_with(&store, &repo, true).await
}

/// The finding of `kind` for `method path` that names `site` among its call
/// sites (any, when `site` is `None`).
fn seed_finding(
    payload: &serde_json::Value,
    kind: &str,
    method: &str,
    path: &str,
    site: Option<&str>,
) -> serde_json::Value {
    let findings = findings_of(payload);
    findings
        .iter()
        .find(|f| {
            f["kind"] == kind
                && f["method"] == method
                && f["path"] == path
                && site.is_none_or(|site| {
                    f["call_sites"]
                        .as_array()
                        .is_some_and(|sites| sites.iter().any(|s| s == site))
                })
        })
        .cloned()
        .unwrap_or_else(|| panic!("no {kind} {method} {path} at {site:?} in {findings:#?}"))
}

/// The finding of `kind` at call site `site`, whatever labels it carries.
fn seed_finding_at(payload: &serde_json::Value, kind: &str, site: &str) -> serde_json::Value {
    let findings = findings_of(payload);
    findings
        .iter()
        .find(|f| {
            f["kind"] == kind
                && f["call_sites"]
                    .as_array()
                    .is_some_and(|sites| sites.iter().any(|s| s == site))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no {kind} at {site} in {findings:#?}"))
}

/// What a finding says about main and about how its pair was made. `None`
/// means the field is absent.
fn assert_marks(
    finding: &serde_json::Value,
    on_main: Option<bool>,
    edge_source: Option<&str>,
    verdict_state: Option<&str>,
) {
    assert_eq!(
        finding.get("on_main").and_then(|v| v.as_bool()),
        on_main,
        "on_main: {finding:#}"
    );
    assert_eq!(
        finding.get("edge_source").and_then(|v| v.as_str()),
        edge_source,
        "edge_source: {finding:#}"
    );
    assert_eq!(
        finding.get("verdict_state").and_then(|v| v.as_str()),
        verdict_state,
        "verdict_state: {finding:#}"
    );
}

/// The `kind` rows the payload holds for `name`, which is a path or a package.
fn rows_named(payload: &serde_json::Value, kind: &str, name: &str) -> usize {
    findings_of(payload)
        .iter()
        .filter(|f| f["kind"] == kind && (f["path"] == name || f["package_name"] == name))
        .count()
}

fn assert_delta(payload: &serde_json::Value, new: &[&str], removed: &[&str]) {
    assert_eq!(
        delta_rows(&payload["delta"], "new_endpoints"),
        new,
        "new: {payload:#}"
    );
    assert_eq!(
        delta_rows(&payload["delta"], "removed_endpoints"),
        removed,
        "removed: {payload:#}"
    );
}

/// `file:name:change` of every function the PR changed.
fn changed_functions(payload: &serde_json::Value) -> Vec<String> {
    payload["functions"]["changed"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    format!(
                        "{}:{}:{}",
                        r["file"].as_str().unwrap_or_default(),
                        r["name"].as_str().unwrap_or_default(),
                        r["change"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

const ORDER_ROUTE: &str = "app/routes/api.v1.orders.$orderId.ts";
const ORDER_PATH: &str = "/api/v1/orders/:orderId";
/// The two call sites of the consumer's request helper (trap 3): the helper
/// call inside the client member, and the caller that reads the response.
const ORDER_SITES: [&str; 2] = ["src/checkout.ts:4", "src/orders-client.ts:14"];

fn assert_order_type_mismatch(payload: &serde_json::Value) {
    for site in ORDER_SITES {
        let finding = seed_finding(payload, "type_mismatch", "GET", ORDER_PATH, Some(site));
        assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
    }
    assert_delta(payload, &[], &[]);
}

// ---- HTTP --------------------------------------------------------------

#[tokio::test]
async fn seed_http_field_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            ORDER_ROUTE,
            &[
                ("total: number;", "amount: number;"),
                ("total: 1200", "amount: 1200"),
            ],
        )
    })
    .await;
    assert_order_type_mismatch(&payload);
}

#[tokio::test]
async fn seed_http_field_removal() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            ORDER_ROUTE,
            &[("  currency: string;\n", ""), (", currency: \"EUR\"", "")],
        )
    })
    .await;
    assert_order_type_mismatch(&payload);
}

#[tokio::test]
async fn seed_http_verb_change() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            ORDER_ROUTE,
            &[(
                "export async function loader",
                "export async function action",
            )],
        )
    })
    .await;
    let finding = seed_finding(&payload, "method_mismatch", "GET", ORDER_PATH, None);
    assert_eq!(finding["expected_method"], "POST", "{finding:#}");
    assert_eq!(
        finding["call_sites"],
        serde_json::json!(ORDER_SITES),
        "{finding:#}"
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("not_checked"));
    assert_delta(
        &payload,
        &["POST /api/v1/orders/:orderId"],
        &["GET /api/v1/orders/:orderId"],
    );
}

async fn http_route_rename() -> serde_json::Value {
    seed_payload("orders-api", |repo| {
        git(
            repo,
            &["mv", ORDER_ROUTE, "app/routes/api.v1.purchases.$orderId.ts"],
        )
    })
    .await
}

#[tokio::test]
async fn seed_http_route_rename() {
    let _serial = SERIAL.lock().await;
    let payload = http_route_rename().await;
    let finding = seed_finding(
        &payload,
        "missing_endpoint",
        "GET",
        "/api/v1/orders/:encoded",
        None,
    );
    assert_eq!(
        finding["call_sites"],
        serde_json::json!(ORDER_SITES),
        "{finding:#}"
    );
    assert_delta(
        &payload,
        &["GET /api/v1/purchases/:orderId"],
        &["GET /api/v1/orders/:orderId"],
    );
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_http_route_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

// ---- GraphQL -----------------------------------------------------------

const ORDER_QUERY_SITE: &str = "src/order-query.ts:9";

fn graphql_field_rename(repo: &Path) {
    edit(
        repo,
        "src/schema.graphql",
        &[("  total: Int!", "  amount: Int!")],
    );
    edit(
        repo,
        "src/resolvers.ts",
        &[("  total: number;", "  amount: number;")],
    );
}

/// The removed line sits above the resolver, so the model's answer for the
/// file moves its resolver line with it.
fn graphql_field_removal(repo: &Path) {
    edit(repo, "src/schema.graphql", &[("  currency: String!\n", "")]);
    edit(repo, "src/resolvers.ts", &[("  currency: string;\n", "")]);
    edit(
        repo,
        "__llm__/analyze-file/resolvers.json",
        &[("\"resolver_line\": 11", "\"resolver_line\": 10")],
    );
}

/// What holds today: the break is found at the consumer, as a PR's, resolved.
fn assert_graphql_found(payload: &serde_json::Value) {
    let finding = seed_finding_at(payload, "type_mismatch", ORDER_QUERY_SITE);
    assert_eq!(finding["on_main"], false, "{finding:#}");
    assert_eq!(finding["verdict_state"], "resolved", "{finding:#}");
    assert_delta(payload, &[], &[]);
}

fn assert_graphql_marked(payload: &serde_json::Value) {
    let finding = seed_finding(
        payload,
        "type_mismatch",
        "QUERY",
        "order",
        Some(ORDER_QUERY_SITE),
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
}

#[tokio::test]
async fn seed_graphql_field_rename() {
    let _serial = SERIAL.lock().await;
    assert_graphql_found(&seed_payload("orders-api", graphql_field_rename).await);
}

#[tokio::test]
#[ignore = "carrick#3090: the label is a join pseudo-label (GRAPHQL query|order); carrick#3091: no edge_source"]
async fn seed_graphql_field_rename_marking() {
    let _serial = SERIAL.lock().await;
    assert_graphql_marked(&seed_payload("orders-api", graphql_field_rename).await);
}

#[tokio::test]
async fn seed_graphql_field_removal() {
    let _serial = SERIAL.lock().await;
    assert_graphql_found(&seed_payload("orders-api", graphql_field_removal).await);
}

#[tokio::test]
#[ignore = "carrick#3090: the label is a join pseudo-label (GRAPHQL query|order); carrick#3091: no edge_source"]
async fn seed_graphql_field_removal_marking() {
    let _serial = SERIAL.lock().await;
    assert_graphql_marked(&seed_payload("orders-api", graphql_field_removal).await);
}

#[tokio::test]
async fn seed_graphql_root_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/schema.graphql",
            &[("  order(id: ID!)", "  orderById(id: ID!)")],
        );
        edit(
            repo,
            "src/resolvers.ts",
            &[("    order: (_parent", "    orderById: (_parent")],
        );
        edit(
            repo,
            "__llm__/analyze-file/resolvers.json",
            &[
                ("\"field\": \"order\"", "\"field\": \"orderById\""),
                (
                    "\"resolver_function\": \"order\"",
                    "\"resolver_function\": \"orderById\"",
                ),
            ],
        );
    })
    .await;
    seed_finding(
        &payload,
        "missing_endpoint",
        "QUERY",
        "order",
        Some(ORDER_QUERY_SITE),
    );
    seed_finding(&payload, "orphaned_endpoint", "QUERY", "orderById", None);
    assert_delta(&payload, &["QUERY orderById"], &["QUERY order"]);
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_graphql_root_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

// ---- Socket events -----------------------------------------------------

async fn socket_field_rename() -> serde_json::Value {
    seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/realtime.ts",
            &[("  status: string;", "  state: string;")],
        )
    })
    .await
}

#[tokio::test]
async fn seed_socket_field_rename() {
    let _serial = SERIAL.lock().await;
    let payload = socket_field_rename().await;
    let finding = seed_finding_at(&payload, "type_mismatch", "src/live.ts:12");
    assert_eq!(finding["on_main"], false, "{finding:#}");
    assert_eq!(finding["verdict_state"], "resolved", "{finding:#}");
    assert_delta(&payload, &[], &[]);
}

#[tokio::test]
#[ignore = "carrick#3090: the label is a join pseudo-label (SOCKET CLIENT->SERVER|order:update); carrick#3091: no edge_source"]
async fn seed_socket_field_rename_marking() {
    let _serial = SERIAL.lock().await;
    let payload = socket_field_rename().await;
    let finding = seed_finding(
        &payload,
        "type_mismatch",
        "CLIENT->SERVER",
        "order:update",
        Some("src/live.ts:12"),
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
}

/// The emit's payload is an inline object literal the emitter's own file
/// states; dropping `carrier` from it breaks the listener in `storefront`.
#[tokio::test]
#[ignore = "carrick#3094: the emitter's inline payload reads unknown, so the pair is unverifiable; then carrick#3090 (labels) and carrick#3091 (edge_source)"]
async fn seed_socket_field_removal() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/realtime.ts",
            &[(
                "{ orderId: update.orderId, carrier: \"post\" }",
                "{ orderId: update.orderId }",
            )],
        )
    })
    .await;
    let finding = seed_finding(
        &payload,
        "type_mismatch",
        "SERVER->CLIENT",
        "order:shipped",
        Some("src/realtime.ts:12"),
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
}

#[tokio::test]
async fn seed_socket_listener_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/realtime.ts",
            &[("socket.on(\"order:update\"", "socket.on(\"order:changed\"")],
        )
    })
    .await;
    seed_finding(
        &payload,
        "missing_endpoint",
        "CLIENT->SERVER",
        "order:update",
        Some("src/live.ts:12"),
    );
    assert_delta(
        &payload,
        &["CLIENT->SERVER order:changed"],
        &["CLIENT->SERVER order:update"],
    );
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_socket_listener_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

/// The emit is a call, so the delta, which reads endpoints, shows nothing.
#[tokio::test]
async fn seed_socket_emit_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/realtime.ts",
            &[("io.emit(\"order:shipped\"", "io.emit(\"order:dispatched\"")],
        )
    })
    .await;
    seed_finding(
        &payload,
        "missing_endpoint",
        "SERVER->CLIENT",
        "order:dispatched",
        Some("src/realtime.ts:12"),
    );
    seed_finding(
        &payload,
        "orphaned_endpoint",
        "SERVER->CLIENT",
        "order:shipped",
        None,
    );
    assert_delta(&payload, &[], &[]);
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_socket_emit_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

// ---- Pub/sub -----------------------------------------------------------

#[tokio::test]
#[ignore = "carrick#3089: library-stated pub/sub rows have no type manifest entry, so both pairs are not_compared"]
async fn seed_pubsub_field_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/events.ts",
            &[
                ("  amount: number;", "  total: number;"),
                ("event.amount", "event.total"),
            ],
        )
    })
    .await;
    let finding = seed_finding(
        &payload,
        "type_mismatch",
        "PUBSUB",
        "payments.settled",
        Some("src/payments.ts:10"),
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
}

#[tokio::test]
#[ignore = "carrick#3089: library-stated pub/sub rows have no type manifest entry, so both pairs are not_compared"]
async fn seed_pubsub_field_removal() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/events.ts",
            &[("{ orderId, carrier: \"post\" }", "{ orderId }")],
        );
        edit(
            repo,
            "__llm__/analyze-file/events.json",
            &[(r#""{ orderId, carrier: \"post\" }""#, r#""{ orderId }""#)],
        );
    })
    .await;
    let finding = seed_finding(
        &payload,
        "type_mismatch",
        "PUBSUB",
        "orders.shipped",
        Some("src/events.ts:15"),
    );
    assert_marks(&finding, Some(false), Some("fact"), Some("resolved"));
}

#[tokio::test]
async fn seed_pubsub_subscriber_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/events.ts",
            &[(
                "bus.subscribe(\"payments.settled\"",
                "bus.subscribe(\"payments.cleared\"",
            )],
        );
        edit(
            repo,
            "__llm__/analyze-file/events.json",
            &[(
                "\"topic\": \"payments.settled\"",
                "\"topic\": \"payments.cleared\"",
            )],
        );
    })
    .await;
    seed_finding(
        &payload,
        "missing_endpoint",
        "PUBSUB",
        "payments.settled",
        Some("src/payments.ts:10"),
    );
    assert_delta(
        &payload,
        &["PUBSUB payments.cleared"],
        &["PUBSUB payments.settled"],
    );
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_pubsub_subscriber_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

#[tokio::test]
async fn seed_pubsub_publish_rename() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "src/events.ts",
            &[(
                "bus.publish(\"orders.shipped\"",
                "bus.publish(\"orders.dispatched\"",
            )],
        );
        edit(
            repo,
            "__llm__/analyze-file/events.json",
            &[(
                "\"topic\": \"orders.shipped\"",
                "\"topic\": \"orders.dispatched\"",
            )],
        );
    })
    .await;
    seed_finding(
        &payload,
        "missing_endpoint",
        "PUBSUB",
        "orders.dispatched",
        Some("src/events.ts:15"),
    );
    seed_finding(
        &payload,
        "orphaned_endpoint",
        "PUBSUB",
        "orders.shipped",
        None,
    );
    assert_delta(&payload, &[], &[]);
}

#[test]
#[ignore = "carrick#3092: the rename marking shape is not pinned"]
fn seed_pubsub_publish_rename_marking() {
    panic!("carrick#3092: shape not pinned");
}

// ---- Functions and dependencies ----------------------------------------

/// The copy of `storefront`'s `formatMinor` is a function the PR added. The
/// similarity row is the cloud's (carrick-cloud#2433).
#[tokio::test]
async fn seed_copied_helper() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        let money = "export function formatMinor(amount: number, currency: string): string {\n  const major = Math.trunc(amount / 100);\n  const minor = String(Math.abs(amount % 100)).padStart(2, \"0\");\n  return `${currency} ${major}.${minor}`;\n}\n";
        std::fs::write(repo.join("src/money.ts"), money).unwrap();
    })
    .await;
    assert!(
        changed_functions(&payload).contains(&"src/money.ts:formatMinor:added".to_string()),
        "{payload:#}"
    );
    assert!(findings_of(&payload).is_empty(), "{payload:#}");
}

#[tokio::test]
async fn seed_major_bump() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "package.json",
            &[("\"zod\": \"^3.22.0\"", "\"zod\": \"^4.0.0\"")],
        )
    })
    .await;
    let finding = findings_of(&payload)
        .into_iter()
        .find(|f| f["kind"] == "dependency_conflict" && f["package_name"] == "zod")
        .unwrap_or_else(|| panic!("no dependency_conflict on zod in {payload:#}"));
    assert_eq!(finding["tier"], "major", "{finding:#}");
    assert_eq!(
        finding["changed_by_pr"],
        serde_json::json!([{ "service": "orders-api", "from": "^3.22.0", "to": "^4.0.0" }]),
        "{finding:#}"
    );
    assert_marks(&finding, Some(false), None, None);
    assert_eq!(
        payload["checked"]["dependencies"]["changed"], 1,
        "{payload:#}"
    );
}

// ---- Negatives ---------------------------------------------------------

/// `typescript` moves a major in a `devDependencies` block and `storefront`
/// keeps its own, also a dev dependency: no runtime code shares the package.
#[tokio::test]
#[ignore = "carrick#3093: a dev-only major split is reported as a dependency conflict"]
async fn seed_dev_only_split() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        edit(
            repo,
            "package.json",
            &[("\"typescript\": \"^5.4.0\"", "\"typescript\": \"^6.0.0\"")],
        )
    })
    .await;
    assert_eq!(
        rows_named(&payload, "dependency_conflict", "typescript"),
        0,
        "{payload:#}"
    );
}

/// A respelled route parameter is the same route.
#[tokio::test]
async fn seed_respell_param() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        let renamed = "app/routes/api.v1.orders.$id.ts";
        git(repo, &["mv", ORDER_ROUTE, renamed]);
        edit(
            repo,
            renamed,
            &[
                ("params: { orderId: string }", "params: { id: string }"),
                ("params.orderId", "params.id"),
            ],
        );
    })
    .await;
    assert_delta(&payload, &[], &[]);
    assert!(findings_of(&payload).is_empty(), "{payload:#}");
}

/// A route module copied under a new name is a real new route and a new
/// function. The reuse row it would earn is the cloud's (carrick-cloud#2433).
#[tokio::test]
async fn seed_convention_copy() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("orders-api", |repo| {
        let copy = "app/routes/api.v1.invoices.$invoiceId.ts";
        std::fs::copy(repo.join(ORDER_ROUTE), repo.join(copy)).unwrap();
        edit(repo, copy, &[("orderId", "invoiceId")]);
    })
    .await;
    assert!(
        changed_functions(&payload)
            .contains(&"app/routes/api.v1.invoices.$invoiceId.ts:loader:added".to_string()),
        "{payload:#}"
    );
    assert_delta(&payload, &["GET /api/v1/invoices/:invoiceId"], &[]);
    assert!(
        findings_of(&payload)
            .iter()
            .all(|f| f["kind"] == "orphaned_endpoint"),
        "{payload:#}"
    );
}

/// Registrations that swap places keep their keys, so the delta lists none.
#[tokio::test]
async fn seed_route_moved_within_its_file() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("admin-api", |repo| {
        edit(
            repo,
            "src/admin.ts",
            &[(
                "app.get(\"/api/v1/admin/orders\", listOrders);\napp.post(\"/api/v1/admin/orders/:orderId/cancel\", cancelOrder);",
                "// Cancelling comes first now.\napp.post(\"/api/v1/admin/orders/:orderId/cancel\", cancelOrder);\n\n// The list.\napp.get(\"/api/v1/admin/orders\", listOrders);",
            )],
        );
        // The model's answer moves its lines with the registrations.
        edit(
            repo,
            "__llm__/analyze-file/admin.json",
            &[
                ("\"candidate_id\": \"@line:16\",\n      \"line_number\": 16,", "\"candidate_id\": \"@line:20\",\n      \"line_number\": 20,"),
                (
                    "\"candidate_id\": \"@line:20\",\n      \"line_number\": 20,\n      \"owner_node\": \"app\",\n      \"method\": \"GET\",\n      \"path\": \"/api/v1/admin/reports",
                    "\"candidate_id\": \"@line:23\",\n      \"line_number\": 23,\n      \"owner_node\": \"app\",\n      \"method\": \"GET\",\n      \"path\": \"/api/v1/admin/reports",
                ),
            ],
        );
    })
    .await;
    assert_delta(&payload, &[], &[]);
}

/// A loop's registration is one model row with template text; editing inside
/// the loop keeps its key (carrick#2093).
#[tokio::test]
async fn seed_loop_route_edit() {
    let _serial = SERIAL.lock().await;
    let payload = seed_payload("admin-api", |repo| {
        edit(
            repo,
            "src/admin.ts",
            &[(
                "  app.get(`/api/v1/admin/reports/${period}`, report);",
                "  // One report route per period.\n  app.get(`/api/v1/admin/reports/${period}`, (_request, response) => response.send({ rows: [], period }));",
            )],
        );
        edit(
            repo,
            "__llm__/analyze-file/admin.json",
            &[("\"candidate_id\": \"@line:20\",\n      \"line_number\": 20,", "\"candidate_id\": \"@line:21\",\n      \"line_number\": 21,")],
        );
    })
    .await;
    assert_delta(&payload, &[], &[]);
}

/// carrick#3082: a PR that adds a table of `{ method, path }` bookkeeping
/// records adds no endpoint, so the delta lists none of its paths.
#[tokio::test]
async fn a_pr_adding_a_table_of_route_records_lists_no_new_endpoint() {
    let _serial = SERIAL.lock().await;
    let (_tmp, producer, _consumer) = setup();

    let store = Store::default();
    scan(&store, &producer).await;
    std::fs::write(
        producer.join("app/fixture-data.js"),
        r#"const RECORDS = [
  { method: "GET", path: "/shipments/:id", line: 12, source: "fixture" },
  { method: "POST", path: "/shipments", line: 20, source: "fixture" },
];

export function describeRecords() {
  return RECORDS.map((record) => `${record.method} ${record.path}`);
}
"#,
    )
    .unwrap();
    commit(&producer, "pr: add a table of route records");

    let delta = pr_delta(&store, &producer).await;
    let new_paths: Vec<String> = delta["new_endpoints"]
        .as_array()
        .map(|refs| {
            refs.iter()
                .map(|r| r["path"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !new_paths.iter().any(|path| path.contains("/shipments")),
        "a table of records is not a route: {delta:#}"
    );
}

/// Run the built binary over `repo` as a main scan does, or as a
/// `pull_request` run when `pr` is set, offline, writing to the local store
/// `store`. Returns the run's output.
fn binary_scan(repo: &Path, store: &Path, pr: bool) -> String {
    std::fs::create_dir_all(store).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(repo)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", store);
    if pr {
        cmd.env("GITHUB_EVENT_NAME", "pull_request")
            .env("GITHUB_REF", "refs/pull/7/merge")
            .env("CARRICK_PR_RESULT_OUT", store.join("pr-result.json"));
    } else {
        cmd.env_remove("GITHUB_EVENT_NAME").env_remove("GITHUB_REF");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_EVENT_PATH",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let run = cmd.output().expect("failed to spawn carrick");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.status.success(), "{output}");
    output
}

/// The stored blob of a single-service repo named `name`, as JSON.
fn stored_blob(store: &Path, name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(store.join(format!("{name}.json")))
        .unwrap_or_else(|error| panic!("no blob for {name} ({error})"));
    serde_json::from_str(&text).unwrap()
}

/// carrick#3101: a main scan stores the service's type-reach record beside
/// its blob, and the blob names it. The record holds the import graph of the
/// program that typed the service, with paths relative to the repo root, and
/// each alias the type stage asked about with the file of its request.
#[tokio::test]
async fn a_main_scan_stores_the_type_reach_record_of_a_typed_service() {
    use sha2::Digest;
    use std::io::Read;

    let _serial = SERIAL.lock().await;
    let (tmp, _producer, consumer) = setup();
    let store = tmp.path().join("main-store");
    let output = binary_scan(&consumer, &store, false);

    let blob = stored_blob(&store, "inventory-svc");
    let reference = blob
        .get("type_reach")
        .unwrap_or_else(|| panic!("the blob names no type-reach record:\n{output}"));
    let object = std::fs::read(store.join("inventory-svc.type-reach.json.gz"))
        .unwrap_or_else(|error| panic!("no type-reach object ({error}):\n{output}"));
    // The blob names exactly the bytes stored.
    assert_eq!(reference["bytes"], serde_json::json!(object.len()));
    assert_eq!(
        reference["sha256"],
        serde_json::json!(format!("{:x}", sha2::Sha256::digest(&object)))
    );
    eprintln!("type-reach record: {} bytes gzipped", object.len());

    let mut json = String::new();
    flate2::read::GzDecoder::new(&object[..])
        .read_to_string(&mut json)
        .unwrap();
    let record: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(record["version"], 1);
    assert!(record.get("scanner_build").is_some(), "{record:#}");

    // The graph names the fixture's source files, relative to the repo.
    let strings: Vec<&str> = record["graph"]["strings"]
        .as_array()
        .unwrap_or_else(|| panic!("a graph with no string table: {record:#}"))
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    for file in ["src/client.ts", "src/inventory.ts", "src/send.ts"] {
        assert!(strings.contains(&file), "{file} missing from {strings:?}");
    }
    assert!(
        !strings.iter().any(|s| s.starts_with('/')),
        "a machine path in the graph: {strings:?}"
    );

    // Each alias maps to the file of the request that carried it, and every
    // alias the manifest typed is there.
    let aliases = record["aliases"].as_object().unwrap();
    let manifest = blob["type_manifest"].as_array().unwrap();
    let typed: Vec<&serde_json::Value> = manifest
        .iter()
        .filter(|entry| entry["type_state"] != "unknown")
        .collect();
    assert!(!typed.is_empty(), "the fixture types nothing: {blob:#}");
    for entry in typed {
        let alias = entry["type_alias"].as_str().unwrap();
        assert_eq!(
            aliases.get(alias),
            Some(&serde_json::json!([entry["file_path"]])),
            "{alias} in {aliases:#?}"
        );
    }
    for alias in aliases.keys() {
        assert!(
            manifest.iter().any(|entry| entry["type_alias"] == **alias),
            "{alias} is not a manifest alias"
        );
    }
}

/// carrick#3101: a pull request run reads main's record and stores none,
/// even where its store keeps what it uploads.
#[tokio::test]
async fn a_pull_request_scan_stores_no_type_reach_record() {
    let _serial = SERIAL.lock().await;
    let (tmp, _producer, consumer) = setup();
    let store = tmp.path().join("pr-store");
    let output = binary_scan(&consumer, &store, true);

    assert!(
        !store.join("inventory-svc.type-reach.json.gz").exists(),
        "a PR run stored a type-reach record:\n{output}"
    );
    let blob = stored_blob(&store, "inventory-svc");
    assert!(blob.get("type_reach").is_none(), "{blob:#}");
}

// ---- Type reach (carrick#3102) -------------------------------------------
//
// A PR run decides, per service, which manifest aliases its change can reach
// and dumps the decision (`CARRICK_TYPE_REACH_DUMP`). A service no alias of
// which is reached takes main's types and sends the sidecar no type request
// (carrick#3103); the payload is the same with the decision off, which
// `seed_payload` asserts for every seed.

/// Sets one environment variable for its lifetime.
struct EnvVar(&'static str, Option<String>);

impl EnvVar {
    fn set(name: &'static str, value: &str) -> Self {
        let before = std::env::var(name).ok();
        // SAFETY: the tests in this binary run one at a time (SERIAL).
        unsafe { std::env::set_var(name, value) };
        EnvVar(name, before)
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        // SAFETY: the tests in this binary run one at a time (SERIAL).
        unsafe {
            match &self.1 {
                Some(value) => std::env::set_var(self.0, value),
                None => std::env::remove_var(self.0),
            }
        }
    }
}

/// The PR run's payload and `orders-api`'s dumped decision, for `change`.
async fn reach_of(change: impl Fn(&Path)) -> (serde_json::Value, serde_json::Value) {
    let dump = tempfile::tempdir().unwrap();
    let _dump = EnvVar::set("CARRICK_TYPE_REACH_DUMP", &dump.path().to_string_lossy());
    let payload = seed_payload("orders-api", change).await;
    let file = dump.path().join("orders-api.json");
    let decided = serde_json::from_str(
        &std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("no dump at {}: {e}", file.display())),
    )
    .unwrap();
    (payload, decided)
}

/// The reason of a whole decision.
fn whole_reason(dump: &serde_json::Value) -> &str {
    let decided = &dump["decided"];
    assert_eq!(decided["decision"], "whole", "{dump:#}");
    decided["reason"].as_str().unwrap()
}

/// The protocol of each alias of a partial decision, split by whether the
/// change reached it.
fn reached_protocols(dump: &serde_json::Value) -> (BTreeSet<String>, BTreeSet<String>) {
    let decided = &dump["decided"];
    assert_eq!(decided["decision"], "partial", "{dump:#}");
    let (mut reached, mut unreached) = (BTreeSet::new(), BTreeSet::new());
    for alias in decided["aliases"].as_array().unwrap() {
        let protocol = alias["protocol"].as_str().unwrap().to_string();
        if alias["reached"] == true {
            reached.insert(protocol);
        } else {
            unreached.insert(protocol);
        }
    }
    (reached, unreached)
}

/// A partial decision reached `protocol`'s aliases, no other's, and left
/// some alias unreached.
fn assert_reached_only(dump: &serde_json::Value, protocol: &str) {
    let (reached, unreached) = reached_protocols(dump);
    assert_eq!(
        reached,
        BTreeSet::from([protocol.to_string()]),
        "reached: {dump:#}"
    );
    assert!(!unreached.is_empty(), "nothing left unreached: {dump:#}");
}

fn edit_contract(repo: &Path) {
    edit(
        repo,
        "src/contracts/order-ref.ts",
        &[("  id: string;", "  id: string;\n  region?: string;")],
    );
}

#[tokio::test]
async fn reach_a_contract_edit_reaches_only_its_routes_aliases() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(edit_contract).await;
    assert_reached_only(&dump, "http");
}

#[tokio::test]
async fn reach_a_resolver_edit_reaches_only_graphql_aliases() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "src/resolvers.ts",
            &[(
                "new Map<string, OrderView>();",
                "new Map<string, OrderView>([]);",
            )],
        )
    })
    .await;
    assert_reached_only(&dump, "graphql");
}

#[tokio::test]
async fn reach_an_edit_to_the_socket_file_reaches_only_socket_aliases() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "src/realtime.ts",
            &[("new Server(4000)", "new Server(4001)")],
        )
    })
    .await;
    assert_reached_only(&dump, "websocket");
}

#[tokio::test]
async fn reach_an_edit_to_the_publisher_reaches_only_pubsub_aliases() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "src/events.ts",
            &[("void event.amount;", "void event.orderId;")],
        )
    })
    .await;
    // Library-stated pub/sub rows carry no manifest entry yet
    // (carrick#3089), so the service has no pub/sub alias to reach: the
    // publisher's file is reached and no other protocol's alias is.
    let (reached, _) = reached_protocols(&dump);
    assert!(
        reached.iter().all(|protocol| protocol == "pubsub"),
        "{dump:#}"
    );
    assert!(
        dump["decided"]["reached_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == "src/events.ts"),
        "{dump:#}"
    );
}

#[tokio::test]
async fn reach_an_augmentation_edit_is_whole() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "src/augment.ts",
            &[("channel?: string;", "channel?: number;")],
        )
    })
    .await;
    assert_eq!(whole_reason(&dump), "augmentation");
}

#[tokio::test]
async fn reach_a_schema_edit_is_whole() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "src/schema.graphql",
            &[("currency: String!", "currency: String")],
        )
    })
    .await;
    assert_eq!(whole_reason(&dump), "repo_input");
}

#[tokio::test]
async fn reach_a_tsconfig_edit_is_whole() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "tsconfig.json",
            &[("\"target\": \"es2020\"", "\"target\": \"es2021\"")],
        )
    })
    .await;
    assert_eq!(whole_reason(&dump), "config_file");
}

#[tokio::test]
async fn reach_off_is_whole_and_changes_no_payload() {
    let _serial = SERIAL.lock().await;
    let (on, dump) = reach_of(edit_contract).await;
    assert_eq!(dump["decided"]["decision"], "partial", "{dump:#}");
    let _off = EnvVar::set("CARRICK_TYPE_REACH", "off");
    let (off, dump) = reach_of(edit_contract).await;
    assert_eq!(whole_reason(&dump), "off");
    assert_eq!(on, off);
}

/// An edit no alias's types read: `src/legacy.js` is outside the program.
fn edit_legacy(repo: &Path) {
    edit(
        repo,
        "src/legacy.js",
        &[("\"order \" + id", "\"order #\" + id")],
    );
}

/// carrick#3103: a service no alias of which is reached takes main's types.
/// The dump's `kept_main_types` is written by the engine where it skips the
/// type stage, so it reads what the run did; `seed_payload` has already
/// asserted the posted result equals the one with the decision off.
#[tokio::test]
async fn reach_a_service_no_change_reaches_takes_mains_types() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(edit_legacy).await;
    let (reached, unreached) = reached_protocols(&dump);
    assert!(reached.is_empty(), "{dump:#}");
    assert!(!unreached.is_empty(), "{dump:#}");
    let aliases = dump["decided"]["aliases"].as_array().unwrap().len();
    assert_eq!(dump["kept_main_types"], aliases, "{dump:#}");
}

#[tokio::test]
async fn reach_a_service_with_a_reached_alias_is_retyped() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(edit_contract).await;
    assert!(dump.get("kept_main_types").is_none(), "{dump:#}");
}

#[tokio::test]
async fn reach_off_takes_no_types_from_main() {
    let _serial = SERIAL.lock().await;
    let _off = EnvVar::set("CARRICK_TYPE_REACH", "off");
    let (_, dump) = reach_of(edit_legacy).await;
    assert!(dump.get("kept_main_types").is_none(), "{dump:#}");
}

/// What `npm version` rewrites: the manifest's and the lockfile's own
/// `version` (the lockfile names it at the top and under `packages[""]`).
fn bump_own_version(repo: &Path) {
    edit(
        repo,
        "package.json",
        &[("\"version\": \"1.0.0\"", "\"version\": \"1.0.1\"")],
    );
    edit(
        repo,
        "package-lock.json",
        &[("\"version\": \"1.0.0\"", "\"version\": \"1.0.1\"")],
    );
}

/// carrick#3126: a release bump moves no package, so it re-types nothing a
/// source edit outside the program does not reach, and asks no setup again
/// (carrick#2568).
#[tokio::test]
async fn reach_a_version_bump_keeps_reach() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        bump_own_version(repo);
        edit_legacy(repo);
    })
    .await;
    let (reached, unreached) = reached_protocols(&dump);
    assert!(reached.is_empty(), "{dump:#}");
    assert!(!unreached.is_empty(), "{dump:#}");
    let aliases = dump["decided"]["aliases"].as_array().unwrap().len();
    assert_eq!(dump["kept_main_types"], aliases, "{dump:#}");
}

/// A bump that also adds a dependency moves a package: whole. The reason is
/// not pinned: the manifest hash moves, so the setup is asked again, and
/// `setup_differs` is read before `repo_input`.
#[tokio::test]
async fn reach_a_version_bump_with_a_new_dependency_stays_whole() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        bump_own_version(repo);
        edit(
            repo,
            "package.json",
            &[(
                "\"zod\": \"^3.22.0\"",
                "\"zod\": \"^3.22.0\",\n    \"left-pad\": \"^1.3.0\"",
            )],
        );
        edit_legacy(repo);
    })
    .await;
    assert_eq!(dump["decided"]["decision"], "whole", "{dump:#}");
    assert!(dump.get("kept_main_types").is_none(), "{dump:#}");
}

/// A bump of a package the service loads stays whole: this harness runs no
/// scope plan, so only the move itself is read.
#[tokio::test]
async fn reach_a_lockfile_move_of_a_loaded_package_stays_whole() {
    let _serial = SERIAL.lock().await;
    let (_, dump) = reach_of(|repo| {
        edit(
            repo,
            "package-lock.json",
            &[("\"version\": \"2.4.1\"", "\"version\": \"2.4.2\"")],
        );
        edit_legacy(repo);
    })
    .await;
    assert_eq!(whole_reason(&dump), "repo_input");
    assert_eq!(dump["decided"]["whole_by"], "package-lock.json", "{dump:#}");
}
