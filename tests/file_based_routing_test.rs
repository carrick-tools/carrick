//! End-to-end coverage for file-based routing against *real on-disk fixtures*.
//!
//! The unit tests in `src/file_based_router.rs` and `src/agents/file_orchestrator.rs`
//! feed synthetic string paths into the deriver. These tests instead walk the
//! actual fixture trees under `tests/fixtures/{nextjs-app,astro,remix-flat}` and run them
//! through the same deterministic synthesis the orchestrator uses in production
//! (`FileOrchestrator::file_based_endpoints` over `builtin_conventions`), so a
//! regression in route derivation, the SWC handler extractor, or the framework
//! gate is caught with files that look like a user's repository.

use carrick::agents::file_orchestrator::FileOrchestrator;
use carrick::file_based_router::{RoutingConvention, builtin_conventions};
use carrick::swc_scanner::SwcScanner;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Recursively collect every file under `dir`.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {}: {}", dir.display(), e));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Derive every `METHOD path` pair the file-based pass would synthesize for a
/// fixture, exactly as the orchestrator does: relativize against the repo root,
/// run the SWC gatekeeper's handler extractor, gate on `builtin_conventions`.
fn synthesized_routes(fixture: &str, conventions: &[RoutingConvention]) -> BTreeSet<String> {
    let root = fixture_root(fixture);
    let scanner = SwcScanner::new();
    let mut files = Vec::new();
    walk(&root, &mut files);

    let mut routes = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(&root).expect("file under fixture root");
        let content = fs::read_to_string(file).expect("read fixture file");
        let endpoints =
            FileOrchestrator::file_based_endpoints(&scanner, rel, file, &content, conventions);
        for ep in endpoints {
            // Every synthesized file-based endpoint must carry the metadata the
            // downstream sidecar type-resolution relies on: a convention label,
            // the file-based owner marker, and a declaration span. Asserting it
            // here means a regression that produced the right method+path but
            // dropped this metadata is caught, not silently projected away.
            assert!(
                !ep.pattern_matched.is_empty(),
                "{rel:?}: endpoint missing convention label"
            );
            assert_eq!(
                ep.owner_node, "__file_based_route__",
                "{rel:?}: endpoint not tagged as file-based"
            );
            assert!(
                ep.call_expression_span_start.is_some(),
                "{rel:?}: endpoint missing handler declaration span"
            );
            routes.insert(format!("{} {}", ep.method, ep.path));
        }
    }
    routes
}

#[test]
fn nextjs_app_router_fixture_derives_expected_routes() {
    let routes = synthesized_routes(
        "nextjs-app",
        &builtin_conventions(&["Next.js".to_string()], &[]),
    );

    let expected: BTreeSet<String> = [
        "GET /users",
        "POST /users",
        "GET /users/:id",
        "DELETE /users/:id",
        "GET /health",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "app-router fixture should yield exactly the route handlers, \
         and skip page.tsx / non-HTTP exports like `runtime`"
    );
}

/// Monorepo trap: the app lives under `apps/web/app/**`, so deriving against
/// the REPO root leaves paths (`apps/web/app/...`) that no convention root
/// glob (`app`, `src/app`) matches — zero routes, silently. The engine must
/// strip the SERVICE root (carrick.json `services[].directory`, here
/// `apps/web`) instead; deriving against it yields the routes. The fixture
/// also locks two export shapes route files use in the wild: a wrapped-const
/// handler (`export const GET = wrapper({...})`) and a pure re-export route
/// file whose handlers live outside the `app/` tree (the modules file itself
/// derives nothing).
#[test]
fn nextjs_app_router_monorepo_derives_routes_relative_to_service_root() {
    let conventions = builtin_conventions(&["Next.js".to_string()], &[]);

    let repo_root_relative = synthesized_routes("nextjs-app-monorepo", &conventions);
    assert!(
        repo_root_relative.is_empty(),
        "repo-root-relative derivation must find nothing in a monorepo layout \
         (this is why the engine strips the service root), got {repo_root_relative:?}"
    );

    let routes = synthesized_routes("nextjs-app-monorepo/apps/web", &conventions);
    let expected: BTreeSet<String> = [
        "GET /api/v1/client/:workspaceId/environment",
        "POST /api/v2/client/:workspaceId/user",
        "OPTIONS /api/v2/client/:workspaceId/user",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        routes, expected,
        "service-root-relative derivation should yield the app routes: the \
         wrapped-const GET, and the re-exported POST/OPTIONS at the app-side \
         path (never the modules/ implementation path)"
    );
}

