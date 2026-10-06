//! The route at a call through a declaration is the declaration's, not the
//! model's (carrick#1794).
//!
//! A call written on an imported function (`loadSharedStats(id)`) states no
//! URL: the path is written in the function's own body, in another module. The
//! model answers the call site with a route anyway, read off the function's
//! name or its comments, and where the module declares one function per
//! sibling route it can name the sibling. Nothing checked it, and once the
//! site's value is typed, that type is judged against the producer of a route
//! the call never requests: a false incompatible.
//!
//! This pass reads the route where it is written. For a model row at a direct
//! call of an imported function, the declaration the call reaches is resolved
//! through the module graph ([`declaration_reached`]), and the rows the
//! declaring module states inside that function's body are read. When the body
//! states one operation and the row's path cannot be it, the row takes the
//! body's operation: its method, target, base and dispatch value. The row
//! stays the model's row at its own site, with its own type anchors; only
//! which request it reaches moves.
//!
//! Every part of that is read off the AST and the rows the scan already holds,
//! and no library, framework or hook is named. Each guard says nothing rather
//! than guessing:
//!
//! - **Only a model row is corrected.** A deterministic row read its target
//!   off the source it was resolved from.
//! - **The site must be a direct call of the imported binding** and state no
//!   request of its own. A member call (`client.list()`) runs a member, not the
//!   binding's declaration, and a call that states its own verb or options is
//!   read where it is written.
//! - **The declaration is a function the module declares at top level** under
//!   the name the import resolves to. Anything else (an object, a class, a
//!   value a call returned) has no one body to read.
//! - **Its body states exactly one operation.** No row there, or rows that
//!   disagree, leave the row as it was; the second case is counted.
//! - **Every call in the body that could make a request sits on a line the
//!   body's rows state.** A call that is a request itself, or a call on a
//!   binding the module imports or declares, at a line with no row may issue a
//!   request the rows do not show, so the body says nothing.
//! - **The body's path is written in the body.** Its last literal segment is a
//!   segment of a string or template the body writes, so the correction rests
//!   on the source and not on one model reading over another. And the row's
//!   own path is NOT written there: a body that writes both may request both.
//! - **A row whose path the body's matches is left alone** ([`paths_match`]): a
//!   parameter the call fills, or a placeholder on either side, is the same
//!   route.
//!
//! The same site states what its value is (carrick#1801). The call is the
//! function's call, so its value is what the function returns: a mapped copy
//! of the body, a wrapper, a flag, a raw response. When the function's body
//! writes the row's path (its last literal segment, read after any correction
//! above), the request is made in there, and unless the function hands back
//! that request's parsed body unchanged
//! ([`crate::request_summary::RequestSummaryIndex::passes_body`]) the row is
//! marked [`DataCallResult::at_caller`], as the request summaries mark the
//! rows they restate at a caller (carrick#1601): the type layer infers no
//! consumer response type there. A site whose own arguments carry the path (a
//! transport called with a URL) is not this: the function's body does not
//! write the path.
//!
//! The correction moves the row's operation, so it runs before the verb pass
//! ([`crate::wrapper_call_method`]) and the passes that decide which rows are
//! the same request ([`crate::consumer_row_fold`],
//! [`crate::wrapper_call_join`]). Like them it runs over the model's cached raw
//! answer on every scan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use carrick_match::{is_param_segment, paths_match};
use swc_common::{
    SourceMap, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    Decl, DefaultDecl, Expr, ImportSpecifier, Module, ModuleDecl, ModuleItem, Pat, Stmt, Str, Tpl,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult, ResolutionSource};
use crate::graphql_document_sites::unwrap_expression;
use crate::import_bindings::BindingResolver;
use crate::parser::parse_file;
use crate::url_normalizer::UrlNormalizer;
use crate::workspace_resolver::WorkspaceIndex;
use crate::wrapper_call_join::{
    CallSite, call_sites, declaration_reached, operation_key, read_call_sites, site_of,
};
use crate::wrapper_request_shape::RequestShapeSignal;

/// What the pass decided, so a scan can state it rather than change the index
/// silently.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WrapperRouteCorrections {
    /// Rows whose route the body of the declaration they call states, and
    /// disagreed with.
    pub corrected: usize,
    /// Rows left alone because the body they reach states no single operation
    /// it writes the path of, or may request the row's own path too. The row
    /// keeps the model's route, and this is how often that happened.
    pub declaration_unreadable: usize,
    /// Rows at a call of a function whose body writes their path and does
    /// not hand back its parsed body: the call's value is that function's
    /// return value, so the row is marked
    /// [`DataCallResult::at_caller`] (carrick#1801).
    pub restated_at_caller: usize,
}

