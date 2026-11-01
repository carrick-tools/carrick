//! A scan's services analysed side by side (carrick#1066, carrick#2216).
//!
//! Each service runs on a thread of its own with a sidecar process of its
//! own, admitted in declared order. What a service uploads must not depend on
//! how many ran beside it or which finished first, and a service that fails
//! must stop none of the others.
//!
//! Driven over `tests/fixtures/parallel-services`, four services of different
//! shapes declared largest first, with the model's answers replayed from its
//! cassette. Every test sets [`WIDTH_ENV`], a process-global, so they are
//! `#[serial]`.

use async_trait::async_trait;
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::engine::service_workers::{SERVICE_CONCURRENCY_ENV as WIDTH_ENV, take_schedules};
use serial_test::serial;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const FIXTURE: &str = "parallel-services";

/// The services of the fixture, in declared order.
const SERVICES: [&str; 4] = ["orders-api", "catalog-api", "storefront-web", "notifier"];

/// In-memory storage that keeps every upload in the order it arrived.
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
        self.repos.lock().unwrap().push(data.clone());
        Ok(UploadOutcome::default())
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
    fn supports_multi_service(&self) -> bool {
        true
    }
}

impl StubStorage {
    /// The services uploaded, in the order they arrived.
    fn upload_order(&self) -> Vec<String> {
        self.repos
            .lock()
            .unwrap()
            .iter()
            .map(|blob| blob.service_name.clone().unwrap_or_default())
            .collect()
    }

    /// Every blob as the text the upload sends, keyed by service, with the
    /// fields that state the run or the copy rather than the service pinned.
    fn uploaded_text(&self) -> BTreeMap<String, String> {
        self.repos
            .lock()
            .unwrap()
            .iter()
            .map(|blob| {
                let mut blob = blob.clone();
                blob.last_updated = chrono::DateTime::UNIX_EPOCH;
                // Peaks are measured, so they differ from run to run at any
                // width (carrick#3083).
                blob.memory_peaks = None;
                // The commit names the copy, and the copy with its services
                // reversed is another tree.
                blob.commit_hash = String::new();
                if let Some(boundary) = blob.boundary.as_mut() {
                    boundary.files_attempted = 0;
                    boundary.commit_hash = String::new();
                }
                (
                    blob.service_name.clone().unwrap_or_default(),
                    serde_json::to_string(&blob).expect("the upload serializes"),
                )
            })
            .collect()
    }

    fn blobs(&self) -> Vec<CloudRepoData> {
        self.repos.lock().unwrap().clone()
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// A committed copy of the fixture under `tmp`, each of `edits` written into
/// it before the commit, and its cassette directory.
fn committed_copy(tmp: &Path, edits: &[(&str, String)]) -> (PathBuf, PathBuf) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE);
    // The same path for every copy: a few fields of a blob are read from
    // the copy's absolute path, and the copies must differ in nothing else.
    let repo = tmp.join("workspace");
    let _ = std::fs::remove_dir_all(&repo);
    copy_dir(&fixture, &repo);
    for (relative, contents) in edits {
        std::fs::write(repo.join(relative), contents).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassette = repo.join("__llm__");
    (repo, cassette)
}

/// The fixture's `carrick.json` with its services in the reverse order.
fn reversed_config() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
        .join("carrick.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    config["services"]
        .as_array_mut()
        .expect("the fixture declares services")
        .reverse();
    serde_json::to_string_pretty(&config).unwrap()
}

fn mock_env(cassette: &Path, width: usize) {
    // SAFETY: every test in this binary is `#[serial]`, so no other thread
    // reads the environment while these are set.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassette.display()),
        );
        std::env::set_var("CARRICK_SKIP_INTENTS", "1");
        std::env::set_var(WIDTH_ENV, width.to_string());
        // Four sidecars building their programs at once on a small runner
        // must not miss the readiness budget and degrade the types.
        std::env::set_var("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600");
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
    }
}

/// The real type sidecar, built from `src/sidecar` and initialised on `repo`:
/// the process `main` starts, which each service's own is started like.
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
        .wait_ready(std::time::Duration::from_secs(300))
        .expect("the sidecar initialises on the fixture");
    sidecar
}

/// One scan of a fresh copy of the fixture at `width`, through a sidecar or
/// not, into a store of its own.
async fn scan_at(
    tmp: &Path,
    width: usize,
    edits: &[(&str, String)],
    with_types: bool,
) -> (StubStorage, Result<(), String>) {
    let (repo, cassette) = committed_copy(tmp, edits);
    mock_env(&cassette, width);
    let storage = StubStorage::default();
    let sidecar = with_types.then(|| real_sidecar(&repo));
    let result = run_analysis_engine_with_sidecar(
        storage.clone(),
        repo.to_str().unwrap(),
        sidecar.as_ref(),
        false,
    )
    .await
    .map_err(|error| error.to_string());
    (storage, result)
}