#[test]
fn astro_fixture_derives_expected_routes() {
    let routes = synthesized_routes("astro", &builtin_conventions(&["Astro".to_string()], &[]));

    let expected: BTreeSet<String> = [
        "GET /api/users",
        "POST /api/users",
        "GET /posts/:id",
        "GET /health",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "astro fixture should yield endpoints from .ts/.js files only — \
         skipping index.astro, _helpers.ts, and the `prerender` export"
    );
}

/// carrick#473: flat routes pack the whole route chain into one dot-separated
/// filename, and their route modules export a *read* handler and a *write*
/// handler rather than one export per HTTP method. Crucially, those handlers
/// are often the RESULT OF A CALL — a route-builder factory owns auth, parsing
/// and the response envelope — so the module contains no HTTP registration and
/// no path literal at all. Measured on a large OSS TypeScript monorepo, 86 of
/// its 140 internal route files were `Skipped (no API patterns)` for exactly
/// this reason.
///
/// The fixture locks both the recall and the precision side: the declared and
/// called forms must derive identically, while the UI page plane (`.tsx`),
/// framework-private files, and non-handler exports must derive nothing.
#[test]
fn flat_routes_fixture_derives_expected_routes() {
    let routes = synthesized_routes(
        "remix-flat",
        &builtin_conventions(&["Remix".to_string()], &[]),
    );

    let expected: BTreeSet<String> = [
        // Call-expression export, dot-separated path, `$param` -> `:param`.
        "POST /api/v1/widgets/:widgetId/activate",
        "GET /api/v1/widgets/:widgetId",
        // Declared-function exports derive exactly the same way.
        "GET /api/v1/widgets",
        "POST /api/v1/widgets",
        // Bare `$` is the splat; a trailing `index` collapses onto its parent
        // (which is why `/api/v1/widgets` above is not duplicated).
        "GET /api/v1/blobs/**",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "flat-route fixture should yield the .ts route handlers only — \
         skipping the .tsx page plane, the `_`-prefixed module, and the \
         `config`/helper exports, and never deriving from the route builder \
         module itself"
    );
}

/// The gate must open on the manifest alone. Framework detection reports the
/// HTTP *server* a service runs, so a file-routed app served by a generic HTTP
/// server is reported as that server and nothing else — which is exactly the
/// shape measured on a large OSS TypeScript monorepo, where the whole flat-route
/// tree stayed dark. The service's declared dependencies say what its routing
/// scheme is without asking a model anything.
#[test]
fn declared_dependencies_alone_open_the_flat_route_gate() {
    let detected_server = vec!["express".to_string()];

    let dark = synthesized_routes("remix-flat", &builtin_conventions(&detected_server, &[]));
    assert!(
        dark.is_empty(),
        "framework labels alone cannot open the gate here, got {dark:?}"
    );

    let lit = synthesized_routes(
        "remix-flat",
        &builtin_conventions(&detected_server, &["@remix-run/node".to_string()]),
    );
    assert_eq!(
        lit,
        synthesized_routes(
            "remix-flat",
            &builtin_conventions(&["Remix".to_string()], &[])
        ),
        "a declared dependency must derive exactly the routes the framework label does"
    );
    assert!(!lit.is_empty(), "fixture should yield routes");
}

