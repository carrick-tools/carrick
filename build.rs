use std::env;
use std::path::Path;
use std::process::Command;

/// The paths whose sources make up the scanner a build produces: this crate,
/// the matcher crate it links, the sidecar it ships beside, and the files that
/// decide how they compile. The build stamp's `dirty` flag is computed over
/// exactly these paths, and every tracked file under them is a rerun trigger,
/// so the flag cannot go stale against its own inputs.
const SOURCE_PATHS: [&str; 5] = ["src", "crates", "build.rs", "Cargo.toml", "Cargo.lock"];

fn main() {
    let api_endpoint = env::var("CARRICK_API_ENDPOINT")
        .unwrap_or_else(|_| "https://api.carrick.tools".to_string());

    println!("cargo:rustc-env=CARRICK_API_ENDPOINT={}", api_endpoint);
    println!("cargo:rerun-if-env-changed=CARRICK_API_ENDPOINT");

    stamp_build();
}

/// Compile in the commit this build was made from and whether its sources
/// matched it (carrick#1739). `CARGO_PKG_VERSION` only moves at a release, so
/// a build of main between two releases names the previous one; this is what
/// says which code it actually was. Read by `ScannerBuild::current`.
///
/// Release builds only (carrick#2157). A debug build stamps nothing and watches
/// neither `.git` nor any source file: cargo reruns a build script, and so
/// rebuilds the whole crate, on every commit and every sidecar edit otherwise.
/// Debug builds are local and test builds; the release guard and installed
/// binaries are release builds.
///
/// Both values are empty/false when no commit can be named without guessing:
/// no git, no checkout, or a checkout whose root is not this package (a copy
/// of the sources inside some other repository would otherwise stamp that
/// repository's commit). No CI variable is used as a fallback: on a dispatched
/// release, `GITHUB_SHA` is main's head, not the tag being built.
fn stamp_build() {
    if env::var("PROFILE").as_deref() != Ok("release") {
        println!("cargo:rustc-env=CARRICK_BUILD_COMMIT=");
        println!("cargo:rustc-env=CARRICK_BUILD_DIRTY=false");
        return;
    }
    let in_own_checkout = git(&["rev-parse", "--show-toplevel"])
        .and_then(|top| Path::new(top.trim()).canonicalize().ok())
        .zip(
            env::current_dir()
                .ok()
                .and_then(|dir| dir.canonicalize().ok()),
        )
        .is_some_and(|(top, here)| top == here);
    let commit = git(&["rev-parse", "HEAD"])
        .map(|out| out.trim().to_string())
        .filter(|sha| is_object_id(sha));
    let status = git_over_sources(&["status", "--porcelain", "--untracked-files=no"]);
    let (commit, dirty) = match (in_own_checkout, commit, status) {
        (true, Some(commit), Some(status)) => (commit, !status.trim().is_empty()),
        _ => {
            println!("cargo:rustc-env=CARRICK_BUILD_COMMIT=");
            println!("cargo:rustc-env=CARRICK_BUILD_DIRTY=false");
            return;
        }
    };
    println!("cargo:rustc-env=CARRICK_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=CARRICK_BUILD_DIRTY={dirty}");

    // Rerun when HEAD moves: `HEAD` changes on a checkout, and the HEAD reflog
    // is appended on every commit, amend, reset and pull, including when the
    // branch's own ref is packed. Both resolve per worktree. Neither
    // `.git/index` nor `packed-refs` is watched: `git status` and
    // `git fetch --prune` rewrite those without HEAD moving, and each rewrite
    // would rebuild the whole crate.
    for name in ["HEAD", "logs/HEAD"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            watch(path.trim());
        }
    }
    // Rerun when a source file changes, so `dirty` is recomputed. Tracked
    // files only: a new untracked file reaches the binary only through an edit
    // to a tracked one, which is a trigger and makes the build dirty.
    if let Some(files) = git_over_sources(&["ls-files", "-z"]) {
        for file in files.split('\0').filter(|file| !file.is_empty()) {
            watch(file);
        }
    }
}

/// A rerun trigger, emitted only for a path that exists: cargo reruns a build
/// script on every build when a watched path is missing.
fn watch(path: &str) {
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

/// Run git in the package root. `--no-optional-locks` keeps `git status` from
/// refreshing and rewriting the index of the tree being built. Inherited git
/// env is cleared so the repository is discovered from the package root, not
/// from an ambient `GIT_DIR` (a pre-commit hook sets one); mirrors
/// `cloud_storage::get_current_commit_hash`.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// `git <args> -- <SOURCE_PATHS>`.
fn git_over_sources(args: &[&str]) -> Option<String> {
    let mut full = args.to_vec();
    full.push("--");
    full.extend(SOURCE_PATHS);
    git(&full)
}

/// A full SHA-1 or SHA-256 object id, lowercase hex.
fn is_object_id(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