/// Correct the route of every model row at a direct call of an imported
/// function whose body states another one, and mark every such row whose
/// value is not the response body.
///
/// `workspace` resolves the specifiers the repo declares (aliases, workspace
/// packages) as well as relative ones; `None` follows relative specifiers only.
/// `passes_body` answers, for a declaring module's canonical path and the name
/// it declares a function under, whether a call of that function is worth its
/// request's parsed body ([`crate::request_summary::RequestSummaryIndex::passes_body`]).
pub fn correct_wrapper_call_routes(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    normalizer: &UrlNormalizer,
    workspace: Option<&WorkspaceIndex>,
    passes_body: &dyn Fn(&Path, &str) -> bool,
) -> WrapperRouteCorrections {
    let mut corrections = WrapperRouteCorrections::default();

    // Only a file with a model row has a site to read; nothing else is parsed.
    let mut keys: Vec<String> = file_results
        .iter()
        .filter(|(_, result)| result.data_calls.iter().any(is_correctable_row))
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();
    if keys.is_empty() {
        return corrections;
    }

    // The declaring module is reached by its canonical path; its rows are
    // keyed the way the scan walked it.
    let walked: HashMap<PathBuf, String> = file_results
        .keys()
        .filter_map(|key| {
            Path::new(key)
                .canonicalize()
                .ok()
                .map(|canonical| (canonical, key.clone()))
        })
        .collect();

    let mut resolver = match workspace {
        Some(workspace) => BindingResolver::with_workspace(workspace.clone()),
        None => BindingResolver::new(),
    };
    // One module is parsed once, and one body read once, however many sites
    // reach them.
    let mut modules: HashMap<PathBuf, Option<ModuleFunctions>> = HashMap::new();
    let mut bodies: HashMap<(PathBuf, String), BodyRoute> = HashMap::new();

    for key in keys {
        let file = Path::new(&key);
        let Ok(canonical) = file.canonicalize() else {
            continue;
        };
        let Some(calls) = read_call_sites(file) else {
            continue;
        };
        let Some(result) = file_results.get(&key) else {
            continue;
        };

        let mut stated: Vec<(usize, Option<DataCallResult>, bool)> = Vec::new();
        for (index, call) in result.data_calls.iter().enumerate() {
            if !is_correctable_row(call) {
                continue;
            }
            let Some(site) = site_of(&calls.sites, call) else {
                continue;
            };
            // A member call runs a member, not the binding's declaration, and
            // a call that states its own request is read where it is written.
            if !site.direct || site.request != RequestShapeSignal::NotARequest {
                continue;
            }
            let Some(reached) = declaration_reached(
                &mut resolver,
                workspace,
                file,
                &canonical,
                &calls.imports,
                &site.root,
            ) else {
                continue;
            };
            let Some(local) = reached.local.clone() else {
                continue;
            };
            let route = bodies
                .entry((reached.file.clone(), local.clone()))
                .or_insert_with(|| {
                    let functions = modules
                        .entry(reached.file.clone())
                        .or_insert_with(|| ModuleFunctions::read(&reached.file));
                    let rows = walked
                        .get(&reached.file)
                        .and_then(|walked| file_results.get(walked))
                        .map(|result| result.data_calls.as_slice())
                        .unwrap_or_default();
                    match functions {
                        Some(functions) => functions.route(&local, rows, normalizer),
                        None => BodyRoute::None,
                    }
                })
                .clone();
            let own_path = normalizer.consumer_call_path(&call.target);
            let corrected = match route {
                BodyRoute::None => None,
                BodyRoute::Unreadable => {
                    corrections.declaration_unreadable += 1;
                    debug!(
                        "  - {key}: line {} calls {} in {}, whose body states no single \
                         operation it writes; keeping the extracted route",
                        call.line_number,
                        reached.published,
                        reached.file.display()
                    );
                    None
                }
                // The row's path is the body's: nothing to correct.
                BodyRoute::Stated(body) if paths_match(&body.path, &own_path) => None,
                // The body writes the row's own path as well: it may request
                // both, whatever its rows say.
                BodyRoute::Stated(body)
                    if last_literal_segment(&own_path)
                        .is_some_and(|segment| body.segments.contains(&segment)) =>
                {
                    corrections.declaration_unreadable += 1;
                    debug!(
                        "  - {key}: line {} calls {} in {}, whose body writes {own_path} as \
                         well as {}; keeping the extracted route",
                        call.line_number,
                        reached.published,
                        reached.file.display(),
                        body.path
                    );
                    None
                }
                BodyRoute::Stated(body) => {
                    debug!(
                        "  - {key}: line {} calls {} in {}, which requests {} {}; correcting \
                         {} {own_path}",
                        call.line_number,
                        reached.published,
                        reached.file.display(),
                        body.row.method.as_deref().unwrap_or_default(),
                        body.path,
                        call.method.as_deref().unwrap_or_default(),
                    );
                    Some(body.row)
                }
            };

            // The call's value is what the function returns (carrick#1801).
            // Read once the route is the one the row will carry.
            let path = corrected
                .as_ref()
                .map(|request| normalizer.consumer_call_path(&request.target))
                .unwrap_or(own_path);
            let writes_path = modules
                .get(&reached.file)
                .and_then(Option::as_ref)
                .and_then(|functions| functions.functions.get(&local))
                .is_some_and(|body| {
                    last_literal_segment(&path)
                        .is_some_and(|segment| body.segments.contains(&segment))
                });
            let at_caller = writes_path && !passes_body(&reached.file, &local);
            if at_caller {
                debug!(
                    "  - {key}: line {} calls {} in {}, which writes {path} and hands back \
                     something other than its parsed body; the call's value is not the \
                     response",
                    call.line_number,
                    reached.published,
                    reached.file.display(),
                );
            }
            if corrected.is_some() || at_caller {
                stated.push((index, corrected, at_caller));
            }
        }

        if stated.is_empty() {
            continue;
        }
        let Some(result) = file_results.get_mut(&key) else {
            continue;
        };
        for (index, corrected, at_caller) in stated {
            let row = &mut result.data_calls[index];
            if let Some(request) = corrected {
                row.method = request.method;
                row.target = request.target;
                row.base = request.base;
                row.loopback_default_url = request.loopback_default_url;
                row.dispatch = request.dispatch;
                corrections.corrected += 1;
            }
            if at_caller {
                row.at_caller = true;
                corrections.restated_at_caller += 1;
            }
        }
    }

    corrections
}

