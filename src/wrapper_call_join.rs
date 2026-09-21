//! A model row at a call through a client method is not a second request
//! (carrick#1403).
//!
//! [`crate::consumer_row_fold`] folds the rows of one request WITHIN one file.
//! What is left is the shape a client is normally written in: the call through
//! the client method and the request that method's own body issues are two
//! rows in two files, one importing the other. [`crate::mount_graph::ConsumerRole`]
//! labels the row the imported-member pass resolved, because that pass read the
//! target out of the other module and so knows the row is a call through a
//! declaration. It cannot label the row the MODEL stated: nothing the model
//! answers says whether the site opens a connection.
//!
//! This pass answers that from the module graph, one file boundary out from the
//! fold. A model row at a call whose callee binding this scan resolved to
//! another module, where that module holds a deterministic request row for the
//! SAME operation, is a call through that declaration: it carries the site the
//! request is written at ([`DataCallResult::reaches_request`], carrick#1402),
//! and the row it produces is a `wrapper_call` rather than a request.
//!
//! Both halves are facts the scan already holds — the file's import table and
//! every row's resolution source — and nothing here names a library, a
//! framework or a hook.
//!
//! Every guard says nothing rather than guessing:
//!
//! - **Only a model row is relabelled.** A deterministic row read the request
//!   off its own source, and this pass does not second-guess it.
//! - **The import must be the one the CALL uses.** The callee's root binding is
//!   resolved through the file's own import table and the re-export chain
//!   behind it ([`BindingResolver`]), so a file that merely imports the
//!   declaring module somewhere else reaches nothing here.
//! - **One declaration, or nothing.** Two rows in the declaring module for one
//!   operation leave the row alone and are counted: which of them the site
//!   reaches would then be a guess.
//! - **Another file's row is never touched.** The link names the declaring
//!   module's own request line, which that module's row already carries.
//!
//! The link is the scan's own path spelling, the same `"<file>:<line>"` form
//! `file_location` uses, and is made repo-relative with it on the way into the
//! blob (`relativize_cloud_paths`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{CallExpr, Callee, Expr, Module, ModuleDecl, ModuleItem};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult, ResolutionSource};
use crate::graphql_document_sites::{Import, import_table, unwrap_expression};
use crate::import_bindings::BindingResolver;
use crate::mount_graph::ConsumerRole;
use crate::parser::parse_file;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::url_normalizer::UrlNormalizer;
use crate::workspace_resolver::WorkspaceIndex;
use crate::wrapper_request_shape::{RequestShapeSignal, call_request_verb};

/// What the join decided, so a scan can state it rather than change the index
/// silently.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WrapperCallJoins {
    /// Model rows carrying the request site they reach, which the graph then
    /// builds as `wrapper_call` rows.
    pub linked: usize,
    /// Model rows left alone because the declaring module states the operation
    /// on more than one line.
    pub ambiguous: usize,
}

