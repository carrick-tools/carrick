//! carrick#1564 end to end: calls through a library client read through the
//! client semantics framework detection states, as the REAL type sidecar
//! verifies them against the fixture's vendored declarations.
//!
//! Framework detection is replayed from the contract sample, byte for byte.
//! The model's per-file answers are wrong the way a model's are for these
//! calls: the verb a method name suggests, no base, no body. Every assertion
//! on a stated row is about what the scanner read through a verified claim,
//! never about what a cassette said; where a site must stay the model's, the
//! assertion is that the cassette's wrong row is still what the index holds.
//!
//! The rows are read off the uploaded blob's mount graph, which is what the
//! cloud reads. See `tests/fixtures/client-semantics/README.md` for the shape
//! and the answer key.
//!
//! Every test here is `#[serial]`: the mock environment and the request
//! counters are process-global.

use async_trait::async_trait;
use carrick::agents::file_analyzer_agent::ResolutionSource;
use carrick::client_semantics::{SemanticsStatus, derive_claims};
use carrick::cloud_storage::{CloudRepoData, CloudStorage, StorageError, UploadOutcome};
use carrick::engine::run_analysis_engine_with_sidecar;
use carrick::mount_graph::DataFetchingCall;
use carrick::services::type_sidecar::{SemanticsVerdict, TypeSidecar};
use serial_test::serial;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// In-memory storage: an upload replaces the stored row for its repo, so the
/// next scan's previous generation is the scan before it.
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

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/client-semantics")
}

/// Which packages the copy has installed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Install {
    /// The vendored declarations, as checked in.
    Vendored,
    /// `@fixture/http` as a release whose `create` takes another option key
    /// than the one framework detection claims.
    KeyAbsent,
    /// No `node_modules` at all.
    None,
    /// A Deno service importing both packages through `npm:` specifiers into
    /// Deno's own cache, which holds nothing: the packages resolve nowhere.
    DenoUncached,
    /// The vendored declarations, and the package detection leaves
    /// `pending` installed too.
    PendingInstalled,
}

/// `deno.json` for [`Install::DenoUncached`].
const DENO_CONFIG: &str = r#"{
  "nodeModulesDir": "none",
  "imports": {
    "@fixture/http": "npm:@fixture/http@^1.4.0",
    "fixture-prefix-http": "npm:fixture-prefix-http@^2.1.0"
  }
}
"#;

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

/// A committed copy of the fixture with `install` in place, and the cassette
/// directory to replay the model from. Committed so a second scan has a
/// previous commit to reuse; the declaration variants are not part of the
/// scanned tree.
fn fixture_copy(tmp: &Path, install: Install) -> (PathBuf, PathBuf) {
    let repo = tmp.join("service");
    copy_dir(&fixture_root(), &repo);
    std::fs::remove_dir_all(repo.join("variants")).unwrap();
    match install {
        Install::Vendored => {}
        Install::KeyAbsent => {
            std::fs::copy(
                fixture_root().join("variants/http-without-base-key.d.ts"),
                repo.join("node_modules/@fixture/http/index.d.ts"),
            )
            .unwrap();
        }
        Install::None => std::fs::remove_dir_all(repo.join("node_modules")).unwrap(),
        Install::DenoUncached => {
            std::fs::remove_dir_all(repo.join("node_modules")).unwrap();
            std::fs::write(repo.join("deno.json"), DENO_CONFIG).unwrap();
        }
        Install::PendingInstalled => {
            let manifest = repo.join("node_modules/fixture-slow-http/package.json");
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            std::fs::write(
                manifest,
                r#"{ "name": "fixture-slow-http", "version": "3.0.2" }"#,
            )
            .unwrap();
        }
    }
    run_git(&repo, &["init", "-q"]);
    // The copy's own node_modules is part of the fixture, not an install to
    // ignore, so a clean `git diff` between two scans means what it says.
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
        std::env::remove_var("GITHUB_EVENT_NAME");
        std::env::remove_var("GITHUB_REF");
        std::env::remove_var("CARRICK_OUTPUT_JSON");
        // The in-scan schedule's waits, without the sleeping.
        std::env::set_var(carrick::client_semantics::PENDING_REASK_WAITS_ENV, "0,0");
    }
}

/// The real type sidecar, built from `src/sidecar`, initialised on `repo`.
/// Required, not optional: this binary is the one place a field renamed on
/// either side of the protocol would show up, because the Rust protocol tests
/// read hand-written JSON.
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