/// Whether this row is one the pass may correct: the model's own reading.
///
/// Deliberately NOT restricted to a row with a span. A call written on an
/// imported binding raises no HTTP candidate of its own, so the rows this pass
/// exists for are routinely placed by their line.
fn is_correctable_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model)
}

/// What one declaration's body states about the request it makes.
#[derive(Debug, Clone)]
enum BodyRoute {
    /// No function body to read, or no row in it.
    None,
    /// Rows that state no single operation the body writes the path of, or a
    /// call that may request something the rows do not show.
    Unreadable,
    /// The one request the body makes.
    Stated(Box<StatedBody>),
}

#[derive(Debug, Clone)]
struct StatedBody {
    /// The body's row, whose request fields a corrected row takes.
    row: DataCallResult,
    /// Its consumer path.
    path: String,
    /// Every path segment a string or template in the body writes.
    segments: HashSet<String>,
}

/// One module's top-level functions and the names it binds, read in one
/// parse.
struct ModuleFunctions {
    /// Local name -> the function's body.
    functions: HashMap<String, FunctionBody>,
    /// Every name the module binds at top level: its imports, of any kind, and
    /// its declarations. A call on one of these may run a request.
    names: HashSet<String>,
    /// Every call in the module written on a binding.
    sites: Vec<CallSite>,
}

struct FunctionBody {
    /// 1-based lines the declaration spans.
    start: u32,
    end: u32,
    /// Every path segment a string or template in it writes.
    segments: HashSet<String>,
}

impl ModuleFunctions {
    fn read(module_path: &Path) -> Option<Self> {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let module = parse_file(module_path, &cm, &handler)?;
        let mut read = Self {
            functions: HashMap::new(),
            names: HashSet::new(),
            sites: call_sites(&module, &cm),
        };
        read.collect(&module, &cm);
        Some(read)
    }

