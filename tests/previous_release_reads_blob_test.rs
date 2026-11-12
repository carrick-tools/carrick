//! An older release reads every blob this build writes.
//!
//! Siblings on different releases read each other's index blobs: a runner
//! still on release N downloads a blob that release N+1 wrote. carrick#3762
//! stopped writing a field 0.3.153 requires, and every 0.3.153 reader of a
//! 0.3.154 blob failed with "missing field `unemitted_literal_candidates`".
//!
//! This test writes one blob per service of a three-service fixture with this
//! build, then hands each blob, alone, to the oldest release that must still
//! read it (`tests/fixtures/blob-reader-floor/FLOOR`) and to the latest
//! release, as a sibling to download. That release reads its siblings before anything else in a scan
//! and stops on a blob it cannot read, so removing or retyping a field it
//! requires fails here. Raising the floor is the deprecation step: a field
//! may leave the blob only once no release at or above the floor requires it.
//!
//! The older binary comes from `CARRICK_PREVIOUS_RELEASE_BINS` (CI downloads
//! the floor and the latest release's linux builds; see the `steps` shard in
//! ci.yml). Under CI the test fails without them; on a laptop it says so and
//! passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = "tests/fixtures/recheck-one-repo-two-services/shop";
const SERVICES: usize = 3;
/// The first release that reads a reason word it does not know leniently
/// (carrick#3989 S0). An older release refuses a blob carrying any word added
/// after it, so the every-reason-word blob is checked only from here; once
/// FLOOR reaches it, every release this test runs is checked.
const LENIENT_REASONS_SINCE: &str = "0.3.155";

/// Whether `version` is at or above [`LENIENT_REASONS_SINCE`].
fn reads_unknown_reason_words(version: &str) -> bool {
    let parts = |text: &str| -> Vec<u32> {
        text.split('.')
            .map(|part| {
                part.parse()
                    .unwrap_or_else(|_| panic!("`{text}` is no release"))
            })
            .collect()
    };
    parts(version) >= parts(LENIENT_REASONS_SINCE)
}

fn scan(bin: &Path, fixture: &Path, storage: &Path, envs: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(bin);
    command
        .arg(fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", storage)
        .env_remove("CARRICK_LOCAL_STORAGE_ISOLATE")
        .env_remove("CARRICK_LOCAL_STORAGE_PEERS")
        .env_remove("CARRICK_OUTPUT_JSON")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI");
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("failed to spawn carrick binary")
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The older releases to check: `CARRICK_PREVIOUS_RELEASE_BINS`, one
/// `<version>=<binary>` pair per whitespace-separated word. CI passes the
/// floor and the latest release.
fn older_releases() -> Vec<(String, PathBuf)> {
    std::env::var("CARRICK_PREVIOUS_RELEASE_BINS")
        .unwrap_or_default()
        .split_whitespace()
        .map(|pair| {
            let (version, bin) = pair
                .split_once('=')
                .unwrap_or_else(|| panic!("`{pair}` is not <version>=<binary>"));
            (version.to_string(), PathBuf::from(bin))
        })
        .collect()
}

#[test]
fn older_releases_read_every_blob_this_build_writes() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let floor = std::fs::read_to_string(repo.join("tests/fixtures/blob-reader-floor/FLOOR"))
        .expect("the reader floor is recorded")
        .trim()
        .to_string();
    let releases = older_releases();
    if releases.is_empty() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must set CARRICK_PREVIOUS_RELEASE_BINS, the floor ({floor}) included"
        );
        eprintln!("CARRICK_PREVIOUS_RELEASE_BINS unset: not checked against {floor}");
        return;
    }
    assert!(
        releases.iter().any(|(version, _)| *version == floor),
        "the floor release {floor} is among those checked: {releases:?}"
    );
    let fixture = repo.join(FIXTURE);

    // This build writes one blob per service, alone.
    let written = tempfile::tempdir().expect("temp storage dir");
    let output = scan(
        Path::new(env!("CARGO_BIN_EXE_carrick")),
        &fixture,
        written.path(),
        &[("CARRICK_LOCAL_STORAGE_ISOLATE", Path::new("1"))],
    );
    assert!(
        output.status.success(),
        "this build's scan failed:\n{}",
        text(&output)
    );
    let mut blobs: Vec<PathBuf> = std::fs::read_dir(written.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    blobs.sort();
    assert_eq!(blobs.len(), SERVICES, "one blob per service: {blobs:?}");

    // carrick#3989: one more blob carries every reason word this build
    // knows, wherever a reason word sits on the blob, so a word added to
    // the vocabulary is checked here without anyone listing it.
    let every_word = every_reason_word_blob(&blobs[0], written.path());

    // Each older release downloads each blob as its only sibling, and scans
    // to the end.
    for (version, bin) in &releases {
        let every_word = if reads_unknown_reason_words(version) {
            Some(&every_word)
        } else {
            eprintln!(
                "release {version} is below {LENIENT_REASONS_SINCE}, which reads an unknown \
                 reason word leniently: the every-reason-word blob is not checked against it \
                 (carrick#3989; raise FLOOR to close this)"
            );
            None
        };
        for blob in blobs.iter().chain(every_word) {
            let peers = tempfile::tempdir().unwrap();
            std::fs::copy(blob, peers.path().join(blob.file_name().unwrap())).unwrap();
            let own = tempfile::tempdir().unwrap();
            let output = scan(
                bin,
                &fixture,
                own.path(),
                &[("CARRICK_LOCAL_STORAGE_PEERS", peers.path())],
            );
            assert!(
                output.status.success(),
                "release {version} cannot read {} written by this build:\n{}",
                blob.display(),
                text(&output)
            );
        }
    }
}

