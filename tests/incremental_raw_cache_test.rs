//! The incremental cache holds the model's RAW answer per file, not the joined
//! result (Phase A of the scanner consolidation).
//!
//! Before this split, an unchanged file replayed the rows a previous scan's
//! deterministic layer had already folded in, so every resolver improvement
//! needed a `CACHE_VERSION` bump and a full re-analysis of every indexed repo.
//! Now the deterministic layer runs on every scan over every discovered file
//! and only the model's reply is cached, so a scanner improvement reaches an
//! indexed repo on the next push with zero model calls.
//!
//! Both tests drive the real engine over a copy of the `env-var-whole-url`
//! fixture with its own cassette replayed for the model, so the model's answer
//! is fixed and any difference between the two scans is the scanner's.
//!
//! They are `#[serial]` and read [`carrick::scan_health::attempted_count`],
//! which is a process-global: two of these running at once would see each
//! other's dispatch counts.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// In-memory storage with no synthetic seed repos and direct access to what
/// was uploaded, so a test can read scan #1's payload back and mutate it
/// before scan #2 picks it up as `previous_data`.
///
/// An upload replaces the stored row for its (repo, service), as the index
/// does, so each scan's previous generation is the scan before it rather than
/// the first one ever stored.
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
    /// Whatever this stub recorded, at the commit it recorded it with.
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
    /// One row per service, as the index stores a monorepo.
    fn supports_multi_service(&self) -> bool {
        true
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

/// A committed copy of the fixture, plus the cassette directory to replay the
/// model from. The copy is committed so the incremental branch has a previous
/// commit to diff against; nothing else is committed afterwards, so HEAD stays
/// equal to scan #1's `commit_hash` and `git diff` reports no changed file.
fn committed_fixture(tmp: &Path) -> (PathBuf, PathBuf) {
    committed_fixture_with(tmp, &[])
}

/// The same, plus files written into the copy BEFORE the commit, so a test can
/// add a source file of its own and still start from a tree `git diff` reports
/// as clean.
fn committed_fixture_with(tmp: &Path, extra: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    committed_copy_of("env-var-whole-url", tmp, extra)
}

/// A committed copy of any fixture that carries its own cassette in `__llm__`.
fn committed_copy_of(name: &str, tmp: &Path, extra: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let repo_path = tmp.join("service");
    copy_dir(&fixture, &repo_path);
    for (relative, contents) in extra {
        let path = repo_path.join(relative);
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    run_git(&repo_path, &["init", "-q"]);
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "init"]);
    let cassette = repo_path.join("__llm__");
    (repo_path, cassette)
}

/// A module the deterministic scan raises no candidate for: no request, no
/// route, no import of anything that performs one.
const NO_CANDIDATES: &str = "export function slugify(value: string): string {\n  \
    return value.trim().toLowerCase().replace(/\\s+/g, \"-\");\n}\n";

/// The same file after a scanner improvement would start seeing a call in it.
const ONE_CANDIDATE: &str = "export async function fetchNotes(): Promise<string> {\n  \
    const res = await fetch(\"http://localhost:9100/api/notes\");\n  \
    return res.text();\n}\n";

fn mock_env(cassette: &Path) {
    // SAFETY: both tests in this binary are `#[serial]`, so no other thread is
    // reading the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        // The engine refuses to upload from a pull_request run or a non-main
        // ref. This scan targets a temp repo and a stub store, so the runner's
        // GitHub context must not apply.
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

fn latest_upload(storage: &StubStorage) -> CloudRepoData {
    storage
        .repos
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("a scan uploaded nothing")
}

/// Canonical, order-free view of a projection array, so a HashMap iteration
/// order cannot read as a difference between two scans.
fn canonical(rows: &[carrick::analyzer::ApiEndpointDetails]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|row| serde_json::to_string(row).expect("row serializes"))
        .collect();
    out.sort();
    out
}

/// Every data-call and endpoint row the cache holds, as
/// `(file, line, target-or-path, resolution_source)`.
fn cached_rows(data: &CloudRepoData) -> Vec<(String, i32, String, String)> {
    let mut rows = Vec::new();
    let results = data
        .file_results
        .as_ref()
        .expect("a scan must populate the incremental cache");
    for (path, result) in results {
        for call in &result.data_calls {
            rows.push((
                path.clone(),
                call.line_number,
                call.target.clone(),
                format!("{:?}", call.resolution_source),
            ));
        }
        for endpoint in &result.endpoints {
            rows.push((
                path.clone(),
                endpoint.line_number,
                endpoint.path.clone(),
                format!("{:?}", endpoint.resolution_source),
            ));
        }
    }
    rows.sort();
    rows
}

/// The cache is the model's raw reply: no row in it may carry a deterministic
/// provenance, because the deterministic layer emits its rows AFTER the cache
/// is read and re-emits them on every scan.
fn assert_no_deterministic_rows(data: &CloudRepoData, label: &str) {
    let deterministic: Vec<_> = cached_rows(data)
        .into_iter()
        .filter(|(_, _, _, source)| !matches!(source.as_str(), "None" | "Some(Model)"))
        .collect();
    assert!(
        deterministic.is_empty(),
        "{label}: the cache holds rows the deterministic layer stated, which it re-states on \
         every scan: {deterministic:#?}"
    );
}

