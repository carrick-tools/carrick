//! Two scans of one tree render byte-identical analyze-file prompts
//! (carrick#1220).
//!
//! The cloud keys its analysis cache on the rendered `user_message` — the
//! guidance block by id, everything after it by hash of the bytes. A single
//! byte of drift in the per-file body is a total cache miss for that file, and
//! a collection that reaches the prompt through a `HashMap`/`HashSet` walk
//! drifts between PROCESSES, not within one: `RandomState` is seeded once per
//! process, so two renders inside one test agree by construction and prove
//! nothing. This test therefore spawns the binary twice and diffs what the two
//! processes would have put on the wire.
//!
//! carrick#954 was one instance of the class (a repo-wide import map keyed on
//! local name lost members to walk order). The guard is the general one: every
//! collection feeding the prompt — candidate hints and contexts, the import
//! table, the GraphQL producer/consumer hints, the imported-wrapper sources —
//! has to render the same bytes in a fresh process.
//!
//! `CARRICK_EVAL_DUMP_DIR` is the capture: the analyzer writes the exact
//! `user_message` it sent, per file, before anything downstream touches it.
//! Under `CARRICK_MOCK_ALL` nothing reaches the cloud, so the run is free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The corpus is the oracle here, so it has to be one that exercises the
/// optional sections: without a GraphQL SDL, an imported wrapper and a
/// multi-source import table in the rendered prompts, "two runs agree" would
/// only be saying that a handful of bare file bodies agree. `asserts_oracle`
/// below pins that.
const FIXTURE: &str = "xrepo-corpus-3";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
}

/// One scan's prompts, keyed by the file they were built for.
fn capture(dump: &Path) -> BTreeMap<String, String> {
    capture_from(dump, &fixture_dir())
}

/// As [`capture`], over a copy of the corpus at `corpus` — the same content at
/// a different absolute path.
fn capture_from(dump: &Path, corpus: &Path) -> BTreeMap<String, String> {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    // No cassettes: the generated mock answers every call, and only the
    // REQUEST is under test.
    let cassettes = tempfile::tempdir().expect("temp cassette dir");

    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(corpus)
        .env("CARRICK_EVAL_DUMP_DIR", dump)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.path().display()),
        )
        .env("CARRICK_OUTPUT_JSON", "1")
        // The prompt is built in phase 1, before anything asks the sidecar
        // anything, so the type layer is not under test and skipping it keeps
        // the run hermetic beside other sidecar-driving tests.
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_ALLOW_MISSING_TYPES", "1");
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "{FIXTURE} scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut prompts = BTreeMap::new();
    for entry in std::fs::read_dir(dump).expect("dump dir").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let artifact: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read dump"))
                .expect("dump is JSON");
        let file = artifact["file_path"]
            .as_str()
            .expect("dump names its file")
            .to_string();
        let message = artifact["request_user_message"]
            .as_str()
            .expect("dump carries the user message")
            .to_string();
        prompts.insert(file, message);
    }
    prompts
}

