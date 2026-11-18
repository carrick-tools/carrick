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
use carrick::file_based_router::{RoutingConvention, builtin_conventions, conventions_for_service};
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
    let routes = synthesized_routes("nextjs-app", &builtin_conventions(&["next".to_string()]));

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
    let conventions = builtin_conventions(&["next".to_string()]);

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
    let routes = synthesized_routes("astro", &builtin_conventions(&["astro".to_string()]));

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
    let routes = synthesized_routes("remix-flat", &builtin_conventions(&["remix".to_string()]));

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
    let rows = synthesized_rows("remix-flat", &builtin_conventions(&["remix".to_string()]));
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
    let dark = synthesized_routes("remix-flat", &builtin_conventions(&[]));
    assert!(
        dark.is_empty(),
        "no declared dependency leaves the gate shut, got {dark:?}"
    );

    let lit = synthesized_routes(
        "remix-flat",
        &builtin_conventions(&["@remix-run/node".to_string()]),
    );
    assert_eq!(
        lit,
        synthesized_routes("remix-flat", &builtin_conventions(&["remix".to_string()])),
        "a declared dependency must derive exactly the routes the framework label does"
    );
    assert!(!lit.is_empty(), "fixture should yield routes");
}

#[test]
fn flat_route_convention_rejects_a_non_flat_layout() {
    // The convention must be scoped by its own root and grammar, not merely by
    // the framework gate: run a non-empty flat convention over the app-router
    // and astro trees and expect nothing.
    let conventions = builtin_conventions(&["remix".to_string()]);
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
    for framework in ["next", "astro"] {
        let routes =
            synthesized_routes("remix-flat", &builtin_conventions(&[framework.to_string()]));
        assert!(
            routes.is_empty(),
            "{framework} conventions must not match flat-route files, got {routes:?}"
        );
    }
}

