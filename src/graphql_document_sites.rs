//! Call sites that execute a GraphQL document: their HTTP twins
//! (carrick#1154) and the operations they send (carrick#1157).
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
//!
//! The same reading places the GraphQL consumer rows. A document written in a
//! `.graphql` file is compiled into a declaration that states its operation
//! (`export const OrdersDocument = {"kind":"Document",...}`), and the call that
//! passes that declaration is where the operation is sent from. Before this,
//! the index pointed at the line of the `.graphql` file, which no agent edits
//! when it changes the call. [`collect_document_site_consumers`] reads each
//! executed declaration's operations (kind, name, root fields) and places one
//! consumer row per root field at the call. A document-file operation with
//! the same kind, name and root fields is then the same operation seen from
//! its source, and its rows are removed; a document-file operation no call
//! executes keeps its rows.
//!
//! A declaration the GraphQL extraction already indexes where it is written (a
//! `gql` tagged template in source) keeps its row at the declaration: the
//! source line IS the document, and the call-site anchor for its result type
//! is read there. An operation with no name is placed at its calls but covers
//! nothing, because nothing ties it to one document-file operation.

use crate::agents::file_analyzer_agent::FileAnalysisResult;
use crate::graphql::{GraphqlExtraction, GraphqlOp};
use crate::import_bindings::BindingResolver;
use crate::operation::{GraphqlOperationKind, OperationKey};
use crate::parser::parse_file;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::workspace_resolver::WorkspaceIndex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{
    SourceMap, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    CallExpr, Callee, Decl, Expr, ImportDecl, ImportSpecifier, Lit, Module, ModuleDecl,
    ModuleExportName, ModuleItem, ObjectLit, Pat, Prop, PropName, PropOrSpread, Stmt,
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
    /// Every document a call in this file executes, with the call's line.
    executed: Vec<ExecutedDocument>,
}

/// One executable operation a document states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentOperation {
    pub kind: GraphqlOperationKind,
    /// `None` for an anonymous operation.
    pub name: Option<String>,
    /// Root field names (not aliases), introspection fields left out, in
    /// document order.
    pub fields: Vec<String>,
}

/// A document bound to a name, or a document file imported whole.
#[derive(Debug, Clone)]
struct DeclaredDocument {
    /// Canonical path of the file the document is written in.
    file: PathBuf,
    /// 1-based line its text or literal starts on: `1` for a document file.
    line: u32,
    operations: Vec<DocumentOperation>,
}

/// A call that passes a document.
#[derive(Debug, Clone)]
struct ExecutedDocument {
    /// 1-based line of the call.
    line: u32,
    document: DeclaredDocument,
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

/// The documents one service's calls execute, read before they are placed in
/// the service's GraphQL extraction ([`DocumentSiteConsumers::apply`]).
#[derive(Debug, Default)]
pub struct DocumentSiteConsumers {
    /// `(site file as the service lists it, executed document)`, in file
    /// order.
    sites: Vec<(PathBuf, ExecutedDocument)>,
}

/// What placing the call-site rows changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DocumentSiteSummary {
    /// Consumer rows added at calls.
    pub site_rows: usize,
    /// Document-file rows removed because a call executes their operation.
    pub covered_rows: usize,
    /// Calls whose document is already indexed where it is declared.
    pub declared_in_source: usize,
}

/// Read every call in `files` that passes a GraphQL document, with the
/// operations that document states. `workspace` resolves aliased and package
/// specifiers; relative specifiers resolve without it.
pub fn collect_document_site_consumers(
    files: &[PathBuf],
    workspace: Option<&WorkspaceIndex>,
) -> DocumentSiteConsumers {
    let mut reader = DocumentReader::new(workspace);
    let mut sites = Vec::new();
    for file in files {
        let is_script = file
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mts" | "cts"));
        if !is_script {
            continue;
        }
        for executed in reader.file_sites(file).executed {
            sites.push((file.clone(), executed));
        }
    }
    DocumentSiteConsumers { sites }
}