/// Link every model row that reaches a request declared in another module.
///
/// `workspace` resolves the specifiers the repo declares (aliases, workspace
/// packages) as well as relative ones; `None` follows relative specifiers only.
pub fn join_wrapper_calls(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    normalizer: &UrlNormalizer,
    workspace: Option<&WorkspaceIndex>,
) -> WrapperCallJoins {
    let mut joins = WrapperCallJoins::default();

    // Every request a file states in its own source, by the module that states
    // it. A model row can only reach one of these: a row the scan did not read
    // off a site's own file says nothing about where the request is made.
    let mut requests: HashMap<PathBuf, Vec<RequestRow>> = HashMap::new();
    for (key, result) in file_results.iter() {
        let Ok(canonical) = Path::new(key).canonicalize() else {
            continue;
        };
        for call in &result.data_calls {
            if ConsumerRole::of(call.resolution_source) != Some(ConsumerRole::NetworkRequest) {
                continue;
            }
            let Some(operation) = operation_key(call, normalizer) else {
                continue;
            };
            requests
                .entry(canonical.clone())
                .or_default()
                .push(RequestRow {
                    operation,
                    site: format!("{}:{}", key, call.line_number),
                });
        }
    }
    if requests.is_empty() {
        return joins;
    }

    // Only a file with a model row has a site to classify; nothing else is
    // parsed.
    let mut keys: Vec<String> = file_results
        .iter()
        .filter(|(_, result)| result.data_calls.iter().any(is_linkable_row))
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();

    let mut resolver = match workspace {
        Some(workspace) => BindingResolver::with_workspace(workspace.clone()),
        None => BindingResolver::new(),
    };

    for key in keys {
        let file = Path::new(&key);
        let Ok(canonical) = file.canonicalize() else {
            continue;
        };
        let Some(calls) = read_call_sites(file) else {
            continue;
        };

        // The declaration each site's callee reaches, resolved once per call
        // site and read row by row below.
        let Some(result) = file_results.get(&key) else {
            continue;
        };
        let mut reached: Vec<(usize, String)> = Vec::new();
        for (index, call) in result.data_calls.iter().enumerate() {
            if !is_linkable_row(call) {
                continue;
            }
            let Some(site) = site_of(&calls.sites, call) else {
                continue;
            };
            let Some(declaration) = declaration_reached(
                &mut resolver,
                workspace,
                file,
                &canonical,
                &calls.imports,
                &site.root,
            ) else {
                continue;
            };
            let declaring = declaration.file;
            let Some(rows) = requests.get(&declaring) else {
                continue;
            };
            let Some(operation) = operation_key(call, normalizer) else {
                continue;
            };
            let mut matching = rows.iter().filter(|row| row.operation == operation);
            let Some(first) = matching.next() else {
                continue;
            };
            if matching.next().is_some() {
                joins.ambiguous += 1;
                debug!(
                    "  - {key}: line {} reaches {} in {}, which states {operation} on more than \
                     one line; left unclassified",
                    call.line_number,
                    declaration.published,
                    declaring.display()
                );
                continue;
            }
            reached.push((index, first.site.clone()));
        }

        if reached.is_empty() {
            continue;
        }
        let Some(result) = file_results.get_mut(&key) else {
            continue;
        };
        for (index, site) in reached {
            debug!(
                "  - {key}: line {} is the call through the declaration that requests {site}",
                result.data_calls[index].line_number
            );
            result.data_calls[index].reaches_request = Some(site);
            joins.linked += 1;
        }
    }

    joins
}

/// One request a module states in its own source.
struct RequestRow {
    operation: String,
    /// `"<file>:<line>"` in the scan's own path spelling, the form
    /// `file_location` carries.
    site: String,
}

/// Whether this row is one the join may classify: the model's own reading,
/// which has not been linked already.
///
/// Deliberately NOT restricted to a row with a span. A call written on an
/// imported binding raises no HTTP candidate of its own unless the binding
/// comes from a package detection flagged as a data fetcher, so a row at the
/// shape this join exists for routinely has no span and is placed by its line.
fn is_linkable_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model) && call.reaches_request.is_none()
}

/// The call a row was recorded at: the one at its span, or — for a row the
/// candidate scanner raised nothing for — the only call on its line.
///
/// A line carrying more than one call states nothing about which of them the
/// row is, and answers `None` rather than picking one.
pub(crate) fn site_of<'a>(sites: &'a [CallSite], call: &DataCallResult) -> Option<&'a CallSite> {
    if let Some(span) = call.call_expression_span_start {
        return sites.iter().find(|site| site.span == span);
    }
    let line = u32::try_from(call.line_number).ok()?;
    let mut on_line = sites.iter().filter(|site| site.line == line);
    let first = on_line.next()?;
    on_line.next().is_none().then_some(first)
}

/// The operation a row states: the method it will be indexed under and its
/// consumer path, so two spellings of one base do not read as two operations.
/// `None` for a row whose method is not an HTTP one.
fn operation_key(call: &DataCallResult, normalizer: &UrlNormalizer) -> Option<String> {
    let method = crate::agents::file_orchestrator::FileOrchestrator::normalize_consumer_method(
        call.method.as_deref(),
    )?;
    Some(format!(
        "{method} {}",
        normalizer.consumer_call_path(&call.target)
    ))
}

/// The module a call site reaches, and the name it reaches it under.
pub(crate) struct ReachedDeclaration {
    /// Canonical path of the module that DECLARES the binding the call is
    /// written on — never a barrel that republishes it.
    pub(crate) file: PathBuf,
    /// The name that module publishes it as.
    pub(crate) published: String,
}

