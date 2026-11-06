//! Language-feature coverage, tracked by test (replaces a one-off report).
//!
//! Scans `tests/fixtures/feature-probes/repo` once, offline (mock, with the
//! fixture's library-store answer), then reads each probe's outcome from the
//! blob and compares it with `expected.json`. A probe going down fails the
//! test; a probe going up fails it too, with the message to record the gain.
//! Outcomes, by the rules of the coverage report:
//!
//! - `covered`: a non-model row in the probe's file states every expected
//!   name (a payload probe also needs a resolved request type);
//! - `partial`: a non-model row states a different name;
//! - `lead`: no row, but the site is a possible call;
//! - `not_reached`: neither a row nor a lead.
//!
//! Fixture README: `tests/fixtures/feature-probes/README.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/feature-probes")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
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

fn scan_blob() -> serde_json::Value {
    let tmp = tempfile::tempdir().expect("temp dir");
    let repo = tmp.path().join("workspace");
    copy_dir(&fixture_dir().join("repo"), &repo);
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["add", "-A", "-f"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);
    let cassettes = tmp.path().join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        fixture_dir().join("library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
    let storage = tmp.path().join("storage");
    std::fs::create_dir_all(&storage).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
        .env("HOME", &home)
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
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
    let mut blobs = Vec::new();
    for entry in std::fs::read_dir(&storage).expect("the store reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            blobs.push(serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap());
        }
    }
    assert_eq!(blobs.len(), 1, "the scan stored one blob");
    blobs.remove(0)
}

/// `src/<probe>.<ext>` at the top of `src/` names a probe; helpers in
/// subfolders do not.
fn probe_of(file_path: &str) -> Option<String> {
    let file = file_path.rsplit_once(':').map_or(file_path, |(f, _)| f);
    let name = file.strip_prefix("src/")?;
    let (stem, ext) = name.rsplit_once('.')?;
    let stem_ok = !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    (stem_ok && matches!(ext, "ts" | "js" | "cjs" | "mjs")).then(|| stem.to_string())
}

fn row_name(row: &serde_json::Value) -> String {
    let key = &row["key"];
    ["path", "topic", "event", "field"]
        .iter()
        .find_map(|k| key[*k].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A leading `${...}` base is not part of the name; `:id`-style params
/// collapse to one spelling.
fn canon(name: &str) -> String {
    let mut s = name;
    if s.starts_with("${") {
        if let Some(end) = s.find('}') {
            s = &s[end + 1..];
        }
    }
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ':' && chars.peek().is_some_and(|n| n.is_alphabetic() || *n == '_') {
            while chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
            {
                chars.next();
            }
            out.push_str("param");
        }
    }
    out
}

fn outcomes(blob: &serde_json::Value, labels: &serde_json::Value) -> BTreeMap<String, String> {
    let mut rows: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for side in ["calls", "endpoints"] {
        for row in blob[side].as_array().into_iter().flatten() {
            if let Some(pid) = probe_of(row["file_path"].as_str().unwrap_or_default()) {
                rows.entry(pid).or_default().push((
                    row["resolution_source"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    canon(&row_name(row)),
                ));
            }
        }
    }
    let leads: BTreeSet<String> = blob["possible_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| probe_of(p["file_path"].as_str().unwrap_or_default()))
        .collect();
    let mut manifest: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in blob["type_manifest"].as_array().into_iter().flatten() {
        if m["type_kind"] == "request" {
            if let Some(pid) = probe_of(m["file_path"].as_str().unwrap_or_default()) {
                manifest
                    .entry(pid)
                    .or_default()
                    .push(m["type_alias"].as_str().unwrap_or_default().to_string());
            }
        }
    }
    let mut alias_text: BTreeMap<String, String> = BTreeMap::new();
    for line in blob["bundled_types"].as_str().unwrap_or_default().lines() {
        if let Some(rest) = line.strip_prefix("export type ") {
            if let Some((name, text)) = rest.split_once(" = ") {
                alias_text.insert(name.to_string(), text.to_string());
            }
        }
    }
    let typed = |pid: &str| {
        manifest.get(pid).into_iter().flatten().any(|a| {
            !alias_text
                .get(a)
                .map_or("unknown", String::as_str)
                .starts_with("unknown")
        })
    };
    let mut out = BTreeMap::new();
    for (pid, label) in labels.as_object().expect("labels object") {
        let want: BTreeSet<String> = label["expect"]
            .as_str()
            .unwrap()
            .split('|')
            .map(canon)
            .collect();
        let probe_rows = rows.get(pid).map(Vec::as_slice).unwrap_or_default();
        let got: BTreeSet<String> = probe_rows
            .iter()
            .filter(|(src, _)| src != "model")
            .map(|(_, n)| n.clone())
            .collect();
        let has_fact = !got.is_empty();
        let names_ok = want.is_subset(&got);
        let is_payload = label["kind"] == "payload";
        let status = if names_ok && (!is_payload || typed(pid)) {
            "covered"
        } else if names_ok || (has_fact && !is_payload) {
            "partial"
        } else if leads.contains(pid) {
            "lead"
        } else {
            "not_reached"
        };
        out.insert(pid.clone(), status.to_string());
    }
    out
}

fn rank(status: &str) -> u8 {
    match status {
        "not_reached" => 0,
        "lead" => 1,
        "partial" => 2,
        "covered" => 3,
        other => panic!("unknown status {other}"),
    }
}

fn read_json(name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(name)).unwrap()).unwrap()
}

/// Each probe's outcome equals `expected.json`; a drop is a regression, a
/// rise is a gain that must be recorded in the file.
#[test]
fn feature_probes_match_expected() {
    let labels = read_json("probes.json");
    let expected = read_json("expected.json");
    let got = outcomes(&scan_blob(), &labels);

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for status in got.values() {
        *counts.entry(status.as_str()).or_default() += 1;
    }
    let c = |k: &str| counts.get(k).copied().unwrap_or(0);
    println!(
        "feature probes: covered {} of {} (partial {}, lead {}, not reached {})",
        c("covered"),
        got.len(),
        c("partial"),
        c("lead"),
        c("not_reached")
    );

    let mut down = Vec::new();
    let mut up = Vec::new();
    let known: BTreeSet<&str> = expected
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let scanned: BTreeSet<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(
        known, scanned,
        "expected.json and probes.json list different probes"
    );
    for (pid, now) in &got {
        let was = expected[pid]["status"].as_str().unwrap();
        let line = format!(
            "  {pid} ({}): {was} -> {now}",
            expected[pid]["feature"].as_str().unwrap_or_default()
        );
        match rank(now).cmp(&rank(was)) {
            std::cmp::Ordering::Less => down.push(line),
            std::cmp::Ordering::Greater => up.push(line),
            std::cmp::Ordering::Equal => {}
        }
    }
    assert!(
        down.is_empty(),
        "feature probes went down:\n{}",
        down.join("\n")
    );
    assert!(
        up.is_empty(),
        "feature probes went up: update tests/fixtures/feature-probes/expected.json to record the gain:\n{}",
        up.join("\n")
    );
}
