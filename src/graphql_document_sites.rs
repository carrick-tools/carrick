//! HTTP rows at call sites that execute a GraphQL document (carrick#1154).
//!
//! A call that hands a GraphQL document to a client (`useQuery(OrdersDocument)`,
//! `client.query(OrdersDocument, vars)`, `client.request(ORDERS_QUERY)`)
//! executes an operation. The transport URL lives in the client's
//! configuration, not at the site, and the operation is indexed from the
//! document itself. The file-analyzer still sometimes answers such a site with
//! an HTTP row: `POST /graphql`, or a host it made up. That row duplicates the
//! GraphQL operation at best, and at worst claims an HTTP contract the site
//! never states.
//!
//! This pass drops those rows, in two steps, both read off the AST:
//!
//! 1. **The argument is a document.** A call argument that is an identifier
//!    bound, in this file or through the module graph, to a GraphQL document:
//!    - an object literal with `kind: "Document"` (the graphql-js AST node,
//!      which is what generated typed documents are);
//!    - a tagged template or a single-argument call whose text parses as an
//!      executable GraphQL document (the parse decides, not the tag name);
//!    - the default import of a `.graphql` / `.gql` file.
//! 2. **The callee executes documents.** Generated document modules are often
//!    gitignored, so in a bare checkout the argument's import resolves to
//!    nothing. When the SAME callee binding is seen executing a resolved
//!    document elsewhere in the service, its sites whose argument import is
//!    unresolvable are GraphQL executions too. A callee never seen with a
//!    resolved document keeps its rows: there is no fallback.
//!
//! What a site is, is decided here from the argument alone. Whether a GraphQL
//! row for the document survives (a vendor document, an unresolved schema) is
//! a separate question the GraphQL extraction answers, and it never brings an
//! HTTP twin back.
//!
//! Every dropped row is counted and logged with its reason.

use crate::agents::file_analyzer_agent::FileAnalysisResult;
use crate::import_bindings::BindingResolver;
use crate::parser::parse_file;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::workspace_resolver::WorkspaceIndex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    CallExpr, Callee, Decl, Expr, ImportDecl, ImportSpecifier, Module, ModuleDecl,
    ModuleExportName, ModuleItem, Pat, Prop, PropName, PropOrSpread, Stmt,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

/// How many rows the pass dropped, by the step that decided it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DocumentSiteDrops {
    /// The call's argument resolves to a GraphQL document.
    pub document_argument: usize,
    /// The argument's import resolves to nothing on disk, and the callee
    /// executes a resolved document elsewhere in the service.
    pub document_executor: usize,
}

impl DocumentSiteDrops {
    pub fn total(&self) -> usize {
        self.document_argument + self.document_executor
    }
}

/// A callee binding, identified by the module it comes from and the name it
/// is published under, so two files that import one hook under the same
/// specifier (or two relative spellings of one module) agree on it.
type CalleeKey = (String, String);

/// What one call site in one file states about GraphQL.
#[derive(Debug, Default)]
struct FileSites {
    /// Span starts (candidate units) of calls with a document argument.
    document_sites: HashSet<u32>,
    /// Callees of those calls.
    executors: HashSet<CalleeKey>,
    /// Calls with no document argument but an identifier argument whose
    /// import resolves to nothing, with their callee.
    unresolved_sites: Vec<(u32, CalleeKey)>,
}