async fn scan(storage: &StubStorage, repo: &Path, sidecar: Option<&TypeSidecar>) -> CloudRepoData {
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

/// Every consumer row in the blob's graph.
fn rows(data: &CloudRepoData) -> Vec<DataFetchingCall> {
    let mut rows = data
        .mount_graph
        .as_ref()
        .expect("the blob carries the graph")
        .data_calls
        .clone();
    rows.sort_by(|a, b| a.file_location.cmp(&b.file_location));
    rows
}

/// Rows as sorted JSON, so two scans compare field by field in any order.
fn rendered(rows: Vec<DataFetchingCall>) -> Vec<String> {
    let mut rendered: Vec<String> = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect();
    rendered.sort();
    rendered
}

/// The rows a scan of `repo` states when detection answers the same four
/// lists and no library semantics: what every site reads as without them.
/// The cassette is restored before this returns.
async fn rows_without_semantics(
    repo: &Path,
    cassette: &Path,
    sidecar: &TypeSidecar,
) -> Vec<DataFetchingCall> {
    let path = cassette.join("framework-detect/framework-detect.json");
    let original = std::fs::read_to_string(&path).unwrap();
    let mut answer: serde_json::Value = serde_json::from_str(&original).unwrap();
    answer
        .as_object_mut()
        .expect("the answer is an object")
        .remove("client_semantics")
        .expect("the sample answers library semantics");
    std::fs::write(&path, answer.to_string()).unwrap();
    let without = rows(&scan(&StubStorage::default(), repo, Some(sidecar)).await);
    std::fs::write(&path, original).unwrap();
    without
}

fn rows_at(rows: &[DataFetchingCall], file: &str, line: u32) -> Vec<DataFetchingCall> {
    rows.iter()
        .filter(|row| row.line == Some(line) && row.file_location.contains(file))
        .cloned()
        .collect()
}

/// The one row at `file:line`.
fn row_at(rows: &[DataFetchingCall], file: &str, line: u32) -> DataFetchingCall {
    let at = rows_at(rows, file, line);
    assert_eq!(at.len(), 1, "expected one row at {file}:{line}: {rows:#?}");
    at.into_iter().next().unwrap()
}

/// A row the scanner read through the client's verified semantics.
fn assert_library_row(
    rows: &[DataFetchingCall],
    file: &str,
    line: u32,
    method: &str,
    target: &str,
    claims: &[&str],
) {
    let row = row_at(rows, file, line);
    assert_eq!(
        row.resolution_source,
        Some(ResolutionSource::RequestSummary),
        "{file}:{line} is stated by the source through verified semantics: {row:#?}"
    );
    assert_eq!(
        (row.method.as_str(), row.target_url.as_str()),
        (method, target),
        "{file}:{line}: {row:#?}"
    );
    assert_eq!(row.library_semantics, claims, "{file}:{line}: {row:#?}");
}

/// A row read through no library claim: the site as it reads without the
/// semantics, with the base-less target the cassette's model answer states.
/// Which layer states it is the scan's ordinary rules; `source` says which.
fn assert_unread_row(
    rows: &[DataFetchingCall],
    file: &str,
    line: u32,
    source: ResolutionSource,
    method: &str,
    target: &str,
) {
    let row = row_at(rows, file, line);
    assert!(
        row.library_semantics.is_empty(),
        "{file}:{line} was read through no claim: {row:#?}"
    );
    assert_eq!(
        (
            row.resolution_source,
            row.method.as_str(),
            row.target_url.as_str()
        ),
        (Some(source), method, target),
        "{file}:{line} reads as it does without library semantics: {row:#?}"
    );
}

const HTTP: &str = "@fixture/http@1:default";
const PREFIX: &str = "fixture-prefix-http@2:default";

fn ids(prefix: &str, kinds: &[&str]) -> Vec<String> {
    kinds
        .iter()
        .map(|kind| format!("{prefix}:{kind}"))
        .collect()
}

/// The answer key's positive rows, stated through the vendored declarations.
fn assert_answer_key(rows: &[DataFetchingCall]) {
    let http = |kinds: &[&str]| ids(HTTP, kinds);
    let prefix = |kinds: &[&str]| ids(PREFIX, kinds);
    assert_library_row(
        rows,
        "src/http-client.ts",
        6,
        "GET",
        "/api/v1/users",
        &http(&["factory:create", "verb:get"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_library_row(
        rows,
        "src/http-client.ts",
        11,
        "POST",
        "/api/v1/orders",
        &http(&["factory:create", "verb:post", "verb_body:post"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_library_row(
        rows,
        "src/http-client.ts",
        16,
        "POST",
        "/inventory/sync",
        &http(&["request:request:config", "request_body:request:config"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_library_row(
        rows,
        "src/prefix-client.ts",
        7,
        "GET",
        "/svc/jobs/queued",
        &prefix(&["factory:create", "verb:get"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_library_row(
        rows,
        "src/prefix-client.ts",
        11,
        "POST",
        "/svc/jobs/run",
        &prefix(&["factory:create", "verb:post", "verb_body:post"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_library_row(
        rows,
        "src/prefix-client.ts",
        15,
        "PUT",
        "/svc/jobs/reports",
        &prefix(&[
            "factory:create",
            "request:():path_options",
            "request_body:():path_options",
        ])
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>(),
    );
}

/// The base path prefix and the prefix-style factory both join, the verb and
/// the body come from the verified claims, and the model's wrong verbs and
/// base-less paths are gone from every site.
#[tokio::test]
#[serial]
async fn verified_semantics_state_the_joined_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let data = scan(&StubStorage::default(), &repo, Some(&sidecar)).await;
    assert_answer_key(&rows(&data));
}

/// `new Map().get("/r")`, a `Map` held in a module constant, and a `Map`
/// bound to the client's own name reach no claim and state no row.
#[tokio::test]
#[serial]
async fn a_map_is_never_read_as_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    let negatives: Vec<_> = rows
        .iter()
        .filter(|row| row.file_location.contains("src/negatives.ts"))
        .collect();
    assert!(negatives.is_empty(), "{negatives:#?}");
}

/// The re-review's sites (carrick#1564 re-review, R1, R2 and R6), through
/// the real sidecar. A base key written after every spread, beside a method
/// or under a string key, or agreed by both branches of a conditional
/// spread, is read. A key a later spread, a getter, a computed key, a
/// disagreeing or possibly empty conditional spread, a written constant, a
/// constructor branch or loop, a method, or a write through the client or
/// an alias of it may change reads exactly as the tree does without the
/// semantics.
#[tokio::test]
#[serial]
async fn the_rereviews_sites_read_a_base_only_where_nothing_can_change_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    let get = ids(HTTP, &["factory:create", "verb:get"]);
    let get: Vec<&str> = get.iter().map(String::as_str).collect();
    for (file, line, target) in [
        ("src/n1-spread-before.ts", 7, "/after/stated"),
        ("src/n3-cond-spread.ts", 10, "/x/keep"),
        ("src/n3-cond-spread.ts", 11, "/same/agree"),
        ("src/n4-getter-computed.ts", 11, "/s1/string-key"),
        ("src/n4-getter-computed.ts", 12, "/m1/method-prop"),
    ] {
        assert_library_row(&rows, file, line, "GET", target, &get);
    }
    let unread = |rows: &[DataFetchingCall], file: &str, lines: &[u32]| {
        rendered(
            rows.iter()
                .filter(|row| {
                    row.file_location.contains(file)
                        && row.line.is_some_and(|line| lines.contains(&line))
                })
                .cloned()
                .collect(),
        )
    };
    for (file, lines) in [
        ("src/n2-two-spreads.ts", &[8][..]),
        ("src/n3-cond-spread.ts", &[9, 12][..]),
        ("src/n4-getter-computed.ts", &[9, 10][..]),
        ("src/n5-ctor-try-loop.ts", &[12, 23][..]),
        ("src/n6-method-write.ts", &[8, 16][..]),
        ("src/n7-mutated-spread-const.ts", &[10][..]),
        ("src/n8-indirect-write.ts", &[10, 11][..]),
        ("src/n9-phase1-fetch.ts", &[9, 10, 11, 12, 13][..]),
    ] {
        assert_eq!(
            unread(&rows, file, lines),
            unread(&without, file, lines),
            "{file} {lines:?} reads as it does without the semantics"
        );
        assert!(
            rows.iter()
                .filter(|row| {
                    row.file_location.contains(file)
                        && row.line.is_some_and(|line| lines.contains(&line))
                })
                .all(|row| row.library_semantics.is_empty()),
            "{file} {lines:?} is read through no claim"
        );
    }
}

/// The third review's sites (carrick#1564), through the real sidecar. A
/// client a file stores, returns, configures, reads a property of, or calls
/// a member outside the verified surface of reads exactly as the tree does
/// without the semantics, as does one built from options the file passes to
/// a call before the base; a base written after that spread, a client used
/// only to call through it, and one whose export is only tested with
/// `instanceof` (carrick#1568), are read. A plain `fetch` handed a constant
/// the file writes through states nothing, and one handed a clean constant
/// states its method.
#[tokio::test]
#[serial]
async fn the_third_reviews_sites_read_nothing_the_file_can_change() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    let get = ids(HTTP, &["factory:create", "verb:get"]);
    let get: Vec<&str> = get.iter().map(String::as_str).collect();
    assert_library_row(
        &rows,
        "src/r1-passed-const.ts",
        10,
        "GET",
        "/r1b/passed-after",
        &get,
    );
    assert_library_row(&rows, "src/r4c-control.ts", 5, "GET", "/r4c/control", &get);
    assert_library_row(
        &rows,
        "src/r4b-instanceof.ts",
        6,
        "GET",
        "/r4b/instanceof",
        &get,
    );
    let at = |rows: &[DataFetchingCall], file: &str, line: u32| {
        rendered(
            rows.iter()
                .filter(|row| row.file_location.contains(file) && row.line == Some(line))
                .cloned()
                .collect(),
        )
    };
    for (file, line) in [
        ("src/r1-passed-const.ts", 9),
        ("src/r2-stored.ts", 9),
        ("src/r3-returned.ts", 6),
        ("src/r4-export-read.ts", 6),
        ("src/r5-setter.ts", 6),
        ("src/r6-header-write.ts", 6),
        ("src/r7-interceptor.ts", 6),
    ] {
        assert_eq!(
            at(&rows, file, line),
            at(&without, file, line),
            "{file}:{line} reads as it does without the semantics"
        );
        assert!(
            rows.iter()
                .filter(|row| row.file_location.contains(file) && row.line == Some(line))
                .all(|row| row.library_semantics.is_empty()),
            "{file}:{line} is read through no claim"
        );
    }

    let stated = |line: u32| -> Vec<(String, String)> {
        rows.iter()
            .filter(|row| {
                row.file_location.contains("src/p-fetch-consts.ts")
                    && row.line == Some(line)
                    && row.resolution_source == Some(ResolutionSource::RequestSummary)
            })
            .map(|row| (row.method.clone(), row.target_url.clone()))
            .collect()
    };
    assert!(
        stated(15).is_empty(),
        "a written-through constant states nothing"
    );
    assert_eq!(stated(18), [("PATCH".to_string(), "/api/p4".to_string())]);
    assert_eq!(stated(20), [("GET".to_string(), "/api/p6".to_string())]);
}

/// carrick#1568: an instance one module builds and exports states the joined
/// row, with the claims it was read through, at each call in the modules
/// that import it: directly, renamed through a barrel, and as an anonymous
/// default through a barrel or directly. A call through a name the importing
/// file declares again states none, and a call through the import in that
/// file reads exactly as it does without the semantics.
#[tokio::test]
#[serial]
async fn an_imported_instance_states_the_joined_row_at_each_call() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    let get = ids(HTTP, &["factory:create", "verb:get"]);
    let get: Vec<&str> = get.iter().map(String::as_str).collect();
    let post = ids(HTTP, &["factory:create", "verb:post", "verb_body:post"]);
    let post: Vec<&str> = post.iter().map(String::as_str).collect();

    assert_library_row(&rows, "src/i1-users.ts", 3, "GET", "/shared/v1/users", &get);
    assert_library_row(
        &rows,
        "src/i2-orders.ts",
        3,
        "POST",
        "/shared/v1/orders",
        &post,
    );
    for (line, target) in [
        (4, "/shared/v1/renamed"),
        (5, "/svc/barrel-default"),
        (6, "/svc/default"),
    ] {
        assert_library_row(&rows, "src/i4-barrel.ts", line, "GET", target, &get);
    }

    assert!(
        rows_at(&rows, "src/i3-shadowed.ts", 5).is_empty(),
        "a call through the name declared again states nothing: {rows:#?}"
    );
    let at = |rows: &[DataFetchingCall]| rendered(rows_at(rows, "src/i3-shadowed.ts", 8));
    assert_eq!(
        at(&rows),
        at(&without),
        "the import in a file that declares its name again reads as it does without the semantics"
    );
    assert!(
        rows_at(&rows, "src/i3-shadowed.ts", 8)
            .iter()
            .all(|row| row.library_semantics.is_empty()),
        "{rows:#?}"
    );
}

/// The review's adversarial sites (carrick#1564 review, findings 1 to 4): a
/// base key written through the instance, a base named in the call's own
/// options, options and a config open to a spread, a field a constructor
/// branch writes again, a field a subclass declares again, a static member
/// reading `this`, and a block redeclaring the instance's name. Each reads
/// exactly as the same tree does without the semantics; the one row they add
/// is the instance method reading its own class's instance field.
#[tokio::test]
#[serial]
async fn the_reviews_adversarial_sites_read_as_they_do_without_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    let in_file = |rows: &[DataFetchingCall], file: &str| -> Vec<DataFetchingCall> {
        rows.iter()
            .filter(|row| row.file_location.contains(file))
            .cloned()
            .collect()
    };
    for file in [
        "src/a1-mutated.ts",
        "src/a2-percall-base.ts",
        "src/a3-spread-after.ts",
        "src/a4-ctor-branch.ts",
        "src/a13-inherit.ts",
    ] {
        assert_eq!(
            rendered(in_file(&rows, file)),
            rendered(in_file(&without, file)),
            "{file} reads as it does without the semantics"
        );
    }

    // What that is: the receiver-type rows the site's own literals state,
    // and nothing where the receiver's type says nothing.
    let receiver = ResolutionSource::ReceiverType;
    assert_unread_row(&rows, "src/a1-mutated.ts", 7, receiver, "GET", "/mutated");
    assert_unread_row(
        &rows,
        "src/a2-percall-base.ts",
        6,
        receiver,
        "GET",
        "/users",
    );
    assert_unread_row(
        &rows,
        "src/a2-percall-base.ts",
        10,
        receiver,
        "GET",
        "/plain",
    );
    assert_unread_row(
        &rows,
        "src/a3-spread-after.ts",
        7,
        receiver,
        "GET",
        "/spread",
    );
    assert_unread_row(
        &rows,
        "src/a13-inherit.ts",
        20,
        receiver,
        "GET",
        "/outer-ok",
    );
    for (file, line) in [
        ("src/a3-spread-after.ts", 11),
        ("src/a4-ctor-branch.ts", 12),
        ("src/a13-inherit.ts", 6),
        ("src/a13-inherit.ts", 18),
        ("src/a5-static.ts", 7),
    ] {
        assert!(
            rows_at(&rows, file, line).is_empty(),
            "{file}:{line}: {rows:#?}"
        );
    }
    assert_library_row(
        &rows,
        "src/a5-static.ts",
        10,
        "GET",
        "/instance/ran",
        &[
            "@fixture/http@1:default:factory:create",
            "@fixture/http@1:default:verb:get",
        ],
    );
}

/// A claimed option key the declarations do not have fails the factory
/// claim, so the instance built with it is read through nothing: its sites
/// stay the model's candidates, even though the sidecar verifies the
/// instance's own verbs. The export's own claims still stand.
#[tokio::test]
#[serial]
async fn a_claimed_option_key_absent_from_the_declarations_stays_a_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::KeyAbsent);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    // The premise, from the sidecar itself: the factory fails on the key,
    // and the verb on the instance it returns verifies.
    let sample: carrick::framework_detector::DetectionResult = serde_json::from_str(
        &std::fs::read_to_string(cassette.join("framework-detect/framework-detect.json")).unwrap(),
    )
    .unwrap();
    let checks = derive_claims(&sample.client_semantics.unwrap()).checks();
    let results = sidecar
        .verify_client_semantics(&repo.canonicalize().unwrap(), &checks)
        .expect("the sidecar answers");
    let verdict = |claim_id: &str, receiver: &str| {
        results
            .iter()
            .find(|result| result.claim_id == claim_id && result.receiver == receiver)
            .map(|result| (result.verdict, result.reason.clone()))
    };
    assert_eq!(
        verdict(&format!("{HTTP}:factory:create"), "export"),
        Some((SemanticsVerdict::Failed, Some("key_missing".to_string())))
    );
    assert_eq!(
        verdict(&format!("{HTTP}:verb:get"), "instance:create").map(|(v, _)| v),
        Some(SemanticsVerdict::Verified),
        "the instance's verb verifies on its own; only the scanner's factory gate drops it"
    );

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    // The instance's sites read exactly as they do with no semantics at all:
    // the receiver-type rows the site's own literals state (carrick#695),
    // with no base, since nothing verified says where the base is.
    for line in [6, 11] {
        assert_eq!(
            rendered(rows_at(&rows, "src/http-client.ts", line)),
            rendered(rows_at(&without, "src/http-client.ts", line)),
            "src/http-client.ts:{line} must read as it does without the semantics"
        );
    }
    // So do the calls in the modules that import an instance (carrick#1568).
    for (file, line) in [("src/i1-users.ts", 3), ("src/i4-barrel.ts", 4)] {
        assert_eq!(
            rendered(rows_at(&rows, file, line)),
            rendered(rows_at(&without, file, line)),
            "{file}:{line} must read as it does without the semantics"
        );
        assert!(
            rows_at(&rows, file, line)
                .iter()
                .all(|row| row.library_semantics.is_empty()),
            "{file}:{line} is read through no claim"
        );
    }
    assert_unread_row(
        &rows,
        "src/http-client.ts",
        6,
        ResolutionSource::ReceiverType,
        "GET",
        "/users",
    );
    assert_unread_row(
        &rows,
        "src/http-client.ts",
        11,
        ResolutionSource::ReceiverType,
        "POST",
        "/orders",
    );
    assert_library_row(
        &rows,
        "src/http-client.ts",
        16,
        "POST",
        "/inventory/sync",
        &[
            "@fixture/http@1:default:request:request:config",
            "@fixture/http@1:default:request_body:request:config",
        ],
    );
    // The other package's claims are untouched.
    assert_library_row(
        &rows,
        "src/prefix-client.ts",
        7,
        "GET",
        "/svc/jobs/queued",
        &[
            "fixture-prefix-http@2:default:factory:create",
            "fixture-prefix-http@2:default:verb:get",
        ],
    );
}

/// Nothing installed, nothing verified: every site reads exactly as it does
/// without library semantics, and the same source that states the answer key
/// above states none of it here.
#[tokio::test]
#[serial]
async fn without_node_modules_every_site_stays_a_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::None);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    assert_eq!(
        rendered(rows.clone()),
        rendered(without),
        "with nothing installed, the semantics change no row"
    );
    // What that is, on the sites the answer key states: the model's rows,
    // base-less, with the verb the call itself is spelled with; and nothing
    // at the two prefix-client sites whose model answer names no route.
    for (line, method, target) in [(6, "GET", "/users"), (11, "POST", "/orders")] {
        assert_unread_row(
            &rows,
            "src/http-client.ts",
            line,
            ResolutionSource::Model,
            method,
            target,
        );
    }
    assert_unread_row(
        &rows,
        "src/prefix-client.ts",
        11,
        ResolutionSource::Model,
        "POST",
        "/run",
    );
    for line in [7, 15] {
        assert!(rows_at(&rows, "src/prefix-client.ts", line).is_empty());
    }
}

/// A Deno service whose packages the sidecar cannot read (carrick#1570: a
/// Deno service reads no library semantics yet) scans to the end, and every
/// site reads as it does without the semantics.
#[tokio::test]
#[serial]
async fn a_deno_service_whose_packages_do_not_resolve_states_no_library_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::DenoUncached);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    // The premise: every check is unchecked, none verified.
    let sample: carrick::framework_detector::DetectionResult = serde_json::from_str(
        &std::fs::read_to_string(cassette.join("framework-detect/framework-detect.json")).unwrap(),
    )
    .unwrap();
    let checks = derive_claims(&sample.client_semantics.unwrap()).checks();
    let results = sidecar
        .verify_client_semantics(&repo.canonicalize().unwrap(), &checks)
        .expect("the sidecar answers a Deno service too");
    assert!(
        results
            .iter()
            .all(|result| result.verdict == SemanticsVerdict::Unchecked),
        "{results:#?}"
    );

    let without = rows_without_semantics(&repo, &cassette, &sidecar).await;
    let rows = rows(&scan(&StubStorage::default(), &repo, Some(&sidecar)).await);
    assert!(
        rows.iter().all(|row| row.library_semantics.is_empty()),
        "{rows:#?}"
    );
    assert_eq!(rendered(rows), rendered(without));
}

/// Two scans of one tree state the same rows, claim ids included.
#[tokio::test]
#[serial]
async fn two_identical_scans_state_identical_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let first = rendered(rows(
        &scan(&StubStorage::default(), &repo, Some(&sidecar)).await,
    ));
    let second = rendered(rows(
        &scan(&StubStorage::default(), &repo, Some(&sidecar)).await,
    ));
    assert!(first.iter().any(|row| row.contains("library_semantics")));
    assert_eq!(first, second);
}

// ---------------------------------------------------------------------------
// The re-ask rule: no CACHE_VERSION bump, a cached detection is asked again.
// ---------------------------------------------------------------------------

fn requests_to(route: &str) -> usize {
    carrick::agent_service::request_counts()
        .get(route)
        .copied()
        .unwrap_or(0)
}

/// Requests to detection and to guidance made by one scan.
async fn scan_counting(storage: &StubStorage, repo: &Path) -> (CloudRepoData, usize, usize) {
    let (detect, guidance) = (
        requests_to("/framework-detect"),
        requests_to("/framework-guidance"),
    );
    let data = scan(storage, repo, None).await;
    (
        data,
        requests_to("/framework-detect") - detect,
        requests_to("/framework-guidance") - guidance,
    )
}

/// Rewrite the stored blob's cached detection before the next scan reads it.
fn age_cached_detection(
    storage: &StubStorage,
    edit: impl FnOnce(&mut carrick::framework_detector::DetectionResult),
) {
    let mut repos = storage.repos.lock().unwrap();
    let previous = repos.last_mut().expect("a previous scan uploaded");
    assert!(previous.file_results.is_some(), "the cache is populated");
    edit(
        previous
            .cached_detection
            .as_mut()
            .expect("the blob carries the detection"),
    );
}

/// A detection written before the field existed reads as never asked: the
/// next scan asks detection once, and not guidance, because the four lists
/// come back the same.
#[tokio::test]
#[serial]
async fn a_detection_never_asked_is_asked_again_once_and_guidance_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let storage = StubStorage::default();
    scan(&storage, &repo, None).await;
    age_cached_detection(&storage, |detection| detection.client_semantics = None);

    let (data, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!((detect, guidance), (1, 0));
    let semantics = data
        .cached_detection
        .and_then(|detection| detection.client_semantics)
        .expect("the answer is kept for the next scan");
    assert_eq!(semantics.len(), 4);
}

/// The re-ask is best-effort: when it fails, the cached detection and its
/// guidance stand, and the service is analysed as it was, not deferred.
#[tokio::test]
#[serial]
async fn a_failed_reask_keeps_the_cached_detection() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let storage = StubStorage::default();
    scan(&storage, &repo, None).await;
    age_cached_detection(&storage, |detection| detection.client_semantics = None);

    carrick::agent_service::inject_mock_failure("/framework-detect", "ask_client_semantics", 1);
    let (data, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!((detect, guidance), (1, 0));
    let detection = data
        .cached_detection
        .clone()
        .expect("the cached detection stands");
    assert_eq!(detection.client_semantics, None, "nothing new was answered");
    assert_eq!(detection.data_fetchers.len(), 4);
    assert!(
        data.cached_guidance.is_some(),
        "the service was not deferred: its guidance is persisted"
    );
    assert!(
        rows(&data)
            .iter()
            .any(|row| row.resolution_source == Some(ResolutionSource::Model)),
        "the model's cached answers still joined"
    );
}

/// An answer that names other packages than the cached detection changes
/// what guidance and the analysis are keyed on, so guidance is asked again,
/// from the answer already in hand: no second detection request.
#[tokio::test]
#[serial]
async fn a_reask_that_names_other_packages_asks_for_guidance_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let storage = StubStorage::default();
    scan(&storage, &repo, None).await;
    age_cached_detection(&storage, |detection| {
        detection.client_semantics = None;
        detection
            .data_fetchers
            .retain(|package| package != "fixture-slow-http");
    });

    let (data, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!(detect, 1, "detection is asked once");
    assert!(guidance > 0, "guidance is asked again for the new lists");
    let detection = data.cached_detection.expect("the new detection is kept");
    assert_eq!(detection.data_fetchers.len(), 4);
    assert!(detection.client_semantics.is_some());
}

/// An entry still `pending` for a package that is installed is asked again
/// on the next scan: once across scans, then twice more on the in-scan
/// schedule, which the mock's fixed answer never settles.
#[tokio::test]
#[serial]
async fn a_pending_entry_is_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::PendingInstalled);
    mock_env(&cassette);
    let storage = StubStorage::default();
    let first = scan(&storage, &repo, None).await;
    assert!(
        first
            .cached_detection
            .and_then(|detection| detection.client_semantics)
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry.status == SemanticsStatus::Pending)),
        "the sample leaves one package pending"
    );

    let (_, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!((detect, guidance), (3, 0));
}

