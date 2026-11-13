//! The build stamp of the compiler engine (carrick#4133).
//!
//! `npm run build` in `src/sidecar` writes `dist/src/engine-stamp.json`
//! (`scripts/write-engine-stamp.mjs`): a sha256 over the inputs that shape
//! `dist/src`, which are `package.json`, `package-lock.json`, `tsconfig*.json`
//! and `src/**`. Tests are not inputs, so editing one never makes a good build
//! look stale. [`hash_inputs`] computes the same hash and must stay
//! byte-identical with the writer; `tests/engine_stamp_test.rs` pins that.
//!
//! The scanner logs the stamp on every spawn. Only an engine found in a source
//! checkout is re-hashed and refused when stale, because only there can the
//! sources sit beside a build older than they are. An engine an install put
//! there (the npm package, the Action tarball) has no sources to compare and
//! is never refused; a missing stamp is a warning at most. Reference:
//! `src/sidecar/README.md, "Build stamp"`.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where the stamp sits, relative to the engine's `dist/src/index.js` folder.
pub const STAMP_FILE: &str = "engine-stamp.json";

/// Where `discover_sidecar_path` found the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineOrigin {
    /// `CARRICK_SIDECAR_DIR`, which the npm package sets. Never refused.
    EnvVar,
    /// Next to the binary (the Action tarball). Never refused.
    NextToBinary,
    /// The compile-time `src/sidecar` folder or the relative `src/sidecar`
    /// fallback: a checkout with sources beside the build.
    SourceCheckout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampVerdict {
    /// Log the stamp; nothing to say.
    Ok { stamp: String },
    /// Spawn, and warn.
    Warn(String),
    /// Do not spawn; the message says how to rebuild.
    Refuse(String),
}

/// The files that shape the build, as `/`-separated paths below `root`, sorted
/// by bytes.
fn input_files(root: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let wanted = name == "package.json"
            || name == "package-lock.json"
            || (name.starts_with("tsconfig") && name.ends_with(".json"));
        if wanted && entry.path().is_file() {
            files.push(name);
        }
    }
    let src = root.join("src");
    if src.is_dir() {
        walk(&src, "src", &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            walk(&path, &format!("{rel}/{name}"), out)?;
        } else if path.is_file() {
            out.push(format!("{rel}/{name}"));
        }
    }
    Ok(())
}

/// The hex sha256 of the engine's build inputs below `root` (the
/// `src/sidecar` folder). Each file contributes `<path>\0<byte length>\0`
/// then its bytes.
pub fn hash_inputs(root: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    for rel in input_files(root)? {
        let bytes = std::fs::read(root.join(&rel))?;
        hasher.update(format!("{rel}\0{}\0", bytes.len()).as_bytes());
        hasher.update(&bytes);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The engine folder (the one holding `package.json`) for an entry point at
/// `<root>/dist/src/index.js`.
fn engine_root(entry: &Path) -> Option<PathBuf> {
    Some(entry.parent()?.parent()?.parent()?.to_path_buf())
}

/// The stamp the build wrote beside `entry`, if it wrote one.
pub fn read_stamp(entry: &Path) -> Option<String> {
    let text = std::fs::read_to_string(entry.parent()?.join(STAMP_FILE)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("inputs_sha256")?.as_str().map(str::to_owned)
}

/// Judge the engine at `entry` (`<root>/dist/src/index.js`).
pub fn check(entry: &Path, origin: EngineOrigin) -> StampVerdict {
    let shown = entry.display();
    let stamp = read_stamp(entry);
    if origin != EngineOrigin::SourceCheckout {
        return match stamp {
            Some(stamp) => StampVerdict::Ok { stamp },
            None => StampVerdict::Warn(format!(
                "The compiler engine at {shown} has no build stamp, so its version cannot be checked."
            )),
        };
    }
    let Some(stamp) = stamp else {
        return StampVerdict::Refuse(format!(
            "The compiler engine at {shown} has no build stamp, so it may be older than its sources. {REBUILD}"
        ));
    };
    // No sources beside the build (a packaged layout reached by a relative
    // path) or sources that cannot be read: nothing to compare, so do not
    // refuse an engine over a folder this process cannot see.
    let Some(now) = engine_root(entry)
        .filter(|root| root.join("src").is_dir())
        .and_then(|root| hash_inputs(&root).ok())
    else {
        return StampVerdict::Ok { stamp };
    };
    if now == stamp {
        StampVerdict::Ok { stamp }
    } else {
        StampVerdict::Refuse(format!(
            "The compiler engine at {shown} was built from different sources than the ones beside it (built {}, sources now {}). {REBUILD}",
            &stamp[..stamp.len().min(12)],
            &now[..12]
        ))
    }
}

const REBUILD: &str = "Rebuild it with `cd src/sidecar && npm ci && npm run build`, or set CARRICK_SIDECAR_DIR to a current build.";