/// The declaration a call written on `root` reaches, or `None` when `root` is
/// not a binding this file imports, or the module graph cannot reach it.
///
/// This is the one question both wrapper-call passes ask of a site: which
/// module does this line reach. carrick#1403 reads that module's rows;
/// carrick#1384 reads the requests it issues.
///
/// A binding that resolves back to `file` itself — a circular barrel — is not a
/// call one file boundary out, and answers `None`: what one file's rows are is
/// [`crate::consumer_row_fold`]'s question.
pub(crate) fn declaration_reached(
    resolver: &mut BindingResolver,
    workspace: Option<&WorkspaceIndex>,
    file: &Path,
    canonical: &Path,
    imports: &HashMap<String, Import>,
    root: &str,
) -> Option<ReachedDeclaration> {
    // Not imported here: a binding this file declares, whose rows are one
    // file's own.
    let import = imports.get(root)?;
    // Canonical, because every file's rows are keyed that way and the
    // resolver's ALIAS branch answers with the path as joined: on a checkout
    // reached through a link, `/var/…` and `/private/var/…` are one file and
    // only one of them matches a row.
    let module = resolve_module(workspace, file, &import.specifier)
        .map(|module| module.canonicalize().unwrap_or(module))?;
    // The module that DECLARES the binding, which is the module whose rows
    // state the request: a barrel republishing it holds none.
    let declaring = resolver
        .resolve_export(&module, &import.imported)
        .map(|binding| binding.file)
        .unwrap_or(module);
    if declaring == canonical {
        return None;
    }
    Some(ReachedDeclaration {
        file: declaring,
        published: import.imported.clone(),
    })
}

pub(crate) fn resolve_module(
    workspace: Option<&WorkspaceIndex>,
    importer: &Path,
    specifier: &str,
) -> Option<PathBuf> {
    match workspace {
        Some(workspace) => workspace.resolve_module_path(importer, specifier),
        None => crate::agents::file_orchestrator::FileOrchestrator::resolve_relative_import(
            importer, specifier,
        ),
    }
}

/// One call in a file, as much of it as both wrapper-call passes need: where
/// it is, the binding it is written on, and what it says about being a request
/// itself.
pub(crate) struct CallSite {
    /// Start of the call expression, in candidate span units.
    pub(crate) span: u32,
    /// 1-based line the call opens on, which is how a row with no span is
    /// placed.
    pub(crate) line: u32,
    /// The root binding the callee is written on (`client` in
    /// `client.list()`).
    pub(crate) root: String,
    /// What this call states about its own request (carrick#1384): a site that
    /// is a request itself states its own verb, and neither pass looks
    /// elsewhere for it.
    pub(crate) request: RequestShapeSignal,
}

/// What one file's calls are, read in a single parse.
pub(crate) struct FileCalls {
    /// Every call written on a binding, in source order.
    pub(crate) sites: Vec<CallSite>,
    /// Local name → where it was imported from.
    pub(crate) imports: HashMap<String, Import>,
    /// Every module specifier the file imports from, in source order.
    pub(crate) specifiers: Vec<String>,
}

/// Read one file's calls and imports.
pub(crate) fn read_call_sites(file: &Path) -> Option<FileCalls> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let module = parse_file(file, &cm, &handler)?;
    Some(FileCalls {
        sites: call_sites(&module, &cm),
        imports: import_table(&module),
        specifiers: module
            .body
            .iter()
            .filter_map(|item| match item {
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) => {
                    Some(import.src.value.to_string())
                }
                _ => None,
            })
            .collect(),
    })
}

fn call_sites(module: &Module, cm: &Lrc<SourceMap>) -> Vec<CallSite> {
    let mut collector = CallCollector::default();
    module.visit_with(&mut collector);
    collector
        .calls
        .into_iter()
        .filter_map(|(pos, root, request)| {
            let span = cm
                .lookup_byte_offset(pos)
                .pos
                .0
                .checked_add(SWC_SPAN_BASE)?;
            Some(CallSite {
                span,
                line: cm.lookup_char_pos(pos).line as u32,
                root,
                request,
            })
        })
        .collect()
}

