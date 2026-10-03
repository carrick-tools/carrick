//! A scan is read-only on the repo it scans (carrick#1823).
//!
//! Owner ruling 2026-10-03, after carrick#1742: a scan never edits the
//! repo it scans. The sidecar's own tests guard each write site; this one
//! runs the WHOLE scan, the real binary and the real type sidecar, over a
//! committed git tree and then reads the tree back.
//!
//! The tree is carrick#1742's shape, in
//! `tests/fixtures/scan-leaves-repo-unedited/repo/`:
//!
//! - a pnpm workspace with two packages: `packages/api` (the service) and
//!   `packages/shared`;
//! - the route's response type `Report` has a member typed by `@ws/shared`;
//! - `@ws/shared` publishes its TypeScript source as its `types`, and that
//!   source imports `generated-db-client`, a module the checkout does not
//!   have (a generated client that was never generated);
//! - `packages/api/node_modules/@ws/shared` is the link pnpm writes for a
//!   `workspace:*` dependency.
//!
//! The capture's self-check borrows `packages/api/node_modules`, follows that
//! link into `packages/shared/src/index.ts` and finds the missing module
//! there. In carrick#1742 the dangling-import repair then dropped the import
//! and wrote `unknown` over its names, in the user's file.
//!
//! The link is laid here rather than by `pnpm install`: pnpm is not on the
//! `ubuntu-latest` runner image, and the link is the only part of an install
//! this shape needs.
//!
//! The model and the storage are mocked; nothing is paid for. Run against a
//! sidecar whose `WriteGuard` refuses nothing (point `CARRICK_SIDECAR_DIR` at
//! a copy built that way) and this test fails, naming the rewritten file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scan-leaves-repo-unedited")
}

/// One entry of the tree, by what it holds: a file's bytes, a link's target.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every directory, file and link under `root` except `.git/` and
/// `.carrick/`. Links are recorded, never followed: the workspace link points
/// back into the tree, and what it reaches is recorded where it lives.
fn snapshot(root: &Path) -> BTreeMap<String, Entry> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Entry>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            let rel = path
                .strip_prefix(root)
                .expect("under the root")
                .to_string_lossy()
                .into_owned();
            if rel == ".git" || rel == ".carrick" {
                continue;
            }
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.file_type().is_symlink() {
                out.insert(rel, Entry::Link(fs::read_link(&path).expect("read link")));
            } else if meta.is_dir() {
                out.insert(rel, Entry::Dir);
                walk(root, &path, out);
            } else {
                out.insert(rel, Entry::File(fs::read(&path).expect("read file")));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create the destination");
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display())) {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

/// `git` in `repo`; its stdout as bytes. Panics with stderr on failure.
fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed in {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// `git status --ignored` with every untracked file named, minus anything
/// under `.carrick/`. Ignored entries are listed so that a file written into
/// `node_modules`, or under any other ignored path, shows up too.
fn status(repo: &Path) -> Vec<String> {
    let raw = git(
        repo,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored",
            "--untracked-files=all",
        ],
    );
    String::from_utf8(raw)
        .expect("status is UTF-8")
        .split('\0')
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let path = line.get(3..).unwrap_or_default();
            path != ".carrick" && !path.starts_with(".carrick/")
        })
        .map(str::to_owned)
        .collect()
}

/// The fixture copied to a temp dir, its workspace link laid, committed.
/// Returns the temp dir (which owns the tree) and the repo root inside it.
fn committed_workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    // A plain name: the repo's name is its directory's, and a temp dir's own
    // name starts with a dot.
    let repo = dir.path().join("workspace");
    copy_tree(&fixture_dir().join("repo"), &repo);

    // The link pnpm writes for `"@ws/shared": "workspace:*"` in packages/api.
    let scope = repo.join("packages/api/node_modules/@ws");
    fs::create_dir_all(&scope).expect("create the scope dir");
    std::os::unix::fs::symlink("../../../shared", scope.join("shared"))
        .expect("link the workspace package");

    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@carrick.test",
            "-c",
            "user.name=fixture",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    (dir, repo)
}

/// One offline scan of `repo` with the sidecar live. Returns the stored blobs.
fn scan(repo: &Path) -> String {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/__llm__/", fixture_dir().display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1");
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
        // A sidecar that fails to start must fail the scan, not read as a
        // service with no types and a capture that never ran.
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("failed to spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut blobs = String::new();
    for entry in fs::read_dir(storage.path()).expect("read the storage dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            blobs.push_str(&fs::read_to_string(&path).expect("read a blob"));
        }
    }
    blobs
}

#[test]
fn a_scan_leaves_every_file_of_the_scanned_repo_as_it_was() {
    let (_dir, repo) = committed_workspace();
    let status_before = status(&repo);
    let tree_before = snapshot(&repo);

    let blobs = scan(&repo);

    // The case is live only if the scan's types reached the shared package's
    // source through the workspace link: that is where the missing module is
    // reported, and the file the repair rewrote in carrick#1742. `sharedId`
    // is a member of `Shared`, declared nowhere else, so its presence in the
    // stored type says the link was walked. Without it, every check below
    // would pass for want of anything to refuse.
    assert!(
        blobs.contains("sharedId"),
        "the scan stored no type that reached packages/shared through the \
         workspace link, so this test proves nothing; check the fixture's \
         cassette and the capture:\n{blobs}"
    );

    let status_after = status(&repo);
    assert_eq!(
        status_after,
        status_before,
        "the scan changed the scanned repo (git status, outside .carrick/). \
         A scan is read-only on the repo it scans (carrick#1823); find the \
         write that landed here:\n{}",
        String::from_utf8_lossy(&git(&repo, &["diff"]))
    );

    // Every tracked file, byte for byte against the commit.
    let tracked = git(&repo, &["ls-files", "-z"]);
    for path in String::from_utf8(tracked)
        .expect("paths are UTF-8")
        .split('\0')
        .filter(|path| !path.is_empty())
    {
        let committed = git(&repo, &["show", &format!("HEAD:{path}")]);
        let now = fs::read(repo.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert!(
            now == committed,
            "the scan rewrote tracked file {path}:\n{}",
            String::from_utf8_lossy(&git(&repo, &["diff", "--", path]))
        );
    }

    // And everything else, ignored trees included: no file, directory or
    // link added, removed or changed outside .git/ and .carrick/.
    let tree_after = snapshot(&repo);
    let changed: BTreeSet<&String> = tree_before
        .keys()
        .chain(tree_after.keys())
        .filter(|path| tree_before.get(*path) != tree_after.get(*path))
        .collect();
    assert!(
        changed.is_empty(),
        "the scan added, removed or changed these paths in the scanned repo: {changed:?}"
    );
}