/// The whole split, end to end: a second scan of an unchanged tree calls the
/// model zero times, produces exactly the projection the first scan did, and
/// caches only what the model said — the deterministic rows are recomputed.
#[tokio::test]
#[serial]
async fn an_unchanged_scan_replays_the_cached_model_answer_and_re_emits_the_rest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);

    let storage = StubStorage::default();

    let before_scan_one = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");
    let scan_one = latest_upload(&storage);
    let dispatched_one = carrick::scan_health::attempted_count() - before_scan_one;
    assert!(
        dispatched_one > 0,
        "scan #1 is the cold scan: it must dispatch files to the model"
    );

    // No commit between the scans, so HEAD is still scan #1's `commit_hash`
    // and every file is unchanged.
    let before_scan_two = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    let scan_two = latest_upload(&storage);
    let dispatched_two = carrick::scan_health::attempted_count() - before_scan_two;

    assert_eq!(
        dispatched_two, 0,
        "an unchanged tree must reach the model zero times (scan #1 dispatched {dispatched_one})"
    );

    // The deterministic layer re-ran over every file and the cached model
    // answer joined onto it, so the projection is the cold scan's.
    assert_eq!(
        canonical(&scan_two.calls),
        canonical(&scan_one.calls),
        "the incremental scan's calls differ from the cold scan's"
    );
    assert_eq!(
        canonical(&scan_two.endpoints),
        canonical(&scan_one.endpoints),
        "the incremental scan's endpoints differ from the cold scan's"
    );

    assert_no_deterministic_rows(&scan_one, "scan #1");
    assert_no_deterministic_rows(&scan_two, "scan #2");

    // The named case: the model reports the binding's NAME as the target of
    // the whole-URL call (`fetch(url)`), and the source states the URL. The
    // cache must hold the model's words; the projection must hold the
    // source's.
    let cached = cached_rows(&scan_two);
    assert!(
        cached
            .iter()
            .any(|(file, line, target, _)| file.ends_with("helpdesk.ts")
                && *line == 7
                && target == "url"),
        "the cache must hold the model's own target for src/helpdesk.ts:7: {cached:#?}"
    );
    assert!(
        !cached
            .iter()
            .any(|(_, _, target, _)| target == "${process.env.HELPDESK_URL}/api/answer"),
        "the cache must not hold the target the deterministic layer resolved: {cached:#?}"
    );
    assert!(
        scan_two
            .calls
            .iter()
            .any(|call| call.key.to_string().contains("/api/answer")),
        "the projection must still carry the resolved whole-URL call: {:#?}",
        scan_two.calls
    );
}

/// Every row the stored generation holds, as `service side key site source`,
/// sorted, so two scans' rows compare whatever order they were stated in.
fn stored_rows(storage: &StubStorage) -> Vec<String> {
    let mut rows = Vec::new();
    for blob in storage.repos.lock().unwrap().iter() {
        let service = blob.service_name.as_deref().unwrap_or_default();
        for (side, list) in [("endpoint", &blob.endpoints), ("call", &blob.calls)] {
            for row in list {
                rows.push(format!(
                    "{service} {side} {} {} {:?}",
                    row.key.canonical(),
                    row.file_path.display(),
                    row.resolution_source
                ));
            }
        }
    }
    rows.sort();
    rows
}

/// carrick#1874: a rescan of an unchanged tree states the rows the cold scan
/// stated.
///
/// The tree is two services around an in-process event bus: one calls
/// `bus.emit("itemArchived", ...)` and the other `bus.on("itemArchived", ...)`.
/// The event-bus pass and the model both read each call, and the pass defers
/// to the model's row for the same site. A rescan holds the model's answers
/// under repo-relative keys and the pass's rows under the path each file was
/// discovered at, so the two were never seen as one site, and from the second
/// scan on every such call was stated twice.
#[tokio::test]
#[serial]
async fn a_rescan_states_the_rows_the_cold_scan_stated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_copy_of("pubsub-wrapper-monorepo", tmp.path(), &[]);
    mock_env(&cassette);
    let storage = StubStorage::default();

    let (_, dispatched_cold) = scan(&storage, &repo_path).await;
    assert!(dispatched_cold > 0, "scan #1 is the cold scan");
    let cold = stored_rows(&storage);
    let on_the_bus = |rows: &[String]| {
        rows.iter()
            .filter(|row| row.contains("pubsub|itemArchived"))
            .count()
    };
    assert_eq!(
        on_the_bus(&cold),
        2,
        "the cold scan states the publish and the subscription once each: {cold:#?}"
    );

    let (_, dispatched_rescan) = scan(&storage, &repo_path).await;
    assert_eq!(
        dispatched_rescan, 0,
        "an unchanged tree reaches the model zero times"
    );
    assert_eq!(
        stored_rows(&storage),
        cold,
        "the rescan and the cold scan state different rows for one tree"
    );
}

/// A cache written by the previous format is refused, and the scan that
/// refuses it re-reads every file from the model. The version is what carries
/// a change of join rule (or of prompt or schema) to already-indexed repos.
#[tokio::test]
#[serial]
async fn a_previous_format_cache_is_refused_and_the_scan_goes_back_to_the_model() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);

    let storage = StubStorage::default();

    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");

    // Age scan #1's payload to the format that held JOINED results.
    {
        let mut repos = storage.repos.lock().unwrap();
        let prev = repos.last_mut().expect("no prior upload to mutate");
        assert!(
            prev.file_results.is_some(),
            "scan #1 must have populated the cache"
        );
        prev.cache_version = Some(20);
    }

    let before_scan_two = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    let dispatched_two = carrick::scan_health::attempted_count() - before_scan_two;

    assert!(
        dispatched_two > 0,
        "a v20 cache holds joined rows this version's join would fold in again: it must be \
         refused and every file re-read from the model"
    );
}