/// Every call written on an identifier. A callee that is not (an immediately
/// invoked expression, a call on a call) states no binding to resolve and is
/// left out.
#[derive(Default)]
struct CallCollector {
    calls: Vec<(swc_common::BytePos, String, RequestShapeSignal)>,
}

impl Visit for CallCollector {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(expr) = &node.callee
            && let Some(root) = callee_root(expr)
        {
            self.calls.push((
                node.span.lo,
                root,
                call_request_verb(node, callee_property(expr).as_deref()),
            ));
        }
        node.visit_children_with(self);
    }
}

/// The property a call is made through (`post` in `client.post(...)`), which
/// is one of the two ways a call states its own verb.
fn callee_property(expr: &Expr) -> Option<String> {
    match unwrap_expression(expr) {
        Expr::Member(member) => member.prop.as_ident().map(|ident| ident.sym.to_string()),
        _ => None,
    }
}

/// `send` for `send(url)`, `client` for `client.get(url)` and for
/// `client.v1.get(url)`.
fn callee_root(expr: &Expr) -> Option<String> {
    match unwrap_expression(expr) {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) => callee_root(&member.obj),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client module: one member, one request written in its own source.
    /// The row for line 4 is what a deterministic pass emits for it.
    const CLIENT: &str = r#"export const shelvesApi = {
  list: async () => {
    const response = await fetch("/v1/shelves");
    return response.json();
  },
};
"#;

    /// The consumer: the call through the client member, which states neither
    /// the method nor the path.
    const CONSUMER: &str = r#"import { shelvesApi } from "../lib/shelves";

