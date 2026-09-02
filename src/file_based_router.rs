//! Generic file-based routing: derive HTTP route paths from filesystem layout.
//!
//! Some frameworks (Next.js, Remix, SvelteKit, Nuxt, ...) declare API routes by
//! *file location* rather than by a path string in code. The route `/users/:id`
//! for `app/users/[id]/route.ts` appears nowhere in that file's bytes — it lives
//! in the directory structure. The LLM pipeline cannot recover information that
//! is absent from the source it reads, so this module supplies that one
//! structural fact deterministically.
//!
//! The module is framework-agnostic: it executes a [`RoutingConvention`] (plain
//! data), never a hardcoded framework branch. Built-in conventions
//! ([`builtin_conventions`]) exist only as a *bootstrap* so common stacks work
//! out of the box; a convention supplied by framework detection (carrick-cloud)
//! or by `carrick.json` overrides them. This keeps framework knowledge out of
//! the scanner core while still shipping value today.
//!
//! The bootstrap is selected from the service's *declared dependencies* as well
//! as from detection's framework labels, so it does not depend on an LLM having
//! named the file-router framework. Detection classifies the HTTP server a
//! service runs; a file-routed app fronted by a generic HTTP server reports
//! only that server, and the manifest is the reliable witness.

use crate::type_manifest::is_http_method;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// How the HTTP method of a file-based endpoint is determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MethodSource {
    /// The HTTP method is the name of an exported handler function, e.g.
    /// Next.js app-router `export async function GET(...) {}`. Conventions
    /// whose route modules name their exports for a *role* rather than a
    /// method (a read export and a write export) map those names to methods
    /// through [`RoutingConvention::method_exports`].
    ExportName,
    /// A single default-exported handler serves every method and branches on the
    /// request at runtime (e.g. pages-router `req.method`). The concrete method
    /// is not derivable from structure and is left to the LLM / downstream.
    DefaultExport,
}

/// Whether route path segments come from the directory chain (with a fixed
/// terminal filename) or from the filename itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SegmentSource {
    /// App-router style: the endpoint is marked by a fixed terminal filename
    /// (e.g. `route.ts`); the path is built from the enclosing directory chain.
    DirectoryChain { terminal_files: Vec<String> },
    /// Pages-router style: the filename (minus extension) is the final path
    /// segment. `index` collapses to its directory.
    ///
    /// When `segment_separator` is set, the filename is not one segment but a
    /// *flattened chain* of them: the stem is split on that separator and each
    /// piece becomes its own path segment (`a.b.$id.ts` -> `/a/b/:id`). This is
    /// how "flat route" schemes encode nesting without directories.
    FileName {
        extensions: Vec<String>,
        #[serde(default)]
        segment_separator: Option<String>,
    },
}

/// A declarative description of a file-based routing scheme. Executed by
/// [`derive_route`]; never branched on by framework name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingConvention {
    /// Human label (e.g. "nextjs-app"). Diagnostic only — not matched against.
    pub name: String,
    /// Directory prefixes (repo-relative, `/`-separated) under which route files
    /// live, e.g. `["app", "src/app"]`. The longest matching root wins.
    pub root_globs: Vec<String>,
    /// Where path segments come from.
    pub segment_source: SegmentSource,
    /// Prefix prepended to every derived path, e.g. `""` or `"/api"`.
    #[serde(default)]
    pub path_prefix: String,
    /// Opening delimiter for a dynamic segment, e.g. `"["`.
    pub dynamic_open: String,
    /// Closing delimiter for a dynamic segment, e.g. `"]"`.
    pub dynamic_close: String,
    /// Marker that turns a dynamic segment into a catch-all, e.g. `"..."`.
    pub catch_all_marker: String,
    /// Opening delimiter for a non-path "group" segment, e.g. `"("`.
    pub group_open: String,
    /// Closing delimiter for a non-path "group" segment, e.g. `")"`.
    pub group_close: String,
    /// How the HTTP method is determined for endpoints under this convention.
    pub method_source: MethodSource,
    /// Conventional route-module export names that are not themselves HTTP
    /// method names, mapped to the method they serve (e.g. a read export ->
    /// `GET`, a write export -> `POST`). Consulted only for
    /// [`MethodSource::ExportName`], and only for exports whose own name is not
    /// already a method. Plain data on the convention — the executor never
    /// branches on a framework, and an export not listed here yields no
    /// endpoint.
    #[serde(default)]
    pub method_exports: BTreeMap<String, String>,
}

/// A route successfully derived from a file's location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedRoute {
    /// The normalized route path (leading slash, `:param` for dynamic segments,
    /// `*` for catch-alls).
    pub path: String,
    /// How to determine the HTTP method(s) for this endpoint.
    pub method_source: MethodSource,
    /// The convention name that matched (diagnostic).
    pub convention: String,
    /// The matched convention's role-export -> method map (see
    /// [`RoutingConvention::method_exports`]).
    pub method_exports: BTreeMap<String, String>,
}