/// Guidance the blob carries without an id is asked for again, not replayed.
///
/// `/analyze-file` sends the guidance id so the cloud's analysis cache can key
/// the repo-global guidance block by identity instead of by its text; without
/// one the whole message is the key, and every file in the service re-pays
/// whenever the guidance regenerates into different words. A blob written
/// before the id existed carries none, and the `package_json_hash` gate around
/// the replay does not move on an ordinary scan — so before carrick#1224 such
/// a repo replayed keyless guidance on every scan from then on and never
/// recovered. Asking again costs the five guidance calls once.
#[tokio::test]
#[serial]
async fn guidance_without_an_id_is_asked_for_again_instead_of_replayed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);

    let storage = StubStorage::default();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");
    assert!(
        guidance_ids(&latest_upload(&storage))
            .iter()
            .all(Option::is_some),
        "scan #1 must persist an id for every protocol's guidance"
    );

    // The control: nothing about the tree changed, so the keyed guidance in
    // the blob is replayed and no guidance call goes out.
    let before_replay = requests_to("/framework-guidance");
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    assert_eq!(
        requests_to("/framework-guidance") - before_replay,
        0,
        "guidance carrying an id is replayed, not asked for again"
    );

    // Age the stored payload to a blob written before the id existed. Nothing
    // else about it moves: same cache version, same manifests, same commit.
    {
        let mut repos = storage.repos.lock().unwrap();
        let prev = repos.last_mut().expect("no prior upload to mutate");
        let guidance = prev
            .cached_guidance
            .as_mut()
            .expect("scan #2 must have persisted guidance");
        for answer in guidance.values_mut() {
            answer.guidance_key = None;
        }
    }

    let before_reask = requests_to("/framework-guidance");
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #3 failed");
    assert!(
        requests_to("/framework-guidance") - before_reask > 0,
        "keyless guidance must be asked for again, or the repo keys the whole message forever"
    );
    assert!(
        guidance_ids(&latest_upload(&storage))
            .iter()
            .all(Option::is_some),
        "the scan that re-asked must persist the id, so the next scan replays again"
    );
}

/// Two services: the fixture's own files, and one whose only file raises no
/// candidate, so the analyzer is never asked about it.
const TWO_SERVICES: &str = r#"{
  "services": [
    { "name": "helpdesk", "directory": "src" },
    { "name": "slugs", "directory": "slugs" }
  ]
}
"#;

/// A service whose scan asked the analyzer about no file stores no answers,
/// and that is an empty answer cache, not a missing generation: the next scan
/// of the unchanged tree replays its detection and guidance like any other
/// service's. Before carrick#1746 it ran a full analysis instead, which asks
/// detection and every guidance section again, on every scan.
#[tokio::test]
#[serial]
async fn a_service_with_no_answered_file_replays_its_detection_and_guidance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture_with(
        tmp.path(),
        &[
            ("carrick.json", TWO_SERVICES),
            ("slugs/slugify.ts", NO_CANDIDATES),
        ],
    );
    mock_env(&cassette);

    let storage = StubStorage::default();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");
    let first = stored_service(&storage, "slugs");
    // The shape this test is about. Should the writer ever store an empty map
    // instead, the replay below proves nothing about a stored null.
    assert!(
        first.file_results.is_none(),
        "the service analysed no file, so it stores no answers: {:?}",
        first.file_results
    );
    assert!(
        first.cached_detection.is_some()
            && first.cached_guidance.is_some()
            && first.cached_extraction_config.is_some(),
        "scan #1 must store the setup the next scan replays"
    );
    assert!(
        stored_service(&storage, "helpdesk").file_results.is_some(),
        "the other service's answers are stored as before"
    );

    let detect = requests_to("/framework-detect");
    let guidance = requests_to("/framework-guidance");
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    assert_eq!(
        (
            requests_to("/framework-detect") - detect,
            requests_to("/framework-guidance") - guidance,
        ),
        (0, 0),
        "an unchanged tree replays every service's detection and guidance, \
         including one that stored no answers"
    );
    let second = stored_service(&storage, "slugs");
    assert_eq!(
        serde_json::to_value(&second.cached_detection).unwrap(),
        serde_json::to_value(&first.cached_detection).unwrap(),
        "the stored detection is carried forward unchanged"
    );
    assert_eq!(
        serde_json::to_value(&second.cached_guidance).unwrap(),
        serde_json::to_value(&first.cached_guidance).unwrap(),
        "the stored guidance is carried forward unchanged"
    );
}

/// The blob stored for `service`.
fn stored_service(storage: &StubStorage, service: &str) -> CloudRepoData {
    let repos = storage.repos.lock().unwrap();
    repos
        .iter()
        .find(|stored| stored.service_name.as_deref() == Some(service))
        .cloned()
        .unwrap_or_else(|| {
            let held: Vec<_> = repos
                .iter()
                .map(|stored| (&stored.repo_name, &stored.service_name))
                .collect();
            panic!("no blob stored for service {service}; the store holds {held:?}")
        })
}

/// The guidance id each protocol's persisted answer carries, `None` for an
/// answer that has none.
fn guidance_ids(data: &CloudRepoData) -> Vec<Option<String>> {
    data.cached_guidance
        .as_ref()
        .expect("the payload carries guidance")
        .values()
        .map(|answer| answer.guidance_key.clone())
        .collect()
}

/// How many requests this process has made to `route` so far. The counter is a
/// process-global, which is why every test here is `#[serial]` and reads it as
/// a delta.
fn requests_to(route: &str) -> usize {
    carrick::agent_service::request_counts()
        .get(route)
        .copied()
        .unwrap_or(0)
}