/// A package that stays `pending` on every ask of the in-scan schedule is
/// asked about exactly three times in the scan (owner ruling on
/// carrick#1564): the first detection and two re-asks. The rows the other
/// packages' semantics state do not move, and the pending entry is kept for
/// the next scan.
#[tokio::test]
#[serial]
async fn a_package_pending_on_every_ask_is_asked_three_times_in_one_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::PendingInstalled);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    let detect = requests_to("/framework-detect");
    let data = scan(&StubStorage::default(), &repo, Some(&sidecar)).await;
    assert_eq!(requests_to("/framework-detect") - detect, 3);
    assert_answer_key(&rows(&data));
    assert!(
        data.cached_detection
            .and_then(|detection| detection.client_semantics)
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry.package == "fixture-slow-http" && entry.status == SemanticsStatus::Pending
            })),
        "still pending, for the next scan to ask about"
    );
}

/// A package the scan's first ask leaves `pending` and a re-ask answers
/// states its rows in that same scan: the summaries are composed only once
/// the in-scan schedule has finished (owner ruling on carrick#1564).
#[tokio::test]
#[serial]
async fn a_package_a_reask_answers_states_its_rows_in_the_same_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let sidecar = real_sidecar(&repo);

    // The first ask leaves `@fixture/http` pending; every later one answers
    // the sample as it is.
    let mut first: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(cassette.join("framework-detect/framework-detect.json")).unwrap(),
    )
    .unwrap();
    let entry = first["client_semantics"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["package"] == "@fixture/http")
        .unwrap();
    entry["status"] = "pending".into();
    entry["clients"] = serde_json::json!([]);
    carrick::agent_service::inject_mock_answer("/framework-detect", "", 1, &first.to_string());

    let detect = requests_to("/framework-detect");
    let data = scan(&StubStorage::default(), &repo, Some(&sidecar)).await;
    assert_eq!(requests_to("/framework-detect") - detect, 2);
    assert_answer_key(&rows(&data));
}