/// Where two strings first differ, as a byte offset and the window around it.
/// A prompt is tens of kilobytes; asserting equality without this prints the
/// whole pair twice and says nothing about which section drifted.
fn first_difference(a: &str, b: &str) -> String {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let at = ab
        .iter()
        .zip(bb.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(ab.len().min(bb.len()));
    let from = at.saturating_sub(120);
    let window = |s: &str, end: usize| {
        let to = end.min(s.len());
        String::from_utf8_lossy(&s.as_bytes()[from.min(to)..to]).to_string()
    };
    format!(
        "first differing byte at {at} (lengths {} vs {})\n--- run 1 ---\n{}\n--- run 2 ---\n{}",
        ab.len(),
        bb.len(),
        window(a, at + 120),
        window(b, at + 120),
    )
}

#[test]
fn two_processes_render_the_same_analyze_file_prompts() {
    let first_dump = tempfile::tempdir().expect("temp dump dir");
    let second_dump = tempfile::tempdir().expect("temp dump dir");
    let first = capture(first_dump.path());
    let second = capture(second_dump.path());

    assert!(
        !first.is_empty(),
        "{FIXTURE} dispatched no files, so the comparison has nothing to compare"
    );
    // Membership before bytes: a collection that drops a row between runs
    // (carrick#954) changes WHICH files are asked about, and that reads as a
    // missing key rather than a differing string.
    let (left, right): (Vec<&String>, Vec<&String>) =
        (first.keys().collect(), second.keys().collect());
    assert_eq!(
        left, right,
        "the two runs asked about different files: the dispatch set is not deterministic"
    );

    for (file, message) in &first {
        let other = &second[file];
        assert!(
            message == other,
            "analyze-file prompt for {file} differs between two runs; every cache key \
             built from it misses.\n{}",
            first_difference(message, other)
        );
    }
}

/// The corpus really does exercise the sections whose inputs are walked
/// collections. Without this, a fixture change could quietly reduce the test
/// above to comparing bare file bodies and it would still pass.
#[test]
fn the_corpus_exercises_every_collection_backed_section() {
    let dump = tempfile::tempdir().expect("temp dump dir");
    let prompts = capture(dump.path());

    let with = |needle: &str| prompts.values().filter(|m| m.contains(needle)).count();
    assert!(
        with("### GRAPHQL SCHEMA PRODUCERS") > 0,
        "no prompt carries the GraphQL producer section"
    );
    assert!(
        with("### IMPORTED HTTP WRAPPER DEFINITIONS") > 0,
        "no prompt carries the imported-wrapper section"
    );
    assert!(
        with("  - From '") > 0,
        "no prompt carries a resolved import table"
    );
    assert!(
        with("### CANDIDATE CONTEXT (Structured JSON)") > 0,
        "no prompt carries structured candidate contexts"
    );
}

/// The prompt names every file the way the index does, so identical content
/// scanned from a different directory renders identical bytes (carrick#1223).
///
/// The cloud's analysis cache is content-addressed and deliberately keys
/// nothing about where a scan ran, but the scanner used to write ABSOLUTE
/// paths into the message it hashes — the file's own header, the imported
/// wrapper snippets, the GraphQL consumer hints — so every entry was private
/// to one checkout: two developers on one commit shared nothing, a laptop and
/// CI shared nothing, and moving or re-cloning a tree re-paid the whole repo.
/// None of that is visible as an error; it reads as a normal first index. Two
/// copies of one corpus at different paths is the assertion that says so.
#[test]
fn the_same_tree_at_two_paths_renders_the_same_prompts() {
    let one = tempfile::tempdir().expect("temp corpus dir");
    let two = tempfile::tempdir().expect("temp corpus dir");
    // Different depths as well as different names: a path length that happens
    // to match would hide a leak that shifted no byte counts.
    let left = one.path().join("checkout");
    let right = two.path().join("nested/deeper/checkout-with-a-longer-name");
    copy_dir(&fixture_dir(), &left);
    copy_dir(&fixture_dir(), &right);

    let left_dump = tempfile::tempdir().expect("temp dump dir");
    let right_dump = tempfile::tempdir().expect("temp dump dir");
    let from_left = capture_from(left_dump.path(), &left);
    let from_right = capture_from(right_dump.path(), &right);

    assert!(
        !from_left.is_empty(),
        "the copied corpus dispatched no files, so the comparison has nothing to compare"
    );
    // Named one pair at a time: printing two 34-path lists says nothing about
    // WHICH name moved, and an absolute path moves all of them at once.
    for (mine, theirs) in from_left.keys().zip(from_right.keys()) {
        assert_eq!(
            mine, theirs,
            "the two checkouts named this file differently: a path in the prompt is still absolute"
        );
    }
    assert_eq!(
        from_left.len(),
        from_right.len(),
        "the two checkouts dispatched different numbers of files"
    );
    for (file, message) in &from_left {
        let other = &from_right[file];
        assert!(
            message == other,
            "the prompt for {file} depends on where the checkout lives; every cache entry it \
             writes is private to that directory.\n{}",
            first_difference(message, other)
        );
    }
}

/// Copy a directory tree, so one corpus can be scanned from two paths.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create copy target");
    for entry in std::fs::read_dir(from).expect("read corpus").flatten() {
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target);
        } else {
            std::fs::copy(&source, &target).expect("copy corpus file");
        }
    }
}