fn assert_same_bytes(
    what: &str,
    first: &BTreeMap<String, String>,
    second: &BTreeMap<String, String>,
) {
    assert_eq!(
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>(),
        "{what}: a different set of services was uploaded"
    );
    for (service, text) in first {
        if text == &second[service] {
            continue;
        }
        let other = &second[service];
        let at = text
            .bytes()
            .zip(other.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| text.len().min(other.len()));
        let window = |t: &str| {
            let from = at.saturating_sub(80);
            let to = (at + 120).min(t.len());
            String::from_utf8_lossy(&t.as_bytes()[from..to]).into_owned()
        };
        panic!(
            "{what}: service {service} uploaded different bytes, first at byte {at}:\n  one:   {}\n  other: {}",
            window(text),
            window(other)
        );
    }
}

/// Every service landed, each with something worth comparing, and, with
/// types, none lost its type layer.
fn assert_all_landed(storage: &StubStorage, what: &str, with_types: bool) {
    let blobs = storage.blobs();
    let mut landed: Vec<String> = blobs
        .iter()
        .map(|blob| blob.service_name.clone().unwrap_or_default())
        .collect();
    landed.sort();
    let mut expected: Vec<String> = SERVICES.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(landed, expected, "{what}: every service lands, once");
    let rows: usize = blobs
        .iter()
        .map(|blob| blob.endpoints.len() + blob.calls.len())
        .sum();
    assert!(
        rows >= 10,
        "{what}: the scan must index the fixture's operations ({rows} rows)"
    );
    // carrick#3083: every analysed service states what its sidecar held.
    let cap = carrick::services::type_sidecar::heap_cap_mb();
    for blob in &blobs {
        let peaks = blob
            .memory_peaks
            .as_ref()
            .unwrap_or_else(|| panic!("{what}: {:?} carries no memory_peaks", blob.service_name));
        // Without types there is no analysis sidecar to measure.
        assert_eq!(
            peaks.analysis_mb.unwrap_or(0) > 0,
            with_types,
            "{what}: {:?} analysis peak: {peaks:?}",
            blob.service_name
        );
        assert_eq!(peaks.heap_cap_mb, cap, "{what}: {:?}", blob.service_name);
        // The known positive for walked rows: every service's walk ran.
        if with_types && std::env::var("CARRICK_POSITION_ROWS").as_deref() == Ok("1") {
            assert!(
                peaks.walk_mb.unwrap_or(0) > 0,
                "{what}: {:?} was not walked: {peaks:?}",
                blob.service_name
            );
        }
    }
    if with_types {
        for blob in &blobs {
            assert!(
                blob.types_degraded.is_none(),
                "{what}: {:?} lost its types: {:?}",
                blob.service_name,
                blob.types_degraded
            );
        }
        assert!(
            blobs.iter().any(|blob| blob.bundled_types.is_some()),
            "{what}: the scan must write type text worth comparing"
        );
    }
}

/// (a) and (b): the blobs at widths 1, 2 and 4, and at 4 with the services
/// declared in reverse, are the same bytes; and at 2 and 4 the services
/// really did run side by side and finish out of declared order.
async fn widths_upload_the_same_bytes(with_types: bool) {
    let label = if with_types {
        "with types"
    } else {
        "without types"
    };
    take_schedules();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (one, result) = scan_at(tmp.path(), 1, &[], with_types).await;
    result.expect("the scan at width 1 succeeds");
    assert_all_landed(&one, "width 1", with_types);
    assert_eq!(
        one.upload_order(),
        SERVICES,
        "uploads arrive in declared order"
    );
    let passes = take_schedules();
    assert_eq!(passes[0].most_in_flight, 1, "width 1 runs one at a time");
    assert_eq!(passes[0].finished, vec![0, 1, 2, 3]);
    let reference = one.uploaded_text();

    for width in [2, 4] {
        let (wide, result) = scan_at(tmp.path(), width, &[], with_types).await;
        result.expect("the scan succeeds");
        assert_all_landed(&wide, &format!("width {width}"), with_types);
        assert_eq!(
            wide.upload_order(),
            SERVICES,
            "uploads arrive in declared order"
        );
        // The known positive: without it, equal bytes prove nothing.
        let passes = take_schedules();
        assert!(
            passes[0].most_in_flight >= 2,
            "{label}, width {width}: the services never overlapped: {:?}",
            passes[0]
        );
        assert_ne!(
            passes[0].finished,
            vec![0, 1, 2, 3],
            "{label}, width {width}: the services finished in declared order, so the order of \
             finishing was never tested"
        );
        assert_same_bytes(
            &format!("{label}: width 1 against width {width}"),
            &reference,
            &wide.uploaded_text(),
        );
    }

    let (reversed, result) = scan_at(
        tmp.path(),
        4,
        &[("carrick.json", reversed_config())],
        with_types,
    )
    .await;
    result.expect("the scan with the services reversed succeeds");
    assert_all_landed(&reversed, "reversed", with_types);
    let mut declared: Vec<String> = SERVICES.iter().map(|s| s.to_string()).collect();
    declared.reverse();
    assert_eq!(
        reversed.upload_order(),
        declared,
        "uploads arrive in declared order"
    );
    take_schedules();
    assert_same_bytes(
        &format!("{label}: width 1 against width 4 with the services reversed"),
        &reference,
        &reversed.uploaded_text(),
    );
}

