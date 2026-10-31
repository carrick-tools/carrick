//! carrick#2618 slice 3: the scan reads the services a service calls into.
//!
//! The fixture (`tests/fixtures/discovery-siblings/repo/`) has an `app`
//! service that never calls `fetch` itself and a `client` service whose
//! functions do. Scanned as the Action scans it (`carrick .` from the repo
//! root) with the compiler rows and site questions on, models mocked, and
//! the store's label for the default library's `fetch` from the cassette,
//! the app states the requests the client's code makes, at the app's own
//! calls. Its `README.md` is the answer key.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
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

fn sidecar_built() -> bool {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !script.exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return false;
    }
    true
}

/// A committed copy of the fixture's repo under `tmp`.
fn committed_copy(tmp: &Path) -> PathBuf {
    committed_copy_of(tmp, "discovery-siblings", |_| {})
}

/// A committed copy of `fixture`'s repo under `tmp`, `edit`ed before the
/// commit.
fn committed_copy_of(tmp: &Path, fixture: &str, edit: impl FnOnce(&Path)) -> PathBuf {
    let repo = tmp.join("sibling-sites");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{fixture}/repo")),
        &repo,
    );
    edit(&repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    repo
}

/// One scan of `repo` as the Action runs it, into `store`, with `flags`; the
/// stored blob of each service by name. A store shared by two scans lets the
/// second read the first's generation, so `isolated` is off for those.
fn scan(
    repo: &Path,
    store: &Path,
    flags: &[&str],
    isolated: bool,
) -> Vec<(String, serde_json::Value)> {
    let cache = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .args(["--repo", "fixture/siblings"])
        .args(flags)
        .current_dir(repo)
        .env("HOME", home.path())
        .env("CARRICK_LOCAL_STORAGE_DIR", store)
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_UNPREPARED", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        )
        .env("CARRICK_POSITION_ROWS", "1")
        .env("CARRICK_SITE_QUESTIONS", "1");
    if isolated {
        cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
    } else {
        cmd.env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_FACT_FILES",
        "CARRICK_OUTPUT_JSON",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    blobs(store)
}

fn blobs(store: &Path) -> Vec<(String, serde_json::Value)> {
    let mut blobs: Vec<(String, serde_json::Value)> = fs::read_dir(store)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let blob: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).ok()?;
            let service = blob["service_name"].as_str()?.to_string();
            Some((service, blob))
        })
        .collect();
    blobs.sort_by(|a, b| a.0.cmp(&b.0));
    blobs
}

fn blob_of<'a>(blobs: &'a [(String, serde_json::Value)], service: &str) -> &'a serde_json::Value {
    &blobs
        .iter()
        .find(|(name, _)| name == service)
        .unwrap_or_else(|| panic!("no blob for {service}"))
        .1
}