    fn collect(&mut self, module: &Module, cm: &Lrc<SourceMap>) {
        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                    for specifier in &import.specifiers {
                        let local = match specifier {
                            ImportSpecifier::Named(named) => &named.local,
                            ImportSpecifier::Default(default) => &default.local,
                            ImportSpecifier::Namespace(namespace) => &namespace.local,
                        };
                        self.names.insert(local.sym.to_string());
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                    self.declaration(&export.decl, cm);
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                    if let DefaultDecl::Fn(function) = &export.decl
                        && let Some(ident) = &function.ident
                    {
                        self.function(ident.sym.to_string(), &*function.function, cm);
                    }
                }
                ModuleItem::Stmt(Stmt::Decl(decl)) => self.declaration(decl, cm),
                _ => {}
            }
        }
    }

    fn declaration(&mut self, decl: &Decl, cm: &Lrc<SourceMap>) {
        match decl {
            Decl::Fn(function) => {
                self.function(function.ident.sym.to_string(), &*function.function, cm);
            }
            Decl::Class(class) => {
                self.names.insert(class.ident.sym.to_string());
            }
            Decl::Var(var) => {
                for declarator in &var.decls {
                    let Pat::Ident(binding) = &declarator.name else {
                        continue;
                    };
                    let name = binding.id.sym.to_string();
                    match declarator.init.as_deref().map(unwrap_expression) {
                        Some(init @ (Expr::Arrow(_) | Expr::Fn(_))) => {
                            self.function(name, init, cm);
                        }
                        _ => {
                            self.names.insert(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn function<N>(&mut self, name: String, node: &N, cm: &Lrc<SourceMap>)
    where
        N: Spanned + VisitWith<Literals>,
    {
        let span = node.span();
        let mut literals = Literals::default();
        node.visit_with(&mut literals);
        self.names.insert(name.clone());
        self.functions.insert(
            name,
            FunctionBody {
                start: cm.lookup_char_pos(span.lo).line as u32,
                end: cm.lookup_char_pos(span.hi).line as u32,
                segments: literals.segments,
            },
        );
    }

    /// The one request the function `local` makes, read off `rows`, the rows
    /// its module states.
    fn route(&self, local: &str, rows: &[DataCallResult], normalizer: &UrlNormalizer) -> BodyRoute {
        let Some(body) = self.functions.get(local) else {
            return BodyRoute::None;
        };
        let within = |line: u32| (body.start..=body.end).contains(&line);
        let in_body: Vec<&DataCallResult> = rows
            .iter()
            .filter(|row| u32::try_from(row.line_number).is_ok_and(within))
            .collect();
        let Some(first) = in_body.first() else {
            return BodyRoute::None;
        };

        let mut operations = HashSet::new();
        for row in &in_body {
            match operation_key(row, normalizer) {
                Some(operation) => operations.insert(operation),
                None => return BodyRoute::Unreadable,
            };
        }
        if operations.len() != 1 {
            return BodyRoute::Unreadable;
        }

        // A call that may issue a request at a line no row states: the body
        // may make one the rows do not show.
        let row_lines: HashSet<u32> = in_body
            .iter()
            .filter_map(|row| u32::try_from(row.line_number).ok())
            .collect();
        let unstated = self.sites.iter().any(|site| {
            within(site.line)
                && !row_lines.contains(&site.line)
                && (site.request != RequestShapeSignal::NotARequest
                    || (site.root != local && self.names.contains(&site.root)))
        });
        if unstated {
            return BodyRoute::Unreadable;
        }

        // The path has to be written in the body: otherwise its row is one
        // model reading against another.
        let path = normalizer.consumer_call_path(&first.target);
        if !last_literal_segment(&path).is_some_and(|segment| body.segments.contains(&segment)) {
            return BodyRoute::Unreadable;
        }
        BodyRoute::Stated(Box::new(StatedBody {
            row: (*first).clone(),
            path,
            segments: body.segments.clone(),
        }))
    }
}

/// Every path segment the strings and templates under a node write,
/// lowercased: `/v1/shelves/stats?shelf=` writes `v1`, `shelves`, `stats`
/// and `shelf`.
#[derive(Default)]
struct Literals {
    segments: HashSet<String>,
}

impl Literals {
    fn add(&mut self, text: &str) {
        self.segments.extend(
            text.split(|c: char| matches!(c, '/' | '?' | '#' | '&' | '=') || c.is_whitespace())
                .filter(|segment| !segment.is_empty())
                .map(str::to_ascii_lowercase),
        );
    }
}

impl Visit for Literals {
    fn visit_str(&mut self, node: &Str) {
        self.add(&node.value.to_string_lossy());
    }

    fn visit_tpl(&mut self, node: &Tpl) {
        for quasi in &node.quasis {
            match &quasi.cooked {
                Some(cooked) => self.add(&cooked.to_string_lossy()),
                None => self.add(&quasi.raw),
            }
        }
        node.visit_children_with(self);
    }
}

/// The last segment of a consumer path that carries literal text, lowercased,
/// with any query dropped. `None` for a path with no literal segment, which
/// names no route to correct to.
fn last_literal_segment(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    path.split('/')
        .rev()
        .map(str::trim)
        .find(|segment| {
            !segment.is_empty()
                && !is_param_segment(segment)
                && !matches!(*segment, "*" | "**" | "(.*)")
                && !segment.contains("${")
        })
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declaring module: one function per sibling route, each handing its
    /// options to a transport the module imports, so no row here is one the
    /// scanner can read deterministically.
    const SHELVES: &str = r#"import { request } from "./transport";

export const loadStats = (shelfId: string) => {
  return request({
    path: `/v1/shelves/stats?shelfId=${encodeURIComponent(shelfId)}`,
    method: "GET",
  }).map(readStats);
};

export const loadSharedStats = (shelfId: string) => {
  return request({
    path: `/v1/shelves/shared-stats?shelfId=${encodeURIComponent(shelfId)}`,
    method: "GET",
  }).map(readSharedStats);
};
"#;

    /// The site: a direct call of the imported function, stating no URL.
    const SITE: &str = r#"import { loadSharedStats } from "../lib/shelves";

export const useSharedStats = (shelfId: string) => {
  const request = loadSharedStats(shelfId);
  return request;
};
"#;

    fn write(root: &Path, rel: &str, content: &str) -> String {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A model row at the call that starts at `needle`, placed by its LINE
    /// with no span: the shape a call through an imported binding has.
    fn row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        let offset = content.find(needle).expect("needle is in the source");
        let line = content[..offset].matches('\n').count() as i32 + 1;
        DataCallResult {
            candidate_id: "model".to_string(),
            line_number: line,
            target: target.to_string(),
            method: Some(method.to_string()),
            call_kind: None,
            pattern_matched: needle.to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: Some(line),
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            dispatch: None,
            resolution_source: Some(ResolutionSource::Model),
            reaches_request: None,
            body_literals: Default::default(),
            library_semantics: Vec::new(),
            at_caller: false,
            call_body: None,
        }
    }

    /// The model's rows for [`SHELVES`]: each at its own request line, naming
    /// the route that line writes.
    fn shelves_rows(content: &str) -> Vec<DataCallResult> {
        let first = content.find("request({").unwrap();
        let second = first + 1 + content[first + 1..].find("request({").unwrap();
        let line = |offset: usize| content[..offset].matches('\n').count() as i32 + 1;
        vec![
            DataCallResult {
                line_number: line(first),
                ..row(content, "request({", "GET", "/v1/shelves/stats")
            },
            DataCallResult {
                line_number: line(second),
                ..row(content, "request({", "GET", "/v1/shelves/shared-stats")
            },
        ]
    }

    fn correct(
        files: Vec<(String, Vec<DataCallResult>)>,
    ) -> (HashMap<String, FileAnalysisResult>, WrapperRouteCorrections) {
        correct_with(files, &|_, _| false)
    }

    /// [`correct`], with the summaries' answer to whether a function hands
    /// back its parsed body.
    fn correct_with(
        files: Vec<(String, Vec<DataCallResult>)>,
        passes_body: &dyn Fn(&Path, &str) -> bool,
    ) -> (HashMap<String, FileAnalysisResult>, WrapperRouteCorrections) {
        let mut results: HashMap<String, FileAnalysisResult> = files
            .into_iter()
            .map(|(path, data_calls)| {
                (
                    path,
                    FileAnalysisResult {
                        data_calls,
                        ..Default::default()
                    },
                )
            })
            .collect();
        let corrections = correct_wrapper_call_routes(
            &mut results,
            &UrlNormalizer::default_permissive(),
            None,
            passes_body,
        );
        (results, corrections)
    }

    fn target(results: &HashMap<String, FileAnalysisResult>, file: &str) -> String {
        results[file].data_calls[0].target.clone()
    }

    fn at_caller(results: &HashMap<String, FileAnalysisResult>, file: &str) -> bool {
        results[file].data_calls[0].at_caller
    }

    /// carrick#1794's acceptance: a helper requesting one route, its caller
    /// recorded by the model on a sibling route, reads the helper's route on
    /// the caller's row.
    #[test]
    fn a_caller_recorded_on_a_sibling_route_reads_the_helper_s_route() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let shelves = write(root, "src/lib/shelves.ts", SHELVES);
        let site = write(root, "src/hooks/useSharedStats.ts", SITE);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    SITE,
                    "loadSharedStats(shelfId)",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (shelves.clone(), shelves_rows(SHELVES)),
        ]);

        assert_eq!(
            target(&results, &site),
            "/v1/shelves/shared-stats",
            "the helper's body writes and requests the shared route"
        );
        assert!(
            at_caller(&results, &site),
            "the helper maps the body before returning it, so the call is worth the mapped value"
        );
        assert_eq!(
            corrections,
            WrapperRouteCorrections {
                corrected: 1,
                declaration_unreadable: 0,
                restated_at_caller: 1,
            }
        );
        let operations = |rows: &[DataCallResult]| -> Vec<(i32, Option<String>, String)> {
            rows.iter()
                .map(|row| (row.line_number, row.method.clone(), row.target.clone()))
                .collect()
        };
        assert_eq!(
            operations(&results[&shelves].data_calls),
            operations(&shelves_rows(SHELVES)),
            "the declaring module's own rows are never touched"
        );
        assert!(
            results[&shelves]
                .data_calls
                .iter()
                .all(|row| !row.at_caller),
            "a request line is no caller"
        );
    }

    /// The import names the function under another name than the one its
    /// module declares it by: the declaration is found by the module's own.
    #[test]
    fn a_function_exported_under_another_name_is_read_by_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = SHELVES
            .replace("export const loadSharedStats", "const loadShared")
            .replace("export const loadStats", "const loadStats")
            + "\nexport { loadStats, loadShared as loadSharedStats };\n";
        let shelves = write(root, "src/lib/shelves.ts", &source);
        let site = write(root, "src/hooks/useSharedStats.ts", SITE);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    SITE,
                    "loadSharedStats(shelfId)",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (shelves, shelves_rows(&source)),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/shared-stats");
        assert_eq!(corrections.corrected, 1);
    }

    /// A row that names the helper's own route, with a parameter the call
    /// fills, is the same route.
    #[test]
    fn a_row_on_the_helper_s_own_route_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport const loadShelf = (shelfId: string) => {\n  return request({ path: `/v1/shelves/${shelfId}/stats`, method: \"GET\" });\n};\n";
        let shelves = write(root, "src/lib/shelves.ts", source);
        let site_source = "import { loadShelf } from \"../lib/shelves\";\n\nexport const useShelf = (id: string) => {\n  return loadShelf(id);\n};\n";
        let site = write(root, "src/hooks/useShelf.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "loadShelf(id)",
                    "GET",
                    "/v1/shelves/:id/stats",
                )],
            ),
            (
                shelves,
                vec![row(
                    source,
                    "request({",
                    "GET",
                    "/v1/shelves/:shelfId/stats",
                )],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/:id/stats");
        assert_eq!(
            corrections,
            WrapperRouteCorrections {
                restated_at_caller: 1,
                ..Default::default()
            },
            "the route stands; the call is the helper's, and the helper's body writes it"
        );
    }

    /// A helper whose body writes two routes, one per branch, may request
    /// either: its rows say nothing about which one a call reaches.
    #[test]
    fn a_body_that_states_two_operations_keeps_the_extracted_route() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport const loadStats = (shared: boolean) => {\n  if (shared) {\n    return request({ path: \"/v1/shelves/shared-stats\", method: \"GET\" });\n  }\n  return request({ path: \"/v1/shelves/stats\", method: \"GET\" });\n};\n";
        let shelves = write(root, "src/lib/shelves.ts", source);
        let site_source = "import { loadStats } from \"../lib/shelves\";\n\nexport const useStats = () => {\n  return loadStats(true);\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);
        let first = source.find("request({").unwrap();
        let second_line = source[..first + 1 + source[first + 1..].find("request({").unwrap()]
            .matches('\n')
            .count() as i32
            + 1;

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "loadStats(true)",
                    "GET",
                    "/v1/shelves/overview",
                )],
            ),
            (
                shelves,
                vec![
                    row(source, "request({", "GET", "/v1/shelves/shared-stats"),
                    DataCallResult {
                        line_number: second_line,
                        ..row(source, "request({", "GET", "/v1/shelves/stats")
                    },
                ],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/overview");
        assert_eq!(
            corrections,
            WrapperRouteCorrections {
                corrected: 0,
                declaration_unreadable: 1,
                restated_at_caller: 0,
            },
            "the body writes no part of the row's path, so it says nothing about the row"
        );
    }

    /// A generic transport: the path is the caller's argument, and the body
    /// writes none. A concrete row the model stated in it is a guess, so it
    /// corrects nothing.
    #[test]
    fn a_body_that_does_not_write_its_row_s_path_corrects_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport const send = (path: string) => {\n  return request({ path, method: \"GET\" });\n};\n";
        let shelves = write(root, "src/lib/send.ts", source);
        let site_source = "import { send } from \"../lib/send\";\n\nexport const useStats = () => {\n  return send(STATS_PATH);\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "send(STATS_PATH)",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (
                shelves,
                vec![row(source, "request({", "GET", "/v1/shelves/shared-stats")],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert_eq!(corrections.corrected, 0);
        assert_eq!(corrections.declaration_unreadable, 1);
        assert!(
            !at_caller(&results, &site),
            "a transport handed its path writes none of it: what it returns is not read here"
        );
    }

    /// The body writes the row's own path too, at a line the model stated no
    /// row for: it may request it, so the row keeps it.
    #[test]
    fn a_body_that_writes_the_row_s_path_too_keeps_the_extracted_route() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport const loadStats = (shared: boolean) => {\n  const path = shared ? \"/v1/shelves/shared-stats\" : \"/v1/shelves/stats\";\n  return request({ path, method: \"GET\" });\n};\n";
        let shelves = write(root, "src/lib/shelves.ts", source);
        let site_source = "import { loadStats } from \"../lib/shelves\";\n\nexport const useStats = () => {\n  return loadStats(false);\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "loadStats(false)",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (
                shelves,
                vec![row(source, "request({", "GET", "/v1/shelves/shared-stats")],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert_eq!(corrections.declaration_unreadable, 1);
        assert!(
            at_caller(&results, &site),
            "whichever route it takes, the call is the helper's, which writes this path"
        );
    }

    /// The body calls another function the module imports, on a line no row
    /// states: that call may make a request of its own, which the row could
    /// be naming.
    #[test]
    fn a_body_with_an_unstated_call_keeps_the_extracted_route() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\nimport { refreshSession } from \"./session\";\n\nexport const loadSharedStats = async () => {\n  await refreshSession();\n  return request({ path: \"/v1/shelves/shared-stats\", method: \"GET\" });\n};\n";
        let shelves = write(root, "src/lib/shelves.ts", source);
        let site = write(root, "src/hooks/useSharedStats.ts", SITE);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    SITE,
                    "loadSharedStats(shelfId)",
                    "POST",
                    "/v1/session/refresh",
                )],
            ),
            (
                shelves,
                vec![row(source, "request({", "GET", "/v1/shelves/shared-stats")],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/session/refresh");
        assert_eq!(corrections.declaration_unreadable, 1);
    }

    /// A call through a member of an imported object runs the member, not one
    /// body the binding declares.
    #[test]
    fn a_member_call_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport const shelves = {\n  stats: () => request({ path: \"/v1/shelves/stats\", method: \"GET\" }),\n  shared: () => request({ path: \"/v1/shelves/shared-stats\", method: \"GET\" }),\n};\n";
        let module = write(root, "src/lib/shelves.ts", source);
        let site_source = "import { shelves } from \"../lib/shelves\";\n\nexport const useStats = () => {\n  return shelves.stats();\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "shelves.stats()",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (
                module,
                vec![row(
                    source,
                    "request({ path: \"/v1/shelves/shared-stats\"",
                    "GET",
                    "/v1/shelves/shared-stats",
                )],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    /// A member the module attaches to its function is not the function's
    /// body: a call through it reaches whatever the member does.
    #[test]
    fn a_call_through_a_member_of_the_function_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "import { request } from \"./transport\";\n\nexport function loadStats(id: string) {\n  return request({ path: `/v1/shelves/stats?id=${id}`, method: \"GET\" });\n}\nloadStats.shared = (id: string) => request({ path: `/v1/shelves/shared-stats?id=${id}`, method: \"GET\" });\n";
        let module = write(root, "src/lib/shelves.ts", source);
        let site_source = "import { loadStats } from \"../lib/shelves\";\n\nexport const useSharedStats = (id: string) => {\n  return loadStats.shared(id);\n};\n";
        let site = write(root, "src/hooks/useSharedStats.ts", site_source);
        let shared = source.find("request({ path: `/v1/shelves/shared").unwrap();
        let shared_line = source[..shared].matches('\n').count() as i32 + 1;

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "loadStats.shared(id)",
                    "GET",
                    "/v1/shelves/shared-stats",
                )],
            ),
            (
                module,
                vec![
                    row(source, "request({", "GET", "/v1/shelves/stats"),
                    DataCallResult {
                        line_number: shared_line,
                        ..row(source, "request({", "GET", "/v1/shelves/shared-stats")
                    },
                ],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/shared-stats");
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    /// A deterministic row read its target off its own source.
    #[test]
    fn a_deterministic_row_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let shelves = write(root, "src/lib/shelves.ts", SHELVES);
        let site = write(root, "src/hooks/useSharedStats.ts", SITE);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![DataCallResult {
                    resolution_source: Some(ResolutionSource::RequestSummary),
                    ..row(SITE, "loadSharedStats(shelfId)", "GET", "/v1/shelves/stats")
                }],
            ),
            (shelves, shelves_rows(SHELVES)),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    /// A call that states its own request (its options carry a method) is read
    /// where it is written, whatever the transport's body says.
    #[test]
    fn a_site_that_states_its_own_request_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "export const request = (options: { path: string; method: string }) => {\n  return fetch(`/v1/shelves/shared-stats`, { method: options.method });\n};\n";
        let transport = write(root, "src/lib/transport.ts", source);
        let site_source = "import { request } from \"../lib/transport\";\n\nexport const useStats = () => {\n  return request({ path: \"/v1/shelves/stats\", method: \"GET\" });\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(site_source, "request({", "GET", "/v1/shelves/stats")],
            ),
            (
                transport,
                vec![row(source, "fetch(", "GET", "/v1/shelves/shared-stats")],
            ),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    /// The helper of carrick#1801's real case: its path is picked by a
    /// ternary, so the model states no row in its body, and it parses the
    /// body into a value of its own before handing it back.
    const MAPPING_HELPER: &str = r#"import { request } from "./transport";

const parseStats = (payload: unknown) => ({ books: Number((payload as { books: unknown }).books) });

export const fetchStats = (shelfId?: string) => {
  const path = shelfId
    ? `/v1/shelves/stats?shelfId=${encodeURIComponent(shelfId)}`
    : "/v1/shelves/stats";

  return request({ path, method: "GET" }).mapOk(parseStats);
};
"#;

    const MAPPING_SITE: &str = r#"import { fetchStats } from "../lib/shelves";

export const useStats = (shelfId?: string) => {
  const request = fetchStats(shelfId);
  return request;
};
"#;

    /// carrick#1801's acceptance: the model's row at the call of a helper that
    /// writes the path and maps the body keeps its route and is marked, so
    /// the type layer reads no response type off the helper's return value.
    #[test]
    fn a_caller_of_a_helper_that_maps_the_body_is_restated_at_the_caller() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let shelves = write(root, "src/lib/shelves.ts", MAPPING_HELPER);
        let site = write(root, "src/hooks/useStats.ts", MAPPING_SITE);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    MAPPING_SITE,
                    "fetchStats(shelfId)",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (shelves, Vec::new()),
        ]);

        assert_eq!(target(&results, &site), "/v1/shelves/stats");
        assert!(at_caller(&results, &site));
        assert_eq!(
            corrections,
            WrapperRouteCorrections {
                restated_at_caller: 1,
                ..Default::default()
            }
        );
    }

    /// A helper the summaries read as handing back its parsed body makes its
    /// call worth that body: the row stays typed. The summaries are asked by
    /// the declaring module's canonical path and the name it declares the
    /// function under.
    #[test]
    fn a_caller_of_a_helper_that_hands_back_its_parsed_body_is_left_unmarked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let shelves = write(root, "src/lib/shelves.ts", MAPPING_HELPER);
        let site = write(root, "src/hooks/useStats.ts", MAPPING_SITE);
        let declaring = Path::new(&shelves).canonicalize().unwrap();

        let (results, corrections) = correct_with(
            vec![
                (
                    site.clone(),
                    vec![row(
                        MAPPING_SITE,
                        "fetchStats(shelfId)",
                        "GET",
                        "/v1/shelves/stats",
                    )],
                ),
                (shelves, Vec::new()),
            ],
            &|file, function| file == declaring && function == "fetchStats",
        );

        assert!(!at_caller(&results, &site));
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    /// Exported under another name: the summaries know the function by the
    /// name its module declares it under.
    #[test]
    fn the_summaries_are_asked_by_the_declared_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = MAPPING_HELPER.replace("export const fetchStats", "const loadStats")
            + "\nexport { loadStats as fetchStats };\n";
        let shelves = write(root, "src/lib/shelves.ts", &source);
        let site = write(root, "src/hooks/useStats.ts", MAPPING_SITE);

        let (results, _) = correct_with(
            vec![
                (
                    site.clone(),
                    vec![row(
                        MAPPING_SITE,
                        "fetchStats(shelfId)",
                        "GET",
                        "/v1/shelves/stats",
                    )],
                ),
                (shelves, Vec::new()),
            ],
            &|_, function| function == "loadStats",
        );

        assert!(!at_caller(&results, &site));
    }

    /// A transport called with the URL: the site writes the path, the
    /// transport's body writes none of it, and what the transport returns is
    /// read where the site reads it.
    #[test]
    fn a_transport_called_with_its_url_is_left_unmarked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let source = "export const send = async (path: string) => {\n  const response = await fetch(path, { method: \"GET\" });\n  return response;\n};\n";
        let transport = write(root, "src/lib/send.ts", source);
        let site_source = "import { send } from \"../lib/send\";\n\nexport const useStats = () => {\n  return send(\"/v1/shelves/stats\");\n};\n";
        let site = write(root, "src/hooks/useStats.ts", site_source);

        let (results, corrections) = correct(vec![
            (
                site.clone(),
                vec![row(
                    site_source,
                    "send(\"/v1/shelves/stats\")",
                    "GET",
                    "/v1/shelves/stats",
                )],
            ),
            (transport, Vec::new()),
        ]);

        assert!(!at_caller(&results, &site));
        assert_eq!(corrections, WrapperRouteCorrections::default());
    }

    #[test]
    fn last_literal_segment_skips_parameters_queries_and_holes() {
        assert_eq!(
            last_literal_segment("/v1/shelves/shared-stats?shelfId=1").as_deref(),
            Some("shared-stats")
        );
        assert_eq!(
            last_literal_segment("/v1/shelves/:id").as_deref(),
            Some("shelves")
        );
        assert_eq!(last_literal_segment("/:path").as_deref(), None);
        assert_eq!(last_literal_segment("${base}/${path}").as_deref(), None);
    }
}