/// HEAD of the fixture copy, so a test can tell the stored payload which
/// commit its cache was written against.
fn git_head(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git rev-parse failed to spawn");
    assert!(out.status.success(), "git rev-parse failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A file the model was never asked about must not hold a cache entry, and a
/// file with no entry must be dispatched.
///
/// This is the #478 class, which the raw-answer split makes permanent unless
/// the two are kept apart: a phase-1 skip used to write an empty
/// `FileAnalysisResult` into the cache, indistinguishable from a model that
/// answered with nothing. With no `CACHE_VERSION` bump left to force a
/// re-analysis (a deterministic improvement no longer moves the constant), an
/// unchanged file that a later scanner raises a candidate for would be
/// partitioned as reused and never reach the model again.
///
/// `src/notes.ts` starts as a module with no request in it — the deterministic
/// scan raises no candidate, so phase 1 skips it and the model never sees it.
/// It is then rewritten to make a call and the stored payload is re-pointed at
/// the new commit, which is the scanner's view of "this file did not change,
/// but this scan raises a candidate for it".
#[tokio::test]
#[serial]
async fn a_skipped_file_is_absent_from_the_cache_and_is_dispatched_once_it_raises_a_candidate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) =
        committed_fixture_with(tmp.path(), &[("src/notes.ts", NO_CANDIDATES)]);
    mock_env(&cassette);

    let storage = StubStorage::default();

    let before_scan_one = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #1 failed");
    let scan_one = latest_upload(&storage);
    assert!(
        carrick::scan_health::attempted_count() - before_scan_one > 0,
        "scan #1 is the cold scan: it must dispatch files to the model"
    );

    let cached_files: Vec<String> = scan_one
        .file_results
        .as_ref()
        .expect("scan #1 must populate the cache")
        .keys()
        .cloned()
        .collect();
    assert!(
        !cached_files.iter().any(|path| path.ends_with("notes.ts")),
        "a file phase 1 skipped was never asked about, so the cache must hold no answer for \
         it: {cached_files:#?}"
    );

    // The file now makes a request, and the cache is re-pointed at the commit
    // that says so — from the scanner's side this is an unchanged file that
    // this scan raises a candidate for, which is what a resolver improvement
    // looks like to an already-indexed repo.
    std::fs::write(repo_path.join("src/notes.ts"), ONE_CANDIDATE).unwrap();
    run_git(&repo_path, &["add", "-A"]);
    run_git(&repo_path, &["commit", "-q", "-m", "notes calls out"]);
    {
        let mut repos = storage.repos.lock().unwrap();
        let prev = repos.last_mut().expect("no prior upload to re-point");
        prev.commit_hash = git_head(&repo_path);
    }

    let before_scan_two = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan #2 failed");
    let dispatched_two = carrick::scan_health::attempted_count() - before_scan_two;

    assert_eq!(
        dispatched_two, 1,
        "the one file with no cached answer must be the one file dispatched; every other file \
         replays the answer the previous scan recorded"
    );
    let scan_two_cached: Vec<String> = latest_upload(&storage)
        .file_results
        .as_ref()
        .expect("scan #2 must populate the cache")
        .keys()
        .cloned()
        .collect();
    assert!(
        scan_two_cached
            .iter()
            .any(|path| path.ends_with("notes.ts")),
        "the file the model was asked about must now hold its answer: {scan_two_cached:#?}"
    );
}

/// The same file, edited in the working tree and not committed.
const EDITED_CANDIDATE: &str = "export async function fetchNotes(): Promise<string> {\n  \
    const res = await fetch(\"http://localhost:9100/api/notes/archived\");\n  \
    return res.text();\n}\n";

/// Run one scan and return what it uploaded and how many files it sent to the
/// model.
async fn scan(storage: &StubStorage, repo_path: &Path) -> (CloudRepoData, usize) {
    let before = carrick::scan_health::attempted_count();
    run_analysis_engine_with_sidecar(storage.clone(), repo_path.to_str().unwrap(), None, false)
        .await
        .expect("scan failed");
    (
        latest_upload(storage),
        carrick::scan_health::attempted_count() - before,
    )
}

fn cached_files(data: &CloudRepoData) -> Vec<String> {
    let mut files: Vec<String> = data
        .file_results
        .as_ref()
        .map(|results| results.keys().cloned().collect())
        .unwrap_or_default();
    files.sort();
    files
}

/// carrick#1079: one untracked file used to make a scan non-incremental both
/// ways, because a dirty run uploaded no cache and a dirty previous generation
/// was ignored. Now the untracked file is the only one sent to the model, the
/// dirty payload keeps every answer about a committed file, and the next dirty
/// scan replays that payload rather than starting cold.
#[tokio::test]
#[serial]
async fn an_untracked_file_costs_one_dispatch_not_the_whole_cache() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture(tmp.path());
    mock_env(&cassette);
    let storage = StubStorage::default();

    let (clean, dispatched_cold) = scan(&storage, &repo_path).await;
    assert!(dispatched_cold > 0, "scan #1 is the cold scan");
    assert_eq!(clean.dirty, None);
    let committed_answers = cached_files(&clean);
    assert!(!committed_answers.is_empty(), "scan #1 seeds the cache");

    std::fs::write(repo_path.join("src/notes.ts"), ONE_CANDIDATE).unwrap();

    let (dirty, dispatched_dirty) = scan(&storage, &repo_path).await;
    assert_eq!(
        dispatched_dirty, 1,
        "only the untracked file goes to the model; every committed file replays its answer"
    );
    assert_eq!(dirty.dirty, Some(true));
    assert_eq!(
        cached_files(&dirty),
        committed_answers,
        "a dirty run keeps every answer about a committed file, and none about the untracked one"
    );

    // The previous generation is now the dirty one, and it is replayed.
    let (_, dispatched_again) = scan(&storage, &repo_path).await;
    assert_eq!(
        dispatched_again, 1,
        "a dirty previous generation still serves every committed file's answer"
    );
}

