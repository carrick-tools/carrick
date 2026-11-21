//! Functions served one per folder (`supabase/functions/<name>/index.ts`)
//! reach the index as one fact row each, `ALL /functions/v1/<name>`, stated
//! by the routing convention the platform's config file selects. The model's
//! guess for the same file folds onto that row and is never a second one. An
//! entry file whose handler is not located (an imported handler, no serving
//! call) still states its row, with no handler, and is saved as an unread
//! site.
//!
//! Drives the real scanner binary on `.`, offline, over a committed copy of
//! `tests/fixtures/hosted-functions` (its answer key is the README there),
//! once with its `carrick.json` (the folder is its own service) and once
//! without (one service at the root).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

/// Every blob the scan of the fixture writes, with `carrick.json` kept or
/// removed, and no store answer (P1's case).
fn scan_blobs(with_config: bool) -> Vec<serde_json::Value> {
    scans(Tree {
        config: with_config,
        claims: false,
        marker: true,
        extra: &[],
    })
    .remove(0)
    .blobs
}

/// The fixture as one scan reads it.
#[derive(Clone, Copy)]
struct Tree<'a> {
    /// `carrick.json` kept (two services) or removed (one at the root).
    config: bool,
    /// The fixture's `library-claims.json` replayed as the store's answer.
    claims: bool,
    /// `supabase/config.toml` kept.
    marker: bool,
    /// Files written into the copy before the commit.
    extra: &'a [(&'a str, &'a str)],
}

/// What one scan wrote: its blobs and its run log.
struct Scanned {
    blobs: Vec<serde_json::Value>,
    log: String,
}

/// One scan of `tree`.
fn scan(tree: Tree<'_>) -> Scanned {
    scans_of(tree, 1).remove(0)
}

/// [`scan`], returned as a one-element list.
fn scans(tree: Tree<'_>) -> Vec<Scanned> {
    scans_of(tree, 1)
}

/// `runs` scans of one committed copy of `tree`, sharing one store and
/// cache: every scan after the first takes the incremental path.
fn scans_of(tree: Tree<'_>, runs: usize) -> Vec<Scanned> {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("workspace");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hosted-functions"),
        &repo,
    );
    if !tree.config {
        std::fs::remove_file(repo.join("carrick.json")).unwrap();
    }
    if !tree.marker {
        std::fs::remove_file(repo.join("supabase/config.toml")).unwrap();
    }
    if tree.claims {
        std::fs::create_dir_all(repo.join("__llm__/library-claims")).unwrap();
        std::fs::copy(
            repo.join("library-claims.json"),
            repo.join("__llm__/library-claims/default.json"),
        )
        .unwrap();
    }
    for (file, text) in tree.extra {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let (storage, home, cache) = (
        tmp.path().join("storage"),
        tmp.path().join("home"),
        tmp.path().join("cache"),
    );
    for dir in [&storage, &home, &cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("HOME", &home)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_CACHE_DIR", &cache)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", repo.join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_NO_UPDATE_CHECK", "1")
        .env(
            "CARRICK_SIDECAR_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"),
        );
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_EVENT_PATH",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "CARRICK_OUTPUT_JSON",
        "CARRICK_NO_MODEL",
        "CARRICK_ALLOW_MISSING_TYPES",
        "CARRICK_LOCAL_STORAGE_ISOLATE",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let logs = home.join(".carrick/logs/runs");
    let mut out = Vec::with_capacity(runs);
    for run in 0..runs {
        // The first scan finds no earlier blob; a later one reads the store
        // back and takes the incremental path.
        if run == 0 {
            cmd.env("CARRICK_LOCAL_STORAGE_ISOLATE", "1");
        } else {
            cmd.env_remove("CARRICK_LOCAL_STORAGE_ISOLATE");
        }
        let before: BTreeSet<PathBuf> = run_logs(&logs);
        let output = cmd.output().expect("spawn carrick");
        assert!(
            output.status.success(),
            "the scan failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let log = run_logs(&logs)
            .difference(&before)
            .map(|path| std::fs::read_to_string(path).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let blobs = std::fs::read_dir(&storage)
            .expect("storage dir")
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                serde_json::from_slice(&std::fs::read(path).expect("read blob"))
                    .expect("parse blob")
            })
            .collect();
        out.push(Scanned { blobs, log });
    }
    out
}

