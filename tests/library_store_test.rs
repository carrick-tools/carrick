//! carrick#1664 end to end: the scan asks the library store about the
//! registry packages its library calls go through, and states a library row
//! wherever the REAL type sidecar verifies the answer against the fixture's
//! vendored declarations.
//!
//! The store's answer is replayed from the fixture's cassette
//! (`__llm__/library-claims/default.json`). Every assertion on a stated row is
//! about what the scanner read through a verified claim; the cassette only
//! says what the package is. See `tests/fixtures/library-store/README.md` for
//! the shape and the answer key.
//!
//! Every test here is `#[serial]`: the mock environment, the injected answers
//! and the request counters are process-global.

use async_trait::async_trait;
use carrick::agents::file_analyzer_agent::ResolutionSource;
use carrick::analyzer::ApiEndpointDetails;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::library_store::ROUTE;
use carrick::services::type_sidecar::TypeSidecar;
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// In-memory storage: an upload replaces the stored row for its repo.
#[derive(Default, Clone)]
struct StubStorage {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
}

#[async_trait]
impl CloudStorage for StubStorage {
    async fn upload_repo_data(
        &self,
        data: &CloudRepoData,
        _final_in_run: bool,
    ) -> Result<UploadOutcome, StorageError> {
        let mut repos = self.repos.lock().unwrap();
        repos.retain(|stored| {
            stored.repo_name != data.repo_name || stored.service_name != data.service_name
        });
        repos.push(data.clone());
        Ok(UploadOutcome::default())
    }
    async fn index_landed(
        &self,
        data: &CloudRepoData,
        _written_after: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, StorageError> {
        Ok(self.repos.lock().unwrap().iter().any(|stored| {
            stored.repo_name == data.repo_name && stored.commit_hash == data.commit_hash
        }))
    }
    async fn download_all_repo_data(
        &self,
    ) -> Result<(Vec<CloudRepoData>, HashMap<String, String>), StorageError> {
        Ok((Vec::new(), HashMap::new()))
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
    async fn upload_logs(&self, _repo: &str, _log_content: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn post_pr_result(
        &self,
        _payload: &carrick::findings::PrResultPayload,
    ) -> Result<(), StorageError> {
        Ok(())
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library-store")
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

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .expect("git failed to spawn");
    assert!(status.success(), "git {args:?} failed");
}

/// A committed copy of the fixture, and the cassette directory to replay
/// the model and the store from.
fn fixture_copy(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = tmp.join("relay");
    copy_dir(&fixture_root(), &repo);
    std::fs::remove_file(repo.join("README.md")).ok();
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassette = repo.join("__llm__");
    (repo, cassette)
}

fn mock_env(cassette: &Path) {
    // SAFETY: every test in this binary is `#[serial]`.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::remove_var("CARRICK_NO_MODEL");
        std::env::remove_var("YARN_NPM_REGISTRY_SERVER");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("CARRICK_OUTPUT_JSON");
        std::env::set_var(carrick::client_semantics::PENDING_REASK_WAITS_ENV, "0,0");
    }
}

/// The real type sidecar, built from `src/sidecar`, initialised on `repo`.
fn real_sidecar(repo: &Path) -> TypeSidecar {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        entry.exists(),
        "build the sidecar first: cd src/sidecar && npm ci && npm run build"
    );
    let sidecar = TypeSidecar::spawn(&entry).expect("the sidecar spawns");
    sidecar.start_init(repo, None);
    sidecar
        .wait_ready(Duration::from_secs(120))
        .expect("the sidecar initialises on the fixture");
    sidecar
}

async fn scan(repo: &Path, sidecar: Option<&TypeSidecar>) -> CloudRepoData {
    let storage = StubStorage::default();
    run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), sidecar, false)
        .await
        .expect("the scan failed");
    storage
        .repos
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the scan uploaded nothing")
}

/// How many requests the store's route has had so far in this process.
fn store_requests() -> usize {
    carrick::agent_service::request_counts()
        .get(ROUTE)
        .copied()
        .unwrap_or(0)
}

/// One row as `(key, file:line, source, claim ids)`.
type Row = (String, String, Option<ResolutionSource>, Vec<String>);

