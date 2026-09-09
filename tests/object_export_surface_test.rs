//! carrick#830: the function members of a module's exported object literal are
//! indexed, instead of the file reading as one with no functions in it.
//!
//! Deterministic and AST-only. The LLM is replayed from `__llm__/` and states
//! nothing, so every row asserted here is derived from the source.
//!
//! The scan is read from the stored blob rather than the projection: function
//! definitions are what this is about, and the projection carries operations.
//!
//! See `tests/fixtures/object-export-surface/README.md` for the shape.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/object-export-surface")
}

/// The function definitions of one isolated offline scan, keyed as the scan
/// keys them.
fn function_definitions() -> serde_json::Map<String, serde_json::Value> {
    let cache = tempfile::tempdir().expect("temp cache dir");
    let cassettes = fixture_dir().join("__llm__");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(fixture_dir())
        .env("CARRICK_LOCAL_STORAGE_DIR", cache.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", cassettes.display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .output()
        .expect("failed to spawn carrick binary");
    assert!(
        output.status.success(),
        "scanner exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let blob = find_blob(cache.path()).expect("the scan stored a blob");
    let text = std::fs::read_to_string(&blob).expect("read the stored blob");
    let stored: serde_json::Value = serde_json::from_str(&text).expect("the blob is JSON");
    stored["function_definitions"]
        .as_object()
        .expect("the blob carries function definitions")
        .clone()
}

/// The stored blob, wherever the isolated cache put it.
fn find_blob(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_blob(&path) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|ext| ext == "json") {
            let text = std::fs::read_to_string(&path).ok()?;
            if text.contains("\"function_definitions\"") {
                return Some(path);
            }
        }
    }
    None
}

/// The keys, in a form a panic message can print.
fn names(definitions: &serde_json::Map<String, serde_json::Value>) -> Vec<&String> {
    definitions.keys().collect()
}

fn definition<'a>(
    definitions: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> &'a serde_json::Value {
    definitions
        .get(name)
        .unwrap_or_else(|| panic!("no definition named {name}: {:?}", names(definitions)))
}

#[test]
fn a_module_that_exports_one_object_states_its_functions() {
    let definitions = function_definitions();

    // The ticket's shape: the whole module is `export default { async fetch }`.
    let fetch = definition(&definitions, "default.fetch");
    assert_eq!(fetch["file_path"], "src/edge.js");
    assert_eq!(fetch["line_number"], 8, "at the method, not the object");
    assert_eq!(
        fetch["is_exported"], true,
        "a member of the default export is exported: {fetch:#?}"
    );
    assert_eq!(fetch["signature"], "(request) => unknown");

    // The named form, in each of its three shapes.
    let list = definition(&definitions, "handlers.list");
    assert_eq!(list["file_path"], "src/handlers.ts");
    assert_eq!(list["signature"], "() => Promise<string[]>");
    assert_eq!(list["is_exported"], true);

    let create = definition(&definitions, "handlers.create");
    assert_eq!(create["signature"], "(name: string) => Promise<string>");

    let restore = definition(&definitions, "handlers.archive.restore");
    assert_eq!(restore["signature"], "(id: string) => string");

    // The control: an ordinary exported function reads exactly as it did.
    let normalize = definition(&definitions, "normalize");
    assert_eq!(normalize["signature"], "(input: string) => string");

    // The bound: a local object is a value the module uses, not a surface it
    // offers, and collecting every one of them would flood the index.
    assert!(
        !definitions.contains_key("internals.tidy"),
        "a module-local object is not an export surface: {:?}",
        names(&definitions)
    );
}