impl DocumentSiteConsumers {
    /// Place a consumer row at every call for each root field its document
    /// selects, and remove the document-file rows of every operation a call
    /// executes.
    pub fn apply(self, extraction: &mut GraphqlExtraction) -> DocumentSiteSummary {
        let mut summary = DocumentSiteSummary::default();
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        // Documents the extraction indexes where they are written in source.
        let indexed_in_source: HashSet<(PathBuf, u32)> = extraction
            .consumers
            .iter()
            .filter(|op| !is_graphql_document_path(&op.file_path))
            .map(|op| (canonical(&op.file_path), op.document_line))
            .collect();

        let mut seen: HashSet<(PathBuf, u32, String)> = HashSet::new();
        let mut executed_operations: HashSet<(GraphqlOperationKind, String, Vec<String>)> =
            HashSet::new();
        let mut rows = Vec::new();
        for (site_file, executed) in self.sites {
            let document = &executed.document;
            if !is_graphql_document_path(&document.file)
                && indexed_in_source.contains(&(document.file.clone(), document.line))
            {
                summary.declared_in_source += 1;
                continue;
            }
            for operation in &document.operations {
                if let Some(name) = &operation.name {
                    executed_operations.insert((
                        operation.kind,
                        name.clone(),
                        operation.fields.clone(),
                    ));
                }
                for field in &operation.fields {
                    let key = OperationKey::graphql(operation.kind, field.clone());
                    if !seen.insert((site_file.clone(), executed.line, key.canonical())) {
                        continue;
                    }
                    rows.push(site_consumer(key, &site_file, executed.line));
                }
            }
        }

        // The document-file operations those calls execute, located by the
        // line and key of each of their rows.
        let mut document_files: HashMap<PathBuf, HashMap<(u32, String), DocumentOperation>> =
            HashMap::new();
        extraction.consumers.retain(|op| {
            if !is_graphql_document_path(&op.file_path) {
                return true;
            }
            let operations = document_files
                .entry(op.file_path.clone())
                .or_insert_with(|| document_file_rows(&op.file_path));
            let Some(operation) = operations.get(&(op.line, op.key.canonical())) else {
                return true;
            };
            let Some(name) = &operation.name else {
                return true;
            };
            if executed_operations.contains(&(
                operation.kind,
                name.clone(),
                operation.fields.clone(),
            )) {
                summary.covered_rows += 1;
                false
            } else {
                true
            }
        });

        summary.site_rows = rows.len();
        extraction.consumers.extend(rows);
        if summary != DocumentSiteSummary::default() {
            debug!(
                site_rows = summary.site_rows,
                covered_rows = summary.covered_rows,
                declared_in_source = summary.declared_in_source,
                "GraphQL consumer rows placed at the calls that execute their documents"
            );
        }
        summary
    }
}

/// A consumer row at a call. The call is its own document for attribution:
/// its root fields are the basis the schema catalogue judges.
fn site_consumer(key: OperationKey, file: &Path, line: u32) -> GraphqlOp {
    GraphqlOp {
        key,
        file_path: file.to_path_buf(),
        line,
        document_line: line,
        primary_type_symbol: None,
        payload_type_symbol: None,
        payload_type_source: None,
        resolver_file: None,
        resolver_line: None,
        response_type_symbol: None,
        response_type_source: None,
        consumer_located_type_symbol: None,
        consumer_located_type_source: None,
        // Set by attribution, which runs after these rows are placed.
        schema_binding: None,
        arguments: None,
    }
}

/// `(root field line, canonical key) -> operation` for a document file, the
/// coordinates its extracted consumer rows carry.
fn document_file_rows(file: &Path) -> HashMap<(u32, String), DocumentOperation> {
    let mut rows = HashMap::new();
    let Ok(text) = std::fs::read_to_string(file) else {
        return rows;
    };
    for (operation, lines) in text_operations_with_lines(&text) {
        for (field, line) in operation.fields.iter().zip(lines) {
            let key = OperationKey::graphql(operation.kind, field.clone());
            rows.insert((line, key.canonical()), operation.clone());
        }
    }
    rows
}

