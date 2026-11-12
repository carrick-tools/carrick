//! A scan says which local stage it is in (carrick#3896).
//!
//! Drives the real scanner binary on `.` over a committed copy of
//! `tests/fixtures/command-object-send/repo`, offline, and reads its stderr:
//! the analysis of the service starts, each stage ends in order, and the
//! analysis ends, every line carrying the service's place, the elapsed time and
//! the resident memory, and none carrying a path.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/command-object-send")
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

fn scan_stderr() -> String {
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
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_scan_says_when_each_local_stage_starts_and_ends() {
    let stderr = scan_stderr();
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("Phase progress:"))
        .collect();
    let whats: Vec<&str> = lines
        .iter()
        .map(|line| {
            let rest = &line[line.find("Phase progress: ").unwrap() + "Phase progress: ".len()..];
            &rest[..rest.find(" (service").expect("a service position")]
        })
        .collect();
    assert_eq!(whats.first(), Some(&"analysis started"), "{stderr}");
    assert_eq!(whats.last(), Some(&"analysis ended"), "{stderr}");
    let ended: Vec<&str> = whats
        .iter()
        .filter_map(|what| what.strip_suffix(" ended"))
        .filter(|stage| *stage != "analysis")
        .collect();
    // Stages end in the order the analysis runs them.
    let order = [
        "discover",
        "cache",
        "implementations",
        "model",
        "graph",
        "protocols",
        "intents",
        "signatures",
        "surface",
        "manifest",
        "types",
        "capture",
        "definitions",
        "other",
    ];
    assert!(ended.len() >= 3, "stages ended: {whats:?}");
    for stage in &ended {
        assert!(order.contains(stage), "{stage} is a named stage");
    }
    for line in &lines {
        assert!(line.contains("(service 1/1,"), "{line}");
        assert!(line.contains("elapsed ") && line.contains(" ms,"), "{line}");
        assert!(line.contains("rss "), "{line}");
        // The fixture's own names, and any path separator beyond the
        // service's `1/1`, stay out of the line.
        assert!(!line.replace("1/1", "").contains('/'), "a path in {line}");
        assert!(
            !line.contains("command-object") && !line.contains("workspace"),
            "{line}"
        );
        assert!(!line.contains(".ts") && !line.contains(".js"), "{line}");
    }
}
