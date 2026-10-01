//! carrick#1616 $0 slice (PROTOTYPE, not for merge): calls through broker and
//! socket libraries read through library claims in the one shape every
//! protocol shares, end to end through the engine.
//!
//! Framework detection is replayed from a hand-written cassette carrying
//! `library_claims` for four invented packages (`tests/fixtures/library-claims`):
//! a task SDK (a definition maker with a name key and a handler key, instance
//! sends, export sends with the name at argument 0), a key-value store's
//! pub/sub client, a socket library with a client maker and a server maker,
//! and an emitter that only inherits the runtime's `emit` and `on`.
//!
//! The reader's own rules are tested with every claim read as verified
//! (`CARRICK_SLICE_ASSUME_VERIFIED`), so what the reader states and what it
//! refuses does not depend on the verifier. What the verifier refuses (the
//! inherited emitter, the options bag with two string keys) is tested with the
//! real sidecar. Mock mode supplies no claims of its own: every claim here is
//! the cassette's.
//!
//! Every test is `#[serial]`: the mock environment is process-global.

use async_trait::async_trait;
use carrick::agents::file_analyzer_agent::ResolutionSource;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::services::type_sidecar::TypeSidecar;
use serial_test::serial;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        Ok((self.repos.lock().unwrap().clone(), HashMap::new()))
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

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
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

/// A committed copy of fixture `name`, and its cassette directory.
fn fixture_copy(tmp: &Path, name: &str) -> (PathBuf, PathBuf) {
    let repo = tmp.join("service");
    copy_dir(&fixture(name), &repo);
    let _ = std::fs::remove_dir_all(repo.join("variants"));
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassette = repo.join("__llm__");
    (repo, cassette)
}

fn mock_env(cassette: &Path, assume_verified: bool) {
    // SAFETY: every test in this binary is `#[serial]`.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("CARRICK_OUTPUT_JSON");
        std::env::set_var(carrick::client_semantics::PENDING_REASK_WAITS_ENV, "0,0");
        if assume_verified {
            std::env::set_var(carrick::library_claims::ASSUME_VERIFIED_ENV, "1");
        } else {
            std::env::remove_var(carrick::library_claims::ASSUME_VERIFIED_ENV);
        }
    }
}

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

/// A protocol row as `<side> <file>:<line> <key>`, the file relative to the
/// fixture's `src/`.
fn row(side: &str, file_path: &Path, key: &serde_json::Value) -> String {
    let location = file_path.display().to_string();
    let location = location
        .rsplit("src/")
        .next()
        .unwrap_or(&location)
        .to_string();
    let key = match key["protocol"].as_str() {
        Some("pubsub") => format!("pubsub|{}", key["topic"].as_str().unwrap()),
        Some("socket") => format!(
            "socket|{}|{}",
            key["direction"].as_str().unwrap(),
            key["event"].as_str().unwrap()
        ),
        Some("http") => format!(
            "http|{} {}",
            key["method"].as_str().unwrap(),
            key["path"].as_str().unwrap()
        ),
        _ => key.to_string(),
    };
    format!("{side} {location} {key}")
}

/// Every operation row the blob holds whose source is `source`.
fn rows_from(data: &CloudRepoData, source: Option<ResolutionSource>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (side, list) in [("producer", &data.endpoints), ("consumer", &data.calls)] {
        for endpoint in list {
            if endpoint.resolution_source == source {
                let key = serde_json::to_value(&endpoint.key).unwrap();
                out.insert(row(side, &endpoint.file_path, &key));
            }
        }
    }
    out
}

fn set(rows: &[&str]) -> BTreeSet<String> {
    rows.iter().map(|row| row.to_string()).collect()
}