/// Drop HTTP data calls at GraphQL document sites across one service's file
/// results. `workspace` resolves aliased and package specifiers; relative
/// specifiers resolve without it.
pub fn suppress_document_site_http_rows(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    workspace: Option<&WorkspaceIndex>,
) -> DocumentSiteDrops {
    let mut reader = DocumentReader::new(workspace);

    // Only files with a candidate-backed HTTP row can lose one; nothing else
    // is parsed.
    let mut keys: Vec<String> = file_results
        .iter()
        .filter(|(_, result)| {
            result
                .data_calls
                .iter()
                .any(|call| call.call_expression_span_start.is_some())
        })
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();

    let mut sites: HashMap<String, FileSites> = HashMap::new();
    let mut executors: HashSet<CalleeKey> = HashSet::new();
    for key in &keys {
        let file_sites = reader.file_sites(Path::new(key));
        executors.extend(file_sites.executors.iter().cloned());
        sites.insert(key.clone(), file_sites);
    }

    let mut drops = DocumentSiteDrops::default();
    for key in &keys {
        let (Some(file_sites), Some(result)) = (sites.get(key), file_results.get_mut(key)) else {
            continue;
        };
        let executor_sites: HashSet<u32> = file_sites
            .unresolved_sites
            .iter()
            .filter(|(_, callee)| executors.contains(callee))
            .map(|(span, _)| *span)
            .collect();
        if file_sites.document_sites.is_empty() && executor_sites.is_empty() {
            continue;
        }
        result.data_calls.retain(|call| {
            let Some(span) = call.call_expression_span_start else {
                return true;
            };
            // A target already rewritten to the operation key is the GraphQL
            // row, not its HTTP twin.
            if call.target.starts_with("graphql|") {
                return true;
            }
            let reason = if file_sites.document_sites.contains(&span) {
                drops.document_argument += 1;
                "its argument is a GraphQL document"
            } else if executor_sites.contains(&span) {
                drops.document_executor += 1;
                "its callee executes GraphQL documents elsewhere in this service and \
                 its argument's import is not on disk"
            } else {
                return true;
            };
            debug!(
                "Dropping HTTP row {} {} at {}:{}: {}",
                call.method.as_deref().unwrap_or("?"),
                call.target,
                key,
                call.line_number,
                reason
            );
            false
        });
    }
    drops
}

/// Reads call sites and document declarations, caching every module it
/// parses: one generated document module is read once however many files
/// import it.
struct DocumentReader<'a> {
    source_map: Lrc<SourceMap>,
    handler: Handler,
    resolver: BindingResolver,
    workspace: Option<&'a WorkspaceIndex>,
    /// Module → the names it binds to a GraphQL document at module scope.
    documents: HashMap<PathBuf, HashSet<String>>,
}

impl<'a> DocumentReader<'a> {
    fn new(workspace: Option<&'a WorkspaceIndex>) -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        let resolver = match workspace {
            Some(workspace) => BindingResolver::with_workspace(workspace.clone()),
            None => BindingResolver::new(),
        };
        Self {
            source_map,
            handler,
            resolver,
            workspace,
            documents: HashMap::new(),
        }
    }

    fn module_documents(&mut self, file: &Path) -> &HashSet<String> {
        if !self.documents.contains_key(file) {
            let names = parse_file(file, &self.source_map, &self.handler)
                .map(|module| document_bindings(&module))
                .unwrap_or_default();
            self.documents.insert(file.to_path_buf(), names);
        }
        &self.documents[file]
    }

    /// The module key a specifier names from `importer`: the resolved file
    /// when there is one, else the specifier as written (a package name reads
    /// the same from every file).
    fn module_key(&self, importer: &Path, specifier: &str) -> String {
        let resolved = match self.workspace {
            Some(workspace) => workspace.resolve_module_path(importer, specifier),
            None => crate::agents::file_orchestrator::FileOrchestrator::resolve_relative_import(
                importer, specifier,
            ),
        };
        match resolved {
            Some(path) => path.to_string_lossy().into_owned(),
            None => specifier.to_string(),
        }
    }

    fn file_sites(&mut self, file: &Path) -> FileSites {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let Some(module) = parse_file(file, &cm, &handler) else {
            return FileSites::default();
        };
        let local_documents = document_bindings(&module);
        let local_declarations = module_declarations(&module);
        let imports = import_table(&module);

        let mut calls = CallCollector::default();
        module.visit_with(&mut calls);

        let mut sites = FileSites::default();
        for call in calls.calls {
            let Some(span) = cm
                .lookup_byte_offset(call.span_lo)
                .pos
                .0
                .checked_add(SWC_SPAN_BASE)
            else {
                continue;
            };
            let mut has_document = false;
            let mut has_unresolved = false;
            for name in &call.ident_args {
                match self.classify_argument(file, name, &local_documents, &imports) {
                    Argument::Document => has_document = true,
                    Argument::Unresolved => has_unresolved = true,
                    Argument::Other => {}
                }
            }
            let callee = call
                .callee
                .as_ref()
                .and_then(|callee| self.callee_key(file, callee, &local_declarations, &imports));
            if has_document {
                sites.document_sites.insert(span);
                if let Some(callee) = callee {
                    sites.executors.insert(callee);
                }
            } else if has_unresolved && let Some(callee) = callee {
                sites.unresolved_sites.push((span, callee));
            }
        }
        sites
    }

    fn classify_argument(
        &mut self,
        file: &Path,
        name: &str,
        local_documents: &HashSet<String>,
        imports: &HashMap<String, Import>,
    ) -> Argument {
        if local_documents.contains(name) {
            return Argument::Document;
        }
        let Some(import) = imports.get(name) else {
            return Argument::Other;
        };
        if import.imported == "default" && is_graphql_document_file(&import.specifier) {
            return Argument::Document;
        }
        let Some(binding) = self
            .resolver
            .resolve(file, &import.specifier, &import.imported)
        else {
            return Argument::Unresolved;
        };
        let Some(local_name) = binding.local_name else {
            return Argument::Other;
        };
        if self.module_documents(&binding.file).contains(&local_name) {
            Argument::Document
        } else {
            Argument::Other
        }
    }

    fn callee_key(
        &self,
        file: &Path,
        callee: &CalleeName,
        local_declarations: &HashSet<String>,
        imports: &HashMap<String, Import>,
    ) -> Option<CalleeKey> {
        let (root, member) = match callee {
            CalleeName::Ident(root) => (root, None),
            CalleeName::Member(root, member) => (root, Some(member)),
        };
        let (module, published) = if let Some(import) = imports.get(root) {
            (
                self.module_key(file, &import.specifier),
                import.imported.clone(),
            )
        } else if local_declarations.contains(root) {
            (file.to_string_lossy().into_owned(), root.clone())
        } else {
            return None;
        };
        let name = match member {
            Some(member) => format!("{published}.{member}"),
            None => published,
        };
        Some((module, name))
    }
}

