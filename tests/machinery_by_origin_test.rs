//! A type is framework machinery by where it is declared, never by its name
//! (blueprint row N8).
//!
//! A service's own payload types are often called `Context`, `Request` or
//! `Router`. They are the service's contract, not a framework's request or
//! reply object, so they keep their type. The platform's `Response`, declared
//! by the TypeScript library, is still machinery and still reads `unknown`.
//!
//! Drives the real sidecar over `variable` inferences, whose text is the
//! declared type's bare name: the shape the name rule scrubbed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use tempfile::TempDir;

const APP_TS: &str = r#"export interface Context { id: string; tenant: string }
export interface Request { orderId: string; quantity: number }
export interface Router { routes: string[] }

declare function load<T>(): T;

export const ctx: Context = load<Context>();
export const req: Request = load<Request>();
export const router: Router = load<Router>();

export async function call() {
  const res: Response = await fetch("http://orders.internal/orders");
  return res;
}
"#;

fn write_fixture_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).expect("create src");
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "orders-api", "version": "1.0.0" }"#,
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
    fs::write(repo.join("src/app.ts"), APP_TS).unwrap();
}

fn variable_item(file: &str, declaration: &str, alias: &str) -> InferRequestItem {
    let start = APP_TS
        .find(declaration)
        .unwrap_or_else(|| panic!("fixture must contain {declaration}"));
    let line = APP_TS[..start].lines().count() as u32 + 1;
    InferRequestItem {
        file_path: file.to_string(),
        line_number: line,
        span_start: Some(start as u32),
        span_end: Some((start + declaration.len()) as u32),
        expression_text: None,
        expression_line: None,
        infer_kind: InferKind::Variable,
        alias: Some(alias.to_string()),
        param_name: None,
        member: None,
        result: None,
        route_path: None,
        at_registration: false,
    }
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
fn a_users_own_types_named_like_framework_machinery_keep_their_type() {
    let Some(sidecar_path) = sidecar_path() else {
        return;
    };
    let temp_dir = TempDir::new().expect("temp dir");
    let repo = temp_dir.path().join("orders-api");
    write_fixture_repo(&repo);
    let file = repo.join("src/app.ts").to_string_lossy().to_string();

    let items = vec![
        variable_item(&file, "ctx: Context = load<Context>()", "OwnContext"),
        variable_item(&file, "req: Request = load<Request>()", "OwnRequest"),
        variable_item(&file, "router: Router = load<Router>()", "OwnRouter"),
        variable_item(
            &file,
            "res: Response = await fetch(\"http://orders.internal/orders\")",
            "PlatformResponse",
        ),
    ];

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let result = sidecar
        .resolve_all_types(&[], &items, None, None)
        .expect("resolve_all_types");

    let text_of = |alias: &str| {
        result
            .inferred_types
            .iter()
            .find(|t| t.alias == alias)
            .unwrap_or_else(|| panic!("expected an inference for {alias}: {:?}", result.errors))
            .type_string
            .clone()
    };

    assert_eq!(text_of("OwnContext"), "Context");
    assert_eq!(text_of("OwnRequest"), "Request");
    assert_eq!(text_of("OwnRouter"), "Router");
    // The platform's own `Response` is declared by the TypeScript library: it
    // is the transport, not a body, and stays `unknown`.
    assert_eq!(text_of("PlatformResponse"), "unknown");
    let dts = result.dts_content.unwrap_or_default();
    assert!(
        dts.contains("OwnContext = Context"),
        "the alias must be defined by the user's type, got:\n{dts}"
    );
}