/// Reads call sites and document declarations, caching every module it
/// parses: one generated document module is read once however many files
/// import it.
struct DocumentReader<'a> {
    source_map: Lrc<SourceMap>,
    handler: Handler,
    resolver: BindingResolver,
    workspace: Option<&'a WorkspaceIndex>,
    /// Module → the documents it binds to a name at module scope.
    documents: HashMap<PathBuf, HashMap<String, DeclaredDocument>>,
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

    fn module_documents(&mut self, file: &Path) -> &HashMap<String, DeclaredDocument> {
        if !self.documents.contains_key(file) {
            let documents = parse_file(file, &self.source_map, &self.handler)
                .map(|module| document_declarations(&module, &self.source_map, file))
                .unwrap_or_default();
            self.documents.insert(file.to_path_buf(), documents);
        }
        &self.documents[file]
    }

    /// The module a specifier names from `importer`, when it is on disk.
    fn resolve_module(&self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        match self.workspace {
            Some(workspace) => workspace.resolve_module_path(importer, specifier),
            None => crate::agents::file_orchestrator::FileOrchestrator::resolve_relative_import(
                importer, specifier,
            ),
        }
    }

    /// The module key a specifier names from `importer`: the resolved file
    /// when there is one, else the specifier as written (a package name reads
    /// the same from every file).
    fn module_key(&self, importer: &Path, specifier: &str) -> String {
        match self.resolve_module(importer, specifier) {
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
        let canonical = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        let local_documents = document_declarations(&module, &cm, &canonical);
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
            let line = cm.lookup_char_pos(call.span_lo).line as u32;
            let mut has_document = false;
            let mut has_unresolved = false;
            for name in &call.ident_args {
                match self.classify_argument(file, name, &local_documents, &imports) {
                    Argument::Document(document) => {
                        has_document = true;
                        if let Some(document) = document {
                            sites.executed.push(ExecutedDocument { line, document });
                        }
                    }
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
        local_documents: &HashMap<String, DeclaredDocument>,
        imports: &HashMap<String, Import>,
    ) -> Argument {
        if let Some(document) = local_documents.get(name) {
            return Argument::Document(Some(document.clone()));
        }
        let Some(import) = imports.get(name) else {
            return Argument::Other;
        };
        if import.imported == "default" && is_graphql_document_file(&import.specifier) {
            // The file is the document. One that is not on disk still makes
            // this call a GraphQL execution; it just states no operation.
            let document = self
                .resolve_module(file, &import.specifier)
                .and_then(|path| {
                    let text = std::fs::read_to_string(&path).ok()?;
                    Some(DeclaredDocument {
                        file: path,
                        line: 1,
                        operations: text_operations_with_lines(&text)
                            .into_iter()
                            .map(|(operation, _)| operation)
                            .collect(),
                    })
                });
            return Argument::Document(document);
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
        match self.module_documents(&binding.file).get(&local_name) {
            Some(document) => Argument::Document(Some(document.clone())),
            None => Argument::Other,
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
            // The same spelling an importer's resolved specifier has, so a
            // binding declared here and imported elsewhere is one callee.
            let module = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
            (module.to_string_lossy().into_owned(), root.clone())
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
    /// A GraphQL document, with what it states when it can be read.
    Document(Option<DeclaredDocument>),
    /// An import whose declaration the module graph cannot reach.
    Unresolved,
    Other,
}

/// One name an importing file binds, and where the module graph reads it from.
/// Shared with [`crate::wrapper_call_join`], which resolves a call's callee the
/// same way this pass resolves an argument.
#[derive(Debug, Clone)]
pub(crate) struct Import {
    pub(crate) specifier: String,
    /// The name the module publishes (`default` for a default import).
    pub(crate) imported: String,
}

/// Local name → where it was imported from. Namespace imports are left out:
/// a `ns.Member` argument is not an identifier argument.
pub(crate) fn import_table(module: &Module) -> HashMap<String, Import> {
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

/// The documents a module binds to a name at module scope. `file` is the
/// module's canonical path.
fn document_declarations(
    module: &Module,
    source_map: &Lrc<SourceMap>,
    file: &Path,
) -> HashMap<String, DeclaredDocument> {
    let mut documents = HashMap::new();
    for decl in module_scope_decls(module) {
        let Decl::Var(var) = decl else {
            continue;
        };
        for declarator in &var.decls {
            if let (Pat::Ident(ident), Some(init)) = (&declarator.name, declarator.init.as_deref())
                && is_document_expression(init)
            {
                let expression = unwrap_expression(init);
                documents.insert(
                    ident.id.sym.to_string(),
                    DeclaredDocument {
                        file: file.to_path_buf(),
                        line: source_map.lookup_char_pos(expression.span().lo).line as u32,
                        operations: expression_operations(expression),
                    },
                );
            }
        }
    }
    documents
}

/// The operations a document expression states: read off the graphql-js AST
/// object literal, or parsed from the text of a template or string.
fn expression_operations(expr: &Expr) -> Vec<DocumentOperation> {
    match unwrap_expression(expr) {
        Expr::Object(object) => object_operations(object),
        Expr::TaggedTpl(tagged) => text_operations(&template_text(&tagged.tpl)),
        Expr::Call(call) if call.args.len() == 1 && call.args[0].spread.is_none() => {
            match unwrap_expression(&call.args[0].expr) {
                Expr::Tpl(tpl) => text_operations(&template_text(tpl)),
                Expr::Lit(Lit::Str(s)) => text_operations(s.value.as_ref()),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The value of property `key` in an object literal.
fn property<'e>(object: &'e ObjectLit, key: &str) -> Option<&'e Expr> {
    object.props.iter().find_map(|prop| {
        let PropOrSpread::Prop(prop) = prop else {
            return None;
        };
        let Prop::KeyValue(pair) = &**prop else {
            return None;
        };
        let matches = match &pair.key {
            PropName::Ident(ident) => ident.sym == *key,
            PropName::Str(s) => s.value == *key,
            _ => false,
        };
        matches.then(|| unwrap_expression(&pair.value))
    })
}

fn string_property<'e>(object: &'e ObjectLit, key: &str) -> Option<&'e str> {
    match property(object, key)? {
        Expr::Lit(Lit::Str(s)) => Some(s.value.as_ref()),
        _ => None,
    }
}

/// `{ ..., key: { value: "..." } }`, the graphql-js `Name` node.
fn name_value<'e>(object: &'e ObjectLit, key: &str) -> Option<&'e str> {
    match property(object, key)? {
        Expr::Object(name) => string_property(name, "value"),
        _ => None,
    }
}

/// The object literals in the array property `key`.
fn object_elements<'e>(object: &'e ObjectLit, key: &str) -> Vec<&'e ObjectLit> {
    let Some(Expr::Array(array)) = property(object, key) else {
        return Vec::new();
    };
    array
        .elems
        .iter()
        .flatten()
        .filter(|element| element.spread.is_none())
        .filter_map(|element| match unwrap_expression(&element.expr) {
            Expr::Object(object) => Some(object),
            _ => None,
        })
        .collect()
}

/// Operations of a graphql-js `Document` node written as an object literal.
/// A root fragment spread states no field and is left out, as it is when the
/// same document is parsed from text.
fn object_operations(document: &ObjectLit) -> Vec<DocumentOperation> {
    object_elements(document, "definitions")
        .into_iter()
        .filter(|definition| string_property(definition, "kind") == Some("OperationDefinition"))
        .filter_map(|definition| {
            let kind = match string_property(definition, "operation")? {
                "query" => GraphqlOperationKind::Query,
                "mutation" => GraphqlOperationKind::Mutation,
                "subscription" => GraphqlOperationKind::Subscription,
                _ => return None,
            };
            let fields = match property(definition, "selectionSet") {
                Some(Expr::Object(selection_set)) => object_elements(selection_set, "selections")
                    .into_iter()
                    .filter(|selection| string_property(selection, "kind") == Some("Field"))
                    .filter_map(|selection| name_value(selection, "name"))
                    .filter(|name| !name.starts_with("__"))
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            Some(DocumentOperation {
                kind,
                name: name_value(definition, "name").map(str::to_string),
                fields,
            })
        })
        .collect()
}

fn text_operations(text: &str) -> Vec<DocumentOperation> {
    text_operations_with_lines(text)
        .into_iter()
        .map(|(operation, _)| operation)
        .collect()
}

/// Operations of an executable document's text, each with the 1-based line
/// of every root field, in the order of `fields`. The same reading the
/// GraphQL extraction gives a document's consumer rows: an anonymous
/// selection set is a query, fragment spreads and introspection fields at the
/// root are left out.
fn text_operations_with_lines(text: &str) -> Vec<(DocumentOperation, Vec<u32>)> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection};
    let Ok(document) = graphql_parser::parse_query::<String>(text) else {
        return Vec::new();
    };
    let mut operations = Vec::new();
    for definition in &document.definitions {
        let Definition::Operation(operation) = definition else {
            continue;
        };
        let (kind, name, selection_set) = match operation {
            OperationDefinition::SelectionSet(set) => (GraphqlOperationKind::Query, None, set),
            OperationDefinition::Query(q) => (
                GraphqlOperationKind::Query,
                q.name.clone(),
                &q.selection_set,
            ),
            OperationDefinition::Mutation(m) => (
                GraphqlOperationKind::Mutation,
                m.name.clone(),
                &m.selection_set,
            ),
            OperationDefinition::Subscription(s) => (
                GraphqlOperationKind::Subscription,
                s.name.clone(),
                &s.selection_set,
            ),
        };
        let mut fields = Vec::new();
        let mut lines = Vec::new();
        for selection in &selection_set.items {
            let Selection::Field(field) = selection else {
                continue;
            };
            if field.name.starts_with("__") {
                continue;
            }
            fields.push(field.name.clone());
            lines.push(field.position.line as u32);
        }
        operations.push((DocumentOperation { kind, name, fields }, lines));
    }
    operations
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

pub(crate) fn unwrap_expression(expr: &Expr) -> &Expr {
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

fn is_graphql_document_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "graphql" || ext == "gql")
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
    fn a_client_declared_in_one_module_is_one_executor_across_its_importers() {
        // The client is declared where it executes a resolved document, and
        // imported where it is handed one whose module is not on disk. Both
        // sites must name the same callee, however the scan spelled the path
        // (a temp dir is behind a symlink on some systems).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/generated/documents.ts", GENERATED_DOCUMENTS);
        let client = r#"import { createClient } from "@example/gql-client";
import { OrdersDocument } from "./generated/documents";
export const api = createClient();
// Requête initiale — commandes
export const loadOrders = () => api.query(OrdersDocument);
"#;
        let client_path = write(root, "src/client.ts", client);
        let invoices = r#"import { api } from "./client";
import { InvoicesDocument } from "./generated/vendor";
// Factures — chargement
export const loadInvoices = () => api.query(InvoicesDocument);
"#;
        let invoices_path = write(root, "src/invoices.ts", invoices);

        let mut results = HashMap::from([
            (
                client_path.clone(),
                result_with(vec![row_at(
                    client,
                    "api.query(OrdersDocument",
                    "POST",
                    "/graphql",
                )]),
            ),
            (
                invoices_path.clone(),
                result_with(vec![row_at(
                    invoices,
                    "api.query(InvoicesDocument",
                    "POST",
                    "/graphql",
                )]),
            ),
        ]);
        let drops = suppress_document_site_http_rows(&mut results, None);

        assert!(targets(&results, &invoices_path).is_empty());
        assert_eq!(drops.document_executor, 1);
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

    /// The document file a generated module is compiled from.
    const ORDER_DOCUMENTS: &str = "query Orders {\n  orders { id }\n  shippingZones { code }\n}\n\nmutation PlaceOrder($id: ID!) {\n  placeOrder(id: $id) { id }\n}\n\nquery Carriers {\n  carriers { name }\n}\n";

    /// What a codegen compiles `ORDER_DOCUMENTS` into: every operation as a
    /// graphql-js AST literal, with an alias and an introspection field to
    /// show the field name is read, not the alias.
    const COMPILED_DOCUMENTS: &str = r#"import { TypedDocumentNode as DocumentNode } from "@example/typed-document";
export const OrdersDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Orders"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"orders"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"id"}}]}},{"kind":"Field","alias":{"kind":"Name","value":"zones"},"name":{"kind":"Name","value":"shippingZones"}},{"kind":"Field","name":{"kind":"Name","value":"__typename"}}]}}]} as unknown as DocumentNode<unknown, unknown>;
export const PlaceOrderDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"mutation","name":{"kind":"Name","value":"PlaceOrder"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"placeOrder"}}]}}]} as unknown as DocumentNode<unknown, unknown>;
export const CarriersDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Carriers"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"carriers"}}]}}]} as unknown as DocumentNode<unknown, unknown>;
"#;

    /// Two pages executing two of the three operations; the mutation twice.
    const CHECKOUT_PAGE: &str = r#"import { useMutation, useQuery } from "@example/gql-client";
