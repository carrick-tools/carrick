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

    // Each older release downloads each blob as its only sibling, and scans
    // to the end.
    for (version, bin) in &releases {
        for blob in &blobs {
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