/// Methods a handler may branch on without that branch being the verb it
/// serves, so they never narrow a role-named export (carrick#628). See
/// [`DerivedRoute::http_methods_for_export`] for the reasoning.
const NON_NARROWING_METHODS: &[&str] = &["OPTIONS", "HEAD"];

impl DerivedRoute {
    /// The HTTP methods an exported binding serves, empty when the export is
    /// not a route handler under this convention.
    ///
    /// An export named for a method *is* that method (app-router style
    /// `export function GET`); otherwise the convention's `method_exports` map
    /// names the conventional handler exports. Everything else — helpers,
    /// types, config objects a route module also exports — yields no endpoint.
    ///
    /// `method_guards` are the HTTP-method literals the handler body compares
    /// the request method against (carrick#601). They matter only for a
    /// role-named export, where the convention supplies a *default* verb rather
    /// than reading one from the source: a module that exports one generic
    /// write handler and narrows to PUT inside the body serves PUT and nothing
    /// else, so emitting the default as well would state an endpoint nothing
    /// serves. A wrong-method consumer call then matches that row and the two
    /// errors cancel into a confident edge, which is worse than the missing
    /// row. With no guard the default stands, because that is what the
    /// framework routes to the handler.
    ///
    /// A guard cannot contradict an export *named* for a method: there the name
    /// is the declaration, not a default, and a body that also branches on the
    /// method is doing something else.
    ///
    /// OPTIONS and HEAD never narrow (carrick#628). A handler that answers a
    /// CORS preflight inline opens with a branch on OPTIONS and then serves its
    /// real verb; a handler that answers HEAD is serving the protocol's
    /// read-without-a-body for a resource it also serves under another verb.
    /// Both are plumbing the handler deals with rather than the operation the
    /// route offers, so neither displaces the convention's default, and a guard
    /// naming only them reads as no guard at all. Nor is a row emitted for
    /// them: preflight is not an operation a consumer calls. The rule lives
    /// here rather than in the guard collector because the collector's answer
    /// is a fact about the source, "these are the literals the body compares
    /// the method against", and this is the routing judgement laid over it.
    pub fn http_methods_for_export(&self, export: &str, method_guards: &[String]) -> Vec<String> {
        if is_http_method(export) {
            return vec![export.to_uppercase()];
        }
        let Some(default) = self
            .method_exports
            .get(export)
            .filter(|m| is_http_method(m))
        else {
            return Vec::new();
        };

        let mut guarded: Vec<String> = Vec::new();
        for guard in method_guards {
            if !is_http_method(guard) {
                continue;
            }
            let guard = guard.trim().to_uppercase();
            if NON_NARROWING_METHODS.contains(&guard.as_str()) {
                continue;
            }
            if !guarded.contains(&guard) {
                guarded.push(guard);
            }
        }
        if guarded.is_empty() {
            vec![default.to_uppercase()]
        } else {
            guarded
        }
    }
}

impl RoutingConvention {
    /// Next.js App Router: `app/**/route.{ts,js,tsx}` with method-per-export.
    pub fn nextjs_app() -> Self {
        Self {
            name: "nextjs-app".to_string(),
            root_globs: vec!["app".to_string(), "src/app".to_string()],
            segment_source: SegmentSource::DirectoryChain {
                terminal_files: vec![
                    "route.ts".to_string(),
                    "route.js".to_string(),
                    "route.tsx".to_string(),
                    "route.mts".to_string(),
                ],
            },
            path_prefix: String::new(),
            dynamic_open: "[".to_string(),
            dynamic_close: "]".to_string(),
            catch_all_marker: "...".to_string(),
            group_open: "(".to_string(),
            group_close: ")".to_string(),
            method_source: MethodSource::ExportName,
            method_exports: BTreeMap::new(),
        }
    }

    /// Next.js Pages Router API: `pages/api/**` (or `src/pages/api/**`) where the
    /// filename is the last segment and a single default export serves the route.
    pub fn nextjs_pages() -> Self {
        Self {
            name: "nextjs-pages".to_string(),
            root_globs: vec!["pages/api".to_string(), "src/pages/api".to_string()],
            segment_source: SegmentSource::FileName {
                extensions: vec![
                    "ts".to_string(),
                    "js".to_string(),
                    "tsx".to_string(),
                    "jsx".to_string(),
                ],
                segment_separator: None,
            },
            path_prefix: "/api".to_string(),
            dynamic_open: "[".to_string(),
            dynamic_close: "]".to_string(),
            catch_all_marker: "...".to_string(),
            group_open: "(".to_string(),
            group_close: ")".to_string(),
            method_source: MethodSource::DefaultExport,
            method_exports: BTreeMap::new(),
        }
    }

