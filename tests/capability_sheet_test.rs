//! `docs/capabilities.md` is pinned to the code it describes.
//!
//! - The kinds list in the sheet is the set of `OperationKey` variants.
//! - The sheet says JavaScript is read exactly when both TypeScript programs
//!   the sidecar builds set `allowJs: true`.
//! - The extension list in the sheet is the one the file walk applies.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// The variant names of `OperationKey`, lowercased, from the source text.
fn operation_key_variants() -> BTreeSet<String> {
    let src = read("src/operation.rs");
    let start = src
        .find("pub enum OperationKey {")
        .expect("OperationKey enum in src/operation.rs");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("end of OperationKey enum");
    body[..end]
        .lines()
        .skip(1)
        .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
        .map(str::trim)
        .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .map(|l| {
            l.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
                .to_lowercase()
        })
        .collect()
}

/// The text between `<!-- capability:NAME -->` and `<!-- /capability:NAME -->`.
fn block<'a>(doc: &'a str, name: &str) -> &'a str {
    let open = format!("<!-- capability:{name} -->");
    let close = format!("<!-- /capability:{name} -->");
    let a = doc
        .find(&open)
        .unwrap_or_else(|| panic!("{open} missing from docs/capabilities.md"))
        + open.len();
    let b = doc[a..]
        .find(&close)
        .unwrap_or_else(|| panic!("{close} missing from docs/capabilities.md"));
    &doc[a..a + b]
}

/// The value of a one-line `<!-- capability:NAME: VALUE -->` marker.
fn marker<'a>(doc: &'a str, name: &str) -> &'a str {
    let open = format!("<!-- capability:{name}:");
    let a = doc
        .find(&open)
        .unwrap_or_else(|| panic!("{open} marker missing from docs/capabilities.md"))
        + open.len();
    let b = doc[a..].find("-->").expect("marker close");
    doc[a..a + b].trim()
}

#[test]
fn sheet_lists_exactly_the_operation_key_variants() {
    let doc = read("docs/capabilities.md");
    let listed: BTreeSet<String> = block(&doc, "operation-kinds")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(|s| s.trim().to_lowercase())
        .collect();
    let variants = operation_key_variants();
    assert!(!variants.is_empty(), "parsed no OperationKey variants");
    assert_eq!(
        listed, variants,
        "docs/capabilities.md operation kinds differ from OperationKey in src/operation.rs; \
         update the sheet (its section 1 table too)"
    );
}

#[test]
fn sheet_claims_js_is_read_only_while_allow_js_is_on() {
    let doc = read("docs/capabilities.md");
    let claims_js = match marker(&doc, "js-read") {
        "yes" => true,
        "no" => false,
        other => panic!("capability:js-read must be yes or no, got {other:?}"),
    };
    let on = |rel: &str| {
        let src = read(rel);
        src.contains("allowJs: true,") && src.contains("checkJs: false,")
    };
    let actual = on("src/sidecar/src/project-loader.ts") && on("src/sidecar/src/capture/index.ts");
    assert_eq!(
        claims_js, actual,
        "docs/capabilities.md says js-read: {claims_js} but the sidecar's allowJs/checkJs \
         settings (project-loader.ts, capture/index.ts) say {actual}"
    );
}

#[test]
fn sheet_lists_the_extensions_the_file_walk_takes() {
    let doc = read("docs/capabilities.md");
    let listed: BTreeSet<String> = marker(&doc, "scanned-extensions")
        .split_whitespace()
        .map(String::from)
        .collect();
    let src = read("src/file_finder.rs");
    let at = src
        .find("pub fn is_scanned_source")
        .expect("is_scanned_source");
    let walk = &src[at..];
    let line = walk
        .lines()
        .find(|l| l.contains("\" | \""))
        .expect("extension match arm in is_scanned_source");
    let actual: BTreeSet<String> = line
        .split('"')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect();
    assert_eq!(
        listed, actual,
        "docs/capabilities.md extensions differ from src/file_finder.rs"
    );
}