/// What the reader states with every claim read as verified: each positive
/// in the fixture, and nothing at a site the source does not pin.
#[tokio::test]
#[serial]
async fn the_reader_states_each_positive_and_refuses_each_negative() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), "library-claims");
    mock_env(&cassette, true);
    let data = scan(&repo, None).await;

    assert_eq!(
        rows_from(&data, Some(ResolutionSource::LibraryClaim)),
        set(&[
            // Definitions: the maker's name key, at the maker call.
            "producer tasks.ts:3 pubsub|send-email",
            "producer tasks.ts:12 pubsub|child-task",
            "producer tasks.ts:17 pubsub|parent-task",
            // Two string keys: stated only because every claim is read as
            // verified here; the verifier refuses it
            // (`the_verifier_refuses_what_the_types_cannot_pin`).
            "producer tasks.ts:26 pubsub|nightly-report",
            // Instance sends take the name their maker bound, in this
            // module and in another one.
            "consumer tasks.ts:20 pubsub|child-task",
            "consumer tasks.ts:21 pubsub|send-email",
            "consumer routes.ts:6 pubsub|send-email",
            // Export sends with the name at argument 0, a literal or a
            // module constant read in the module's own scope.
            "consumer routes.ts:5 pubsub|send-email",
            "consumer routes.ts:22 pubsub|parent-task",
            // The key-value client, constructed.
            "consumer kv.ts:7 pubsub|orders.created",
            "producer kv.ts:11 pubsub|orders.created",
            // The emitter: stated only because every claim is read as
            // verified here.
            "producer bus.ts:6 pubsub|cache.flushed",
            "consumer bus.ts:7 pubsub|cache.flushed",
            // Socket rows: the side and the op give the direction.
            "producer socket-client.ts:7 socket|server_to_client|order:accepted",
            "consumer socket-client.ts:8 socket|client_to_server|order:place",
            "producer socket-server.ts:6 socket|client_to_server|order:place",
            "consumer socket-server.ts:7 socket|server_to_client|order:accepted",
        ]),
        "routes.ts:10/11 (computed names), routes.ts:18 (a local shadows the constant), \
         kv.ts:12 (a wildcard), kv.ts:16 (a parameter shadows the constant), kv-cache.ts:9 \
         (a call no claim names on the same instance), socket-client.ts:6 (a name the library \
         emits itself), tasks.ts:36 (an id the source does not state), mocks/ and negatives.ts \
         state nothing"
    );

    // The model's routes at exactly a stated definition's span are
    // withdrawn; the one at the definition the reader could not state stands.
    let routes: BTreeSet<String> = rows_from(&data, Some(ResolutionSource::Model))
        .into_iter()
        .filter(|row| row.contains("http|"))
        .collect();
    assert_eq!(
        routes,
        set(&["producer tasks.ts:36 http|POST /fallback-task"])
    );

    // The model's pub/sub rows at a stated site fold into the library row;
    // the ones at any other line stand.
    let model_pubsub: BTreeSet<String> = rows_from(&data, None)
        .into_iter()
        .filter(|row| row.contains("routes.ts"))
        .collect();
    assert_eq!(
        model_pubsub,
        set(&[
            "consumer routes.ts:9 pubsub|send-email",
            "consumer routes.ts:18 pubsub|not-a-task",
        ])
    );
}

/// What the real verifier refuses: an emitter whose `emit` and `on` are only
/// the runtime's, and a maker whose options carry a second string key the
/// claim leaves unassigned. Neither states a row, and the model's route at
/// the refused definition stands (the withdrawal fails closed).
#[tokio::test]
#[serial]
async fn the_verifier_refuses_what_the_types_cannot_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), "library-claims");
    mock_env(&cassette, false);
    let sidecar = real_sidecar(&repo);
    let data = scan(&repo, Some(&sidecar)).await;
    let rows = rows_from(&data, Some(ResolutionSource::LibraryClaim));

    for refused in [
        "producer bus.ts:6 pubsub|cache.flushed",
        "consumer bus.ts:7 pubsub|cache.flushed",
        "producer tasks.ts:26 pubsub|nightly-report",
    ] {
        assert!(
            !rows.contains(refused),
            "{refused} must not verify: {rows:#?}"
        );
    }
    for stated in [
        "producer tasks.ts:3 pubsub|send-email",
        "consumer routes.ts:5 pubsub|send-email",
        "consumer routes.ts:6 pubsub|send-email",
        "consumer kv.ts:7 pubsub|orders.created",
        "producer kv.ts:11 pubsub|orders.created",
        "producer socket-client.ts:7 socket|server_to_client|order:accepted",
        "consumer socket-server.ts:7 socket|server_to_client|order:accepted",
    ] {
        assert!(rows.contains(stated), "{stated} verifies: {rows:#?}");
    }
    let routes: BTreeSet<String> = rows_from(&data, Some(ResolutionSource::Model))
        .into_iter()
        .filter(|row| row.contains("http|"))
        .collect();
    assert!(
        routes.contains("producer tasks.ts:26 http|POST /nightly-report"),
        "a refused definition withdraws nothing: {routes:#?}"
    );
    assert!(
        !routes.contains("producer tasks.ts:3 http|POST /send-email"),
        "{routes:#?}"
    );
}