/// Every reason word this build knows: [`Stop::ALL`] and the phrase table
/// every surface reads (`npm/carrick/src/unread-reasons.json`).
fn every_reason_word(repo: &Path) -> Vec<String> {
    let mut words: std::collections::BTreeSet<String> = carrick::request_summary::Stop::ALL
        .iter()
        .map(|stop| stop.as_str().to_string())
        .collect();
    let phrases: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(repo.join("npm/carrick/src/unread-reasons.json"))
            .expect("the phrase table reads"),
    )
    .expect("the phrase table is an object");
    words.extend(phrases.keys().cloned());
    words.into_iter().collect()
}

/// `blob` with one unread site, one possible call and one unfollowed member
/// call per reason word, and the counts per word, written beside it.
fn every_reason_word_blob(blob: &Path, dir: &Path) -> PathBuf {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let words = every_reason_word(&repo);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(blob).unwrap()).unwrap();
    let file = "src/every-reason.ts";
    let mut sites = Vec::new();
    let mut calls = Vec::new();
    let mut members = Vec::new();
    let mut by_reason = serde_json::Map::new();
    for (line, word) in (1u32..).zip(&words) {
        sites.push(serde_json::json!({
            "file": file, "line": line, "span_start": line * 10, "span_end": line * 10 + 5,
            "reason": word, "also": [word], "slot": "callee", "depth": 0,
        }));
        calls.push(serde_json::json!({
            "file_path": file, "line_number": line, "span_start": line * 10,
            "span_end": line * 10 + 5, "kind": null, "role": null, "labelled_end": null,
            "unread": { "slot": "path", "reason": word }, "trace_depth": 0,
        }));
        members.push(serde_json::json!({
            "file": file, "line": line, "caller": format!("{file}::caller"),
            "member": "send", "receiver": "client", "reason": word,
        }));
        by_reason.insert(word.clone(), serde_json::json!(1));
    }
    value["unread_sites_total"] = serde_json::json!(sites.len());
    value["unread_sites"] = serde_json::Value::Array(sites);
    value["unread_sites_by_reason"] = serde_json::Value::Object(by_reason);
    value["possible_calls"] = serde_json::Value::Array(calls);
    value["unfollowed_member_calls_total"] = serde_json::json!(members.len());
    value["unfollowed_member_calls"] = serde_json::Value::Array(members);
    let name = blob.file_name().unwrap().to_string_lossy().to_string();
    let every_word = dir.join("every-reason-word").join(name);
    std::fs::create_dir_all(every_word.parent().unwrap()).unwrap();
    std::fs::write(&every_word, value.to_string()).unwrap();
    every_word
}