/// The failure the old whole-cache drop existed to prevent, now prevented per
/// file: an answer about uncommitted bytes must never be replayed.
///
/// `src/notes.ts` is committed and answered. It is then edited without a
/// commit: that scan must ask the model again (a commit-to-commit diff sees no
/// change) and must not cache the answer. Once the edit is reverted, the next
/// scan must ask again too: had the dirty answer been cached, the file would
/// read as unchanged since the commit and the answer about the edit would be
/// replayed for the committed code.
#[tokio::test]
#[serial]
async fn an_answer_about_an_uncommitted_edit_is_never_replayed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) =
        committed_fixture_with(tmp.path(), &[("src/notes.ts", ONE_CANDIDATE)]);
    mock_env(&cassette);
    let storage = StubStorage::default();

    let (clean, _) = scan(&storage, &repo_path).await;
    assert!(
        cached_files(&clean)
            .iter()
            .any(|path| path.ends_with("notes.ts")),
        "the committed file holds an answer: {:?}",
        cached_files(&clean)
    );

    std::fs::write(repo_path.join("src/notes.ts"), EDITED_CANDIDATE).unwrap();
    let (dirty, dispatched_dirty) = scan(&storage, &repo_path).await;
    assert_eq!(
        dispatched_dirty, 1,
        "the edited file is changed on disk even though HEAD did not move"
    );
    assert_eq!(dirty.dirty, Some(true));
    assert!(
        !cached_files(&dirty)
            .iter()
            .any(|path| path.ends_with("notes.ts")),
        "the answer about the uncommitted edit is not cached: {:?}",
        cached_files(&dirty)
    );

    run_git(&repo_path, &["checkout", "-q", "--", "src/notes.ts"]);
    let (reverted, dispatched_reverted) = scan(&storage, &repo_path).await;
    assert_eq!(reverted.dirty, None, "the revert leaves a clean tree");
    assert_eq!(
        dispatched_reverted, 1,
        "the reverted file has no cached answer, so it is asked about again; zero would mean \
         the answer about the edit was replayed for the committed code"
    );
}

/// A GraphQL document consumer whose result type is a co-located interface the
/// file analyzer names (`graphql_consumer_locates`, #268).
const GRAPHQL_CONSUMER: &str = "declare function gql(strings: TemplateStringsArray, ...values: unknown[]): string;\n\
\n\
export const ON_ORDER_UPDATED = gql`\n  \
  subscription OnOrderUpdated {\n    \
    orderUpdated {\n      \
      id\n      \
      note\n    \
    }\n  \
  }\n\
`;\n\
\n\
export interface OrderUpdate {\n  \
  id: string;\n  \
  note: string;\n\
}\n\
\n\
export function renderOrderUpdate(update: OrderUpdate): string {\n  \
  return `${update.id}: ${update.note}`;\n\
}\n";

/// The model's answer for that file: the subscription's result type is
/// `OrderUpdate`.
const GRAPHQL_CONSUMER_ANSWER: &str = r#"{
  "mounts": [],
  "endpoints": [],
  "data_calls": [],
  "graphql_consumer_locates": [
    {"kind": "subscription", "field": "orderUpdated", "result_type_symbol": "OrderUpdate", "result_type_source": null}
  ]
}"#;

/// The type anchor the upload's manifest carries for the consumer of
/// `subscription orderUpdated`.
fn order_updated_consumer_anchor(data: &CloudRepoData) -> Option<String> {
    let blob = serde_json::to_value(data).expect("the upload serializes");
    let entries = blob["type_manifest"]
        .as_array()
        .expect("the upload carries a type manifest");
    let entry = entries
        .iter()
        .find(|entry| entry["field"] == "orderUpdated" && entry["role"] == "consumer")
        .unwrap_or_else(|| panic!("no manifest entry for the orderUpdated consumer: {entries:#?}"));
    entry["primary_type_symbol"].as_str().map(str::to_string)
}

/// carrick#1725: the incremental path keys the replayed answers repo-relative,
/// and the GraphQL consumer ops carry the path the scan discovered. The locate
/// joined on both, so on every scan after the first the located result type was
/// dropped, the consumer's type read `unknown`, and its edges lost their
/// verdicts. The warm scan must anchor the consumer exactly as the cold one did.
#[tokio::test]
#[serial]
async fn an_unchanged_graphql_consumer_keeps_its_located_result_type() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_fixture_with(
        tmp.path(),
        &[
            ("src/order_feed.ts", GRAPHQL_CONSUMER),
            (
                "__llm__/analyze-file/order_feed.json",
                GRAPHQL_CONSUMER_ANSWER,
            ),
        ],
    );
    mock_env(&cassette);
    let storage = StubStorage::default();

    let (cold, dispatched_cold) = scan(&storage, &repo_path).await;
    assert!(dispatched_cold > 0, "scan #1 is the cold scan");
    assert_eq!(
        order_updated_consumer_anchor(&cold).as_deref(),
        Some("OrderUpdate"),
        "the cold scan anchors the consumer on the located type"
    );

    let (warm, dispatched_warm) = scan(&storage, &repo_path).await;
    assert_eq!(
        dispatched_warm, 0,
        "an unchanged tree reaches the model zero times"
    );
    assert_eq!(
        order_updated_consumer_anchor(&warm).as_deref(),
        Some("OrderUpdate"),
        "the warm scan replays the same answer, so it must anchor the consumer the same way"
    );
}

/// Every blob one scan uploaded, as the text the upload sends
/// (`serde_json::to_string`, as `upload_repo_data` writes it), keyed by
/// service.
///
/// Two fields state the run and not the tree, so they are pinned:
/// `last_updated` is when the scan ran, and `boundary.files_attempted` is how
/// many files this run sent to the model, which a rescan that replays every
/// stored answer puts at zero. Everything else is a statement about the tree.
fn uploaded_text(storage: &StubStorage) -> std::collections::BTreeMap<String, String> {
    storage
        .repos
        .lock()
        .unwrap()
        .iter()
        .map(|blob| {
            let mut blob = blob.clone();
            blob.last_updated = chrono::DateTime::UNIX_EPOCH;
            if let Some(boundary) = blob.boundary.as_mut() {
                boundary.files_attempted = 0;
            }
            let service = blob.service_name.clone().unwrap_or_default();
            (
                format!("{}/{service}", blob.repo_name),
                serde_json::to_string(&blob).expect("the upload serializes"),
            )
        })
        .collect()
}