/// The run logs under `dir`.
fn run_logs(dir: &Path) -> BTreeSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

/// (`file:line`, method, path, resolution source) of every endpoint row in `blobs` under
/// `supabase/functions`.
fn function_rows(blobs: &[serde_json::Value]) -> BTreeSet<(String, String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["endpoints"].as_array().into_iter().flatten())
        .filter_map(|row| {
            let file = row["file_path"].as_str()?;
            let at = file.find("supabase/functions/")?;
            Some((
                file[at..].to_string(),
                row["key"]["method"].as_str()?.to_string(),
                row["key"]["path"].as_str()?.to_string(),
                row["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

/// (file, slot, reason) of every unread PRODUCER site under
/// `supabase/functions`. The calls inside the functions (a `serve` imported
/// by URL, an untyped `req.json`) are unread sites of their own, as they are
/// without the convention, and are not what this test is about.
fn function_unread(blobs: &[serde_json::Value]) -> BTreeSet<(String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["unread_sites"].as_array().into_iter().flatten())
        .filter(|site| site["role"] == "producer")
        .filter_map(|site| {
            let file = site["file"].as_str()?;
            let at = file.find("supabase/functions/")?;
            Some((
                file[at..].to_string(),
                site["slot"].as_str()?.to_string(),
                site["reason"].as_str()?.to_string(),
            ))
        })
        .collect()
}

#[test]
fn each_folder_function_is_one_fact_row_at_its_real_address() {
    // The row's place is `file:line`, the line its handler is declared on.
    let expected: BTreeSet<(String, String, String, String)> = [
        ("price-quote", 8, "/functions/v1/price-quote"),
        ("team-digest", 1, "/functions/v1/team-digest"),
        ("purge-sessions", 3, "/functions/v1/purge-sessions"),
        // Handler not located: the route is still the folder's.
        ("send-invite", 4, "/functions/v1/send-invite"),
        ("report-export", 1, "/functions/v1/report-export"),
    ]
    .into_iter()
    .map(|(folder, line, path)| {
        (
            format!("supabase/functions/{folder}/index.ts:{line}"),
            "ALL".to_string(),
            path.to_string(),
            "file_based_route".to_string(),
        )
    })
    .collect();
    let unread_expected: BTreeSet<(String, String, String)> = ["send-invite", "report-export"]
        .into_iter()
        .map(|folder| {
            (
                format!("supabase/functions/{folder}/index.ts"),
                "handler".to_string(),
                "handler_not_located".to_string(),
            )
        })
        .collect();
    for with_config in [true, false] {
        let blobs = scan_blobs(with_config);
        assert_eq!(
            function_rows(&blobs),
            expected,
            "carrick.json kept: {with_config}"
        );
        assert_eq!(
            function_unread(&blobs),
            unread_expected,
            "carrick.json kept: {with_config}"
        );
    }
}

// The consumer half (README "Answer key (consumer rows)"): the client the web
// app makes with `@supabase/supabase-js` on `VITE_SUPABASE_URL` says that
// variable is the repository's own address, so a fetch on it keys the path
// the folder function serves.

/// The fixture with its store answer: the `fetch` claim and the client's
/// make claim, base on argument 0.
const SERVED: Tree<'static> = Tree {
    config: true,
    claims: true,
    marker: true,
    extra: &[],
};

/// (method, path, resolution source) of every call row in `blobs` at `at`
/// (`file:line`, repo-relative).
fn calls_at(blobs: &[serde_json::Value], at: &str) -> Vec<(String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["calls"].as_array().into_iter().flatten())
        .filter(|row| row["file_path"].as_str() == Some(at))
        .map(|row| {
            (
                row["key"]["method"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                row["key"]["path"].as_str().unwrap_or_default().to_string(),
                row["resolution_source"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The one call row at `at`.
fn one_call_at(blobs: &[serde_json::Value], at: &str) -> (String, String, String) {
    let rows = calls_at(blobs, at);
    assert_eq!(rows.len(), 1, "one call row at {at}: {rows:?}");
    rows.into_iter().next().unwrap()
}

/// Whether `path` still starts with an env read of `name`, in either
/// spelling.
fn keeps_env_base(path: &str, name: &str) -> bool {
    [
        format!("${{process.env.{name}}}"),
        format!("${{import.meta.env.{name}}}"),
    ]
    .iter()
    .any(|base| path.starts_with(base.as_str()))
}

const QUOTE: &str = "src/quote.ts:2";
const OTHER: &str = "src/other.ts:2";
const LEARNED_LINE: &str = "Treating env var VITE_SUPABASE_URL as internal because the @supabase/supabase-js client created at src/client.ts:5 uses it as its base address";

#[test]
fn a_client_made_on_an_env_base_this_repo_serves_makes_that_base_internal() {
    let scanned = scan(SERVED);
    let (method, path, _) = one_call_at(&scanned.blobs, QUOTE);
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/functions/v1/price-quote")
    );
    // A variable no client is made on is not learned.
    let (_, other, _) = one_call_at(&scanned.blobs, OTHER);
    assert!(keeps_env_base(&other, "VITE_OTHER_URL"), "{other}");
    assert!(other.ends_with("/x"), "{other}");
    assert!(
        scanned.log.contains(LEARNED_LINE),
        "the scan says what it learned"
    );
}

#[test]
fn the_fetch_pairs_with_the_folder_function() {
    for config in [true, false] {
        let blobs = scan(Tree { config, ..SERVED }).blobs;
        let (method, path, _) = one_call_at(&blobs, QUOTE);
        let served: BTreeSet<(String, String)> = function_rows(&blobs)
            .into_iter()
            .map(|(_, method, path, _)| (method, path))
            .collect();
        assert!(
            served.contains(&("ALL".to_string(), path.clone())),
            "carrick.json kept: {config}: {path} is served: {served:?}"
        );
        assert!(carrick_match::method_matches("ALL", &method));
    }
}

#[test]
fn one_row_at_the_fetch_line() {
    let (_, path, _) = one_call_at(&scan(SERVED).blobs, QUOTE);
    assert!(!path.contains("${"), "host-free: {path}");
}

#[test]
fn the_early_row_and_the_fact_row_key_alike_for_a_learned_base() {
    // The fact row takes the site: the early base-path row read the same
    // learned key and gave way to it.
    let (_, path, source) = one_call_at(&scan(SERVED).blobs, QUOTE);
    assert_eq!(path, "/functions/v1/price-quote");
    assert_ne!(source, "env_base_path");
}

#[test]
fn a_second_scan_keys_the_same() {
    let runs = scans_of(SERVED, 2);
    assert!(
        runs[1].log.contains("Incremental analysis complete"),
        "the second scan took the incremental path"
    );
    for (run, scanned) in runs.iter().enumerate() {
        let (_, path, _) = one_call_at(&scanned.blobs, QUOTE);
        assert_eq!(path, "/functions/v1/price-quote", "scan {run}");
    }
}

#[test]
fn no_marker_learns_nothing() {
    let scanned = scan(Tree {
        marker: false,
        ..SERVED
    });
    let (_, path, _) = one_call_at(&scanned.blobs, QUOTE);
    assert!(keeps_env_base(&path, "VITE_SUPABASE_URL"), "{path}");
    assert!(!scanned.log.contains("Treating env var"));
}

/// A call only the model reads (its receiver is a parameter of no package,
/// so no claim decides it), on the learned base.
const MODEL_ONLY: &[(&str, &str)] = &[
    (
        "src/digest.ts",
        "const DIGEST_URL = `${import.meta.env.VITE_SUPABASE_URL}/functions/v1/team-digest`;\nexport async function sendDigest(api: { post(url: string): Promise<unknown> }): Promise<unknown> {\n  return api.post(DIGEST_URL);\n}\n",
    ),
    (
        "__llm__/analyze-file/digest.json",
        r#"{
  "mounts": [],
  "endpoints": [],
  "data_calls": [
    {
      "candidate_id": "@line:3",
      "line_number": 3,
      "target": "${import.meta.env.VITE_SUPABASE_URL}/functions/v1/team-digest",
      "method": "POST",
      "pattern_matched": ".post(",
      "call_expression_text": "api.post(DIGEST_URL)",
      "call_expression_line": 3,
      "payload_expression_text": null,
      "payload_expression_line": null,
      "primary_type_symbol": null,
      "type_import_source": null
    }
  ]
}
"#,
    ),
];

#[test]
fn a_model_only_row_on_a_learned_base_is_host_free() {
    let blobs = scan(Tree {
        extra: MODEL_ONLY,
        ..SERVED
    })
    .blobs;
    let (method, path, source) = one_call_at(&blobs, "src/digest.ts:3");
    assert_eq!(
        (method.as_str(), path.as_str(), source.as_str()),
        ("POST", "/functions/v1/team-digest", "model")
    );
}

// The client calls that name a function by a string (`client.functions
// .invoke("price-quote", …)`): the claim names the op on the client's
// `functions` sub-object, with the prefix the library puts before the name.

/// `library-claims.json` with `edit` applied, as the store's answer the scan
/// replays (written over the copy the tree's `claims` flag makes).
fn claims_with(edit: impl Fn(&mut serde_json::Value)) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hosted-functions/library-claims.json");
    let mut claims: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    edit(&mut claims);
    serde_json::to_string_pretty(&claims).unwrap()
}

/// The client export's claims in `claims`.
fn client_claims(claims: &mut serde_json::Value) -> &mut serde_json::Value {
    claims["library_claims"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|package| package["package"] == "@supabase/supabase-js")
        .map(|package| &mut package["exports"][0])
        .unwrap()
}

const CLAIMS_ANSWER: &str = "__llm__/library-claims/default.json";

/// Each invoke call the fixture writes, and the row it states.
const INVOKES: [(&str, &str, &str); 5] = [
    ("src/direct.ts:4", "POST", "/functions/v1/price-quote"),
    ("src/digest.ts:6", "POST", "/functions/v1/team-digest"),
    ("src/page.ts:5", "POST", "/functions/v1/send-invite"),
    ("src/page.ts:6", "POST", "/functions/v1/report-export"),
    ("src/method.ts:4", "GET", "/functions/v1/price-quote"),
];

/// Every call row in the files the invoke calls are written in or reached
/// through.
fn invoke_file_calls(blobs: &[serde_json::Value]) -> Vec<(String, String, String)> {
    blobs
        .iter()
        .flat_map(|blob| blob["calls"].as_array().into_iter().flatten())
        .filter_map(|row| {
            let at = row["file_path"].as_str()?;
            [
                "src/direct.ts",
                "src/digest.ts",
                "src/page.ts",
                "src/method.ts",
                "src/callEdge.ts",
                "src/useEdge.ts",
            ]
            .iter()
            .any(|file| at.starts_with(&format!("{file}:")))
            .then(|| {
                (
                    at.to_string(),
                    row["key"]["method"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    row["key"]["path"].as_str().unwrap_or_default().to_string(),
                )
            })
        })
        .collect()
}

#[test]
fn a_client_call_named_by_a_literal_states_its_folder_function_path() {
    let (method, path, source) = one_call_at(&scan(SERVED).blobs, "src/direct.ts:4");
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/functions/v1/price-quote")
    );
    assert_ne!(source, "model", "a fact row");
}

#[test]
fn a_client_call_named_by_a_module_constant_states_its_path() {
    let (method, path, _) = one_call_at(&scan(SERVED).blobs, "src/digest.ts:6");
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("POST", "/functions/v1/team-digest")
    );
}

#[test]
fn a_wrapper_states_each_callers_function() {
    let blobs = scan(SERVED).blobs;
    let rows = |at: &str| {
        let (method, path, _) = one_call_at(&blobs, at);
        (method, path)
    };
    assert_eq!(
        rows("src/page.ts:5"),
        ("POST".to_string(), "/functions/v1/send-invite".to_string())
    );
    assert_eq!(
        rows("src/page.ts:6"),
        (
            "POST".to_string(),
            "/functions/v1/report-export".to_string()
        )
    );
    let inside: Vec<_> = invoke_file_calls(&blobs)
        .into_iter()
        .filter(|(at, _, _)| at.starts_with("src/callEdge.ts") || at.starts_with("src/useEdge.ts"))
        .collect();
    assert!(inside.is_empty(), "the callers state the rows: {inside:?}");
}

#[test]
fn a_method_written_in_the_options_overrides_the_default() {
    let (method, path, _) = one_call_at(&scan(SERVED).blobs, "src/method.ts:4");
    assert_eq!(
        (method.as_str(), path.as_str()),
        ("GET", "/functions/v1/price-quote")
    );
}

#[test]
fn each_client_call_pairs_with_the_function_it_names() {
    for config in [true, false] {
        let blobs = scan(Tree { config, ..SERVED }).blobs;
        let served: BTreeSet<(String, String)> = function_rows(&blobs)
            .into_iter()
            .map(|(_, method, path, _)| (method, path))
            .collect();
        let mut called = BTreeSet::new();
        for at in INVOKES.iter().map(|(at, _, _)| *at).chain([QUOTE]) {
            let (method, path, _) = one_call_at(&blobs, at);
            let pairs: Vec<_> = served
                .iter()
                .filter(|(served_method, served_path)| {
                    *served_path == path && carrick_match::method_matches(served_method, &method)
                })
                .collect();
            assert_eq!(
                pairs.len(),
                1,
                "carrick.json kept: {config}: {at} {method} {path} pairs once: {served:?}"
            );
            called.insert(path);
        }
        assert!(
            served
                .iter()
                .any(|(_, path)| path == "/functions/v1/purge-sessions")
        );
        assert!(
            !called.contains("/functions/v1/purge-sessions"),
            "nothing calls purge-sessions"
        );
    }
}

#[test]
fn without_the_served_marker_an_env_base_stays_on_the_path() {
    let (method, path, _) = one_call_at(
        &scan(Tree {
            marker: false,
            ..SERVED
        })
        .blobs,
        "src/direct.ts:4",
    );
    assert_eq!(
        (method.as_str(), path.as_str()),
        (
            "POST",
            "${import.meta.env.VITE_SUPABASE_URL}/functions/v1/price-quote"
        )
    );
}

#[test]
fn an_op_the_claims_do_not_state_states_no_row() {
    let claims = claims_with(|claims| {
        client_claims(claims)["ops"] = serde_json::json!([]);
    });
    let blobs = scan(Tree {
        extra: &[(CLAIMS_ANSWER, &claims)],
        ..SERVED
    })
    .blobs;
    let rows = invoke_file_calls(&blobs);
    assert!(rows.is_empty(), "no claimed op, no row: {rows:?}");
}

/// The request type aliases (`…_Request_…`) of the web blob's bundled types.
fn request_types(blobs: &[serde_json::Value]) -> BTreeSet<String> {
    blobs
        .iter()
        .filter(|blob| blob["service_name"] == "web")
        .filter_map(|blob| blob["bundled_types"].as_str())
        .flat_map(str::lines)
        .filter(|line| line.contains("_Request_"))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_request_type_joins_the_re_keyed_row() {
    // The control: the same variable declared internal, with no marker to
    // learn it from. Both keys read `/functions/v1/price-quote`, so the
    // request type is the same alias of the same key.
    let declared_config = r#"{
  "services": [
    { "serviceName": "web", "directory": ".", "exclude": ["supabase/functions/"], "internalEnvVars": ["VITE_SUPABASE_URL"] },
    { "serviceName": "edge-functions", "directory": "supabase/functions" }
  ]
}
"#;
    let declared = request_types(
        &scan(Tree {
            marker: false,
            extra: &[("carrick.json", declared_config)],
            ..SERVED
        })
        .blobs,
    );
    assert!(
        declared.iter().any(|line| line.contains("items")),
        "the control types the request: {declared:?}"
    );
    assert_eq!(request_types(&scan(SERVED).blobs), declared);
}