enum Argument {
    Document,
    /// An import whose declaration the module graph cannot reach.
    Unresolved,
    Other,
}

#[derive(Debug, Clone)]
struct Import {
    specifier: String,
    /// The name the module publishes (`default` for a default import).
    imported: String,
}

/// Local name → where it was imported from. Namespace imports are left out:
/// a `ns.Member` argument is not an identifier argument.
fn import_table(module: &Module) -> HashMap<String, Import> {
    let mut table = HashMap::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
            specifiers, src, ..
        })) = item
        else {
            continue;
        };
        let specifier = src.value.to_string();
        for spec in specifiers {
            let (local, imported) = match spec {
                ImportSpecifier::Named(named) => {
                    let imported = match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(s)) => s.value.to_string(),
                        None => named.local.sym.to_string(),
                    };
                    (named.local.sym.to_string(), imported)
                }
                ImportSpecifier::Default(default) => {
                    (default.local.sym.to_string(), "default".to_string())
                }
                ImportSpecifier::Namespace(_) => continue,
            };
            table.insert(
                local,
                Import {
                    specifier: specifier.clone(),
                    imported,
                },
            );
        }
    }
    table
}

/// Names declared at module scope: variables, functions and classes.
fn module_declarations(module: &Module) -> HashSet<String> {
    let mut names = HashSet::new();
    for decl in module_scope_decls(module) {
        match decl {
            Decl::Var(var) => {
                for declarator in &var.decls {
                    if let Pat::Ident(ident) = &declarator.name {
                        names.insert(ident.id.sym.to_string());
                    }
                }
            }
            Decl::Fn(function) => {
                names.insert(function.ident.sym.to_string());
            }
            Decl::Class(class) => {
                names.insert(class.ident.sym.to_string());
            }
            _ => {}
        }
    }
    names
}

fn module_scope_decls(module: &Module) -> impl Iterator<Item = &Decl> {
    module.body.iter().filter_map(|item| match item {
        ModuleItem::Stmt(Stmt::Decl(decl)) => Some(decl),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => Some(&export.decl),
        _ => None,
    })
}

/// The names a module binds to a GraphQL document at module scope.
pub fn document_bindings(module: &Module) -> HashSet<String> {
    let mut names = HashSet::new();
    for decl in module_scope_decls(module) {
        let Decl::Var(var) = decl else {
            continue;
        };
        for declarator in &var.decls {
            if let (Pat::Ident(ident), Some(init)) = (&declarator.name, declarator.init.as_deref())
                && is_document_expression(init)
            {
                names.insert(ident.id.sym.to_string());
            }
        }
    }
    names
}

