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
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::findings::PrResultPayload;
use carrick::pr_baseline::{MainSideFault, inject_mock_main_side_fault, main_side_runs};
use carrick::services::type_sidecar::TypeSidecar;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The tests in this binary share the process environment and the injected
/// fault, so they run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// In-memory index plus every PR result the run posted.
#[derive(Default, Clone)]
struct Store {
    repos: Arc<Mutex<Vec<CloudRepoData>>>,
    pr_results: Arc<Mutex<Vec<PrResultPayload>>>,
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
    let delta = pr_delta(&store, &producer).await;
    assert!(
        !new_paths(&delta).is_empty(),
        "this release's index lacks the endpoint, so the PR lists it: {delta:#}"
    );

    for other in [Some("0.0.1"), None] {
        let store = Store::default();
        scan(&store, &producer).await;
        as_written_by(&store, &repo_name, other);
        let delta = pr_delta(&store, &producer).await;
        assert!(delta.is_null(), "{other:?}: {delta:#}");
    }
}