    /// Astro endpoints: `src/pages/**` where the filename is the last path
    /// segment and methods are named exports (`export function GET() {}`,
    /// `export const POST = ...`). Unlike Next.js pages-router, Astro has no
    /// forced `/api` prefix — the route is literally the file's path under
    /// `src/pages` — and methods come from export names, not a single default
    /// export. Only `.ts`/`.js` files are endpoints; `.astro` files are HTML
    /// pages and are deliberately excluded. (Astro's `ALL` fallback export is
    /// not an HTTP method per [`crate::type_manifest::is_http_method`], so a
    /// route defined solely via `ALL` is not synthesized.)
    pub fn astro() -> Self {
        Self {
            name: "astro".to_string(),
            root_globs: vec!["src/pages".to_string()],
            segment_source: SegmentSource::FileName {
                // Astro routes only `.ts`/`.js` endpoint files under src/pages
                // (`.astro` files are HTML pages, handled elsewhere). `.mts`/
                // `.mjs` are not Astro route extensions, and the SWC handler
                // extractor doesn't parse TS syntax in `.mts` anyway.
                extensions: vec!["ts".to_string(), "js".to_string()],
                segment_separator: None,
            },
            path_prefix: String::new(),
            dynamic_open: "[".to_string(),
            dynamic_close: "]".to_string(),
            catch_all_marker: "...".to_string(),
            // Astro has no route-group syntax; leave the delimiters empty so the
            // group check in `transform_segment` never fires.
            group_open: String::new(),
            group_close: String::new(),
            method_source: MethodSource::ExportName,
            method_exports: BTreeMap::new(),
        }
    }

    /// Flat file-based routes: the whole route chain is encoded in one
    /// dot-separated filename under `app/routes` (`a.b.$id.ts` -> `/a/b/:id`),
    /// dynamic segments are `$`-prefixed, a bare `$` is the splat, and the
    /// route module exports a *read* handler and a *write* handler rather than
    /// one export per HTTP method (carrick#473).
    ///
    /// Only `.ts`/`.js` files are treated as endpoints. That exclusion is the
    /// precision wall of this convention: in these stacks `.tsx` route modules
    /// are the UI page plane, which shares the same directory and the same
    /// export names but is not an API surface.
    pub fn flat_routes() -> Self {
        Self {
            name: "remix-flat".to_string(),
            root_globs: vec!["app/routes".to_string(), "src/app/routes".to_string()],
            segment_source: SegmentSource::FileName {
                extensions: vec!["ts".to_string(), "js".to_string()],
                segment_separator: Some(".".to_string()),
            },
            path_prefix: String::new(),
            dynamic_open: "$".to_string(),
            // The dynamic segment has no closing delimiter — `$id` runs to the
            // end of the segment.
            dynamic_close: String::new(),
            // No catch-all marker: the splat is an *unnamed* dynamic segment
            // (a bare `$`), which `transform_segment` maps to `**`.
            catch_all_marker: String::new(),
            // A `_`-prefixed segment is a pathless layout: it nests the module
            // but contributes no path segment, exactly like a route group.
            group_open: "_".to_string(),
            group_close: String::new(),
            method_source: MethodSource::ExportName,
            method_exports: BTreeMap::from([
                ("loader".to_string(), "GET".to_string()),
                // The write export serves every non-GET method the route
                // accepts; only POST is claimed, because emitting the whole
                // write family would fabricate endpoints the module may not
                // serve.
                ("action".to_string(), "POST".to_string()),
            ]),
        }
    }

