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
    let routes = synthesized_routes("nextjs-app", &builtin_conventions(&["Next.js".to_string()]));

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
    let conventions = builtin_conventions(&["Next.js".to_string()]);

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
    let routes = synthesized_routes("astro", &builtin_conventions(&["Astro".to_string()]));

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
    let routes = synthesized_routes("remix-flat", &builtin_conventions(&["Remix".to_string()]));

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

#[test]
fn flat_route_convention_rejects_a_non_flat_layout() {
    // The convention must be scoped by its own root and grammar, not merely by
    // the framework gate: run a non-empty flat convention over the app-router
    // and astro trees and expect nothing.
    let conventions = builtin_conventions(&["Remix".to_string()]);
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
        let routes =
            synthesized_routes("remix-flat", &builtin_conventions(&[framework.to_string()]));
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
    let routes = synthesized_routes("astro", &builtin_conventions(&["express".to_string()]));
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
    let routes = synthesized_routes("nextjs-app", &builtin_conventions(&["Astro".to_string()]));
    assert!(
        routes.is_empty(),
        "astro conventions must not match app-router files, got {routes:?}"
    );
}
