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
use swc_ecma_ast::{CallExpr, Callee, Expr, Module};
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

    // Only a file with a spanned model row has a site to classify; nothing
    // else is parsed.
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
        let Some((callees, imports)) = read_call_sites(file) else {
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
            let Some(span) = call.call_expression_span_start else {
                continue;
            };
            let Some(root) = callees.get(&span) else {
                continue;
            };
            let Some(import) = imports.get(root) else {
                continue; // declared here, or a name this file never imported
            };
            let Some(module) = resolve_module(workspace, file, &import.specifier) else {
                continue;
            };
            // The module that DECLARES the binding, which is the module whose
            // rows state the request: a barrel republishing it holds none.
            let declaring = resolver
                .resolve_export(&module, &import.imported)
                .map(|binding| binding.file)
                .unwrap_or(module);
            if declaring == canonical {
                continue; // a same-file wrapper: `consumer_row_fold` owns it
            }
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
                    import.imported,
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

/// Whether this row is one the join may classify: the model's own reading, at a
/// call site the scanner raised a candidate for (so the span names the call
/// whose callee is read below).
fn is_linkable_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model)
        && call.call_expression_span_start.is_some()
        && call.reaches_request.is_none()
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

fn resolve_module(
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

/// Each call's span mapped to the root binding its callee is written on
/// (`client.getMine()` → `client`), with the file's import table.
fn read_call_sites(file: &Path) -> Option<(HashMap<u32, String>, HashMap<String, Import>)> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let module = parse_file(file, &cm, &handler)?;
    Some((callee_roots(&module, &cm), import_table(&module)))
}

fn callee_roots(module: &Module, cm: &Lrc<SourceMap>) -> HashMap<u32, String> {
    let mut collector = CalleeRoots::default();
    module.visit_with(&mut collector);
    collector
        .roots
        .into_iter()
        .filter_map(|(pos, root)| {
            let span = cm
                .lookup_byte_offset(pos)
                .pos
                .0
                .checked_add(SWC_SPAN_BASE)?;
            Some((span, root))
        })
        .collect()
}

/// The root identifier of every call's callee. A callee that is not written on
/// an identifier (an immediately invoked expression, a call on a call) states
/// no binding to resolve and is left out.
#[derive(Default)]
struct CalleeRoots {
    roots: Vec<(swc_common::BytePos, String)>,
}

impl Visit for CalleeRoots {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(expr) = &node.callee
            && let Some(root) = callee_root(expr)
        {
            self.roots.push((node.span.lo, root));
        }
        node.visit_children_with(self);
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
    const CLIENT: &str = r#"export const capabilitiesApi = {
  getMine: async () => {
    const response = await fetch("/v1/me/capabilities");
    return response.json();
  },
};
"#;

    /// The consumer: the call through the client member, which states neither
    /// the method nor the path.
    const CONSUMER: &str = r#"import { capabilitiesApi } from "../lib/capabilities";

export function useCapabilities() {
  return useQuery({
    queryKey: ["capabilities"],
    queryFn: () => capabilitiesApi.getMine(),
  });
}
"#;

    fn write(root: &Path, rel: &str, content: &str) -> String {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A row at the call that starts at `needle`, in candidate span units.
    fn row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
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

    fn request_row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        DataCallResult {
            resolution_source: Some(ResolutionSource::SameFileWrapper),
            ..row(content, needle, method, target)
        }
    }

    fn join(
        files: Vec<(String, Vec<DataCallResult>)>,
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
        let joins = join_wrapper_calls(&mut results, &UrlNormalizer::default_permissive(), None);
        (results, joins)
    }

    fn link(results: &HashMap<String, FileAnalysisResult>, file: &str) -> Option<String> {
        results[file].data_calls[0].reaches_request.clone()
    }

    #[test]
    fn a_call_through_an_imported_member_reaches_the_request_that_member_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let client = write(root, "src/lib/capabilities.ts", CLIENT);
        let consumer = write(root, "src/hooks/useCapabilities.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client.clone(),
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                )],
            ),
            (
                consumer.clone(),
                vec![row(
                    CONSUMER,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/capabilities",
                )],
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
        let client = write(root, "src/lib/capabilities.ts", CLIENT);
        write(
            root,
            "src/lib/index.ts",
            "export { capabilitiesApi } from \"./capabilities\";\n",
        );
        let source = CONSUMER.replace("../lib/capabilities", "../lib");
        let consumer = write(root, "src/hooks/useCapabilities.ts", &source);

        let (results, joins) = join(vec![
            (
                client.clone(),
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                )],
            ),
            (
                consumer.clone(),
                vec![row(
                    &source,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/capabilities",
                )],
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
            "export { capabilitiesApi } from \"./capabilities\";\n",
        );
        // The client module reaches its own binding through the barrel beside
        // it, which is how a circular barrel import is written.
        let source = format!(
            "import {{ capabilitiesApi }} from \"./index\";\n\n{CLIENT}\nexport const mine = () => capabilitiesApi.getMine();\n"
        );
        let client = write(root, "src/lib/capabilities.ts", &source);

        let (results, joins) = join(vec![(
            client.clone(),
            vec![
                request_row(
                    &source,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                ),
                row(
                    &source,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/capabilities",
                ),
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
            "src/lib/capabilities.ts",
            &format!("{CLIENT}\nexport function formatCapabilities() {{\n  return {{}};\n}}\n"),
        );
        let local = "import { formatCapabilities } from \"../lib/capabilities\";\n\nconst capabilitiesApi = makeClient();\n\nexport function useCapabilities() {\n  return formatCapabilities(capabilitiesApi.getMine());\n}\n";
        let consumer = write(root, "src/hooks/useCapabilities.ts", local);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                )],
            ),
            (
                consumer.clone(),
                vec![row(
                    local,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/capabilities",
                )],
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
            "  },\n  getMineAgain: async () => fetch(\"/v1/me/capabilities\"),\n};",
        );
        let client = write(root, "src/lib/capabilities.ts", &two_requests);
        let consumer = write(root, "src/hooks/useCapabilities.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![
                    request_row(
                        &two_requests,
                        "fetch(\"/v1/me/capabilities\")",
                        "GET",
                        "/v1/me/capabilities",
                    ),
                    request_row(
                        &two_requests,
                        "getMineAgain: async () => fetch",
                        "GET",
                        "/v1/me/capabilities",
                    ),
                ],
            ),
            (
                consumer.clone(),
                vec![row(
                    CONSUMER,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/capabilities",
                )],
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
        let client = write(root, "src/lib/capabilities.ts", CLIENT);
        let consumer = write(root, "src/hooks/useCapabilities.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                )],
            ),
            (
                consumer.clone(),
                vec![row(
                    CONSUMER,
                    "capabilitiesApi.getMine()",
                    "GET",
                    "/v1/me/settings",
                )],
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
        let client = write(root, "src/lib/capabilities.ts", CLIENT);
        let consumer = write(root, "src/hooks/useCapabilities.ts", CONSUMER);

        let (results, joins) = join(vec![
            (
                client,
                vec![request_row(
                    CLIENT,
                    "fetch(\"/v1/me/capabilities\")",
                    "GET",
                    "/v1/me/capabilities",
                )],
            ),
            (
                consumer.clone(),
                vec![DataCallResult {
                    // The imported-member pass read this row out of the
                    // declaring module itself: it already says what it is.
                    resolution_source: Some(ResolutionSource::ImportedMember),
                    ..row(
                        CONSUMER,
                        "capabilitiesApi.getMine()",
                        "GET",
                        "/v1/me/capabilities",
                    )
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
