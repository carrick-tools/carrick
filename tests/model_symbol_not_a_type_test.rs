//! A model type hint that names a value, a module or a namespace is not a
//! type, and never defines the alias it was asked for (blueprint row N10c).
//!
//! The model may answer a route's response with the symbol it saw the handler
//! read from: a service object, a function, a package's default export. None
//! of them is a type. The bundler reports such a symbol as `not_a_type`, and
//! the inference asked for the same alias defines it.
//!
//! Drives the real sidecar through `resolve_all_types`, with an explicit
//! request and an inference request for each alias, as `collect_type_requests`
//! issues them for a route the model typed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::services::type_sidecar::{
    InferKind, InferRequestItem, SymbolFailureKind, SymbolRequest, TypeSidecar,
};
use tempfile::TempDir;

const SERVICES_TS: &str = r#"export interface Order { id: string; total: number }

export const orderService = {
  list(): Order[] {
    return [];
  },
};

export function loadOrders(): Order[] {
  return [];
}
"#;

const ROUTES_TS: &str = r#"import fixtureServer from "fixture-server";
import { orderService } from "./services";

export function listOrders() {
  const orders: { id: string; total: number }[] = orderService.list();
  return orders;
}

export const server = fixtureServer();
"#;

fn write_fixture_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("node_modules/fixture-server")).unwrap();
    fs::write(
        repo.join("package.json"),
        r#"{ "name": "orders-api", "version": "1.0.0", "dependencies": { "fixture-server": "1.0.0" } }"#,
    )
    .unwrap();
    fs::write(
        repo.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true
  },
  "include": ["src/**/*"]
}"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/fixture-server/package.json"),
        r#"{ "name": "fixture-server", "version": "1.0.0", "types": "index.d.ts" }"#,
    )
    .unwrap();
    fs::write(
        repo.join("node_modules/fixture-server/index.d.ts"),
        "declare function fixtureServer(): { listen(port: number): void };\nexport default fixtureServer;\n",
    )
    .unwrap();
    fs::write(repo.join("src/services.ts"), SERVICES_TS).unwrap();
    fs::write(repo.join("src/routes.ts"), ROUTES_TS).unwrap();
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

fn explicit(symbol: &str, source: &str, alias: &str) -> SymbolRequest {
    SymbolRequest {
        symbol_name: symbol.to_string(),
        source_file: source.to_string(),
        alias: Some(alias.to_string()),
        array_depth: None,
        payload_borrow_witness: false,
        consumer_response: false,
    }
}

/// What the handler at line 4 returns, asked for `alias`.
fn handler_return(file: &str, alias: &str) -> InferRequestItem {
    InferRequestItem {
        file_path: file.to_string(),
        line_number: 4,
        span_start: None,
        span_end: None,
        expression_text: None,
        expression_line: None,
        infer_kind: InferKind::FunctionReturn,
        alias: Some(alias.to_string()),
        param_name: None,
        member: None,
        result: None,
        route_path: None,
        at_registration: false,
    }
}

/// The text after `export type <alias> =` in the bundle, to the end of its
/// line. The alias must be declared once.
fn alias_body(dts: &str, alias: &str) -> String {
    let marker = format!("type {alias} =");
    assert_eq!(
        dts.matches(&format!(" {alias}")).count(),
        1,
        "{alias} must be declared once, got:\n{dts}"
    );
    let start = dts
        .find(&marker)
        .unwrap_or_else(|| panic!("{alias} must be defined as a type alias, got:\n{dts}"));
    let rest = &dts[start + marker.len()..];
    rest[..rest.find('\n').unwrap_or(rest.len())]
        .trim()
        .to_string()
}

#[test]
fn a_model_symbol_that_is_not_a_type_leaves_the_alias_to_the_inference() {
    let Some(sidecar_path) = sidecar_path() else {
        return;
    };
    let temp_dir = TempDir::new().expect("temp dir");
    let repo = temp_dir.path().join("orders-api");
    write_fixture_repo(&repo);
    let services = repo.join("src/services.ts").to_string_lossy().to_string();
    let routes = repo.join("src/routes.ts").to_string_lossy().to_string();

    let explicit = vec![
        // A value: a service object the handler reads from.
        explicit("orderService", &services, "Value_Response"),
        // A value: a function.
        explicit("loadOrders", &services, "Function_Response"),
        // A package's default export, a function, named by its bare specifier.
        explicit("fixtureServer", "fixture-server", "Package_Response"),
    ];
    let infer = vec![
        handler_return(&routes, "Value_Response"),
        handler_return(&routes, "Function_Response"),
        handler_return(&routes, "Package_Response"),
    ];

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(&repo, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    let result = sidecar
        .resolve_all_types(&explicit, &infer, None, None)
        .expect("resolve_all_types");
    let dts = result.dts_content.clone().unwrap_or_default();

    for alias in ["Value_Response", "Function_Response", "Package_Response"] {
        let body = alias_body(&dts, alias);
        assert!(
            body.contains("total: number") && !body.contains("list(") && !body.contains("listen("),
            "{alias} must be the handler's inferred body, got `{body}` in:\n{dts}"
        );
    }

    let kind_of = |symbol: &str| {
        result
            .symbol_failures
            .iter()
            .find(|failure| failure.symbol_name == symbol)
            .and_then(|failure| failure.kind)
    };
    assert_eq!(kind_of("orderService"), Some(SymbolFailureKind::NotAType));
    assert_eq!(kind_of("loadOrders"), Some(SymbolFailureKind::NotAType));
}