/// Each call row of a blob as `file:line METHOD path`.
fn call_rows(blob: &serde_json::Value) -> Vec<String> {
    let mut rows: Vec<String> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            format!(
                "{} {} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// What the app states at its own calls into the client (the answer key's
/// lines 5, 9 and 14 of `main.ts`).
const APP_ROWS: [&str; 3] = [
    "services/app/src/main.ts:14 DELETE /v1/api/items/:id",
    "services/app/src/main.ts:5 GET /v1/api/items/:id",
    "services/app/src/main.ts:9 POST /v1/api/events",
];

fn assert_app_rows(blobs: &[(String, serde_json::Value)]) {
    let app = call_rows(blob_of(blobs, "app"));
    assert_eq!(app, APP_ROWS, "the app states the client's requests");
    // A run states a request only at a call of its own service's files.
    let client = call_rows(blob_of(blobs, "client"));
    assert!(
        client.iter().all(|row| row.starts_with("packages/client/")),
        "{client:#?}"
    );
}

#[test]
fn a_service_states_the_requests_a_sibling_s_code_makes_at_its_own_calls() {
    if !sidecar_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = committed_copy(tmp.path());
    let store = tempfile::tempdir().unwrap();
    let blobs = scan(&repo, store.path(), &[], true);
    assert_app_rows(&blobs);
}

/// carrick#3083: a walked service states what its compiler walk held.
#[test]
fn a_walked_service_stores_its_walk_s_memory_peak() {
    if !sidecar_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = committed_copy(tmp.path());
    let store = tempfile::tempdir().unwrap();
    let blobs = scan(&repo, store.path(), &[], false);
    for service in ["app", "client"] {
        let peaks = &blob_of(&blobs, service)["memory_peaks"];
        assert!(
            peaks["walk_mb"].as_u64().unwrap_or(0) > 0,
            "{service}: no walk peak: {peaks}"
        );
    }
}

/// A scan of a change to the consumer alone carries the client, walks it for
/// the app's calls, and states the same rows as a full scan.
#[test]
fn a_changed_only_scan_reads_a_carried_sibling_for_the_same_rows() {
    if !sidecar_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = committed_copy(tmp.path());
    let store = tempfile::tempdir().unwrap();
    let full = scan(&repo, store.path(), &[], false);
    assert_app_rows(&full);
    let before = blob_of(&full, "client")["commit_hash"].clone();
    // A change to the app alone, after every line the rows sit on.
    let main = repo.join("services/app/src/main.ts");
    let mut text = fs::read_to_string(&main).unwrap();
    text.push_str("\nexport function more(): number {\n  return 1;\n}\n");
    fs::write(&main, text).unwrap();
    run_git(&repo, &["add", "-A"]);
    run_git(&repo, &["commit", "-q", "-m", "app only"]);
    let scoped = scan(&repo, store.path(), &["--changed-only"], false);
    assert_eq!(
        blob_of(&scoped, "client")["commit_hash"],
        before,
        "the client is carried"
    );
    assert_ne!(
        blob_of(&scoped, "app")["commit_hash"],
        blob_of(&full, "app")["commit_hash"],
        "the app is analysed"
    );
    assert_app_rows(&scoped);
}

/// The client's kept rows file in `store`, and its record of what it was
/// walked from.
fn kept_client_rows(store: &Path) -> PathBuf {
    fs::read_dir(store.join("positions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with("__client.jsonl.gz"))
        .expect("the full scan kept the client's rows")
}

/// A file's identity and write time: changed by every walk, which removes the
/// rows file and renames a new one in.
fn identity(path: &Path) -> (u64, std::time::SystemTime) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).unwrap();
    (meta.ino(), meta.modified().unwrap())
}

fn unzipped(path: &Path) -> String {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn zip_to(path: &Path, text: &str) {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(text.as_bytes()).unwrap();
    fs::write(path, gz.finish().unwrap()).unwrap();
}

/// A change to the app alone, committed after every line the rows sit on.
fn change_the_app(repo: &Path) {
    let main = repo.join("services/app/src/main.ts");
    let mut text = fs::read_to_string(&main).unwrap();
    text.push_str("\nexport function more(): number {\n  return 1;\n}\n");
    fs::write(&main, text).unwrap();
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-q", "-m", "app only"]);
}

/// carrick#3027: a pass that carries the client uses the rows the last pass
/// kept for it, instead of walking it again, and states the same rows.
#[test]
fn a_carried_service_s_kept_rows_are_used_not_walked() {
    if !sidecar_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = committed_copy(tmp.path());
    let store = tempfile::tempdir().unwrap();
    scan(&repo, store.path(), &[], false);
    let kept = kept_client_rows(store.path());
    let before = identity(&kept);
    change_the_app(&repo);
    let scoped = scan(&repo, store.path(), &["--changed-only"], false);
    assert_eq!(
        identity(&kept),
        before,
        "the client's rows file was not walked again"
    );
    assert_app_rows(&scoped);
}

/// carrick#3027: kept rows whose file no longer hashes as they say are
/// walked again, though change_scope still carries the service.
#[test]
fn a_kept_rows_file_that_does_not_bind_is_walked_again() {
    if !sidecar_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = committed_copy(tmp.path());
    let store = tempfile::tempdir().unwrap();
    scan(&repo, store.path(), &[], false);
    let kept = kept_client_rows(store.path());
    let honest = unzipped(&kept);
    zip_to(&kept, &honest.replace("\"h\":\"", "\"h\":\"0"));
    let before = identity(&kept);
    change_the_app(&repo);
    let scoped = scan(&repo, store.path(), &["--changed-only"], false);
    assert_ne!(identity(&kept), before, "the client was walked again");
    assert_eq!(unzipped(&kept), honest, "and its rows are the honest ones");
    assert_app_rows(&scoped);
}

/// carrick#3027: rows used again are the rows a walk writes, and the scan
/// states the same call rows either way.
#[test]
fn kept_rows_are_byte_identical_to_a_walk_of_the_same_service() {
    if !sidecar_built() {
        return;
    }
    let arm = |forget: bool| {
        let tmp = tempfile::tempdir().unwrap();
        let repo = committed_copy(tmp.path());
        let store = tempfile::tempdir().unwrap();
        scan(&repo, store.path(), &[], false);
        let kept = kept_client_rows(store.path());
        if forget {
            fs::remove_file(&kept).unwrap();
        }
        let before = (!forget).then(|| identity(&kept));
        change_the_app(&repo);
        let scoped = scan(&repo, store.path(), &["--changed-only"], false);
        if let Some(before) = before {
            assert_eq!(identity(&kept), before, "used, not walked");
        }
        (
            unzipped(&kept),
            sourced_call_rows(blob_of(&scoped, "app")),
            sourced_call_rows(blob_of(&scoped, "client")),
        )
    };
    assert_eq!(arm(false), arm(true));
}

/// Each call row of a blob as `file:line METHOD path source`.
fn sourced_call_rows(blob: &serde_json::Value) -> Vec<String> {
    let mut rows: Vec<String> = blob["calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            format!(
                "{} {} {} {}",
                call["file_path"].as_str().unwrap_or_default(),
                call["key"]["method"].as_str().unwrap_or_default(),
                call["key"]["path"].as_str().unwrap_or_default(),
                call["resolution_source"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// carrick#2618, `tests/fixtures/sibling-wrapper/README.md`: every
/// operation the app reaches through the client ends with exactly one fact
/// row, at a wrapper's call into the client while no leaf row states it at
/// the wrapper's caller. Scanned once with no reader answer for `api.ts`
/// (the reader states nothing there) and once with the reader stating the
/// wrappers' lines (`reader/analyze-file/api.json`): a site row replaces
/// the reader's, and no model row is left.
#[test]
fn each_operation_through_a_client_wrapper_keeps_one_fact_row() {
    if !sidecar_built() {
        return;
    }
    const WRAPPER_ROWS: [&str; 5] = [
        "services/app/src/api.ts:10 GET /api/status request_summary",
        "services/app/src/api.ts:14 GET /api/version request_summary",
        "services/app/src/api.ts:15 GET /api/items/1 request_summary",
        "services/app/src/api.ts:6 GET /api/items/:id request_summary",
        "services/app/src/commands.ts:21 GET /api/health request_summary",
    ];
    let reader = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sibling-wrapper/reader/analyze-file/api.json");
    for answered in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = committed_copy_of(tmp.path(), "sibling-wrapper", |repo| {
            if answered {
                let dir = repo.join("__llm__/analyze-file");
                fs::create_dir_all(&dir).unwrap();
                fs::copy(&reader, dir.join("api.json")).unwrap();
            }
        });
        let store = tempfile::tempdir().unwrap();
        let blobs = scan(&repo, store.path(), &[], true);
        assert_eq!(
            sourced_call_rows(blob_of(&blobs, "app")),
            WRAPPER_ROWS,
            "reader answer for api.ts: {answered}"
        );
    }
}