/// The blob's top-level fields whose text differs between two uploads of one
/// service, each with the text around its first differing byte.
///
/// Read as raw text per field, never as parsed values: a parsed object
/// compares equal whatever order its keys were written in, which is the
/// difference this test exists to see.
fn fields_that_differ(first: &str, second: &str) -> Vec<String> {
    type Fields = std::collections::BTreeMap<String, Box<serde_json::value::RawValue>>;
    let first: Fields = serde_json::from_str(first).expect("a blob is a JSON object");
    let second: Fields = serde_json::from_str(second).expect("a blob is a JSON object");
    let mut names: Vec<&String> = first.keys().chain(second.keys()).collect();
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for name in names {
        let a = first.get(name).map(|raw| raw.get()).unwrap_or("<absent>");
        let b = second.get(name).map(|raw| raw.get()).unwrap_or("<absent>");
        if a == b {
            continue;
        }
        let at = a
            .bytes()
            .zip(b.bytes())
            .position(|(x, y)| x != y)
            .unwrap_or_else(|| a.len().min(b.len()));
        let window = |text: &str| {
            let bytes = text.as_bytes();
            let from = at.saturating_sub(60);
            let to = (at + 100).min(bytes.len());
            String::from_utf8_lossy(&bytes[from..to]).into_owned()
        };
        out.push(format!(
            "{name} (first difference at byte {at}):\n    one scan:   {}\n    the other:  {}",
            window(a),
            window(b)
        ));
    }
    out
}

fn assert_same_bytes(
    fixture: &str,
    what: &str,
    first: &std::collections::BTreeMap<String, String>,
    second: &std::collections::BTreeMap<String, String>,
) {
    assert_eq!(
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>(),
        "{fixture}: {what} uploaded a different set of services"
    );
    let mut differing = Vec::new();
    for (service, text) in first {
        for field in fields_that_differ(text, &second[service]) {
            differing.push(format!("[{service}] {field}"));
        }
    }
    assert!(
        differing.is_empty(),
        "{fixture}: {what} of one tree uploaded different bytes in {} field(s):\n{}",
        differing.len(),
        differing.join("\n")
    );
}

/// carrick#1847: two scans of one tree upload the same bytes.
///
/// The blob is compared byte for byte by whatever stores it, so an unchanged
/// tree whose blob differs reads as changed and costs a recomputation. Its
/// maps were hash maps, which are written in a different order by every
/// process and every map, so no two uploads of one tree were ever equal.
///
/// Three trees of different shapes (an HTTP service with mounted routers, a
/// thirty-file service that publishes and subscribes, and five GraphQL
/// services in one repo), each scanned cold twice (the second from an empty
/// store, so nothing is replayed) and then once more over the stored
/// generation, as an ordinary rescan runs. Without the type sidecar: the scan
/// with types is `two_scans_of_one_tree_with_types_upload_the_same_bytes`.
#[tokio::test]
#[serial]
async fn two_scans_of_one_tree_upload_the_same_bytes() {
    for fixture in [
        "llm-mocked-api",
        "in-process-publish",
        "graphql-walked-vendor-schema",
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (repo_path, cassette) = committed_copy_of(fixture, tmp.path(), &[]);
        mock_env(&cassette);

        let storage = StubStorage::default();
        scan(&storage, &repo_path).await;
        let cold = uploaded_text(&storage);
        assert!(
            cold.values().any(|text| text.len() > 2_000),
            "{fixture}: the scan must upload a blob worth comparing"
        );

        let other_store = StubStorage::default();
        scan(&other_store, &repo_path).await;
        assert_same_bytes(
            fixture,
            "two cold scans",
            &cold,
            &uploaded_text(&other_store),
        );

        scan(&storage, &repo_path).await;
        assert_same_bytes(
            fixture,
            "a cold scan and the rescan after it",
            &cold,
            &uploaded_text(&storage),
        );
    }
}

/// carrick#2028: a blob read back from its own bytes writes the same bytes.
///
/// The upload boundary rebuilds a payload from its wire format whenever it
/// rewrites a machine path, and a peer's blob is read the same way, so every
/// field the scanner writes has to read back as itself. One that did not
/// (each function row's kind read back as `Placeholder`) changed what the
/// index stored for a whole service whenever one unrelated string in it held
/// a machine path. Run over the same three trees as the test above, so a field
/// added to any type a scan fills is held to it.
#[tokio::test]
#[serial]
async fn a_blob_read_back_from_its_bytes_writes_the_same_bytes() {
    for fixture in [
        "llm-mocked-api",
        "in-process-publish",
        "graphql-walked-vendor-schema",
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (repo_path, cassette) = committed_copy_of(fixture, tmp.path(), &[]);
        mock_env(&cassette);

        let storage = StubStorage::default();
        scan(&storage, &repo_path).await;
        let written = uploaded_text(&storage);
        assert!(
            written
                .values()
                .any(|text| text.contains("\"node_type\":\"FunctionDeclaration\"")),
            "{fixture}: the scan must upload a parsed function row"
        );
        let read_back: std::collections::BTreeMap<String, String> = written
            .iter()
            .map(|(service, text)| {
                let blob: CloudRepoData = serde_json::from_str(text).expect("the blob reads back");
                let text = serde_json::to_string(&blob).expect("the blob serializes");
                (service.clone(), text)
            })
            .collect();
        assert_same_bytes(
            fixture,
            "a blob and the blob read back from it",
            &written,
            &read_back,
        );
    }
}