#[tokio::test]
#[serial]
async fn every_width_uploads_the_same_bytes() {
    widths_upload_the_same_bytes(false).await;
}

#[tokio::test]
#[serial]
async fn every_width_uploads_the_same_bytes_with_types() {
    widths_upload_the_same_bytes(true).await;
}

/// carrick#3085: with every service's compiler rows walked, each analysis
/// admitted once the walks it reads have ended, on the budget the walks
/// share, the blobs at every width are still the same bytes.
#[tokio::test]
#[serial]
async fn every_width_uploads_the_same_bytes_with_walked_rows() {
    // SAFETY: `#[serial]`, as in `mock_env`.
    unsafe {
        std::env::set_var("CARRICK_POSITION_ROWS", "1");
    }
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
        widths_upload_the_same_bytes(true),
    ))
    .await;
    // SAFETY: as above.
    unsafe {
        std::env::remove_var("CARRICK_POSITION_ROWS");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// (c) A service that fails, declared second, stops none of the others: the
/// other three land and the run returns that service's error, at width 1 as
/// at width 4.
#[tokio::test]
#[serial]
async fn a_failed_service_stops_none_of_the_others() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for width in [1, 4] {
        let (storage, result) = scan_at(
            tmp.path(),
            width,
            &[(
                "services/catalog/package.json",
                "{ \"name\": \"@fixture/catalog\", ".to_string(),
            )],
            false,
        )
        .await;
        take_schedules();
        let error = result.expect_err("the run returns the failed service's error");
        assert!(
            error.contains("package.json") || error.to_lowercase().contains("pars"),
            "width {width}: the error is the manifest's: {error}"
        );
        assert_eq!(
            storage.upload_order(),
            vec!["orders-api", "storefront-web", "notifier"],
            "width {width}: the other three land, in declared order"
        );
    }
}

fn requests_to(route: &str) -> usize {
    carrick::agent_service::request_counts()
        .get(route)
        .copied()
        .unwrap_or(0)
}

/// (d) A function two services hold is described once at width 4, as at
/// width 1: the functions asked of `/generate-intent` are the same number.
#[tokio::test]
#[serial]
async fn a_shared_function_is_described_once_side_by_side() {
    let mut asked = Vec::new();
    // Each description takes a while, as a real one does, so services side by
    // side have their asks in flight at once.
    carrick::agent_service::set_mock_latency(
        "/generate-intent",
        Some(std::time::Duration::from_millis(400)),
    );
    for width in [1, 4] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (repo, cassette) = committed_copy(tmp.path(), &[]);
        mock_env(&cassette, width);
        // SAFETY: `#[serial]`, as above.
        unsafe {
            std::env::remove_var("CARRICK_SKIP_INTENTS");
            // One function per request, so requests count functions.
            std::env::set_var("CARRICK_INTENT_BATCH_SIZE", "1");
        }
        let before = requests_to("/generate-intent");
        run_analysis_engine_with_sidecar(
            StubStorage::default(),
            repo.to_str().unwrap(),
            None,
            false,
        )
        .await
        .expect("the scan succeeds");
        asked.push(requests_to("/generate-intent") - before);
        let passes = take_schedules();
        if width > 1 {
            assert!(passes[0].most_in_flight >= 2, "{:?}", passes[0]);
        }
    }
    carrick::agent_service::set_mock_latency("/generate-intent", None);
    // SAFETY: `#[serial]`, as above.
    unsafe {
        std::env::remove_var("CARRICK_INTENT_BATCH_SIZE");
    }
    assert!(asked[0] > 0, "the scan must describe functions");
    assert_eq!(
        asked[1], asked[0],
        "width 4 asked for {} function(s), width 1 for {}",
        asked[1], asked[0]
    );
}