    /// Strip the longest matching root prefix from a `/`-normalized relative
    /// path. Returns the remainder, or `None` if no root matches.
    fn strip_root<'a>(&self, rel: &'a str) -> Option<&'a str> {
        self.root_globs
            .iter()
            .filter_map(|root| {
                let root = root.trim_matches('/');
                if root.is_empty() {
                    return Some(rel);
                }
                if let Some(rest) = rel.strip_prefix(root) {
                    // Require a clean segment boundary so "apple/" doesn't match
                    // root "app".
                    if rest.is_empty() {
                        Some("")
                    } else {
                        rest.strip_prefix('/')
                    }
                } else {
                    None
                }
            })
            // Longest matching root wins (e.g. "src/pages/api" over "pages/api").
            .max_by_key(|rest| rest.len().wrapping_neg())
    }

    /// Transform a single raw directory/file segment into its route form.
    /// Returns `None` for group segments (which contribute no path segment).
    fn transform_segment(&self, raw: &str) -> Option<String> {
        // Group segment, e.g. "(marketing)" → omitted.
        if !self.group_open.is_empty()
            && raw.starts_with(&self.group_open)
            && raw.ends_with(&self.group_close)
        {
            return None;
        }

        // Catch-all "[...slug]" / optional catch-all "[[...slug]]" → `**`, the
        // multi-segment wildcard the mount graph matcher recognizes as a suffix
        // catch-all (see `path_matches_with_wildcards` in src/mount_graph.rs).
        // The param name plays no part in matching, so it is dropped. Catch-alls
        // are always terminal in these conventions, so `**` lands at the end.
        // The doubled form only exists in schemes that bracket their dynamic
        // segments; a delimiter-free scheme has no such spelling.
        let double_open = format!("{}{}", self.dynamic_open, self.dynamic_open);
        let double_close = format!("{}{}", self.dynamic_close, self.dynamic_close);
        if !self.dynamic_close.is_empty()
            && raw.starts_with(&double_open)
            && raw.ends_with(&double_close)
        {
            return Some("**".to_string());
        }

        // Dynamic segment "[id]" or catch-all "[...slug]".
        if !self.dynamic_open.is_empty()
            && raw.starts_with(&self.dynamic_open)
            && raw.ends_with(&self.dynamic_close)
            && raw.len() >= self.dynamic_open.len() + self.dynamic_close.len()
        {
            let inner = &raw[self.dynamic_open.len()..raw.len() - self.dynamic_close.len()];
            // An empty marker would make *every* dynamic segment a catch-all,
            // so a convention without one opts out of the marker check.
            if !self.catch_all_marker.is_empty() && inner.starts_with(&self.catch_all_marker) {
                return Some("**".to_string());
            }
            // An unnamed dynamic segment (a bare `$`) matches whatever remains
            // and is the splat of the delimiter-free flat schemes.
            let param = sanitize_param(inner);
            if param.is_empty() {
                return Some("**".to_string());
            }
            return Some(format!(":{}", param));
        }

        // Literal segment.
        Some(raw.to_string())
    }

    /// Build the list of raw segments for a relative path under this convention,
    /// or `None` if the file is not a route file for this convention.
    fn raw_segments(&self, rel_after_root: &str) -> Option<Vec<String>> {
        let components: Vec<&str> = rel_after_root
            .split('/')
            .filter(|c| !c.is_empty())
            .collect();
        let (file, dirs) = components.split_last()?;

        match &self.segment_source {
            SegmentSource::DirectoryChain { terminal_files } => {
                // The file must be one of the terminal markers (e.g. route.ts).
                if !terminal_files.iter().any(|t| t == file) {
                    return None;
                }
                Some(dirs.iter().map(|s| s.to_string()).collect())
            }
            SegmentSource::FileName {
                extensions,
                segment_separator,
            } => {
                // Skip framework-private files like _app / _document / _middleware.
                if file.starts_with('_') {
                    return None;
                }
                let (stem, ext) = file.rsplit_once('.')?;
                if !extensions.iter().any(|e| e == ext) {
                    return None;
                }
                let mut segs: Vec<String> = dirs.iter().map(|s| s.to_string()).collect();
                // A flat scheme packs the whole chain into the stem; otherwise
                // the stem is one segment.
                let stem_segs: Vec<&str> = match segment_separator {
                    Some(sep) if !sep.is_empty() => {
                        stem.split(sep.as_str()).filter(|s| !s.is_empty()).collect()
                    }
                    _ => vec![stem],
                };
                // A trailing `index` collapses to its parent; every other piece
                // is a path segment.
                let last = stem_segs.len().saturating_sub(1);
                for (i, seg) in stem_segs.iter().enumerate() {
                    if i == last && *seg == "index" {
                        continue;
                    }
                    segs.push((*seg).to_string());
                }
                Some(segs)
            }
        }
    }
}

/// Replace characters illegal in a route param name (e.g. catch-all dots).
fn sanitize_param(name: &str) -> String {
    name.trim().replace('.', "")
}

