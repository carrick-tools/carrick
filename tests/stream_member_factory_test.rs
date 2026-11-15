//! An instance made by a member call on a library instance
//! (`new Hub().channel(name)`) is a root where the store's `of`-make was
//! verified at that member's declaration (contract amendment 12,
//! carrick-cloud#3534 T4), pinned by the fixture
//! `tests/fixtures/stream-member-factory` (answer key in its README).
//!
//! Drives the real scanner binary on `.`, as the Action runs it, offline,
//! with the fixture's store answer replayed as the library store's. The
//! broker kind is always asked, so under a mock scan a send whose name is
//! read ends `unanswered` on slot `kind`: that is the outcome here, not a
//! missing row.
//!
//! Protocols: pub/sub (a broker client). The walk and the root rule are
//! protocol-free.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream-member-factory")
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

/// What the one service's blob holds for the fixture's file.
struct Blob {
    /// Each possible call of the `Channel.publish` send, by line:
    /// `slot reason`.
    sends: Vec<(u64, String)>,
    /// Each unread site, as `line reason slot`.
    unread: BTreeSet<String>,
}

fn scan() -> Blob {
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
    let blobs: Vec<PathBuf> = std::fs::read_dir(&storage)
        .expect("the store reads")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    let [path] = blobs.as_slice() else {
        panic!("one blob: {blobs:?}");
    };
    let blob: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut sends = Vec::new();
    for lead in blob["possible_calls"].as_array().into_iter().flatten() {
        if lead["kind"] == "pubsub" && lead["labelled_end"]["member"] == "publish" {
            sends.push((
                lead["line_number"].as_u64().unwrap(),
                format!(
                    "{} {}",
                    lead["unread"]["slot"].as_str().unwrap(),
                    lead["unread"]["reason"].as_str().unwrap()
                ),
            ));
        }
    }
    sends.sort();
    let unread = blob["unread_sites"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|site| {
            format!(
                "{} {} {}",
                site["line"],
                site["reason"].as_str().unwrap_or_default(),
                site["slot"].as_str().unwrap_or_default()
            )
        })
        .collect();
    Blob { sends, unread }
}

/// Each send is read through the member call that made its channel. A
/// factory that returns the member call (`:9`), an awaited async one (`:20`),
/// a held local (`:27`), a module constant (`:34`) and the in-place chain
/// (`:62`) root at the member call, state the name, and ask only the kind.
/// A name taken from a runtime value (`:46`) is unread at the name. An
/// in-repo class with a member of the same name (`:57`) is no library
/// maker: it stays as it was.
#[test]
fn a_channel_made_by_a_member_call_is_a_root_and_its_name_is_read() {
    let blob = scan();
    let want = |line: u64, what: &str| (line, what.to_string());
    assert_eq!(
        blob.sends,
        vec![
            want(9, "kind unanswered"),
            want(20, "kind unanswered"),
            want(27, "kind unanswered"),
            want(34, "kind unanswered"),
            want(46, "name unread_expression"),
            want(57, "name unanswered"),
            want(62, "kind unanswered"),
        ]
    );
}

/// The sites this change does not touch read as before: the maker's own
/// calls that no op names, and the unread `package_unmatched` and
/// `value_never_bound` stops.
#[test]
fn the_sites_the_member_root_does_not_touch_read_as_before() {
    let blob = scan();
    for kept in [
        "5 package_unmatched callee",
        "31 package_unmatched callee",
        "41 package_unmatched callee",
        "62 package_unmatched callee",
        "39 value_never_bound callee",
    ] {
        assert!(blob.unread.contains(kept), "{kept}: {:?}", blob.unread);
    }
}