import { OrdersDocument, PlaceOrderDocument } from "../generated/documents";
// Caisse — récapitulatif
export function CheckoutPage() {
  const [orders] = useQuery(OrdersDocument);
  const [place] = useMutation(PlaceOrderDocument);
  return { orders, place };
}
"#;

    const RETRY_PAGE: &str = r#"import { useMutation } from "@example/gql-client";
import { PlaceOrderDocument } from "../generated/documents";
export function RetryButton() {
  return useMutation(PlaceOrderDocument);
}
"#;

    fn rows(extraction: &GraphqlExtraction, root: &Path) -> Vec<(String, String, u32)> {
        let mut rows: Vec<(String, String, u32)> = extraction
            .consumers
            .iter()
            .map(|op| {
                let file = op
                    .file_path
                    .strip_prefix(root)
                    .unwrap_or(&op.file_path)
                    .display()
                    .to_string();
                (op.key.canonical(), file, op.line)
            })
            .collect();
        rows.sort();
        rows
    }

    fn row(key: &str, file: &str, line: u32) -> (String, String, u32) {
        (key.to_string(), file.to_string(), line)
    }

    #[test]
    fn operations_are_placed_at_the_calls_that_execute_their_compiled_documents() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical, as the service walk lists files on a real checkout.
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "src/graphql/orders.graphql", ORDER_DOCUMENTS);
        write(&root, "src/generated/documents.ts", COMPILED_DOCUMENTS);
        let checkout = write(&root, "src/pages/CheckoutPage.tsx", CHECKOUT_PAGE);
        let retry = write(&root, "src/pages/RetryButton.tsx", RETRY_PAGE);
        let files: Vec<PathBuf> = [
            root.join("src/graphql/orders.graphql")
                .display()
                .to_string(),
            root.join("src/generated/documents.ts")
                .display()
                .to_string(),
            checkout,
            retry,
        ]
        .iter()
        .map(PathBuf::from)
        .collect();

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        assert_eq!(
            rows(&extraction, &root),
            vec![
                row(
                    "graphql|mutation|placeOrder",
                    "src/graphql/orders.graphql",
                    7
                ),
                row("graphql|query|carriers", "src/graphql/orders.graphql", 11),
                row("graphql|query|orders", "src/graphql/orders.graphql", 2),
                row(
                    "graphql|query|shippingZones",
                    "src/graphql/orders.graphql",
                    3
                ),
            ],
            "the document file's rows before the calls are read"
        );

        let summary = collect_document_site_consumers(&files, None).apply(&mut extraction);

        assert_eq!(
            rows(&extraction, &root),
            vec![
                row(
                    "graphql|mutation|placeOrder",
                    "src/pages/CheckoutPage.tsx",
                    6
                ),
                row(
                    "graphql|mutation|placeOrder",
                    "src/pages/RetryButton.tsx",
                    4
                ),
                row("graphql|query|carriers", "src/graphql/orders.graphql", 11),
                row("graphql|query|orders", "src/pages/CheckoutPage.tsx", 5),
                row(
                    "graphql|query|shippingZones",
                    "src/pages/CheckoutPage.tsx",
                    5
                ),
            ],
            "executed operations move to their calls; the one no call executes stays"
        );
        assert_eq!(
            summary,
            DocumentSiteSummary {
                site_rows: 4,
                covered_rows: 3,
                declared_in_source: 0,
            }
        );
    }

    #[test]
    fn a_compiled_document_whose_fields_differ_covers_nothing() {
        // A stale generated module: the document file gained a root field the
        // compiled literal does not have. The call still sends what the
        // literal states, and the document file's rows are not the same
        // operation, so both stay.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let edited = ORDER_DOCUMENTS.replace(
            "  orders { id }\n",
            "  orders { id }\n  taxRates { code }\n",
        );
        write(&root, "src/graphql/orders.graphql", &edited);
        write(&root, "src/generated/documents.ts", COMPILED_DOCUMENTS);
        let checkout = write(&root, "src/pages/CheckoutPage.tsx", CHECKOUT_PAGE);
        let files = vec![
            root.join("src/graphql/orders.graphql"),
            PathBuf::from(checkout),
        ];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        collect_document_site_consumers(&files, None).apply(&mut extraction);
        let kept: Vec<(String, String, u32)> = rows(&extraction, &root)
            .into_iter()
            .filter(|(_, file, _)| file.ends_with(".graphql"))
            .collect();

        // `PlaceOrder` is unchanged, so its row still moves to the call.
        assert_eq!(
            kept,
            vec![
                row("graphql|query|carriers", "src/graphql/orders.graphql", 12),
                row("graphql|query|orders", "src/graphql/orders.graphql", 2),
                row(
                    "graphql|query|shippingZones",
                    "src/graphql/orders.graphql",
                    4
                ),
                row("graphql|query|taxRates", "src/graphql/orders.graphql", 3),
            ],
        );
    }

    #[test]
    fn a_template_document_indexed_where_it_is_written_keeps_its_row_there() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let source = r#"import { gql, request } from "graphql-request";