/// Is `expr` a GraphQL document, read from its shape alone?
///
/// - an object literal whose `kind` is the string `"Document"`, the graphql-js
///   AST node every generated typed document is written as;
/// - a tagged template, or a call with one template or string argument, whose
///   text parses as an executable document (an operation or a fragment).
///
/// Type assertions and parentheses around either are looked through.
pub fn is_document_expression(expr: &Expr) -> bool {
    match unwrap_expression(expr) {
        Expr::Object(object) => object.props.iter().any(|prop| {
            let PropOrSpread::Prop(prop) = prop else {
                return false;
            };
            let Prop::KeyValue(pair) = &**prop else {
                return false;
            };
            let key_is_kind = match &pair.key {
                PropName::Ident(ident) => ident.sym == *"kind",
                PropName::Str(s) => s.value == *"kind",
                _ => false,
            };
            key_is_kind
                && matches!(unwrap_expression(&pair.value), Expr::Lit(swc_ecma_ast::Lit::Str(s)) if s.value == *"Document")
        }),
        Expr::TaggedTpl(tagged) => is_executable_document(&template_text(&tagged.tpl)),
        Expr::Call(call) if call.args.len() == 1 && call.args[0].spread.is_none() => {
            match unwrap_expression(&call.args[0].expr) {
                Expr::Tpl(tpl) => is_executable_document(&template_text(tpl)),
                Expr::Lit(swc_ecma_ast::Lit::Str(s)) => is_executable_document(s.value.as_ref()),
                _ => false,
            }
        }
        _ => false,
    }
}

fn unwrap_expression(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_expression(&inner.expr),
        Expr::TsAs(inner) => unwrap_expression(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap_expression(&inner.expr),
        Expr::TsConstAssertion(inner) => unwrap_expression(&inner.expr),
        Expr::TsTypeAssertion(inner) => unwrap_expression(&inner.expr),
        Expr::TsNonNull(inner) => unwrap_expression(&inner.expr),
        other => other,
    }
}

