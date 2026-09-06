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
    synthesized_rows(fixture, conventions)
        .into_iter()
        .map(|(_, method, path, _)| format!("{method} {path}"))
        .collect()
}

/// The same derivation, one row per (file, method, path, `view_module`), for
/// the assertions that are about a particular module rather than the route set:
/// two modules legitimately serve one path (a layout and its index), so the
/// route set alone cannot say which of them renders a view.
fn synthesized_rows(
    fixture: &str,
    conventions: &[RoutingConvention],
) -> BTreeSet<(String, String, String, bool)> {
    let root = fixture_root(fixture);
    let scanner = SwcScanner::new();
    let mut files = Vec::new();
    walk(&root, &mut files);

    let mut routes = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(&root).expect("file under fixture root");
        let Ok(content) = fs::read_to_string(file) else {
            // A fixture may carry a manifest or another non-source file; the
            // deriver only ever claims source modules anyway.
            continue;
        };
        let endpoints =
            FileOrchestrator::file_based_endpoints(&scanner, rel, file, &content, conventions)
                .endpoints;
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
            routes.insert((
                rel.to_string_lossy().to_string(),
                ep.method.clone(),
                ep.path.clone(),
                ep.view_module,
            ));
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
/// The fixture locks both the recall and the precision side. Recall: the
/// declared and called forms derive identically, a chain written as a
/// DIRECTORY derives what the single-file spelling does (carrick#701), a
/// `_`-prefixed piece is a pathless layout rather than a private file
/// (carrick#702), and a `.tsx` module that exports a handler is the route it
/// serves (carrick#704 / R1b). Precision: a module exporting only a component
/// derives nothing, and neither do `config`/helper exports or the route
/// builder module itself.
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
        // The chain written as a directory holding a terminal module: the
        // directory name splits on the same separator the stem does and the
        // `route` stem contributes nothing (carrick#701).
        "GET /projects/v3/:projectRef/metrics",
        // A `_`-prefixed piece nests without contributing a segment, so this
        // module is a route and not a skipped private file (carrick#702). The
        // `.tsx` page at the same path derives the same row from its own file.
        "GET /widgets/:widgetId",
        // `_index` is pathless too, so this serves the parent path.
        "GET /admin",
        // A `.tsx` module with a handler and no component is a resource route.
        "GET /resources/things",
        // The decoy module's own handlers ARE routes; what it must not yield
        // is a row for the form discriminator in its schema (carrick#703,
        // pinned end-to-end in `deterministic_emission_test`).
        "GET /settings/builds",
        "POST /settings/builds",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "flat-route fixture should yield every module that exports a handler, \
         whatever its extension — while a page exporting only a component, the \
         `config`/helper exports, and the route builder module itself yield \
         nothing"
    );
}

/// R1b's second half: the row records whether the module that serves the route
/// also renders a view, read off its export list. Asserted per FILE, because
/// two modules legitimately serve one path — a pathless layout and the `.tsx`
/// page beneath it both answer `GET /widgets/:widgetId` — and the route set
/// cannot tell them apart.
#[test]
fn flat_routes_fixture_marks_view_modules() {
    let rows = synthesized_rows(
        "remix-flat",
        &builtin_conventions(&["Remix".to_string()], &[]),
    );
    let view_module_of = |file: &str, method: &str| -> bool {
        let found: Vec<&(String, String, String, bool)> = rows
            .iter()
            .filter(|(row_file, row_method, _, _)| row_file == file && row_method == method)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "expected one {method} row in {file}: {rows:#?}"
        );
        found[0].3
    };

    for (file, method) in [
        // A handler plus a default export: the module serves the route AND
        // renders the page.
        ("app/routes/admin._index.tsx", "GET"),
        ("app/routes/settings.builds.tsx", "GET"),
        ("app/routes/settings.builds.tsx", "POST"),
        ("app/routes/widgets.$widgetId.tsx", "GET"),
    ] {
        assert!(
            view_module_of(file, method),
            "{file} exports a component alongside its handler"
        );
    }

    for (file, method) in [
        // Handler, no component: an API surface, whatever the extension.
        ("app/routes/resources.things.tsx", "GET"),
        ("app/routes/api.v1.widgets.ts", "GET"),
        ("app/routes/api.v1.widgets.ts", "POST"),
        ("app/routes/_app.widgets.$widgetId.ts", "GET"),
        ("app/routes/projects.v3.$projectRef.metrics/route.ts", "GET"),
    ] {
        assert!(!view_module_of(file, method), "{file} exports no component");
    }
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
/// carrick#665: a route builder that takes the HTTP method as an option
/// states the route's verb outright. Reading only the convention's default for
/// the export name records a PUT route as a POST, and the consumer that really
/// does PUT it then matches nothing — the producer reads as absent while a
/// route nobody calls reads as present.
#[test]
fn a_declared_method_option_replaces_the_convention_default_verb() {
    let routes = synthesized_routes(
        "flat-routes-declared-method",
        &builtin_conventions(&["Remix".to_string()], &[]),
    );

    let expected: BTreeSet<String> = [
        // Declared PUT: the write export's default (POST) is never served.
        "PUT /api/v1/things/:thingId",
        // A list of verbs is a route serving each of them.
        "PATCH /api/v1/things/:thingId/labels",
        "DELETE /api/v1/things/:thingId/labels",
        // A declared verb outranks a guard in the body.
        "PUT /api/v1/things/:thingId/status",
        // Two handlers out of one call: the option says nothing about which,
        // so both keep the convention's default.
        "GET /api/v1/things/:thingId/pair",
        "POST /api/v1/things/:thingId/pair",
        // A non-literal verb states nothing.
        "POST /api/v1/things/:thingId/dynamic",
        // A `method` nested in another option is not the route's verb.
        "POST /api/v1/things/:thingId/nested",
        // Exported where it is declared, and still narrowed.
        "DELETE /api/v1/things/:thingId/inline",
        // The builder's result parked on a binding, one handler taken off it.
        "PUT /api/v1/things/:thingId/parked",
        // Two handlers off one result: the same ambiguity, so neither narrows.
        "GET /api/v1/things/:thingId/parkedPair",
        "POST /api/v1/things/:thingId/parkedPair",
        // One file, two exports, one of them declaring its verb.
        "GET /api/v1/things",
        "PUT /api/v1/things",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    assert_eq!(
        routes, expected,
        "a route whose builder declares its method should yield exactly that verb"
    );
}

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