/// The HTTP client-semantics fixture's answer, in the shared shape: one
/// `library_claims` entry per package, its HTTP clients as `http_client`
/// exports (the conversion the cloud would make after its normaliser).
fn http_in_shared_shape(client_semantics: &serde_json::Value) -> serde_json::Value {
    let mut out = Vec::new();
    for entry in client_semantics.as_array().unwrap() {
        let mut exports = Vec::new();
        for client in entry["clients"].as_array().unwrap() {
            let mut makes = Vec::new();
            for factory in client["factories"].as_array().unwrap() {
                makes.push(serde_json::json!({
                    "form": "call", "member": factory["member"],
                    "base": { "arg": 0, "key": factory["base_url_key"] }
                }));
            }
            let mut ops = Vec::new();
            for verb in client["verbs"].as_array().unwrap() {
                let mut op = serde_json::json!({
                    "op": "request", "member": verb["member"], "method": verb["method"],
                    "name": { "arg": 0 }, "on": "both"
                });
                match verb["args"].as_str().unwrap() {
                    "path_body" => op["payload"] = serde_json::json!({ "arg": 1 }),
                    _ => {
                        op["options"] = serde_json::json!({ "arg": 1 });
                        if let Some(key) = verb["body_key"].as_str() {
                            op["payload"] = serde_json::json!({ "arg": 1, "key": key });
                        }
                    }
                }
                ops.push(op);
            }
            for request in client["requests"].as_array().unwrap() {
                let config = request["args"] == "config";
                let at = if config { 0 } else { 1 };
                let mut op = serde_json::json!({
                    "op": "request", "member": request["member"],
                    "name": if config {
                        serde_json::json!({ "arg": 0, "key": request["url_key"] })
                    } else {
                        serde_json::json!({ "arg": 0 })
                    },
                    "method_key": { "arg": at, "key": request["method_key"] },
                    "on": "both"
                });
                if let Some(key) = request["body_key"].as_str() {
                    op["payload"] = serde_json::json!({ "arg": at, "key": key });
                }
                ops.push(op);
            }
            exports.push(serde_json::json!({
                "export": client["export"], "role": "http_client", "makes": makes, "ops": ops
            }));
        }
        out.push(serde_json::json!({
            "package": entry["package"], "major": entry["major"], "status": entry["status"],
            "exports": exports
        }));
    }
    serde_json::Value::Array(out)
}

/// HTTP fits the shared shape without loss: the client-semantics fixture,
/// its answer converted into `library_claims`, states exactly the rows it
/// states from `client_semantics`.
#[tokio::test]
#[serial]
async fn http_claims_in_the_shared_shape_state_the_same_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), "client-semantics");
    mock_env(&cassette, false);
    let sidecar = real_sidecar(&repo);
    let rendered = |data: &CloudRepoData| -> Vec<String> {
        let mut rows: Vec<String> = data
            .mount_graph
            .as_ref()
            .expect("the blob carries the graph")
            .data_calls
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect();
        rows.sort();
        rows
    };
    let before = rendered(&scan(&repo, Some(&sidecar)).await);

    let path = cassette.join("framework-detect/framework-detect.json");
    let mut answer: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let semantics = answer
        .as_object_mut()
        .unwrap()
        .remove("client_semantics")
        .expect("the sample answers client semantics");
    answer["library_claims"] = http_in_shared_shape(&semantics);
    std::fs::write(&path, answer.to_string()).unwrap();
    let after = rendered(&scan(&repo, Some(&sidecar)).await);

    assert!(
        before
            .iter()
            .any(|row| row.contains("library_semantics\":[\"")),
        "the fixture states library rows to compare: {before:#?}"
    );
    assert_eq!(before, after);
}