/// A template's literal parts, interpolations dropped (an interpolated
/// fragment leaves a spread that still parses).
fn template_text(tpl: &swc_ecma_ast::Tpl) -> String {
    tpl.quasis
        .iter()
        .map(|quasi| {
            quasi
                .cooked
                .as_ref()
                .map(|cooked| cooked.to_string())
                .unwrap_or_else(|| quasi.raw.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does `text` parse as an executable GraphQL document with at least one
/// named operation keyword or fragment? The bare `{ ... }` shorthand is not
/// enough on its own: plenty of non-GraphQL text parses as one.
fn is_executable_document(text: &str) -> bool {
    use graphql_parser::query::{Definition, OperationDefinition};
    let Ok(document) = graphql_parser::parse_query::<String>(text) else {
        return false;
    };
    document
        .definitions
        .iter()
        .any(|definition| match definition {
            Definition::Fragment(_) => true,
            Definition::Operation(OperationDefinition::SelectionSet(_)) => false,
            Definition::Operation(_) => true,
        })
}

/// A module that IS a GraphQL document, imported whole.
fn is_graphql_document_file(specifier: &str) -> bool {
    specifier.ends_with(".graphql") || specifier.ends_with(".gql")
}

enum CalleeName {
    Ident(String),
    /// `root.member(...)` with an identifier root.
    Member(String, String),
}

struct CallSite {
    span_lo: swc_common::BytePos,
    callee: Option<CalleeName>,
    /// Identifier arguments, in order.
    ident_args: Vec<String>,
}

#[derive(Default)]
struct CallCollector {
    calls: Vec<CallSite>,
}

impl Visit for CallCollector {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        let ident_args: Vec<String> = node
            .args
            .iter()
            .filter(|arg| arg.spread.is_none())
            .filter_map(|arg| match unwrap_expression(&arg.expr) {
                Expr::Ident(ident) => Some(ident.sym.to_string()),
                _ => None,
            })
            .collect();
        if !ident_args.is_empty() {
            let callee = match &node.callee {
                Callee::Expr(expr) => match unwrap_expression(expr) {
                    Expr::Ident(ident) => Some(CalleeName::Ident(ident.sym.to_string())),
                    Expr::Member(member) => match (&*member.obj, member.prop.as_ident()) {
                        (Expr::Ident(root), Some(prop)) => Some(CalleeName::Member(
                            root.sym.to_string(),
                            prop.sym.to_string(),
                        )),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            self.calls.push(CallSite {
                span_lo: node.span.lo,
                callee,
                ident_args,
            });
        }
        node.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::file_analyzer_agent::DataCallResult;

    /// A generated typed-document module: the graphql-js AST node as an
    /// object literal behind a type assertion.
    const GENERATED_DOCUMENTS: &str = r#"import { TypedDocumentNode as DocumentNode } from "@example/typed-document";
export type OrdersQuery = { orders: Array<{ id: string }> };
export const OrdersDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Orders"},"selectionSet":{"kind":"SelectionSet","selections":[]}}]} as unknown as DocumentNode<OrdersQuery, {}>;
export const pageSize = 25;
"#;

    /// Multi-byte text above every site, so a byte-vs-character span slip
    /// cannot pass.
    const ORDERS_PAGE: &str = r#"import { useQuery } from "@example/gql-client";
import { OrdersDocument, pageSize } from "../generated/documents";
// Commandes récentes — liste complète
export function OrdersPage() {
  const [orders] = useQuery(OrdersDocument, { first: pageSize });
  fetch("/api/orders/export");
  return orders;
}
"#;

    /// The document module this page imports is generated and not checked in.
    const INVOICES_PAGE: &str = r#"import { useQuery } from "@example/gql-client";
import { InvoicesDocument } from "../generated/vendor";
// Factures émises — résumé
export function InvoicesPage() {
  const [invoices] = useQuery(InvoicesDocument);
  return invoices;
}
"#;

    fn write(root: &Path, rel: &str, content: &str) -> String {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// An HTTP row joined to the call that starts at `needle` in `content`,
    /// in candidate span units.
    fn row_at(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        let offset = content.find(needle).expect("needle is in the source") as u32;
        serde_json::from_value(serde_json::json!({
            "candidate_id": format!("c-{offset}"),
            "line_number": content[..offset as usize].lines().count() as i32 + 1,
            "target": target,
            "method": method,
            "pattern_matched": "model",
            "call_expression_span_start": offset + SWC_SPAN_BASE,
        }))
        .unwrap()
    }

    fn result_with(rows: Vec<DataCallResult>) -> FileAnalysisResult {
        FileAnalysisResult {
            data_calls: rows,
            ..Default::default()
        }
    }

    fn targets(results: &HashMap<String, FileAnalysisResult>, key: &str) -> Vec<String> {
        results[key]
            .data_calls
            .iter()
            .map(|call| format!("{} {}", call.method.as_deref().unwrap_or("?"), call.target))
            .collect()
    }

    #[test]
    fn a_row_at_a_call_passing_a_generated_document_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let page = write(root, "src/pages/OrdersPage.tsx", ORDERS_PAGE);

        let mut results = HashMap::from([(
            page.clone(),
            result_with(vec![
                row_at(ORDERS_PAGE, "useQuery(OrdersDocument", "POST", "/graphql"),
                row_at(ORDERS_PAGE, "fetch(", "GET", "/api/orders/export"),
            ]),
        )]);
        let drops = suppress_document_site_http_rows(&mut results, None);

        assert_eq!(targets(&results, &page), vec!["GET /api/orders/export"]);
        assert_eq!(
            drops,
            DocumentSiteDrops {
                document_argument: 1,
                document_executor: 0
            }
        );
    }

    #[test]
    fn a_document_import_missing_from_disk_is_dropped_when_its_callee_executes_documents() {
        // The generated module behind the second page is gitignored. The same
        // hook passes a resolved document on the first page, so its sites are
        // GraphQL executions. No GraphQL row for the missing document is
        // consulted: an external or unresolved document keeps its twin
        // suppressed all the same.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let orders = write(root, "src/pages/OrdersPage.tsx", ORDERS_PAGE);
        let invoices = write(root, "src/pages/InvoicesPage.tsx", INVOICES_PAGE);

        let mut results = HashMap::from([
            (
                orders.clone(),
                result_with(vec![row_at(
                    ORDERS_PAGE,
                    "useQuery(OrdersDocument",
                    "POST",
                    "/graphql",
                )]),
            ),
            (
                invoices.clone(),
                result_with(vec![row_at(
                    INVOICES_PAGE,
                    "useQuery(InvoicesDocument",
                    "POST",
                    "https://api.example.test/graphql",
                )]),
            ),
        ]);
        let drops = suppress_document_site_http_rows(&mut results, None);

        assert!(targets(&results, &orders).is_empty());
        assert!(targets(&results, &invoices).is_empty());
        assert_eq!(
            drops,
            DocumentSiteDrops {
                document_argument: 1,
                document_executor: 1
            }
        );
    }

    #[test]
    fn a_missing_import_keeps_its_row_when_no_site_shows_the_callee_executes_documents() {
        let tmp = tempfile::tempdir().unwrap();
        let invoices = write(tmp.path(), "src/pages/InvoicesPage.tsx", INVOICES_PAGE);

        let mut results = HashMap::from([(
            invoices.clone(),
            result_with(vec![row_at(
                INVOICES_PAGE,
                "useQuery(InvoicesDocument",
                "POST",
                "/graphql",
            )]),
        )]);
        let drops = suppress_document_site_http_rows(&mut results, None);

        assert_eq!(targets(&results, &invoices), vec!["POST /graphql"]);
        assert_eq!(drops.total(), 0);
    }

    #[test]
    fn a_known_executor_passed_a_non_document_keeps_its_row() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let orders = write(root, "src/pages/OrdersPage.tsx", ORDERS_PAGE);
        let reports = r#"import { useQuery } from "@example/gql-client";
import { pageSize } from "../generated/documents";
const reportOptions = { url: "/api/reports", size: pageSize };
export function ReportsPage() {
  return useQuery(reportOptions, pageSize);
}
"#;
        let reports_path = write(root, "src/pages/ReportsPage.tsx", reports);

        let mut results = HashMap::from([
            (
                orders.clone(),
                result_with(vec![row_at(
                    ORDERS_PAGE,
                    "useQuery(OrdersDocument",
                    "POST",
                    "/graphql",
                )]),
            ),
            (
                reports_path.clone(),
                result_with(vec![row_at(
                    reports,
                    "useQuery(reportOptions",
                    "GET",
                    "/api/reports",
                )]),
            ),
        ]);
        suppress_document_site_http_rows(&mut results, None);

        assert_eq!(targets(&results, &reports_path), vec!["GET /api/reports"]);
    }

    #[test]
    fn a_same_file_document_template_is_recognised_by_its_parse_not_its_tag() {
        let tmp = tempfile::tempdir().unwrap();
        let source = r#"import { client, doc } from "./client";
const ORDER_QUERY = doc`
  query Order($id: ID!) { order(id: $id) { id total } }
`;
const NOT_A_DOCUMENT = doc`{ color: red }`;
// Détail de la commande
export async function loadOrder(id: string) {
  await client.request(NOT_A_DOCUMENT);
  return client.request(ORDER_QUERY, { id });
}
"#;
        let file = write(tmp.path(), "src/order.ts", source);

        let mut results = HashMap::from([(
            file.clone(),
            result_with(vec![
                row_at(source, "client.request(NOT_A_DOCUMENT", "POST", "/styles"),
                row_at(source, "client.request(ORDER_QUERY", "POST", "/graphql"),
            ]),
        )]);
        suppress_document_site_http_rows(&mut results, None);

        assert_eq!(targets(&results, &file), vec!["POST /styles"]);
    }

    #[test]
    fn an_aliased_document_import_resolves_through_the_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
        );
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let source = ORDERS_PAGE.replace("../generated/documents", "@/generated/documents");
        let page = write(root, "src/pages/OrdersPage.tsx", &source);

        let mut results = HashMap::from([(
            page.clone(),
            result_with(vec![row_at(
                &source,
                "useQuery(OrdersDocument",
                "POST",
                "/graphql",
            )]),
        )]);
        let workspace = WorkspaceIndex::build_with_aliases(root, None);
        let drops = suppress_document_site_http_rows(&mut results, Some(&workspace));

        assert!(targets(&results, &page).is_empty());
        assert_eq!(drops.document_argument, 1);
    }

    #[test]
    fn a_row_already_keyed_to_its_operation_is_not_a_twin() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let page = write(root, "src/pages/OrdersPage.tsx", ORDERS_PAGE);

        let mut results = HashMap::from([(
            page.clone(),
            result_with(vec![row_at(
                ORDERS_PAGE,
                "useQuery(OrdersDocument",
                "POST",
                "graphql|query|orders",
            )]),
        )]);
        let drops = suppress_document_site_http_rows(&mut results, None);

        assert_eq!(targets(&results, &page), vec!["POST graphql|query|orders"]);
        assert_eq!(drops.total(), 0);
    }
}