#[test]
fn flat_route_convention_rejects_a_non_flat_layout() {
    // The convention must be scoped by its own root and grammar, not merely by
    // the framework gate: run a non-empty flat convention over the app-router
    // and astro trees and expect nothing.
    let conventions = builtin_conventions(&["Remix".to_string()], &[]);
    for fixture in ["nextjs-app", "astro"] {
        let routes = synthesized_routes(fixture, &conventions);
        assert!(
            routes.is_empty(),
            "flat-route conventions must not match {fixture} files, got {routes:?}"
        );
    }
}

#[test]
fn other_conventions_reject_the_flat_route_layout() {
    // The reverse containment: the app-router/astro conventions must not claim
    // flat-route files, so adding this convention cannot re-interpret a repo
    // that another convention already covers.
    for framework in ["Next.js", "Astro"] {
        let routes = synthesized_routes(
            "remix-flat",
            &builtin_conventions(&[framework.to_string()], &[]),
        );
        assert!(
            routes.is_empty(),
            "{framework} conventions must not match flat-route files, got {routes:?}"
        );
    }
}

#[test]
fn file_based_pass_is_noop_without_matching_framework() {
    // No convention-bearing framework detected → empty conventions → no routes,
    // regardless of what's on disk.
    let routes = synthesized_routes("astro", &builtin_conventions(&["express".to_string()], &[]));
    assert!(
        routes.is_empty(),
        "no endpoints expected when no file-based framework is detected, got {routes:?}"
    );
}

#[test]
fn astro_convention_rejects_a_non_astro_layout() {
    // Stronger than the empty-conventions gate: run a *non-empty* Astro
    // convention set over the Next.js app-router tree (which has no `src/pages`).
    // The matcher must reject every file via strip_root/raw_segments and yield
    // nothing — proving the convention is correctly scoped, not just that an
    // empty slice produces nothing.
    let routes = synthesized_routes(
        "nextjs-app",
        &builtin_conventions(&["Astro".to_string()], &[]),
    );
    assert!(
        routes.is_empty(),
        "astro conventions must not match app-router files, got {routes:?}"
    );
}

/// carrick#601: a route module that exports a generic write handler and
/// narrows the HTTP method with a guard inside the body serves exactly the
/// guarded verb. Emitting the convention's default verb as well produces a
/// phantom row — an endpoint nothing serves — and a consumer call extracted
/// with the wrong method matches it and reads as a confident green edge. So
/// the guard, where one exists, is the only row; where none exists, the
/// convention's default stands.
#[test]
fn method_guard_replaces_the_convention_default_verb() {
    let routes = synthesized_routes(
        "flat-routes-method-guard",
        &builtin_conventions(&["Remix".to_string()], &[]),
    );

    let expected: BTreeSet<String> = [
        // Guarded on PUT: the default (POST) is never served, so it is absent.
        "PUT /api/v1/items/:itemId",
        // Guarded on GET through a destructured local binding.
        "GET /api/v1/items/:itemId/status",
        // carrick#622: the guard is a comparison against a call on the member
        // (`request.method.toUpperCase()`), which narrows the same way.
        "PUT /api/v1/items/:itemId/archive",
        // The mirror: case-folded down, with a lowercase literal to match.
        "GET /api/v1/items/:itemId/labels",
        // A switch on the case-folded method serves the verbs it branches on.
        "PATCH /api/v1/items/:itemId/settings",
        "DELETE /api/v1/items/:itemId/settings",
        // carrick#628: a branch on OPTIONS is a CORS preflight the handler
        // answers, not the verb it serves, so it never displaces the read
        // export's default. Both spellings of the branch read the same.
        "GET /api/v1/items/:itemId/preflight",
        "GET /api/v1/items/:itemId/preflightFolded",
        // A preflight branch alongside a real narrowing leaves the narrowing
        // alone, and still no OPTIONS row.
        "PUT /api/v1/items/:itemId/preflightWrite",
        // HEAD is protocol plumbing too, so a handler branching only on it
        // reads as unguarded and the write export's default stands.
        "POST /api/v1/items/:itemId/probe",
        // No guard: the convention's default for a write export stands.
        "POST /api/v1/items",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "a method-guarded handler should yield exactly the guarded verb and no \
         phantom default-verb row"
    );
}
