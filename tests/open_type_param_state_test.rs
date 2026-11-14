//! A consumer read inside a generic function whose answer is still the
//! function's own type parameter states no type (carrick#4201), even when the
//! model named a type for the same alias (carrick#4289).
//!
//! Drives the real sidecar through `resolve_all_types` with the explicit
//! request and the inference request `collect_type_requests` issues for a
//! consumer call the model typed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::services::type_sidecar::{InferKind, InferRequestItem, SymbolRequest, TypeSidecar};
use tempfile::TempDir;

const CLIENT_TS: &str = r#"export interface Feed<T> { items: T[]; cursor: string }

export class FeedClient {
  async fetchFeed<T>(id: string): Promise<Feed<T>> {
    const res = await fetch(`/feeds/${id}`);
    return (await res.json()) as Feed<T>;
  }
}

export class FeedReader {
  constructor(private client: FeedClient) {}

  async read<T>(id: string): Promise<Feed<T>> {
    return await this.client.fetchFeed(id);
  }
}
"#;

/// Line of `this.client.fetchFeed(id)` in `CLIENT_TS`.
const CALL_LINE: u32 = 14;

fn write_fixture_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "feed-reader", "version": "1.0.0" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "lib": ["ES2022", "DOM"],
    "strict": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(repo.join("src/client.ts"), CLIENT_TS).unwrap();
}

fn sidecar_path() -> Option<PathBuf> {
    let node = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !node {
        eprintln!("Skipping test: Node.js not available");
        return None;
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    if !path.exists() {
        eprintln!("Skipping test: sidecar not built (run: cd src/sidecar && npm run build)");
        return None;
    }
    Some(path)
}

#[test]
fn a_model_type_and_a_root_abstain_answer_the_same_alias() {
    let Some(sidecar_path) = sidecar_path() else {
        return;
    };
    let temp_dir = TempDir::new().expect("temp dir");
    let repo = temp_dir.path().join("feed-reader");
    write_fixture_repo(&repo);
    let client = repo.join("src/client.ts").to_string_lossy().to_string();
    let alias = "Feed_Consumer_Response";

    let explicit = vec![SymbolRequest {
        symbol_name: "Feed".to_string(),
        source_file: client.clone(),
        alias: Some(alias.to_string()),
        array_depth: None,
        payload_borrow_witness: false,
        consumer_response: true,
    }];
    let infer = vec![InferRequestItem {
        file_path: client.clone(),
        line_number: CALL_LINE,
        span_start: None,
        span_end: None,
        expression_text: Some("this.client.fetchFeed(id)".to_string()),
        expression_line: Some(CALL_LINE),
        infer_kind: InferKind::CallResult,
        alias: Some(alias.to_string()),
        param_name: None,
        member: None,
        result: None,
        route_path: None,
        at_registration: false,
        result_unanswered: false,
        through_call: None,
    }];

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let result = sidecar
        .resolve_all_types(&explicit, &infer, None, None)
        .expect("resolve_all_types");

    // The two answers the manifest join receives for one alias: the bundle
    // defines it from the model's symbol, with the type parameter left
    // dangling, while the compiler's read abstains at the type's root. The
    // join (`enrich_manifest_with_type_resolution`) must read the entry
    // `Unknown` from these; its unit test holds that half.
    let bundled = result
        .explicit_manifest
        .iter()
        .find(|entry| entry.alias == alias)
        .unwrap_or_else(|| {
            panic!(
                "the bundle answers {alias}: {:#?}",
                result.explicit_manifest
            )
        });
    assert!(bundled.is_explicit, "{bundled:#?}");
    assert!(bundled.type_string.contains("T[]"), "{bundled:#?}");

    let read = result
        .inferred_types
        .iter()
        .find(|inferred| inferred.alias == alias)
        .unwrap_or_else(|| panic!("the read answers {alias}: {:#?}", result.inferred_types));
    assert_eq!(read.type_string, "unknown", "{read:#?}");
    let root: Vec<&str> = read
        .any_provenance
        .iter()
        .filter(|p| p.path.is_empty())
        .map(|p| p.reason.as_str())
        .collect();
    assert_eq!(root, vec!["bound_by_callers"], "{read:#?}");
}