/// Every producer and every call of the blob's message protocols, as
/// [`Row`]s, sorted. The file is relative to the repo.
fn message_rows(data: &CloudRepoData, repo: &Path) -> (Vec<Row>, Vec<Row>) {
    let rows = |rows: &[ApiEndpointDetails]| -> Vec<Row> {
        let mut rows: Vec<Row> = rows
            .iter()
            .filter(|row| !row.key.canonical().starts_with("http|"))
            .map(|row| {
                let file = row.file_path.display().to_string();
                let file = file
                    .strip_prefix(&format!("{}/", repo.display()))
                    .unwrap_or(&file)
                    .to_string();
                (
                    row.key.canonical(),
                    file,
                    row.resolution_source,
                    row.library_semantics.clone(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    (rows(&data.endpoints), rows(&data.calls))
}

fn row(key: &str, at: &str, source: Option<ResolutionSource>, claims: &[&str]) -> Row {
    (
        key.to_string(),
        at.to_string(),
        source,
        claims.iter().map(|id| id.to_string()).collect(),
    )
}

const FACT: Option<ResolutionSource> = Some(ResolutionSource::LibraryClaim);
const MAKE: &str = "@fixture/live@1:Socket:make:new:()";
const EMIT: &str = "@fixture/live@1:Socket:op:send:emit:on:instance";
const ON: &str = "@fixture/live@1:Socket:op:receive:on:on:instance";
const PUBLISH: &str = "@fixture/queue@2:bus:op:send:publish:on:export";
const SUBSCRIBE: &str = "@fixture/queue@2:bus:op:receive:subscribe:on:export";

/// What the scan states with the store's answer: every call through the
/// queue and the socket package is a library row, the socket pass's row at
/// the same site and side folded into it, and the socket package the store
/// skipped keeps the pass's row.
fn rows_with_claims() -> (Vec<Row>, Vec<Row>) {
    (
        vec![
            row(
                "pubsub|orders.created",
                "src/emails.ts:3",
                FACT,
                &[SUBSCRIBE],
            ),
            row(
                "socket|SERVER->CLIENT|chat",
                "src/live.ts:5",
                FACT,
                &[MAKE, ON],
            ),
            row("socket|UNKNOWN|presence", "src/presence.ts:5", None, &[]),
        ],
        vec![
            row("pubsub|orders.created", "src/orders.ts:4", FACT, &[PUBLISH]),
            row(
                "socket|CLIENT->SERVER|join",
                "src/live.ts:14",
                FACT,
                &[MAKE, EMIT],
            ),
            row(
                "socket|CLIENT->SERVER|typing",
                "src/live.ts:10",
                FACT,
                &[MAKE, EMIT],
            ),
        ],
    )
}

/// What the scan states with no claims: the socket pass's rows, each of
/// unknown direction, and the in-process bus pass's subscriber, which the
/// library row folds when there is one.
fn rows_without_claims() -> (Vec<Row>, Vec<Row>) {
    (
        vec![
            row("pubsub|orders.created", "src/emails.ts:3", None, &[]),
            row("socket|UNKNOWN|chat", "src/live.ts:5", None, &[]),
            row("socket|UNKNOWN|presence", "src/presence.ts:5", None, &[]),
        ],
        vec![
            row("socket|UNKNOWN|join", "src/live.ts:14", None, &[]),
            row("socket|UNKNOWN|typing", "src/live.ts:10", None, &[]),
        ],
    )
}

/// Every producer and call of the blob, of every protocol, as JSON: what a
/// change to the library rows must leave alone.
fn all_rows(data: &CloudRepoData) -> Vec<String> {
    let mut rows: Vec<String> = data
        .endpoints
        .iter()
        .chain(&data.calls)
        .map(|row| serde_json::to_string(row).unwrap())
        .collect();
    rows.sort();
    rows
}

/// The store answers the queue and the socket package; the sidecar verifies
/// both. The package installed from a private registry is never sent: a
/// request naming it fails here, and nothing would be stated.
#[tokio::test]
#[serial]
async fn the_store_s_claims_state_library_rows_and_fold_the_socket_pass_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path());
    mock_env(&cassette);
    carrick::agent_service::inject_mock_failure(ROUTE, "fixture-private-bus", 1);
    let sidecar = real_sidecar(&repo);
    let before = store_requests();
    let data = scan(&repo, Some(&sidecar)).await;
    assert_eq!(store_requests() - before, 1, "one ask, nothing pending");
    assert_eq!(message_rows(&data, &repo), rows_with_claims());
}

/// A store with nothing to say states nothing, and a refusal (`403
/// scan_not_started`, as a laptop credential with no scan gets) is exactly
/// the same scan: every row of every protocol is unchanged.
#[tokio::test]
#[serial]
async fn a_refused_store_leaves_every_row_as_no_claims_leave_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path());
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);
    carrick::agent_service::inject_mock_answer(ROUTE, "", 1, r#"{"library_claims":[]}"#);
    let empty = scan(&repo, Some(&sidecar)).await;
    assert_eq!(message_rows(&empty, &repo), rows_without_claims());
    carrick::agent_service::inject_mock_envelope(
        ROUTE,
        "",
        1,
        r#"{"success":false,"error":{"code":"scan_not_started","message":"Start a scan first","retriable":false}}"#,
    );
    let refused = scan(&repo, Some(&sidecar)).await;
    assert_eq!(all_rows(&refused), all_rows(&empty));
}

/// A package the store has not finished is asked once more after the
/// analysis, and its answer then states its rows.
#[tokio::test]
#[serial]
async fn a_pending_package_is_asked_again_and_its_answer_read() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path());
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);
    carrick::agent_service::inject_mock_answer(
        ROUTE,
        "",
        1,
        r#"{"library_claims":[
            {"package":"@fixture/live","version":"1.2.0","status":"pending","reason":"building"},
            {"package":"@fixture/queue","version":"2.4.1","status":"pending","reason":"in_flight"}
        ]}"#,
    );
    let before = store_requests();
    let data = scan(&repo, Some(&sidecar)).await;
    assert_eq!(
        store_requests() - before,
        2,
        "the first ask, then the pending packages"
    );
    assert_eq!(message_rows(&data, &repo), rows_with_claims());
}

/// A run with no model asks the cloud nothing, and neither does a scan with
/// no sidecar to verify an answer with.
#[tokio::test]
#[serial]
async fn no_model_or_no_sidecar_asks_the_store_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path());
    mock_env(&cassette);
    let before = store_requests();
    let unverified = scan(&repo, None).await;
    assert_eq!(store_requests() - before, 0, "no sidecar");
    assert!(
        message_rows(&unverified, &repo)
            .0
            .iter()
            .chain(&message_rows(&unverified, &repo).1)
            .all(|row| row.2 != FACT)
    );
    let sidecar = real_sidecar(&repo);
    // SAFETY: every test in this binary is `#[serial]`.
    unsafe { std::env::set_var("CARRICK_NO_MODEL", "1") };
    let local = scan(&repo, Some(&sidecar)).await;
    unsafe { std::env::remove_var("CARRICK_NO_MODEL") };
    assert_eq!(store_requests() - before, 0, "no model");
    assert!(
        message_rows(&local, &repo)
            .0
            .iter()
            .chain(&message_rows(&local, &repo).1)
            .all(|row| row.2 != FACT)
    );
}