/// The real type sidecar, built from `src/sidecar` and initialised on `repo`.
/// A fresh one for each scan, as every scan starts its own.
fn real_sidecar(repo: &Path) -> carrick::services::type_sidecar::TypeSidecar {
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    assert!(
        entry.exists(),
        "build the sidecar first: cd src/sidecar && npm ci && npm run build"
    );
    let sidecar =
        carrick::services::type_sidecar::TypeSidecar::spawn(&entry).expect("the sidecar spawns");
    sidecar.start_init(repo, None);
    sidecar
        .wait_ready(std::time::Duration::from_secs(120))
        .expect("the sidecar initialises on the fixture");
    sidecar
}

/// One scan through a sidecar of its own, into `storage`.
async fn scan_with_types(storage: &StubStorage, repo_path: &Path) {
    let sidecar = real_sidecar(repo_path);
    run_analysis_engine_with_sidecar(
        storage.clone(),
        repo_path.to_str().unwrap(),
        Some(&sidecar),
        false,
    )
    .await
    .expect("scan failed");
}

/// carrick#1876: two scans of one tree with types upload the same bytes.
///
/// With the sidecar, every operation asks for its type, and the bundle and
/// the capture stub are written in the order the answers come back. The
/// requests were collected in the order of a hash map, so that type text was
/// written in another order on every scan while holding the same
/// declarations. They now go out in file order.
///
/// Two HTTP trees whose routes and calls sit in several files, each scanned
/// cold twice through the live sidecar. The blobs must carry type text, or
/// the comparison proves nothing.
#[tokio::test]
#[serial]
async fn two_scans_of_one_tree_with_types_upload_the_same_bytes() {
    for fixture in ["llm-mocked-api", "request-summary"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (repo_path, cassette) = committed_copy_of(fixture, tmp.path(), &[]);
        mock_env(&cassette);

        let storage = StubStorage::default();
        scan_with_types(&storage, &repo_path).await;
        let typed = latest_upload(&storage);
        let declarations = typed
            .bundled_types
            .as_deref()
            .map(|bundle| bundle.matches("export ").count())
            .unwrap_or(0);
        assert!(
            declarations >= 4 && typed.capture_stub.is_some(),
            "{fixture}: the scan must write type text worth comparing \
             ({declarations} declarations in the bundle)"
        );
        let cold = uploaded_text(&storage);

        let other_store = StubStorage::default();
        scan_with_types(&other_store, &repo_path).await;
        assert_same_bytes(
            fixture,
            "two cold scans with types",
            &cold,
            &uploaded_text(&other_store),
        );

        scan_with_types(&storage, &repo_path).await;
        assert_same_bytes(
            fixture,
            "a cold scan with types and the rescan after it",
            &cold,
            &uploaded_text(&storage),
        );
    }
}

/// The pool test's trees print a union of two interfaces and the keys of an
/// object made from a mapped type over literal keys.
const PET_SHAPES: &str = "export interface Dog {\n  bark(): string;\n}\n\n\
    export interface Cat {\n  meow(): string;\n}\n";

/// Asked first of the first tree's functions, by name (`$` sorts before every
/// letter): meets `'alpha'` before `'zeta'` and `Dog` before `Cat`.
const PETS_MET_FIRST: &str = "import type { Cat, Dog } from './shapes.js';\n\n\
    export function $alphaFirst(o: Record<'alpha' | 'zeta', number>) {\n  return { ...o };\n}\n\n\
    export function $dogFirst(d: Dog, c: Cat, flip: boolean) {\n  return flip ? d : c;\n}\n";

/// Two functions that meet the pets' types the other way round from
/// `PETS_MET_FIRST`, named `{name}_keys` and `{name}_pet`.
fn pets_met_later(name: &str) -> String {
    format!(
        "\nexport function {name}_keys(o: Record<'zeta' | 'alpha', number>) {{\n  \
         return {{ ...o }};\n}}\n\
         \nexport function {name}_pet(c: Cat, d: Dog, flip: boolean) {{\n  \
         return flip ? c : d;\n}}\n"
    )
}

/// Types a checker meets in one order. Every file is in the sidecar's program
/// from the start. `PETS_MET_FIRST` is asked first, and forty files of
/// sixteen slots meet the same types the other way round. The first batch
/// ends among those, so a pool process starts from a later file, in the
/// order the scan's own process did not.
fn types_met_in_another_order() -> Vec<(String, String)> {
    let mut files = vec![
        ("src/pets/shapes.ts".to_string(), PET_SHAPES.to_string()),
        ("src/pets/first.ts".to_string(), PETS_MET_FIRST.to_string()),
    ];
    for file in 0..40 {
        let mut text = String::from("import type { Cat, Dog } from './shapes.js';\n");
        for function in 0..8 {
            text.push_str(&pets_met_later(&format!("zz_{file:02}_{function}")));
        }
        files.push((format!("src/pets/later_{file:02}.ts"), text));
    }
    files
}

/// One of two interfaces named `Dog`, in a module of its own, with a
/// function the pass asks about.
fn named_dog(member: &str, returns: &str, function: &str) -> String {
    format!(
        "export interface Dog {{\n  {member}(): {returns};\n}}\n\n\
         export function {function}(d: Dog) {{\n  return d.{member}();\n}}\n"
    )
}