/// Normalize OS path separators to `/` and strip any leading `./` or `/`.
fn normalize_rel(rel: &Path) -> String {
    let s = rel.to_string_lossy().replace('\\', "/");
    s.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// Derive a route from a repo-relative file path using the first convention that
/// claims it. Returns `None` when no convention recognizes the file.
pub fn derive_route(rel_path: &Path, conventions: &[RoutingConvention]) -> Option<DerivedRoute> {
    let rel = normalize_rel(rel_path);
    for convention in conventions {
        let Some(after_root) = convention.strip_root(&rel) else {
            continue;
        };
        let Some(raw_segments) = convention.raw_segments(after_root) else {
            continue;
        };

        let mut path = String::new();
        for seg in &raw_segments {
            if let Some(transformed) = convention.transform_segment(seg) {
                path.push('/');
                path.push_str(&transformed);
            }
        }

        let prefix = convention.path_prefix.trim_end_matches('/');
        let mut full = format!("{}{}", prefix, path);
        if full.is_empty() {
            full = "/".to_string();
        }
        return Some(DerivedRoute {
            path: full,
            method_source: convention.method_source.clone(),
            convention: convention.name.clone(),
            method_exports: convention.method_exports.clone(),
        });
    }
    None
}

/// Dependencies whose presence means the Next.js file router runs.
///
/// Exact names only (see [`builtin_conventions`]): a Next.js app always
/// declares `next` itself, so add-ons that merely *sit next to* Next (
/// `next-auth`, `@next/bundle-analyzer`) buy no recall and a substring rule
/// over them would claim unrelated packages (`nextera-utils`).
const NEXTJS_PACKAGES: &[&str] = &["next"];

/// Dependencies whose presence means the Astro file router runs.
const ASTRO_PACKAGES: &[&str] = &["astro"];

/// Dependencies whose presence means the Remix flat-route file router runs:
/// the server runtime, a host adapter, the React bindings, or the dev
/// toolchain — the packages that only exist in a project Remix actually
/// builds and serves.
///
/// `@remix-run/router` is deliberately absent, and the scope is not wildcarded
/// because of it: that package is React Router's data router, published under
/// the Remix scope but depended on by plain React SPAs with no `app/routes`
/// tree at all. `@remix-run/react` stays on the list because it is the reverse
/// case — Remix's own React bindings, which a React Router app has no reason to
/// declare (it depends on `react-router-dom` instead).
const REMIX_PACKAGES: &[&str] = &[
    "remix",
    "@remix-run/dev",
    "@remix-run/serve",
    "@remix-run/node",
    "@remix-run/express",
    "@remix-run/cloudflare",
    "@remix-run/deno",
    "@remix-run/server-runtime",
    "@remix-run/react",
];

/// Bootstrap conventions for a service, selected from the frameworks reported
/// by detection *and* the package names the service declares as dependencies.
/// This is the *only* place a framework name appears in the scanner; a
/// convention supplied by detection or `carrick.json` should be preferred over
/// these (see module docs).
///
/// Two inputs, two matching rules, deliberately:
///
/// * `frameworks` are free-text labels an LLM produced ("Next.js", "Remix v2"),
///   so they are matched by lowercased **substring**.
/// * `dependency_names` are npm package names, chosen by whoever published
///   them, so they are matched by lowercased **exact equality** against the
///   per-convention lists above. Substring matching here would claim any
///   package that happens to contain "next" or "remix" in its name.
///
/// The dependency path exists because framework detection classifies the HTTP
/// *server* a service runs (an app served by Express reports Express), which
/// says nothing about whether that app also declares its routes by file
/// location. The manifest does say so, deterministically, with no LLM call.
pub fn builtin_conventions(
    frameworks: &[String],
    dependency_names: &[String],
) -> Vec<RoutingConvention> {
    let frameworks_lower: Vec<String> = frameworks.iter().map(|f| f.to_lowercase()).collect();
    let deps_lower: std::collections::HashSet<String> =
        dependency_names.iter().map(|d| d.to_lowercase()).collect();
    let mentions = |needle: &str| frameworks_lower.iter().any(|f| f.contains(needle));
    let declares = |names: &[&str]| names.iter().any(|n| deps_lower.contains(*n));
    let mut out = Vec::new();
    if mentions("next") || declares(NEXTJS_PACKAGES) {
        out.push(RoutingConvention::nextjs_app());
        out.push(RoutingConvention::nextjs_pages());
    }
    if mentions("astro") || declares(ASTRO_PACKAGES) {
        out.push(RoutingConvention::astro());
    }
    if mentions("remix") || declares(REMIX_PACKAGES) {
        out.push(RoutingConvention::flat_routes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn next() -> Vec<RoutingConvention> {
        vec![
            RoutingConvention::nextjs_app(),
            RoutingConvention::nextjs_pages(),
        ]
    }

    fn route(p: &str) -> Option<DerivedRoute> {
        derive_route(&PathBuf::from(p), &next())
    }

    // --- App Router ---

    #[test]
    fn app_router_static() {
        let r = route("app/users/route.ts").unwrap();
        assert_eq!(r.path, "/users");
        assert_eq!(r.method_source, MethodSource::ExportName);
        assert_eq!(r.convention, "nextjs-app");
    }

    #[test]
    fn app_router_root() {
        assert_eq!(route("app/route.ts").unwrap().path, "/");
    }

    #[test]
    fn app_router_dynamic() {
        assert_eq!(route("app/users/[id]/route.ts").unwrap().path, "/users/:id");
    }

    #[test]
    fn app_router_nested_dynamic() {
        assert_eq!(
            route("app/teams/[teamId]/members/[userId]/route.ts")
                .unwrap()
                .path,
            "/teams/:teamId/members/:userId"
        );
    }

    #[test]
    fn app_router_catch_all() {
        assert_eq!(
            route("app/files/[...slug]/route.ts").unwrap().path,
            "/files/**"
        );
    }

    #[test]
    fn app_router_optional_catch_all() {
        assert_eq!(
            route("app/shop/[[...slug]]/route.ts").unwrap().path,
            "/shop/**"
        );
    }

    #[test]
    fn app_router_strips_route_groups() {
        assert_eq!(
            route("app/(marketing)/about/route.ts").unwrap().path,
            "/about"
        );
    }

    #[test]
    fn app_router_src_prefix() {
        assert_eq!(route("src/app/health/route.ts").unwrap().path, "/health");
    }

    #[test]
    fn app_router_ignores_non_route_files() {
        assert!(route("app/users/page.tsx").is_none());
        assert!(route("app/users/layout.tsx").is_none());
        assert!(route("app/users/component.ts").is_none());
    }

    // --- Pages Router ---

    #[test]
    fn pages_api_static() {
        let r = route("pages/api/users.ts").unwrap();
        assert_eq!(r.path, "/api/users");
        assert_eq!(r.method_source, MethodSource::DefaultExport);
        assert_eq!(r.convention, "nextjs-pages");
    }

    #[test]
    fn pages_api_index_collapses() {
        assert_eq!(
            route("pages/api/users/index.ts").unwrap().path,
            "/api/users"
        );
        assert_eq!(route("pages/api/index.ts").unwrap().path, "/api");
    }

    #[test]
    fn pages_api_dynamic_filename() {
        assert_eq!(
            route("pages/api/users/[id].ts").unwrap().path,
            "/api/users/:id"
        );
    }

    #[test]
    fn pages_api_catch_all_filename() {
        assert_eq!(
            route("pages/api/proxy/[...path].ts").unwrap().path,
            "/api/proxy/**"
        );
    }

    #[test]
    fn pages_api_src_prefix() {
        assert_eq!(route("src/pages/api/ping.ts").unwrap().path, "/api/ping");
    }

    #[test]
    fn pages_api_skips_private_files() {
        assert!(route("pages/api/_middleware.ts").is_none());
    }

    // --- Astro ---

    fn astro_route(p: &str) -> Option<DerivedRoute> {
        derive_route(&PathBuf::from(p), &[RoutingConvention::astro()])
    }

    #[test]
    fn astro_static_endpoint() {
        // No forced /api prefix: "api" here is just a literal directory segment.
        let r = astro_route("src/pages/api/users.ts").unwrap();
        assert_eq!(r.path, "/api/users");
        // Methods come from named exports, not a single default handler.
        assert_eq!(r.method_source, MethodSource::ExportName);
        assert_eq!(r.convention, "astro");
    }

    #[test]
    fn astro_top_level_endpoint() {
        assert_eq!(astro_route("src/pages/health.ts").unwrap().path, "/health");
    }

    #[test]
    fn astro_index_collapses() {
        assert_eq!(astro_route("src/pages/index.ts").unwrap().path, "/");
        assert_eq!(astro_route("src/pages/api/index.ts").unwrap().path, "/api");
    }

    #[test]
    fn astro_dynamic_filename() {
        assert_eq!(
            astro_route("src/pages/posts/[id].ts").unwrap().path,
            "/posts/:id"
        );
    }

    #[test]
    fn astro_rest_param() {
        assert_eq!(
            astro_route("src/pages/files/[...path].ts").unwrap().path,
            "/files/**"
        );
    }

    #[test]
    fn astro_javascript_endpoint() {
        assert_eq!(astro_route("src/pages/ping.js").unwrap().path, "/ping");
    }

    #[test]
    fn astro_ignores_page_components_and_private_files() {
        // `.astro` files are HTML pages, not API endpoints.
        assert!(astro_route("src/pages/about.astro").is_none());
        // `_`-prefixed files are excluded from Astro routing.
        assert!(astro_route("src/pages/_helpers.ts").is_none());
        // Pages outside `src/pages` are not endpoints.
        assert!(astro_route("src/lib/db.ts").is_none());
    }

    #[test]
    fn astro_gated_on_framework_detection() {
        assert!(builtin_conventions(&["express".to_string()], &[]).is_empty());
        let astro = builtin_conventions(&["Astro".to_string()], &[]);
        assert_eq!(astro.len(), 1);
        assert_eq!(astro[0].name, "astro");
    }

    // --- Flat routes (dot-separated filename) ---

    fn flat_route(p: &str) -> Option<DerivedRoute> {
        derive_route(&PathBuf::from(p), &[RoutingConvention::flat_routes()])
    }

    #[test]
    fn flat_routes_filename_dots_become_segments() {
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(r.path, "/api/v1/widgets");
        assert_eq!(r.method_source, MethodSource::ExportName);
        assert_eq!(r.convention, "remix-flat");
    }

    #[test]
    fn flat_routes_dollar_params_become_colon_params() {
        assert_eq!(
            flat_route("app/routes/api.v1.widgets.$widgetId.activate.ts")
                .unwrap()
                .path,
            "/api/v1/widgets/:widgetId/activate"
        );
        // Several params in one filename, including adjacent ones.
        assert_eq!(
            flat_route("app/routes/api.v1.projects.$projectRef.$env.ts")
                .unwrap()
                .path,
            "/api/v1/projects/:projectRef/:env"
        );
    }

    #[test]
    fn flat_routes_bare_dollar_is_a_splat() {
        // An unnamed dynamic segment matches everything that remains.
        assert_eq!(
            flat_route("app/routes/api.v1.widgets.$.ts").unwrap().path,
            "/api/v1/widgets/**"
        );
    }

    #[test]
    fn flat_routes_pathless_layout_segments_contribute_no_path() {
        // `_`-prefixed segments nest the module without adding a path segment.
        assert_eq!(
            flat_route("app/routes/widgets._layout.$widgetId.ts")
                .unwrap()
                .path,
            "/widgets/:widgetId"
        );
    }

    #[test]
    fn flat_routes_trailing_index_collapses() {
        assert_eq!(
            flat_route("app/routes/api.v1.widgets.index.ts")
                .unwrap()
                .path,
            "/api/v1/widgets"
        );
    }

    #[test]
    fn flat_routes_src_prefixed_root() {
        assert_eq!(
            flat_route("src/app/routes/api.health.ts").unwrap().path,
            "/api/health"
        );
    }

    #[test]
    fn flat_routes_exclude_the_ui_page_plane() {
        // `.tsx` route modules under the same directory are UI pages, not API
        // endpoints — this exclusion is the convention's precision wall.
        assert!(flat_route("app/routes/api.v1.widgets.$widgetId.tsx").is_none());
        assert!(flat_route("app/routes/widgets.$widgetId.jsx").is_none());
    }

    #[test]
    fn flat_routes_ignore_non_route_files_and_private_files() {
        assert!(flat_route("app/services/widgets.server.ts").is_none());
        assert!(flat_route("app/lib/api.v1.widgets.ts").is_none());
        // Leading-underscore *files* stay excluded (framework-private).
        assert!(flat_route("app/routes/_app.widgets.ts").is_none());
    }

    #[test]
    fn flat_routes_gated_on_framework_detection() {
        assert!(builtin_conventions(&["express".to_string()], &[]).is_empty());
        let flat = builtin_conventions(&["Remix".to_string()], &[]);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].name, "remix-flat");
    }

    fn deps(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn names(conventions: &[RoutingConvention]) -> Vec<String> {
        conventions.iter().map(|c| c.name.clone()).collect()
    }

    #[test]
    fn declared_dependencies_activate_conventions_without_the_framework_label() {
        // The reported framework is the HTTP server the app is served by; the
        // manifest is what says the app declares its routes by file location.
        let express = vec!["express".to_string()];
        assert_eq!(
            names(&builtin_conventions(&express, &deps(&["@remix-run/node"]))),
            vec!["remix-flat"]
        );
        assert_eq!(
            names(&builtin_conventions(&express, &deps(&["next"]))),
            vec!["nextjs-app", "nextjs-pages"]
        );
        assert_eq!(
            names(&builtin_conventions(&express, &deps(&["astro"]))),
            vec!["astro"]
        );
    }

    #[test]
    fn every_listed_dependency_activates_its_convention() {
        for dep in REMIX_PACKAGES {
            assert_eq!(
                names(&builtin_conventions(&[], &deps(&[dep]))),
                vec!["remix-flat"],
                "{dep} should activate the flat-route convention"
            );
        }
    }

    #[test]
    fn dependency_matching_is_exact_not_substring() {
        // Package names are chosen by whoever publishes them, so a substring
        // rule over dependencies claims unrelated packages.
        for dep in [
            "nextera-utils",
            "next-auth",
            "astrolabe",
            "remixer",
            "eslint-plugin-next",
        ] {
            assert!(
                builtin_conventions(&[], &deps(&[dep])).is_empty(),
                "{dep} must not activate a convention"
            );
        }
    }

    #[test]
    fn remix_scope_alone_does_not_activate_flat_routes() {
        // React Router's data router ships under the Remix scope and appears in
        // SPAs with no `app/routes` tree.
        assert!(builtin_conventions(&[], &deps(&["@remix-run/router"])).is_empty());
    }

    #[test]
    fn dependency_matching_is_case_insensitive() {
        assert_eq!(
            names(&builtin_conventions(&[], &deps(&["@Remix-Run/Node"]))),
            vec!["remix-flat"]
        );
    }

    #[test]
    fn a_service_declaring_nothing_relevant_gets_no_conventions() {
        assert!(builtin_conventions(&[], &deps(&["express", "zod", "pino"])).is_empty());
    }

    /// Guard-free lookup: the shape every test predating carrick#601 exercised.
    fn methods(r: &DerivedRoute, export: &str) -> Vec<String> {
        r.http_methods_for_export(export, &[])
    }

    fn guards(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flat_routes_map_role_exports_to_methods() {
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(methods(&r, "loader"), vec!["GET"]);
        assert_eq!(methods(&r, "action"), vec!["POST"]);
        // Anything the convention doesn't name is not a handler.
        assert!(methods(&r, "config").is_empty());
        assert!(methods(&r, "default").is_empty());
        // A method-named export is still its own method.
        assert_eq!(methods(&r, "GET"), vec!["GET"]);
    }

    #[test]
    fn method_named_exports_need_no_alias_map() {
        // The app-router conventions carry no alias map; method-named exports
        // resolve on their own name, and nothing else resolves at all.
        let r = route("app/users/route.ts").unwrap();
        assert!(r.method_exports.is_empty());
        assert_eq!(methods(&r, "POST"), vec!["POST"]);
        assert!(methods(&r, "loader").is_empty());
        assert!(methods(&r, "runtime").is_empty());
    }

    // --- Method guards (carrick#601) ---

    #[test]
    fn a_method_guard_replaces_the_convention_default_verb() {
        // The damaging case: the convention's default for a write export is
        // POST, the handler serves only PUT. Emitting POST as well states an
        // endpoint nothing serves, which a wrong-method consumer call then
        // matches.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["PUT"])),
            vec!["PUT"]
        );
        assert_eq!(
            r.http_methods_for_export("loader", &guards(&["DELETE"])),
            vec!["DELETE"]
        );
    }

    // --- Protocol verbs never narrow (carrick#628) ---

    #[test]
    fn a_preflight_branch_leaves_the_convention_default_standing() {
        // The regression: a read handler that answers a CORS preflight inline
        // branches on OPTIONS, which is not the verb it serves. Letting it
        // narrow drops the route's only real row.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("loader", &guards(&["OPTIONS"])),
            vec!["GET"]
        );
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["HEAD"])),
            vec!["POST"]
        );
        assert_eq!(
            r.http_methods_for_export("loader", &guards(&["OPTIONS", "HEAD"])),
            vec!["GET"]
        );
    }

    #[test]
    fn a_preflight_branch_does_not_disturb_a_real_narrowing() {
        // The protocol verbs drop out and the rest of the guard decides, so
        // neither the default nor an OPTIONS row survives.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["OPTIONS", "PUT"])),
            vec!["PUT"]
        );
    }

    #[test]
    fn a_method_named_options_export_is_still_its_own_method() {
        // The rule is scoped to role-named exports, where the convention
        // supplies a default. An export *named* OPTIONS declares itself.
        let r = route("app/users/route.ts").unwrap();
        assert_eq!(r.http_methods_for_export("OPTIONS", &[]), vec!["OPTIONS"]);
        assert_eq!(r.http_methods_for_export("HEAD", &[]), vec!["HEAD"]);
    }

    #[test]
    fn no_guard_leaves_the_convention_default_standing() {
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(r.http_methods_for_export("action", &[]), vec!["POST"]);
    }

    #[test]
    fn a_guard_on_several_methods_yields_one_row_each() {
        // A handler that narrows to more than one verb serves each of them.
        // The default is still not among them unless the guard names it.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["PUT", "DELETE"])),
            vec!["PUT", "DELETE"]
        );
    }

    #[test]
    fn guard_methods_are_normalized_and_deduplicated() {
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["put", "PUT", " Put "])),
            vec!["PUT"]
        );
    }

    #[test]
    fn a_non_method_guard_literal_is_ignored() {
        // Only HTTP methods narrow a route. A comparison against anything else
        // leaves the convention's default in place rather than inventing a verb.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("action", &guards(&["QUERY", ""])),
            vec!["POST"]
        );
    }

    #[test]
    fn a_guard_cannot_contradict_a_method_named_export() {
        // There the export name is the declaration, not a framework default,
        // so a body that also branches on the method changes nothing.
        let r = route("app/users/route.ts").unwrap();
        assert_eq!(
            r.http_methods_for_export("GET", &guards(&["POST"])),
            vec!["GET"]
        );
    }

    #[test]
    fn a_guard_does_not_promote_a_non_handler_export() {
        // A guard is a narrowing, never a licence: an export the convention
        // does not name is still not a route handler.
        let r = flat_route("app/routes/api.v1.widgets.ts").unwrap();
        assert!(
            r.http_methods_for_export("config", &guards(&["PUT"]))
                .is_empty()
        );
    }

    // --- Negative / boundary ---

    #[test]
    fn non_route_paths_return_none() {
        assert!(route("lib/db.ts").is_none());
        assert!(route("components/Button.tsx").is_none());
        // "app" prefix must respect segment boundaries.
        assert!(route("application/route.ts").is_none());
        // pages routes that aren't under /api are not API endpoints here.
        assert!(route("pages/about.tsx").is_none());
    }

    #[test]
    fn longest_root_wins() {
        // Both "pages/api" and "src/pages/api" exist; the src-prefixed file must
        // resolve via the longer root, not leave "src" in the path.
        assert_eq!(route("src/pages/api/x.ts").unwrap().path, "/api/x");
    }

    #[test]
    fn builtin_conventions_gated_on_framework() {
        assert!(builtin_conventions(&["express".to_string()], &[]).is_empty());
        let next = builtin_conventions(&["Next.js".to_string()], &[]);
        assert_eq!(next.len(), 2);
    }

    #[test]
    fn convention_roundtrips_through_serde() {
        // The B-contract: a cloud/config-supplied convention must deserialize.
        let c = RoutingConvention::nextjs_app();
        let json = serde_json::to_string(&c).unwrap();
        let back: RoutingConvention = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