/// A service whose model stages were deferred (here its guidance failed)
/// runs no in-scan schedule: its analysis is facts-only and reads no model
/// answer, and the pending entry is kept for the next scan to ask about
/// (carrick#1564 re-review).
#[tokio::test]
#[serial]
async fn a_deferred_service_runs_no_schedule() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::PendingInstalled);
    mock_env(&cassette);
    // SAFETY: every test in this binary is `#[serial]`. A spent budget skips
    // the in-run retry, so the service stays deferred for the whole run.
    unsafe {
        std::env::set_var(carrick::retry_budget::BUDGET_ENV, "0");
    }
    carrick::agent_service::inject_mock_failure("/framework-guidance", "\"task\":\"general\"", 1);
    let storage = StubStorage::default();
    let detect = requests_to("/framework-detect");
    let guidance = requests_to("/framework-guidance");
    let _ = run_analysis_engine_with_sidecar(storage.clone(), repo.to_str().unwrap(), None, false)
        .await;
    // SAFETY: as above.
    unsafe {
        std::env::remove_var(carrick::retry_budget::BUDGET_ENV);
    }
    let data = storage
        .repos
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("the deferred service still uploads its facts");
    assert!(
        requests_to("/framework-guidance") > guidance && data.cached_guidance.is_none(),
        "the guidance failed and is still owed"
    );
    assert_eq!(
        requests_to("/framework-detect") - detect,
        1,
        "the detection only: a deferred service runs no schedule"
    );
    assert!(
        data.cached_detection
            .and_then(|detection| detection.client_semantics)
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry.status == SemanticsStatus::Pending)),
        "the pending entry is kept for the next scan"
    );
}