/// Files a process adds to its program in one order. The sidecar's program
/// does not list `tools/` (no tsconfig, and outside its default source
/// folders), so a process adds a file there the first time it is asked about
/// it, with the files it imports. Two interfaces are both named `Dog`, in
/// `tools/yard/` and `tools/kennel/`. The yard's function is asked first of
/// all, so the scan's own process adds the yard's module before the
/// kennel's. Sixty later files under `tools/` import the kennel's module
/// before the yard's and print a union of the two: the first batch ends
/// among them, so a pool process starts from a later file and adds the
/// kennel's first.
fn files_added_in_another_order() -> Vec<(String, String)> {
    let mut files = vec![
        ("src/pets/shapes.ts".to_string(), PET_SHAPES.to_string()),
        (
            "tools/yard/dog.ts".to_string(),
            named_dog("woof", "number", "$0_yard"),
        ),
        (
            "tools/kennel/dog.ts".to_string(),
            named_dog("bark", "string", "zzz_kennel"),
        ),
    ];
    for file in 0..60 {
        let mut text = String::from(
            "import type { Cat, Dog } from '../src/pets/shapes.js';\n\
             import type { Dog as KennelDog } from './kennel/dog.js';\n\
             import type { Dog as YardDog } from './yard/dog.js';\n",
        );
        for function in 0..3 {
            let name = format!("zz_{file:02}_{function}");
            text.push_str(&pets_met_later(&name));
            text.push_str(&format!(
                "\nexport function {name}_pick(k: KennelDog, y: YardDog, flip: boolean) {{\n  \
                 return flip ? k : y;\n}}\n"
            ));
        }
        files.push((format!("tools/later_{file:02}.ts"), text));
    }
    files
}

/// `CARRICK_SIDECAR_POOL` set for as long as this lives, and unset after,
/// a failed assertion included.
struct PoolSize;

impl PoolSize {
    fn set(processes: &str) -> Self {
        // SAFETY: every test in this binary is `#[serial]`.
        unsafe { std::env::set_var(carrick::services::sidecar_pool::POOL_ENV, processes) };
        PoolSize
    }
}

impl Drop for PoolSize {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { std::env::remove_var(carrick::services::sidecar_pool::POOL_ENV) };
    }
}

/// A writer the test reads back: what the scan logged.
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logged {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logged {
    type Writer = Logged;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// carrick#1993: a pool of sidecar processes writes the index one process
/// writes, over `tree` added to the `llm-mocked-api` fixture.
///
/// The signature pass asks the slots after its first batch on a pool, a file
/// at a time. Each process builds its own program and checker, so what it
/// prints must not depend on the order that process met anything in. One
/// scan forced to one process and one to three: the blobs, the function
/// index among them, must be the same bytes. `teeth` names functions whose
/// inferred return must name both members it lists, or the comparison
/// compares nothing.
async fn a_pool_writes_what_one_process_writes(
    label: &str,
    tree: Vec<(String, String)>,
    teeth: &[(&str, [&str; 2])],
) {
    let extra: Vec<(&str, &str)> = tree
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (repo_path, cassette) = committed_copy_of("llm-mocked-api", tmp.path(), &extra);
    mock_env(&cassette);

    let one = StubStorage::default();
    {
        let _size = PoolSize::set("1");
        scan_with_types(&one, &repo_path).await;
    }
    let pooled = StubStorage::default();
    let logged = Logged::default();
    {
        let _size = PoolSize::set("3");
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logged.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _logging = tracing::subscriber::set_default(subscriber);
        scan_with_types(&pooled, &repo_path).await;
    }

    let log = String::from_utf8_lossy(&logged.0.lock().unwrap()).into_owned();
    let pool_lines: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("Signature inference pool") || line.contains("stays on one"))
        .collect();
    assert!(
        log.contains("Signature inference: 3 processes answer the slots after the first batch"),
        "{label}: the second scan must ask on three processes, or it compares nothing: \
         {pool_lines:?}"
    );

    let functions = &latest_upload(&one).function_definitions;
    let printed: Vec<(&str, [&str; 2], String)> = teeth
        .iter()
        .map(|(name, members)| {
            let signature = functions
                .get(*name)
                .and_then(|def| def.signature.clone())
                .unwrap_or_default();
            (*name, *members, signature)
        })
        .collect();
    assert!(
        printed.iter().all(|(_, members, signature)| {
            let returned = signature.rsplit(" => ").next().unwrap_or_default();
            members.iter().all(|member| returned.contains(member))
        }),
        "{label}: each inferred return must name both members, or the test compares nothing: \
         {printed:#?}"
    );

    assert_same_bytes(
        label,
        "a scan on three sidecar processes and a scan on one",
        &uploaded_text(&one),
        &uploaded_text(&pooled),
    );
}

/// The checker's history: types a pool process meets in another order than
/// the scan's own process did (`types_met_in_another_order`).
#[tokio::test]
#[serial]
#[ignore = "red until the sidecar prints types in an order of their own (carrick#2019); \
            `--ignored` shows it fail on TypeScript 5"]
async fn a_pool_prints_types_its_processes_met_in_another_order_as_one_process_does() {
    a_pool_writes_what_one_process_writes(
        "types met in another order",
        types_met_in_another_order(),
        &[
            ("$alphaFirst", ["alpha", "zeta"]),
            ("$dogFirst", ["Cat", "Dog"]),
            ("zz_39_7_keys", ["alpha", "zeta"]),
            ("zz_39_7_pet", ["Cat", "Dog"]),
        ],
    )
    .await;
}

/// The program's file order: files a pool process adds to its program in
/// another order than the scan's own process did
/// (`files_added_in_another_order`).
#[tokio::test]
#[serial]
#[ignore = "red until every process's program holds the same files in the same order \
            (carrick#1993); `--ignored` shows it fail on a sidecar with stable type order"]
async fn a_pool_prints_types_from_files_its_processes_added_in_another_order_as_one_process_does() {
    a_pool_writes_what_one_process_writes(
        "files added in another order",
        files_added_in_another_order(),
        &[
            ("zz_59_2_keys", ["alpha", "zeta"]),
            ("zz_59_2_pet", ["Cat", "Dog"]),
            ("zz_59_2_pick", ["KennelDog", "YardDog"]),
        ],
    )
    .await;
}