/// Blueprint row N7a (carrick#3383): conventions are chosen by the declared
/// package name, matched by exact equality. The deleted rule matched a
/// substring of detection's free-text label, so a name that merely contained
/// "next", "astro" or "remix" claimed a convention.
#[test]
fn a_package_name_that_only_contains_a_framework_word_opens_no_convention() {
    let lookalikes: Vec<String> = ["nextra", "my-next-thing", "astro-icon", "remix-utils"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(
        builtin_conventions(&lookalikes).is_empty(),
        "a substring of a framework name is not a declared framework"
    );

    let declared = builtin_conventions(&["Next".to_string()]);
    assert!(
        !declared.is_empty(),
        "the exact package name opens its convention, whatever its case"
    );
}

#[test]
fn file_based_pass_is_noop_without_matching_framework() {
    // No convention-bearing framework detected → empty conventions → no routes,
    // regardless of what's on disk.
    let routes = synthesized_routes("astro", &builtin_conventions(&[]));
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
    let routes = synthesized_routes("nextjs-app", &builtin_conventions(&["astro".to_string()]));
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
        &builtin_conventions(&["remix".to_string()]),
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
/// carrick#601, carrick#2740: a role-named export whose body compares the
/// request's method serves the verbs it compares, read from the fact layer's
/// guard IR (`request_summary::compared_methods`), and the default verb of
/// its role is not a row.
fn a_compared_request_method_narrows_a_role_named_export() {
    let routes = synthesized_routes(
        "flat-routes-method-guard",
        &builtin_conventions(&["remix".to_string()]),
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
        // carrick#2740: a switch on a local holding the folded method, in an
        // arrow-const export.
        "DELETE /api/v1/items/:itemId/notes",
        "PUT /api/v1/items/:itemId/notes",
        // A branch that returns, then a negated test that exits.
        "DELETE /api/v1/items/:itemId/tags",
        "PUT /api/v1/items/:itemId/tags",
        // A declaration exported by an export list under a role name.
        "DELETE /api/v1/items/:itemId/links",
        "PATCH /api/v1/items/:itemId/links",
        // An arrow export under `satisfies`: the wrapper states a type, so the
        // narrowing still reads and the write default (POST) is not a row.
        "PUT /api/v1/items/:itemId/flags",
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

/// One stated row of a folder-named function: (repo-relative file, method,
/// path, handler, line, convention).
type FunctionRow = (String, String, String, String, i32, String);

/// A route whose handler was not located: (repo-relative file, line, what
/// the serving call was handed).
type Unplaced = (String, u32, Option<String>);

/// The `handler_name` of a row whose handler was not located.
const NOT_LOCATED: &str = "__handler_not_located__";

/// What the file-based pass states for every file under `service_root` of
/// `repo`, with the conventions that service runs under
/// ([`conventions_for_service`], the selection every caller reads): the rows,
/// and where a convention stated a route and located no handler.
fn service_function_rows(
    repo: &Path,
    service_root: &Path,
    conventions: &[RoutingConvention],
) -> (BTreeSet<FunctionRow>, BTreeSet<Unplaced>) {
    let scanner = SwcScanner::new();
    let mut files = Vec::new();
    walk(service_root, &mut files);
    let mut rows = BTreeSet::new();
    let mut unplaced = BTreeSet::new();
    for file in &files {
        let rel = file.strip_prefix(service_root).expect("file under service");
        let in_repo = file
            .strip_prefix(repo)
            .expect("file under repo")
            .to_string_lossy()
            .to_string();
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        let derivation =
            FileOrchestrator::file_based_endpoints(&scanner, rel, file, &content, conventions);
        if let Some(at) = &derivation.handler_not_located {
            unplaced.insert((in_repo.clone(), at.line, at.passed.clone()));
        }
        assert_eq!(
            derivation.claimed,
            !derivation.endpoints.is_empty(),
            "{in_repo}: a stated route claims the module, and only then"
        );
        for ep in derivation.endpoints {
            assert_eq!(ep.owner_node, "__file_based_route__", "{in_repo}");
            let located = ep.handler_name != NOT_LOCATED;
            assert_eq!(
                located,
                derivation.handler_not_located.is_none(),
                "{in_repo}"
            );
            if located {
                assert!(ep.call_expression_span_start.is_some(), "{in_repo}");
                assert_eq!(
                    ep.handler_declaration_line,
                    Some(ep.line_number as u32),
                    "{in_repo}"
                );
            } else {
                assert_eq!(ep.handler_declaration_line, None, "{in_repo}");
            }
            rows.insert((
                in_repo.clone(),
                ep.method,
                ep.path,
                ep.handler_name,
                ep.line_number,
                ep.pattern_matched,
            ));
        }
    }
    (rows, unplaced)
}

fn function_rows_expected() -> BTreeSet<FunctionRow> {
    [
        (
            "supabase/functions/price-quote/index.ts",
            "/functions/v1/price-quote",
            "anonymous",
            8,
        ),
        (
            "supabase/functions/team-digest/index.ts",
            "/functions/v1/team-digest",
            "anonymous",
            1,
        ),
        (
            "supabase/functions/purge-sessions/index.ts",
            "/functions/v1/purge-sessions",
            "purgeSessions",
            3,
        ),
        // An imported handler: the route is stated, the handler is not.
        (
            "supabase/functions/send-invite/index.ts",
            "/functions/v1/send-invite",
            NOT_LOCATED,
            4,
        ),
        // No serving call at all: still the folder's route, with no handler.
        (
            "supabase/functions/report-export/index.ts",
            "/functions/v1/report-export",
            NOT_LOCATED,
            1,
        ),
    ]
    .into_iter()
    .map(|(file, path, handler, line)| {
        (
            file.to_string(),
            "ALL".to_string(),
            path.to_string(),
            handler.to_string(),
            line,
            "supabase-functions".to_string(),
        )
    })
    .collect()
}

/// A function served by its folder (`supabase/functions/<name>/index.ts`) is
/// one row at `ALL /functions/v1/<name>`, its handler the function the module
/// hands to its one top-level call, whether that call is `serve` from a URL
/// import or `Deno.serve`, and whether the function is inline or named. An
/// entry file whose handler is not located (an imported handler, no serving
/// call) still states its one row, with no handler, and is an unplaced
/// handler naming what the call was handed. Only entry files make rows: the
/// shared `_` folder and a helper module beside an entry state nothing. The
/// same rows come out whether the folder is its own service or sits inside
/// one at the repository root.
#[test]
fn a_folder_named_function_states_one_any_method_route() {
    let repo = fixture_root("hosted-functions");
    let unplaced_expected = BTreeSet::from([
        (
            "supabase/functions/send-invite/index.ts".to_string(),
            4,
            Some("handleInvite".to_string()),
        ),
        (
            "supabase/functions/report-export/index.ts".to_string(),
            1,
            None,
        ),
    ]);
    for service_root in [repo.clone(), repo.join("supabase/functions")] {
        let conventions =
            conventions_for_service(&["@supabase/supabase-js".to_string()], &repo, &service_root);
        let (rows, unplaced) = service_function_rows(&repo, &service_root, &conventions);
        assert_eq!(rows, function_rows_expected(), "{}", service_root.display());
        assert_eq!(unplaced, unplaced_expected, "{}", service_root.display());
    }
    // A service elsewhere in the repository holds none of the folder.
    let elsewhere = conventions_for_service(&[], &repo, &repo.join("src"));
    assert!(
        elsewhere.is_empty(),
        "{:?}",
        elsewhere.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
}

/// The derivation of one fixture file under the root service.
fn derive_function_file(rel: &str) -> carrick::agents::file_orchestrator::FileRouteDerivation {
    let repo = fixture_root("hosted-functions");
    let conventions = conventions_for_service(&[], &repo, &repo);
    let file = repo.join(rel);
    let content = fs::read_to_string(&file).unwrap();
    FileOrchestrator::file_based_endpoints(
        &SwcScanner::new(),
        Path::new(rel),
        &file,
        &content,
        &conventions,
    )
}

/// `serve(importedHandler)`: the layout states the route, so the entry file
/// is one claimed row at its real address with no handler, and the unread
/// site names what `serve` was handed.
#[test]
fn an_imported_handler_states_the_route_with_no_handler() {
    let derivation = derive_function_file("supabase/functions/send-invite/index.ts");
    assert!(derivation.claimed);
    let rows: Vec<_> = derivation
        .endpoints
        .iter()
        .map(|ep| {
            (
                ep.method.as_str(),
                ep.path.as_str(),
                ep.handler_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![("ALL", "/functions/v1/send-invite", NOT_LOCATED)]
    );
    let at = derivation.handler_not_located.expect("an unread site");
    assert_eq!((at.line, at.passed.as_deref()), (4, Some("handleInvite")));
}

/// A helper module in a `_` folder, or beside an entry file, is never a
/// function: no row, no claim, no unread site.
#[test]
fn a_helper_file_states_nothing() {
    for rel in [
        "supabase/functions/_shared/cors.ts",
        "supabase/functions/_shared/index.ts",
        "supabase/functions/send-invite/handler.ts",
    ] {
        let derivation = derive_function_file(rel);
        assert!(derivation.endpoints.is_empty(), "{rel}");
        assert!(!derivation.claimed, "{rel}");
        assert!(derivation.handler_not_located.is_none(), "{rel}");
    }
}

/// Copy `src` to `dst`, leaving out the repository-relative `skip`.
fn copy_tree(src: &Path, dst: &Path, root: &Path, skip: &str) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.strip_prefix(root).unwrap() == Path::new(skip) {
            continue;
        }
        let to = dst.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &to, root, skip);
        } else {
            fs::copy(&path, &to).unwrap();
        }
    }
}

/// The platform's config file is what selects the layout: the same tree
/// without it states no function row and saves no unplaced handler.
#[test]
fn no_marker_file_states_no_function_route() {
    let fixture = fixture_root("hosted-functions");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_tree(&fixture, &repo, &fixture, "supabase/config.toml");
    assert!(
        repo.join("supabase/functions/price-quote/index.ts")
            .is_file()
    );
    let conventions = conventions_for_service(&["@supabase/supabase-js".to_string()], &repo, &repo);
    assert!(conventions.is_empty());
    let (rows, unplaced) = service_function_rows(&repo, &repo, &conventions);
    assert!(rows.is_empty(), "{rows:?}");
    assert!(unplaced.is_empty(), "{unplaced:?}");
}

/// A second layout of the same kind is data only: another root, another
/// marker, another prefix, through the same executor.
#[test]
fn a_second_folder_layout_is_a_convention_entry_of_data() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    fs::create_dir_all(repo.join("edge/handlers/ping")).unwrap();
    fs::write(repo.join("edge.toml"), "").unwrap();
    fs::write(
        repo.join("edge/handlers/ping/index.ts"),
        "addHandler(async () => new Response(\"pong\"));\n",
    )
    .unwrap();
    let invented = RoutingConvention {
        name: "invented-edge".to_string(),
        root_globs: vec!["edge/handlers".to_string()],
        path_prefix: "/fn".to_string(),
        marker_files: vec!["edge.toml".to_string()],
        ..RoutingConvention::supabase_functions()
    };
    let (rows, _) = service_function_rows(repo, repo, &[invented]);
    let rows: Vec<_> = rows
        .into_iter()
        .map(|(file, method, path, ..)| format!("{file} {method} {path}"))
        .collect();
    assert_eq!(rows, vec!["edge/handlers/ping/index.ts ALL /fn/ping"]);
}