/// A `pending` package that is not installed is not asked about again,
/// however many other data fetchers are: no answer about it could be
/// verified (carrick#1564 review, finding 7).
#[tokio::test]
#[serial]
async fn a_pending_package_that_is_not_installed_is_not_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let storage = StubStorage::default();
    scan(&storage, &repo, None).await;

    let (_, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!((detect, guidance), (0, 0));
}

/// When a run retries the work it still owes, the service's previous
/// generation is this run's own blob, asked minutes ago: its `pending`
/// entry is not asked about again in the same run (carrick#1564 review,
/// finding 5).
#[tokio::test]
#[serial]
async fn a_retry_of_owed_work_does_not_ask_again_in_the_same_run() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::PendingInstalled);
    mock_env(&cassette);
    // SAFETY: every test in this binary is `#[serial]`.
    unsafe {
        std::env::set_var(carrick::engine::durability::RETRY_DELAY_ENV, "0");
    }
    // One analysis fails, so the service owes it and the run retries it.
    carrick::agent_service::inject_mock_failure("/analyze-file", "JobsGateway", 1);
    let (data, detect, _) = scan_counting(&StubStorage::default(), &repo).await;
    // SAFETY: as above.
    unsafe {
        std::env::remove_var(carrick::engine::durability::RETRY_DELAY_ENV);
    }
    assert!(
        data.file_results.as_ref().is_some_and(|answers| answers
            .keys()
            .any(|file| file.ends_with("prefix-client.ts"))),
        "the run retried the file it owed, and the retry answered"
    );
    assert_eq!(
        detect, 3,
        "the first pass's detection and its schedule's two re-asks; the retry asks nothing"
    );
    assert!(
        data.cached_detection
            .and_then(|detection| detection.client_semantics)
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry.status == SemanticsStatus::Pending)),
        "the pending entry is still there for the next run to ask about"
    );
}

/// Every entry answered (or skipped): nothing is asked again.
#[tokio::test]
#[serial]
async fn an_answered_detection_is_not_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cassette) = fixture_copy(tmp.path(), Install::Vendored);
    mock_env(&cassette);
    let storage = StubStorage::default();
    scan(&storage, &repo, None).await;
    age_cached_detection(&storage, |detection| {
        if let Some(entries) = detection.client_semantics.as_mut() {
            entries.retain(|entry| entry.status != SemanticsStatus::Pending);
        }
    });

    let (_, detect, guidance) = scan_counting(&storage, &repo).await;
    assert_eq!((detect, guidance), (0, 0));
}