const ORDER = gql`
  query Order($id: ID!) { order(id: $id) { id } }
`;
export const loadOrder = (id: string) => request("/graphql", ORDER, { id });
"#;
        let file = PathBuf::from(write(&root, "src/order.ts", source));
        let files = vec![file];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        let summary = collect_document_site_consumers(&files, None).apply(&mut extraction);

        assert_eq!(
            rows(&extraction, &root),
            vec![row("graphql|query|order", "src/order.ts", 3)]
        );
        assert_eq!(summary.declared_in_source, 1);
        assert_eq!(summary.site_rows, 0);
    }

    #[test]
    fn a_document_file_imported_whole_is_placed_at_its_call() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(
            &root,
            "src/graphql/carriers.graphql",
            "query Carriers {\n  carriers { name }\n}\n",
        );
        let source = r#"import { useQuery } from "@example/gql-client";
import CarriersQuery from "../graphql/carriers.graphql";
export function CarrierList() {
  return useQuery(CarriersQuery);
}
"#;
        let page = PathBuf::from(write(&root, "src/pages/CarrierList.tsx", source));
        let files = vec![root.join("src/graphql/carriers.graphql"), page];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        collect_document_site_consumers(&files, None).apply(&mut extraction);

        assert_eq!(
            rows(&extraction, &root),
            vec![row(
                "graphql|query|carriers",
                "src/pages/CarrierList.tsx",
                4
            )]
        );
    }
}
