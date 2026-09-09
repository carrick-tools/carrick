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

/// carrick#863: the same surface, stated the CommonJS way. Every `.js` lambda
/// writes its entry point as an assignment, and an assignment is none of the
/// shapes the extractor knew.
#[test]
fn a_module_that_exports_by_assignment_states_its_functions() {
    let definitions = function_definitions();

    // `exports.handler = async (event) => { … }`: the lambda entry point, and
    // the whole surface of most files written this way.
    let handler = definition(&definitions, "handler");
    assert_eq!(handler["file_path"], "src/lambda.js");
    assert_eq!(handler["line_number"], 6, "at the arrow, not the statement");
    assert_eq!(handler["is_exported"], true);
    assert_eq!(handler["signature"], "(event) => unknown");

    // `module.exports.health = function health(deep) { … }`.
    let health = definition(&definitions, "health");
    assert_eq!(health["file_path"], "src/lambda.js");
    assert_eq!(health["line_number"], 13);
    assert_eq!(health["is_exported"], true);
    assert_eq!(health["signature"], "(deep) => unknown");

    // An object assigned to a named export is a member bag, read exactly as
    // the ESM one is.
    let drop = definition(&definitions, "table.drop");
    assert_eq!(drop["signature"], "(id) => unknown");
    assert_eq!(drop["is_exported"], true);

    // `module.exports = { … }` is the default export, so its members key the
    // same way `export default { … }` does.
    let drain = definition(&definitions, "default.drain");
    assert_eq!(drain["file_path"], "src/queue.js");
    assert_eq!(drain["is_exported"], true);
    assert_eq!(drain["signature"], "(queue) => unknown");

    // A function offered by name has ONE definition, at its own key, and the
    // assignment says the module offers it. A second row for the same body
    // would double the count and re-bill the intent.
    for (offered, shorthand) in [("reset", false), ("consume", true)] {
        let def = definition(&definitions, offered);
        assert_eq!(
            def["is_exported"], true,
            "{offered} is offered by an export assignment: {def:#?}"
        );
        let alias = if shorthand {
            "default.consume"
        } else {
            "table.reset"
        };
        assert!(
            !definitions.contains_key(alias),
            "{offered} already has a definition; {alias} would be a second row for one body: {:?}",
            names(&definitions)
        );
    }

    // The bound: a module-local function no assignment reaches stays local.
    let sweep = definition(&definitions, "sweep");
    assert_eq!(
        sweep["is_exported"], false,
        "nothing exports sweep: {sweep:#?}"
    );
}