export function useShelves() {
  return useQuery({
    queryKey: ["shelves"],
    queryFn: () => shelvesApi.list(),
  });
}
"#;

    fn write(root: &Path, rel: &str, content: &str) -> String {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A row at the call that starts at `needle`, placed by its LINE and
    /// carrying no span — the shape a call through an imported binding has,
    /// because the candidate scanner raises nothing at such a site.
    fn row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        DataCallResult {
            candidate_id: "model".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            ..spanned_row(content, needle, method, target)
        }
    }

    /// The same row as the candidate scanner joined it: with its span.
    fn spanned_row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        let offset = content.find(needle).expect("needle is in the source") as u32;
        let span = offset + SWC_SPAN_BASE;
        let line = content[..offset as usize].lines().count() as i32;
        DataCallResult {
            candidate_id: format!("span:{span}"),
            line_number: line,
            target: target.to_string(),
            method: Some(method.to_string()),
            call_kind: None,
            pattern_matched: "fetch(".to_string(),
            call_expression_span_start: Some(span),
            call_expression_span_end: Some(span + needle.len() as u32),
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
        }
    }

    /// A deterministic row: the request the declaring module writes, which a
    /// pass read off that module's own source at its span.
    fn request_row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        DataCallResult {
            resolution_source: Some(ResolutionSource::SameFileWrapper),
            ..spanned_row(content, needle, method, target)
        }
    }

    fn join(
        files: Vec<(String, Vec<DataCallResult>)>,
    ) -> (HashMap<String, FileAnalysisResult>, WrapperCallJoins) {
        join_with(files, None)
    }

    /// The same, with the module index a scan hands the pass. `None` follows
    /// relative specifiers only, which is what a repo's declared aliases are
    /// tested against.
    fn join_with(
        files: Vec<(String, Vec<DataCallResult>)>,
        workspace: Option<&WorkspaceIndex>,
    ) -> (HashMap<String, FileAnalysisResult>, WrapperCallJoins) {
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
        let joins = join_wrapper_calls(
            &mut results,
            &UrlNormalizer::default_permissive(),
            workspace,
        );
        (results, joins)
    }

    fn link(results: &HashMap<String, FileAnalysisResult>, file: &str) -> Option<String> {
        results[file].data_calls[0].reaches_request.clone()
    }

    /// A client imported by an alias the repo declares, which is how a repo
    /// that declares one writes every import. The index the scan hands this
    /// pass is what reaches the module; without it the specifier resolves to
    /// nothing and the row is left alone.
    #[test]
    fn a_client_imported_through_a_declared_alias_is_reached() {
        for (config, contents) in [
            (
                "tsconfig.json",
                "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@/*\": [\"src/*\"] }\n  }\n}\n",
            ),
            (
                "deno.jsonc",
                "{\n  // the import map a Deno repo declares\n  \"imports\": { \"@/\": \"./src/\" }\n}\n",
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            write(root, config, contents);
            let client = write(root, "src/lib/shelves.ts", CLIENT);
            let source = CONSUMER.replace("../lib/shelves", "@/lib/shelves");
            let consumer = write(root, "src/hooks/useShelves.ts", &source);

            let rows = || {
                vec![
                    (
                        client.clone(),
                        vec![request_row(
                            CLIENT,
                            "fetch(\"/v1/shelves\")",
                            "GET",
                            "/v1/shelves",
                        )],
                    ),
                    (
                        consumer.clone(),
                        vec![row(&source, "shelvesApi.list()", "GET", "/v1/shelves")],
                    ),
                ]
            };

            let workspace = WorkspaceIndex::build_with_aliases(root, None);
            let (linked, joins) = join_with(rows(), Some(&workspace));
            assert_eq!(
                link(&linked, &consumer),
                Some(format!("{client}:3")),
                "{config}: the alias the repo declares reaches the declaring module"
            );
            assert_eq!(joins.linked, 1, "{config}");

            let (relative_only, joins) = join_with(rows(), None);
            assert_eq!(
                link(&relative_only, &consumer),
                None,
                "{config}: without the repo's own resolver the specifier reaches nothing, \
                 and the pass says nothing rather than guessing"
            );
            assert_eq!(joins, WrapperCallJoins::default(), "{config}");
        }
    }

    /// The line below carries two calls, so it says nothing on its own — a row
    /// the candidate scanner DID raise a span for is still placed exactly.
    #[test]
    fn a_row_with_a_span_is_placed_at_the_call_that_span_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        let source = CONSUMER.replace(
            "queryFn: () => shelvesApi.list(),",
            "queryFn: () => shelvesApi.list() ?? fallback(),",
        );
        let consumer = write(root, "src/hooks/useShelves.ts", &source);

        let (results, joins) = join(vec![
            (
                client.clone(),
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![spanned_row(
                    &source,
                    "shelvesApi.list()",
                    "GET",
                    "/v1/shelves",
                )],
            ),
        ]);

        assert_eq!(link(&results, &consumer), Some(format!("{client}:3")));
        assert_eq!(joins.linked, 1);
    }

    /// A row with no span sits on a line carrying a second call, so which of
    /// them it is, is not something the line says.
    #[test]
    fn a_line_carrying_more_than_one_call_states_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        let source = CONSUMER.replace(
            "queryFn: () => shelvesApi.list(),",
            "queryFn: () => shelvesApi.list() ?? fallback(),",
        );
        let consumer = write(root, "src/hooks/useShelves.ts", &source);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![row(&source, "shelvesApi.list()", "GET", "/v1/shelves")],
            ),
        ]);

        assert_eq!(link(&results, &consumer), None);
        assert_eq!(joins, WrapperCallJoins::default());
    }

    #[test]
    fn a_call_through_an_imported_member_reaches_the_request_that_member_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        let consumer = write(root, "src/hooks/useShelves.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client.clone(),
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![row(CONSUMER, "shelvesApi.list()", "GET", "/v1/shelves")],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            Some(format!("{client}:3")),
            "the call through the member names the line its request is written at"
        );
        assert_eq!(
            link(&results, &client),
            None,
            "the request row is the request; it reaches nothing"
        );
        assert_eq!(
            joins,
            WrapperCallJoins {
                linked: 1,
                ambiguous: 0
            }
        );
    }

    #[test]
    fn a_member_reached_through_a_barrel_still_names_the_declaring_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        write(
            root,
            "src/lib/index.ts",
            "export { shelvesApi } from \"./shelves\";\n",
        );
        let source = CONSUMER.replace("../lib/shelves", "../lib");
        let consumer = write(root, "src/hooks/useShelves.ts", &source);

        let (results, joins) = join(vec![
            (
                client.clone(),
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![row(&source, "shelvesApi.list()", "GET", "/v1/shelves")],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            Some(format!("{client}:3")),
            "the barrel republishes the binding; the request is in the module that declares it"
        );
        assert_eq!(joins.linked, 1);
    }

    /// A barrel that republishes the importing file's OWN binding resolves
    /// back to that file, where the request and the call are one file's rows
    /// and [`crate::consumer_row_fold`] decides what they are.
    #[test]
    fn a_binding_that_resolves_back_to_the_calling_file_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "src/lib/index.ts",
            "export { shelvesApi } from \"./shelves\";\n",
        );
        // The client module reaches its own binding through the barrel beside
        // it, which is how a circular barrel import is written.
        let source = format!(
            "import {{ shelvesApi }} from \"./index\";\n\n{CLIENT}\nexport const mine = () => shelvesApi.list();\n"
        );
        let client = write(root, "src/lib/shelves.ts", &source);

        let (results, joins) = join(vec![(
            client.clone(),
            vec![
                request_row(&source, "fetch(\"/v1/shelves\")", "GET", "/v1/shelves"),
                row(&source, "shelvesApi.list()", "GET", "/v1/shelves"),
            ],
        )]);

        assert_eq!(
            results[&client].data_calls[1].reaches_request, None,
            "one file's two rows are the fold's question, not this join's"
        );
        assert_eq!(joins, WrapperCallJoins::default());
    }

    /// The decoy: this file DOES import the module that states the request,
    /// for something else, and the call is on a binding of its own. The import
    /// that matters is the one the CALL is written on.
    #[test]
    fn an_import_the_call_is_not_written_on_reaches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(
            root,
            "src/lib/shelves.ts",
            &format!("{CLIENT}\nexport function formatShelves() {{\n  return {{}};\n}}\n"),
        );
        // The call is alone on its line, so only the import guard stands
        // between this row and a link.
        let local = "import { formatShelves } from \"../lib/shelves\";\n\nconst shelvesApi = makeClient();\n\nexport function useShelves() {\n  const rows = shelvesApi.list();\n  return formatShelves(rows);\n}\n";
        let consumer = write(root, "src/hooks/useShelves.ts", local);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![row(local, "shelvesApi.list()", "GET", "/v1/shelves")],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            None,
            "the call is written on a binding this file declares; the import \
             beside it reaches the declaration for a different name"
        );
        assert_eq!(joins, WrapperCallJoins::default());
    }

    #[test]
    fn a_declaration_stating_the_operation_twice_leaves_the_row_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let two_requests = CLIENT.replace(
            "  },\n};",
            "  },\n  listAgain: async () => fetch(\"/v1/shelves\"),\n};",
        );
        let client = write(root, "src/lib/shelves.ts", &two_requests);
        let consumer = write(root, "src/hooks/useShelves.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![
                    request_row(
                        &two_requests,
                        "fetch(\"/v1/shelves\")",
                        "GET",
                        "/v1/shelves",
                    ),
                    request_row(
                        &two_requests,
                        "listAgain: async () => fetch",
                        "GET",
                        "/v1/shelves",
                    ),
                ],
            ),
            (
                consumer.clone(),
                vec![row(CONSUMER, "shelvesApi.list()", "GET", "/v1/shelves")],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            None,
            "which of the two lines the site reaches would be a guess"
        );
        assert_eq!(
            joins,
            WrapperCallJoins {
                linked: 0,
                ambiguous: 1
            },
            "and the scan says how often it said nothing"
        );
    }

    #[test]
    fn a_row_for_another_operation_reaches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        let consumer = write(root, "src/hooks/useShelves.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![row(CONSUMER, "shelvesApi.list()", "GET", "/v1/crates")],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            None,
            "the declaration states no request for the operation this row carries"
        );
        assert_eq!(joins, WrapperCallJoins::default());
    }

    #[test]
    fn a_deterministic_row_is_never_relabelled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/shelves.ts", CLIENT);
        let consumer = write(root, "src/hooks/useShelves.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/shelves\")",
                    "GET",
                    "/v1/shelves",
                )],
            ),
            (
                consumer.clone(),
                vec![DataCallResult {
                    // The imported-member pass read this row out of the
                    // declaring module itself: it already says what it is.
                    resolution_source: Some(ResolutionSource::ImportedMember),
                    ..row(CONSUMER, "shelvesApi.list()", "GET", "/v1/shelves")
                }],
            ),
        ]);

        assert_eq!(
            link(&results, &consumer),
            None,
            "a row a deterministic pass stated is not the join's to classify"
        );
        assert_eq!(joins, WrapperCallJoins::default());
    }
}
