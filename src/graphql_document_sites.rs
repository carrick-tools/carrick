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
//! This pass drops those rows, in two steps. Which calls they apply to is
//! read before the analysis, through the value resolver over the compiler's
//! rows ([`read_document_sites`], carrick#3504 S9); a service with no rows
//! has no document site.
//!
//! 1. **The argument is a document.** A value a call's arguments may hold
//!    (at any position, under any key of an object literal written there, a
//!    binding's value, a wrapper's parameter through its callers) is a
//!    declaration the compiler's row names, and it binds a GraphQL document:
//!    - an object literal with `kind: "Document"` (the graphql-js AST node,
//!      which is what generated typed documents are);
//!    - a tagged template or a single-argument call whose text parses as an
//!      executable GraphQL document (the parse decides, not the tag name);
//!    - an object literal with one property whose text parses as one;
//!    - the default import of a `.graphql` / `.gql` file.
//! 2. **The callee executes documents.** Generated document modules are often
//!    gitignored, so in a bare checkout the argument's import resolves to
//!    nothing. When the SAME callee (the declaration its row names) is seen
//!    executing a resolved document elsewhere in the service, its sites whose
//!    argument import is unresolvable are GraphQL executions too. A callee
//!    never seen with a resolved document keeps its rows: there is no
//!    fallback.
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
//! when it changes the call. [`read_document_sites`] reads each
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
    Decl, Expr, Lit, Module, ModuleDecl, ModuleItem, ObjectLit, Pat, Prop, PropName, PropOrSpread,
    Stmt, TsEntityName, TsPropertySignature, TsType, TsTypeElement, TsTypeRef, VarDeclarator,
};
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

/// The calls of one service that execute a GraphQL document, read before
/// its analysis ([`read_document_sites`]) and handed to the analysis, which
/// drops their HTTP twins ([`suppress_document_site_http_rows`]).
#[derive(Debug, Default, Clone)]
pub struct DocumentSites {
    /// File (as the service lists it) → span starts (candidate units) of
    /// the calls whose arguments hold a document, or a parameter every
    /// caller fills with one.
    document_sites: HashMap<String, HashSet<u32>>,
    /// File → span starts of the calls whose argument is an import no
    /// declaration answers, through a callee seen executing a document.
    executor_sites: HashMap<String, HashSet<u32>>,
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
    /// The key each root field's value has in the result: its alias when it
    /// has one, else its name. Parallel to `fields`.
    pub response_keys: Vec<String>,
    /// What each root field selects, fragment spreads read through to the
    /// definitions the document holds. Parallel to `fields`. Read against the
    /// schema the document is sent to ([`crate::graphql_selection`]).
    pub selections: Vec<crate::graphql_selection::SelectionSet>,
}

impl DocumentOperation {
    /// What every root field named `field` selects (one per occurrence: an
    /// operation may select a field twice under two aliases).
    pub fn selections_of<'s>(
        &'s self,
        field: &'s str,
    ) -> impl Iterator<Item = &'s crate::graphql_selection::SelectionSet> {
        self.fields
            .iter()
            .zip(&self.selections)
            .filter(move |(name, _)| name.as_str() == field)
            .map(|(_, selection)| selection)
    }
}

/// A document bound to a name, or a document file imported whole.
#[derive(Debug, Clone)]
struct DeclaredDocument {
    /// Canonical path of the file the document is written in.
    file: PathBuf,
    /// 1-based line its text or literal starts on: `1` for a document file.
    line: u32,
    operations: Vec<DocumentOperation>,
    /// Root field name → where the declaration states that field's result
    /// type ([`declared_field_types`], carrick#1761). Empty when it states
    /// none this pass can read.
    field_types: HashMap<String, DeclaredFieldType>,
}

/// Where a document's declaration states one root field's result type
/// (carrick#1761).
///
/// A generated document declaration is asserted to a document type whose
/// type arguments include the operation's result type
/// (`as unknown as DocumentNode<OrdersQuery, OrdersQueryVariables>`), and
/// that result type declares one property per root field, under the field's
/// response key (`orders: Array<{ id: string }>`). This is the span of that
/// property's type: the field's payload, the level a consumer row is keyed
/// at, never the operation wrapper around it (carrick#1760). The type
/// sidecar reads the type at this span. A result type the model locates is
/// read the same way when it is the operation's result
/// ([`read_located_result_types`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredFieldType {
    /// Canonical path of the module that declares the result type.
    pub file: PathBuf,
    /// 1-based line the property's type starts on.
    pub line: u32,
    /// The property type's span in the scanner's own units: UTF-8 bytes
    /// counted from [`SWC_SPAN_BASE`]. Converted to the sidecar's numbering at
    /// the request, never stored converted (carrick#805).
    pub lo: u32,
    pub hi: u32,
}

/// A call that passes a document.
#[derive(Debug, Clone)]
struct ExecutedDocument {
    /// 1-based line of the call.
    line: u32,
    document: DeclaredDocument,
}

/// Drop HTTP data calls at GraphQL document sites across one service's file
/// results, the sites read before the analysis ([`read_document_sites`]).
pub fn suppress_document_site_http_rows(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    sites: &DocumentSites,
) -> DocumentSiteDrops {
    let no_sites = HashSet::new();
    let mut keys: Vec<&String> = sites
        .document_sites
        .keys()
        .chain(sites.executor_sites.keys())
        .collect();
    keys.sort();
    keys.dedup();

    let mut drops = DocumentSiteDrops::default();
    for key in keys {
        let Some(result) = file_results.get_mut(key) else {
            continue;
        };
        let document_sites = sites.document_sites.get(key).unwrap_or(&no_sites);
        let executor_sites = sites.executor_sites.get(key).unwrap_or(&no_sites);
        result.data_calls.retain(|call| {
            let Some(span) = call.call_expression_span_start else {
                return true;
            };
            // A target already rewritten to the operation key is the GraphQL
            // row, not its HTTP twin.
            if call.target.starts_with("graphql|") {
                return true;
            }
            let reason = if document_sites.contains(&span) {
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

/// Read every call of one service that passes a GraphQL document, through
/// the value resolver over the compiler's rows
/// ([`crate::request_summary::document_calls`]): the sites whose HTTP twins
/// the analysis drops, and the documents each call executes, placed once the
/// rows are built ([`DocumentSiteConsumers::apply`]). `siblings` are the
/// run's other services, read as the resolver reads them. `workspace`
/// resolves the specifier of a document file imported whole.
pub fn read_document_sites(
    inputs: &crate::request_summary::RequestSummaryInputs,
    siblings: &[&crate::request_summary::RequestSummaryInputs],
    workspace: Option<&WorkspaceIndex>,
) -> (DocumentSites, DocumentSiteConsumers) {
    let mut reader = DocumentReader::new(workspace);
    let calls = crate::request_summary::document_calls(inputs, siblings, &mut reader);
    // A callee seen executing a document: its sites whose argument is an
    // import no declaration answers execute documents too.
    let executors: HashSet<&(String, String)> = calls
        .iter()
        .filter(|call| !call.documents.is_empty())
        .filter_map(|call| call.callee.as_ref())
        .collect();
    let mut sites = DocumentSites::default();
    let mut consumers = Vec::new();
    for call in &calls {
        let file = call.file.to_string_lossy().into_owned();
        if !call.documents.is_empty() || call.through_callers {
            sites
                .document_sites
                .entry(file)
                .or_default()
                .insert(call.span_start);
        } else if call.unresolved_import
            && call
                .callee
                .as_ref()
                .is_some_and(|callee| executors.contains(callee))
        {
            sites
                .executor_sites
                .entry(file)
                .or_default()
                .insert(call.span_start);
        }
        for end in &call.documents {
            if let Some(document) = reader.document_at(end) {
                consumers.push((
                    call.file.clone(),
                    ExecutedDocument {
                        line: call.line,
                        document,
                    },
                ));
            }
        }
    }
    debug!(
        "GraphQL document calls: {} call(s) read, {} module(s) parsed for their documents",
        calls.len(),
        reader.documents.len()
    );
    (sites, DocumentSiteConsumers { sites: consumers })
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
                    rows.push(site_consumer(
                        key,
                        &site_file,
                        executed.line,
                        operation,
                        document.field_types.get(field).cloned(),
                    ));
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
/// its root fields are the basis the schema catalogue judges. `operation` is
/// the operation the row's field is selected in; `declared` is where the
/// document's declaration states this field's result type, when it does.
fn site_consumer(
    key: OperationKey,
    file: &Path,
    line: u32,
    operation: &DocumentOperation,
    declared: Option<DeclaredFieldType>,
) -> GraphqlOp {
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
        declared_result_type: declared,
        operation: Some(operation.clone()),
        // Read once a locate has joined (carrick#1760).
        located_field_type: None,
        // Set by attribution, which runs after these rows are placed.
        schema_binding: None,
        undeclared_selection: None,
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

/// Reads document declarations for the value resolver's pass
/// ([`crate::request_summary::DocumentLookup`]), caching every module it
/// parses: one generated document module is read once however many files
/// import it.
struct DocumentReader<'a> {
    source_map: Lrc<SourceMap>,
    handler: Handler,
    workspace: Option<&'a WorkspaceIndex>,
    /// Module (canonical) → the documents it binds to a name at module scope.
    documents: HashMap<PathBuf, HashMap<String, DeclaredDocument>>,
}

impl<'a> DocumentReader<'a> {
    fn new(workspace: Option<&'a WorkspaceIndex>) -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        Self {
            source_map,
            handler,
            workspace,
            documents: HashMap::new(),
        }
    }

    fn module_documents(&mut self, file: &Path) -> &HashMap<String, DeclaredDocument> {
        let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        if !self.documents.contains_key(&file) {
            // A declaration file binds no value, so it holds no document.
            let declares_values = !file.to_string_lossy().ends_with(".d.ts");
            let documents = declares_values
                .then(|| parse_file(&file, &self.source_map, &self.handler))
                .flatten()
                .map(|module| document_declarations(&module, &self.source_map, &file))
                .unwrap_or_default();
            self.documents.insert(file.clone(), documents);
        }
        &self.documents[&file]
    }

    /// What the document a call's argument names states, when it can be
    /// read: a declaration's documents, or a document file's text.
    fn document_at(
        &mut self,
        end: &crate::request_summary::DocumentEnd,
    ) -> Option<DeclaredDocument> {
        use crate::request_summary::DocumentEnd;
        match end {
            DocumentEnd::Declared { file, symbol, .. } => {
                self.module_documents(file).get(symbol).cloned()
            }
            DocumentEnd::File(path) => {
                let path = path.as_ref()?;
                let text = std::fs::read_to_string(path).ok()?;
                Some(DeclaredDocument {
                    file: path.canonicalize().unwrap_or_else(|_| path.clone()),
                    line: 1,
                    operations: text_operations_with_lines(&text)
                        .into_iter()
                        .map(|(operation, _)| operation)
                        .collect(),
                    // A document file states no TypeScript type.
                    field_types: HashMap::new(),
                })
            }
        }
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
}

impl crate::request_summary::DocumentLookup for DocumentReader<'_> {
    fn is_document(&mut self, file: &Path, _line: u32, symbol: &str) -> bool {
        // Keyed by the name: the row's line is where the name is declared,
        // and a document's text may start on a later one.
        self.module_documents(file).contains_key(symbol)
    }

    fn document_file(&mut self, importer: &Path, specifier: &str) -> Option<PathBuf> {
        self.resolve_module(importer, specifier)
    }
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
    let object_types = module_object_types(module);
    for decl in module_scope_decls(module) {
        let Decl::Var(var) = decl else {
            continue;
        };
        for declarator in &var.decls {
            if let (Pat::Ident(ident), Some(init)) = (&declarator.name, declarator.init.as_deref())
                && is_document_expression(init)
            {
                let expression = unwrap_expression(init);
                let operations = expression_operations(expression);
                let field_types =
                    declared_field_types(declarator, &operations, &object_types, source_map, file);
                documents.insert(
                    ident.id.sym.to_string(),
                    DeclaredDocument {
                        file: file.to_path_buf(),
                        line: source_map.lookup_char_pos(expression.span().lo).line as u32,
                        operations,
                        field_types,
                    },
                );
            }
        }
    }
    documents
}

/// The object types a module declares at module scope, by name: a
/// non-generic `type X = { ... }` and an `interface X { ... }`, each with its
/// own members (an interface's `extends` is not followed).
fn module_object_types(module: &Module) -> HashMap<String, &[TsTypeElement]> {
    let mut types = HashMap::new();
    for decl in module_scope_decls(module) {
        match decl {
            Decl::TsTypeAlias(alias) if alias.type_params.is_none() => {
                if let TsType::TsTypeLit(literal) = unwrap_type(&alias.type_ann) {
                    types.insert(alias.id.sym.to_string(), literal.members.as_slice());
                }
            }
            Decl::TsInterface(interface) if interface.type_params.is_none() => {
                types.insert(interface.id.sym.to_string(), interface.body.body.as_slice());
            }
            _ => {}
        }
    }
    types
}

/// Where a document declaration states each root field's result type
/// (carrick#1761), keyed by root field name.
///
/// The declaration's type is the one it is annotated with or asserted to
/// (the outermost `as`, `satisfies` or `<T>`). Of that type's arguments, the
/// result type is the one that names an object type this module declares
/// whose properties include the response key of every root field the
/// operation selects. That match is the evidence: no type name, library or
/// argument position is assumed. Each root field's entry is the span of its
/// property's type.
///
/// Nothing is read when the evidence is not exactly one way: a document with
/// more or fewer than one operation, a type with no such argument or with two
/// of them (a variables type that happens to declare the same keys), or a
/// field the operation selects under two response keys.
fn declared_field_types(
    declarator: &VarDeclarator,
    operations: &[DocumentOperation],
    object_types: &HashMap<String, &[TsTypeElement]>,
    source_map: &Lrc<SourceMap>,
    file: &Path,
) -> HashMap<String, DeclaredFieldType> {
    let mut field_types = HashMap::new();
    let [operation] = operations else {
        return field_types;
    };
    if operation.response_keys.is_empty() {
        return field_types;
    }
    let Some(TsType::TsTypeRef(declared)) = declared_document_type(declarator).map(unwrap_type)
    else {
        return field_types;
    };
    let Some(arguments) = &declared.type_params else {
        return field_types;
    };
    let candidates: Vec<HashMap<String, &TsType>> = arguments
        .params
        .iter()
        .filter_map(|argument| object_types.get(bare_type_name(unwrap_type(argument))?))
        .map(|members| property_types(members))
        .filter(|properties| {
            operation
                .response_keys
                .iter()
                .all(|key| properties.contains_key(key))
        })
        .collect();
    let [properties] = candidates.as_slice() else {
        return field_types;
    };
    for (field, key) in operation.fields.iter().zip(&operation.response_keys) {
        if operation
            .fields
            .iter()
            .filter(|other| *other == field)
            .count()
            > 1
        {
            continue;
        }
        if let Some(field_type) = field_type_at(properties[key], source_map, file) {
            field_types.insert(field.clone(), field_type);
        }
    }
    field_types
}

/// Where `ty` is written in `file`, in the scanner's own span units.
fn field_type_at(
    ty: &TsType,
    source_map: &Lrc<SourceMap>,
    file: &Path,
) -> Option<DeclaredFieldType> {
    let span = ty.span();
    let offset = |pos| {
        source_map
            .lookup_byte_offset(pos)
            .pos
            .0
            .checked_add(SWC_SPAN_BASE)
    };
    Some(DeclaredFieldType {
        file: file.to_path_buf(),
        line: source_map.lookup_char_pos(span.lo).line as u32,
        lo: offset(span.lo)?,
        hi: offset(span.hi)?,
    })
}

/// Read the result type the model located for each consumer row against the
/// operation the row's field is selected in (carrick#1760). Returns how many
/// rows it set a field-level type on.
///
/// A row is keyed by one root field, and its type is that field's payload.
/// The model sometimes locates the result type of the row's whole operation
/// instead: an object with one property per root field
/// (`{ __typename: 'Query', invoice?: { ... } | null, settings?: { ... } }`).
/// Bundled as it is, that wrapper is served as the field's type, and the
/// judge compares it with the producer's field-level return. When the located
/// symbol names an object type its module declares
/// ([`operation_field_type`] says when), the row's type is that type's
/// property under the row's response key, and
/// [`GraphqlOp::located_field_type`] records where it is written. The type
/// sidecar reads it there, as it reads a declared field type (carrick#1761).
///
/// The rule reads the located type and the document, never the type's name,
/// so no codegen naming convention is assumed. A type it cannot read (an
/// intersection, a generic, a name the module re-exports from elsewhere) or
/// that is not the operation's result keeps the symbol path, unchanged.
///
/// The located specifier resolves from the row's file through `modules`,
/// the way the symbol path resolves it.
pub fn read_located_result_types(
    graphql: &mut GraphqlExtraction,
    modules: &WorkspaceIndex,
) -> usize {
    let source_map: Lrc<SourceMap> = Default::default();
    let handler =
        Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
    let mut parsed: HashMap<PathBuf, Option<Module>> = HashMap::new();
    let mut read = 0;
    for op in &mut graphql.consumers {
        let (Some(symbol), Some(operation), Some(field)) = (
            op.consumer_located_type_symbol.as_deref(),
            op.operation.as_ref(),
            op.key.graphql_field(),
        ) else {
            continue;
        };
        let module_file = match op.consumer_located_type_source.as_deref() {
            Some(source) => PathBuf::from(
                crate::agents::file_orchestrator::FileOrchestrator::resolve_import_path(
                    &op.file_path.to_string_lossy(),
                    source,
                    modules,
                ),
            ),
            None => op.file_path.clone(),
        };
        let Ok(module_file) = module_file.canonicalize() else {
            continue;
        };
        let module = parsed
            .entry(module_file.clone())
            .or_insert_with(|| parse_file(&module_file, &source_map, &handler));
        let Some(module) = module.as_ref() else {
            continue;
        };
        let located =
            operation_field_type(module, symbol, field, operation, &source_map, &module_file);
        if located.is_some() {
            read += 1;
            debug!(
                op = %op.key.canonical(),
                file = %op.file_path.display(),
                symbol,
                "located GraphQL result type is the operation's; the row reads its field's property"
            );
        }
        op.located_field_type = located;
    }
    read
}

/// Where the object type `symbol` that `module` declares states the result
/// type of `field`, when that object type is the result of the whole
/// `operation` (carrick#1760); `None` otherwise.
///
/// It is the operation's result when its members are a property under the
/// response key of every root field the operation selects, and nothing else
/// but `__typename`. Every root field is required because an operation's
/// result carries each one. Nothing else is allowed because a field's payload
/// that happens to carry a member named like the field (a `folder` with its
/// parent `folder`) also carries the field's own fields, and none of those is
/// a root field. `__typename` says nothing either way: the server answers it
/// on every object. A field the operation selects under two response keys
/// has no one key, and reads nothing.
fn operation_field_type(
    module: &Module,
    symbol: &str,
    field: &str,
    operation: &DocumentOperation,
    source_map: &Lrc<SourceMap>,
    file: &Path,
) -> Option<DeclaredFieldType> {
    let mut positions = operation
        .fields
        .iter()
        .enumerate()
        .filter(|(_, selected)| *selected == field)
        .map(|(index, _)| index);
    let (Some(index), None) = (positions.next(), positions.next()) else {
        return None;
    };
    let key = operation.response_keys.get(index)?;
    let object_types = module_object_types(module);
    let members = *object_types.get(symbol)?;
    let only_root_keys = members.iter().all(|member| {
        property_key(member).is_some_and(|(name, _)| {
            name == "__typename" || operation.response_keys.contains(&name)
        })
    });
    let properties = property_types(members);
    let every_root_key = operation
        .response_keys
        .iter()
        .all(|key| properties.contains_key(key));
    if !(only_root_keys && every_root_key) {
        return None;
    }
    field_type_at(properties.get(key)?, source_map, file)
}

/// The type a variable declaration states for its value: its annotation, or
/// the outermost type assertion around its initialiser.
fn declared_document_type(declarator: &VarDeclarator) -> Option<&TsType> {
    if let Pat::Ident(binding) = &declarator.name
        && let Some(annotation) = &binding.type_ann
    {
        return Some(&annotation.type_ann);
    }
    let mut expr = declarator.init.as_deref()?;
    loop {
        match expr {
            Expr::Paren(inner) => expr = &inner.expr,
            Expr::TsAs(assertion) => return Some(&assertion.type_ann),
            Expr::TsSatisfies(assertion) => return Some(&assertion.type_ann),
            Expr::TsTypeAssertion(assertion) => return Some(&assertion.type_ann),
            _ => return None,
        }
    }
}

/// `X` for an unqualified, non-generic reference to `X`.
fn bare_type_name(ty: &TsType) -> Option<&str> {
    match ty {
        TsType::TsTypeRef(TsTypeRef {
            type_name: TsEntityName::Ident(ident),
            type_params: None,
            ..
        }) => Some(ident.sym.as_ref()),
        _ => None,
    }
}

/// A type with its parentheses taken off.
fn unwrap_type(ty: &TsType) -> &TsType {
    match ty {
        TsType::TsParenthesizedType(inner) => unwrap_type(&inner.type_ann),
        other => other,
    }
}

/// Property key → declared type, for the plain properties among `members`
/// (an identifier or string key, not computed, with a type).
fn property_types(members: &[TsTypeElement]) -> HashMap<String, &TsType> {
    let mut properties = HashMap::new();
    for (key, property) in members.iter().filter_map(property_key) {
        if let Some(annotation) = &property.type_ann {
            properties.entry(key).or_insert(&*annotation.type_ann);
        }
    }
    properties
}

/// The key of a plain property member (an identifier or string key, not
/// computed), with the member.
fn property_key(member: &TsTypeElement) -> Option<(String, &TsPropertySignature)> {
    let TsTypeElement::TsPropertySignature(property) = member else {
        return None;
    };
    if property.computed {
        return None;
    }
    let key = match &*property.key {
        Expr::Ident(ident) => ident.sym.to_string(),
        Expr::Lit(Lit::Str(s)) => s.value.to_string_lossy().into_owned(),
        _ => return None,
    };
    Some((key, property))
}

/// The operations a document expression states: read off the graphql-js AST
/// object literal, or parsed from the text of a template or string.
fn expression_operations(expr: &Expr) -> Vec<DocumentOperation> {
    match unwrap_expression(expr) {
        Expr::Object(object) => match document_text_property(object) {
            Some(text) => text_operations(&text),
            None => object_operations(object),
        },
        Expr::TaggedTpl(tagged) => text_operations(&template_text(&tagged.tpl)),
        Expr::Call(call) if call.args.len() == 1 && call.args[0].spread.is_none() => {
            match unwrap_expression(&call.args[0].expr) {
                Expr::Tpl(tpl) => text_operations(&template_text(tpl)),
                Expr::Lit(Lit::Str(s)) => text_operations(&s.value.to_string_lossy()),
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
        Expr::Lit(Lit::Str(s)) => s.value.as_str(),
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
    let definitions = object_elements(document, "definitions");
    // A compiled document carries the fragments its operations spread.
    let fragments: HashMap<&str, &ObjectLit> = definitions
        .iter()
        .filter(|definition| string_property(definition, "kind") == Some("FragmentDefinition"))
        .filter_map(|definition| Some((name_value(definition, "name")?, *definition)))
        .collect();
    definitions
        .iter()
        .filter(|definition| string_property(definition, "kind") == Some("OperationDefinition"))
        .filter_map(|definition| {
            let kind = match string_property(definition, "operation")? {
                "query" => GraphqlOperationKind::Query,
                "mutation" => GraphqlOperationKind::Mutation,
                "subscription" => GraphqlOperationKind::Subscription,
                _ => return None,
            };
            let mut fields = Vec::new();
            let mut response_keys = Vec::new();
            let mut selections = Vec::new();
            if let Some(Expr::Object(selection_set)) = property(definition, "selectionSet") {
                for selection in object_elements(selection_set, "selections") {
                    if string_property(selection, "kind") != Some("Field") {
                        continue;
                    }
                    let Some(name) = name_value(selection, "name") else {
                        continue;
                    };
                    if name.starts_with("__") {
                        continue;
                    }
                    fields.push(name.to_string());
                    response_keys.push(name_value(selection, "alias").unwrap_or(name).to_string());
                    selections.push(object_selection_set(selection, &fragments, &mut Vec::new()));
                }
            }
            Some(DocumentOperation {
                kind,
                name: name_value(definition, "name").map(str::to_string),
                fields,
                response_keys,
                selections,
            })
        })
        .collect()
}

/// What the graphql-js node `node` (a field, an inline fragment or a fragment
/// definition) selects under its `selectionSet`, written as an object
/// literal: the compiled form of [`crate::graphql_selection::read_selection_set`].
/// A spread of a fragment the document does not carry is not read, and a
/// fragment that spreads itself is read once.
fn object_selection_set<'o>(
    node: &'o ObjectLit,
    fragments: &HashMap<&'o str, &'o ObjectLit>,
    expanding: &mut Vec<&'o str>,
) -> crate::graphql_selection::SelectionSet {
    use crate::graphql_selection::{SelectedField, SelectionSet};
    let mut out = SelectionSet::default();
    let Some(Expr::Object(selection_set)) = property(node, "selectionSet") else {
        return out;
    };
    let condition = |node: &'o ObjectLit| match property(node, "typeCondition") {
        Some(Expr::Object(named)) => name_value(named, "name").map(str::to_string),
        _ => None,
    };
    for selection in object_elements(selection_set, "selections") {
        match string_property(selection, "kind") {
            Some("Field") => {
                let Some(name) = name_value(selection, "name") else {
                    continue;
                };
                out.fields.push(SelectedField {
                    name: name.to_string(),
                    selection: object_selection_set(selection, fragments, expanding),
                });
            }
            Some("InlineFragment") => out.on_types.push((
                condition(selection),
                object_selection_set(selection, fragments, expanding),
            )),
            Some("FragmentSpread") => {
                let Some(name) = name_value(selection, "name") else {
                    continue;
                };
                let Some(fragment) = fragments.get(name) else {
                    continue;
                };
                if expanding.contains(&name) {
                    continue;
                }
                expanding.push(name);
                let nested = object_selection_set(fragment, fragments, expanding);
                expanding.pop();
                out.on_types.push((condition(fragment), nested));
            }
            _ => {}
        }
    }
    out
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
    use graphql_parser::query::Definition;
    let Ok(document) = graphql_parser::parse_query::<String>(text) else {
        return Vec::new();
    };
    let fragments = crate::graphql_selection::fragments_of(&document);
    let mut operations = Vec::new();
    for definition in &document.definitions {
        let Definition::Operation(operation) = definition else {
            continue;
        };
        let (operation, roots) = parsed_operation(operation, &fragments);
        let lines = roots
            .iter()
            .map(|field| field.position.line as u32)
            .collect();
        operations.push((operation, lines));
    }
    operations
}

/// The operation a parsed operation definition states, with the node of
/// each of its root fields in the order of `fields`. An anonymous selection
/// set is a query; fragment spreads and introspection fields at the root are
/// left out, because they name no root field a row can be keyed by. Each root
/// field's selection reads its spreads through `fragments`, the document's
/// own fragment definitions.
pub(crate) fn parsed_operation<'a, 'd>(
    definition: &'d graphql_parser::query::OperationDefinition<'a, String>,
    fragments: &crate::graphql_selection::Fragments<'_>,
) -> (
    DocumentOperation,
    Vec<&'d graphql_parser::query::Field<'a, String>>,
) {
    use graphql_parser::query::{OperationDefinition, Selection};
    let (kind, name, selection_set) = match definition {
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
    let roots: Vec<_> = selection_set
        .items
        .iter()
        .filter_map(|selection| match selection {
            Selection::Field(field) if !field.name.starts_with("__") => Some(field),
            _ => None,
        })
        .collect();
    let operation = DocumentOperation {
        kind,
        name,
        fields: roots.iter().map(|field| field.name.clone()).collect(),
        response_keys: roots
            .iter()
            .map(|field| field.alias.clone().unwrap_or_else(|| field.name.clone()))
            .collect(),
        selections: roots
            .iter()
            .map(|field| {
                crate::graphql_selection::read_selection_set(&field.selection_set, fragments)
            })
            .collect(),
    };
    (operation, roots)
}

/// Is `expr` a GraphQL document, read from its shape alone?
///
/// - an object literal whose `kind` is the string `"Document"`, the graphql-js
///   AST node every generated typed document is written as;
/// - an object literal with exactly one property holding a string or template
///   whose text parses as an executable document (a generated module's
///   `{ id, op, query: \`query x { ... }\` }`; the property's name is not read);
/// - a tagged template, or a call with one template or string argument, whose
///   text parses as an executable document (an operation or a fragment).
///
/// Type assertions and parentheses around either are looked through.
pub fn is_document_expression(expr: &Expr) -> bool {
    match unwrap_expression(expr) {
        Expr::Object(object) => {
            document_text_property(object).is_some()
                || object.props.iter().any(|prop| {
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
                })
        }
        Expr::TaggedTpl(tagged) => is_executable_document(&template_text(&tagged.tpl)),
        Expr::Call(call) if call.args.len() == 1 && call.args[0].spread.is_none() => {
            match unwrap_expression(&call.args[0].expr) {
                Expr::Tpl(tpl) => is_executable_document(&template_text(tpl)),
                Expr::Lit(swc_ecma_ast::Lit::Str(s)) => {
                    is_executable_document(&s.value.to_string_lossy())
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// The document text an object literal carries in a property: the one
/// string or template property whose text parses as an executable document.
/// Two such properties are ambiguous, so neither is read.
fn document_text_property(object: &ObjectLit) -> Option<String> {
    let mut found = object.props.iter().filter_map(|prop| {
        let PropOrSpread::Prop(prop) = prop else {
            return None;
        };
        let Prop::KeyValue(pair) = &**prop else {
            return None;
        };
        let text = match unwrap_expression(&pair.value) {
            Expr::Tpl(tpl) => template_text(tpl),
            Expr::Lit(swc_ecma_ast::Lit::Str(s)) => s.value.to_string_lossy().into_owned(),
            _ => return None,
        };
        is_executable_document(&text).then_some(text)
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
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
                .map(|cooked| cooked.to_string_lossy().into_owned())
                .unwrap_or_else(|| quasi.raw.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does `text` parse as an executable GraphQL document with at least one
/// named operation keyword or fragment? The bare `{ ... }` shorthand is not
/// enough on its own: plenty of non-GraphQL text parses as one.
pub(crate) fn is_executable_document(text: &str) -> bool {
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
pub(crate) fn is_graphql_document_file(specifier: &str) -> bool {
    specifier.ends_with(".graphql") || specifier.ends_with(".gql")
}

fn is_graphql_document_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "graphql" || ext == "gql")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::file_analyzer_agent::DataCallResult;
    use crate::request_summary::fixture_walk::{self, Walked};
    use std::collections::BTreeMap;

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

    /// `tests/fixtures/graphql-document-reader` with `files` written in it,
    /// read by discovery with the compiler's rows (carrick#3504 S9: the
    /// pass reads no name without them). `None` without node or the built
    /// sidecar, outside CI.
    fn walk(files: &[(&str, &str)]) -> Option<Walked> {
        fixture_walk::walked("graphql-document-reader", ".", "web", files, true)
    }

    /// The key the service lists `rel` under.
    fn key(walked: &Walked, rel: &str) -> String {
        walked.path(rel).to_string_lossy().into_owned()
    }

    fn suppress(
        file_results: &mut HashMap<String, FileAnalysisResult>,
        walked: &Walked,
        workspace: Option<&WorkspaceIndex>,
    ) -> DocumentSiteDrops {
        let (sites, _) = read_document_sites(&walked.inputs, &[], workspace);
        suppress_document_site_http_rows(file_results, &sites)
    }

    fn consumers(walked: &Walked) -> DocumentSiteConsumers {
        read_document_sites(&walked.inputs, &[], None).1
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
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", ORDERS_PAGE),
        ]) else {
            return;
        };
        let page = key(&walked, "src/pages/OrdersPage.tsx");

        let mut results = HashMap::from([(
            page.clone(),
            result_with(vec![
                row_at(ORDERS_PAGE, "useQuery(OrdersDocument", "POST", "/graphql"),
                row_at(ORDERS_PAGE, "fetch(", "GET", "/api/orders/export"),
            ]),
        )]);
        let drops = suppress(&mut results, &walked, None);

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
    fn a_row_at_a_call_passing_a_document_under_an_object_key_is_dropped() {
        // carrick#3504 S9: the argument's keys are read as the value
        // resolver lowers them.
        let page = r#"import { useQuery } from "@example/gql-client";
import { OrdersDocument } from "../generated/documents";
export function OrdersPage() {
  return useQuery({ query: OrdersDocument, variables: { first: 10 } });
}
"#;
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", page),
        ]) else {
            return;
        };
        let path = key(&walked, "src/pages/OrdersPage.tsx");

        let mut results = HashMap::from([(
            path.clone(),
            result_with(vec![row_at(page, "useQuery(", "POST", "/graphql")]),
        )]);
        let drops = suppress(&mut results, &walked, None);

        assert!(targets(&results, &path).is_empty());
        assert_eq!(drops.document_argument, 1);
    }

    #[test]
    fn a_row_inside_a_wrapper_every_caller_hands_a_document_is_dropped() {
        // The wrapper's call holds its parameter; its one caller hands it a
        // document, so both calls execute it (carrick#3504 S9).
        let source = r#"import { request } from "@example/gql-client";
import { OrdersDocument } from "./generated/documents";
function run(doc: unknown) {
  return request({ query: doc });
}
export const loadOrders = () => run(OrdersDocument);
"#;
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/orders.ts", source),
        ]) else {
            return;
        };
        let path = key(&walked, "src/orders.ts");

        let mut results = HashMap::from([(
            path.clone(),
            result_with(vec![
                row_at(source, "request({", "POST", "/graphql"),
                row_at(source, "run(OrdersDocument", "POST", "/graphql"),
            ]),
        )]);
        let drops = suppress(&mut results, &walked, None);

        assert!(targets(&results, &path).is_empty());
        assert_eq!(drops.document_argument, 2);
    }

    #[test]
    fn a_document_import_missing_from_disk_is_dropped_when_its_callee_executes_documents() {
        // The generated module behind the second page is gitignored. The same
        // hook passes a resolved document on the first page, so its sites are
        // GraphQL executions. No GraphQL row for the missing document is
        // consulted: an external or unresolved document keeps its twin
        // suppressed all the same.
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", ORDERS_PAGE),
            ("src/pages/InvoicesPage.tsx", INVOICES_PAGE),
        ]) else {
            return;
        };
        let orders = key(&walked, "src/pages/OrdersPage.tsx");
        let invoices = key(&walked, "src/pages/InvoicesPage.tsx");

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
        let drops = suppress(&mut results, &walked, None);

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
        let Some(walked) = walk(&[("src/pages/InvoicesPage.tsx", INVOICES_PAGE)]) else {
            return;
        };
        let invoices = key(&walked, "src/pages/InvoicesPage.tsx");

        let mut results = HashMap::from([(
            invoices.clone(),
            result_with(vec![row_at(
                INVOICES_PAGE,
                "useQuery(InvoicesDocument",
                "POST",
                "/graphql",
            )]),
        )]);
        let drops = suppress(&mut results, &walked, None);

        assert_eq!(targets(&results, &invoices), vec!["POST /graphql"]);
        assert_eq!(drops.total(), 0);
    }

    #[test]
    fn a_known_executor_passed_a_non_document_keeps_its_row() {
        let reports = r#"import { useQuery } from "@example/gql-client";
import { pageSize } from "../generated/documents";
const reportOptions = { url: "/api/reports", size: pageSize };
export function ReportsPage() {
  return useQuery(reportOptions, pageSize);
}
"#;
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", ORDERS_PAGE),
            ("src/pages/ReportsPage.tsx", reports),
        ]) else {
            return;
        };
        let orders = key(&walked, "src/pages/OrdersPage.tsx");
        let reports_path = key(&walked, "src/pages/ReportsPage.tsx");

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
        suppress(&mut results, &walked, None);

        assert_eq!(targets(&results, &reports_path), vec!["GET /api/reports"]);
    }

    #[test]
    fn a_client_declared_in_one_module_is_one_executor_across_its_importers() {
        // The client is declared where it executes a resolved document, and
        // imported where it is handed one whose module is not on disk. Both
        // sites must name the same callee, however the scan spelled the path
        // (a temp dir is behind a symlink on some systems).
        let client = r#"import { createClient } from "@example/gql-client";
import { OrdersDocument } from "./generated/documents";
export const api = createClient();
// Requête initiale — commandes
export const loadOrders = () => api.query(OrdersDocument);
"#;
        let invoices = r#"import { api } from "./client";
import { InvoicesDocument } from "./generated/vendor";
// Factures — chargement
export const loadInvoices = () => api.query(InvoicesDocument);
"#;
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/client.ts", client),
            ("src/invoices.ts", invoices),
        ]) else {
            return;
        };
        let client_path = key(&walked, "src/client.ts");
        let invoices_path = key(&walked, "src/invoices.ts");

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
        let drops = suppress(&mut results, &walked, None);

        assert!(targets(&results, &invoices_path).is_empty());
        assert_eq!(drops.document_executor, 1);
    }

    #[test]
    fn a_same_file_document_template_is_recognised_by_its_parse_not_its_tag() {
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
        let Some(walked) = walk(&[("src/order.ts", source)]) else {
            return;
        };
        let file = key(&walked, "src/order.ts");

        let mut results = HashMap::from([(
            file.clone(),
            result_with(vec![
                row_at(source, "client.request(NOT_A_DOCUMENT", "POST", "/styles"),
                row_at(source, "client.request(ORDER_QUERY", "POST", "/graphql"),
            ]),
        )]);
        suppress(&mut results, &walked, None);

        assert_eq!(targets(&results, &file), vec!["POST /styles"]);
    }

    #[test]
    fn an_aliased_document_import_resolves_through_the_repo_config() {
        let source = ORDERS_PAGE.replace("../generated/documents", "@/generated/documents");
        let Some(walked) = walk(&[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
            ),
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", &source),
        ]) else {
            return;
        };
        let root = walked.repo.as_path();
        let page = key(&walked, "src/pages/OrdersPage.tsx");

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
        let drops = suppress(&mut results, &walked, Some(&workspace));

        assert!(targets(&results, &page).is_empty());
        assert_eq!(drops.document_argument, 1);
    }

    #[test]
    fn a_row_already_keyed_to_its_operation_is_not_a_twin() {
        let Some(walked) = walk(&[
            ("src/generated/documents.ts", GENERATED_DOCUMENTS),
            ("src/pages/OrdersPage.tsx", ORDERS_PAGE),
        ]) else {
            return;
        };
        let page = key(&walked, "src/pages/OrdersPage.tsx");

        let mut results = HashMap::from([(
            page.clone(),
            result_with(vec![row_at(
                ORDERS_PAGE,
                "useQuery(OrdersDocument",
                "POST",
                "graphql|query|orders",
            )]),
        )]);
        let drops = suppress(&mut results, &walked, None);

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
        let Some(walked) = walk(&[
            ("src/graphql/orders.graphql", ORDER_DOCUMENTS),
            ("src/generated/documents.ts", COMPILED_DOCUMENTS),
            ("src/pages/CheckoutPage.tsx", CHECKOUT_PAGE),
            ("src/pages/RetryButton.tsx", RETRY_PAGE),
        ]) else {
            return;
        };
        // Canonical, as the service walk lists files on a real checkout.
        let root = walked.repo.clone();
        let checkout = key(&walked, "src/pages/CheckoutPage.tsx");
        let retry = key(&walked, "src/pages/RetryButton.tsx");
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

        let summary = consumers(&walked).apply(&mut extraction);

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
        let edited = ORDER_DOCUMENTS.replace(
            "  orders { id }\n",
            "  orders { id }\n  taxRates { code }\n",
        );
        let Some(walked) = walk(&[
            ("src/graphql/orders.graphql", &edited),
            ("src/generated/documents.ts", COMPILED_DOCUMENTS),
            ("src/pages/CheckoutPage.tsx", CHECKOUT_PAGE),
        ]) else {
            return;
        };
        let root = walked.repo.clone();
        let checkout = key(&walked, "src/pages/CheckoutPage.tsx");
        let files = vec![
            root.join("src/graphql/orders.graphql"),
            PathBuf::from(checkout),
        ];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        consumers(&walked).apply(&mut extraction);
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
        let source = r#"import { gql, request } from "graphql-request";
const ORDER = gql`
  query Order($id: ID!) { order(id: $id) { id } }
`;
export const loadOrder = (id: string) => request("/graphql", ORDER, { id });
"#;
        let Some(walked) = walk(&[("src/order.ts", source)]) else {
            return;
        };
        let root = walked.repo.clone();
        let file = walked.path("src/order.ts");
        let files = vec![file];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        let summary = consumers(&walked).apply(&mut extraction);

        assert_eq!(
            rows(&extraction, &root),
            vec![row("graphql|query|order", "src/order.ts", 3)]
        );
        assert_eq!(summary.declared_in_source, 1);
        assert_eq!(summary.site_rows, 0);
    }

    #[test]
    fn a_document_file_imported_whole_is_placed_at_its_call() {
        let source = r#"import { useQuery } from "@example/gql-client";
import CarriersQuery from "../graphql/carriers.graphql";
export function CarrierList() {
  return useQuery(CarriersQuery);
}
"#;
        let Some(walked) = walk(&[
            (
                "src/graphql/carriers.graphql",
                "query Carriers {\n  carriers { name }\n}\n",
            ),
            ("src/pages/CarrierList.tsx", source),
        ]) else {
            return;
        };
        let root = walked.repo.clone();
        let page = walked.path("src/pages/CarrierList.tsx");
        let files = vec![root.join("src/graphql/carriers.graphql"), page];

        let mut extraction = crate::graphql::scan_repo(&[root.join("src")], &[], &files);
        consumers(&walked).apply(&mut extraction);

        assert_eq!(
            rows(&extraction, &root),
            vec![row(
                "graphql|query|carriers",
                "src/pages/CarrierList.tsx",
                4
            )]
        );
    }

    /// Root field → the source text of the type `name`'s declaration states
    /// for it (carrick#1761), read through the span the pass records.
    fn declared_field_text(source: &str, name: &str) -> BTreeMap<String, String> {
        let file = Path::new("documents.ts");
        let (source_map, module) =
            crate::swc_scanner::parse_standalone_module(file, source).expect("module parses");
        let documents = document_declarations(&module, &source_map, file);
        documents[name]
            .field_types
            .iter()
            .map(|(field, declared)| {
                let lo = (declared.lo - SWC_SPAN_BASE) as usize;
                let hi = (declared.hi - SWC_SPAN_BASE) as usize;
                (field.clone(), source[lo..hi].to_string())
            })
            .collect()
    }

    fn texts(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items
            .iter()
            .map(|(field, text)| (field.to_string(), text.to_string()))
            .collect()
    }

    /// The operations `document_declarations` reads for `name` in `source`.
    fn declared_operations(source: &str, name: &str) -> Option<Vec<(String, Vec<String>)>> {
        let file = Path::new("documents.ts");
        let (source_map, module) =
            crate::swc_scanner::parse_standalone_module(file, source).expect("module parses");
        let documents = document_declarations(&module, &source_map, file);
        documents.get(name).map(|document| {
            document
                .operations
                .iter()
                .map(|operation| {
                    (
                        operation.name.clone().unwrap_or_default(),
                        operation.fields.clone(),
                    )
                })
                .collect()
        })
    }

    #[test]
    fn a_declaration_holding_document_text_in_a_property_is_a_document() {
        let source = r#"
export const currentUserQuery = { id: "a1", op: "query", query: `query currentUser { currentUser { id } }` };
export const plain = { id: "a2", note: "not a document", query: "select 1" };
export const twice = { first: "query one { a }", second: "query two { b }" };
"#;
        let operations = declared_operations(source, "currentUserQuery")
            .expect("the property text makes the declaration a document");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].1, vec!["currentUser".to_string()]);
        assert!(declared_operations(source, "plain").is_none());
        assert!(
            declared_operations(source, "twice").is_none(),
            "two document properties are ambiguous"
        );
    }

    #[test]
    fn a_declaration_states_each_root_fields_type_under_its_response_key() {
        // The graphql-js AST literal a generator writes, with an aliased root
        // field, below multi-byte prose so a character-for-byte slip shows.
        let source = r#"import type { TypedDocumentNode as DocumentNode } from "@example/typed-document";
// Requête générée — ne pas modifier
export type OrdersQueryVariables = { first: number };
export type OrdersQuery = { __typename: 'Query', orders: Array<{ __typename: 'Order', id: string }>, zones?: Array<{ code: string }> | null };
export const OrdersDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Orders"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"__typename"}},{"kind":"Field","name":{"kind":"Name","value":"orders"}},{"kind":"Field","alias":{"kind":"Name","value":"zones"},"name":{"kind":"Name","value":"shippingZones"}}]}}]} as unknown as DocumentNode<OrdersQuery, OrdersQueryVariables>;
"#;
        assert_eq!(
            declared_field_text(source, "OrdersDocument"),
            texts(&[
                ("orders", "Array<{ __typename: 'Order', id: string }>"),
                ("shippingZones", "Array<{ code: string }> | null"),
            ])
        );
    }

    #[test]
    fn an_interface_or_an_annotated_declaration_states_the_type_too() {
        let source = r#"import { gql } from "@example/gql";
interface InvoiceResult { invoice: { id: string } | null }
export const InvoiceDocument: DocumentNode<InvoiceResult, { id: string }> = gql`
  query Invoice($id: ID!) { invoice(id: $id) { id } }
`;
"#;
        assert_eq!(
            declared_field_text(source, "InvoiceDocument"),
            texts(&[("invoice", "{ id: string } | null")])
        );
    }

    #[test]
    fn a_declaration_whose_evidence_is_not_one_way_states_no_field_type() {
        let source = r#"import { gql } from "@example/gql";
import type { RemoteResult } from "./types";
type UserQuery = { user: { id: string } | null };
type UserQueryVariables = { user: string };
type TwoQuery = { invoice: { id: string }, a: { id: string }, b: { id: string }, total: number };

// The variables type declares the root field's key too: two candidates.
export const UserDocument = gql`query User($user: ID!) { user(id: $user) { id } }` as unknown as DocumentNode<UserQuery, UserQueryVariables>;
// No type arguments at all.
export const BareDocument = gql`query Bare { user(id: "1") { id } }` as unknown as DocumentNode;
// The result type is declared in another module.
export const RemoteDocument = gql`query Remote { user(id: "1") { id } }` as unknown as DocumentNode<RemoteResult, {}>;
// Two operations in one document.
export const PairDocument = gql`query A { user(id: "1") { id } } query B { user(id: "2") { id } }` as unknown as DocumentNode<UserQuery, {}>;
// One field selected under two response keys has no one property; the
// other field still reads.
export const TwoDocument = gql`query Two { a: invoice(id: 1) { id } b: invoice(id: 2) { id } total }` as unknown as DocumentNode<TwoQuery, {}>;
"#;
        for name in [
            "UserDocument",
            "BareDocument",
            "RemoteDocument",
            "PairDocument",
        ] {
            assert_eq!(
                declared_field_text(source, name),
                BTreeMap::new(),
                "{name} states no field type"
            );
        }
        assert_eq!(
            declared_field_text(source, "TwoDocument"),
            texts(&[("total", "number")])
        );
    }

    /// A declared field type that writes `unknown` or `any` (a JSON scalar the
    /// generator inlines) is still the field's payload, and is read like any
    /// other: the capture places its type node (carrick#1775).
    #[test]
    fn a_field_whose_declared_type_writes_unknown_or_any_is_read() {
        let source = r#"import { gql } from "@example/gql";
type LedgerQuery = { ledger?: { id: string, entries?: Array<{ value?: unknown | null }> | null } | null, raw: any, owner: { id: string } };
export const LedgerDocument = gql`query Ledger { ledger { id entries { value } } raw owner { id } }` as unknown as DocumentNode<LedgerQuery, {}>;
"#;
        assert_eq!(
            declared_field_text(source, "LedgerDocument"),
            texts(&[
                (
                    "ledger",
                    "{ id: string, entries?: Array<{ value?: unknown | null }> | null } | null"
                ),
                ("raw", "any"),
                ("owner", "{ id: string }"),
            ])
        );
    }

    /// Result types a model might locate for a consumer row (carrick#1760),
    /// under multi-byte prose so a byte-vs-character span slip cannot pass.
    const LOCATED_TYPES: &str = r#"// Généré — ne pas modifier
export type InvoicePageQuery = { __typename: 'Query', invoice?: { __typename: 'Invoice', id: string, total: number } | null, settings: { __typename: 'Settings', prefix: string } };
export interface CustomerData { customer: { id: string; name: string } | null }
export type MineQuery = { mine: Array<{ id: string }> };
export type NoteView = { id: string; body: string };
export type FolderView = { __typename: 'Folder', id: string, folder?: { id: string } | null };
export type ReceiptQuery = { __typename?: 'Query' } & { receipt?: { id: string } | null };
export type PairQuery = { first?: { id: string } | null, second?: { id: string } | null };
"#;

    /// The text the located `symbol` is read at for `field`, when the one
    /// operation `document` states executes; `None` when the row keeps the
    /// located symbol as it is.
    fn located_field_text(symbol: &str, document: &str, field: &str) -> Option<String> {
        let file = Path::new("types.ts");
        let (source_map, module) =
            crate::swc_scanner::parse_standalone_module(file, LOCATED_TYPES).expect("parses");
        let [operation]: [DocumentOperation; 1] =
            text_operations(document).try_into().expect("one operation");
        let located = operation_field_type(&module, symbol, field, &operation, &source_map, file)?;
        assert_eq!(located.file, file);
        let lo = (located.lo - SWC_SPAN_BASE) as usize;
        let hi = (located.hi - SWC_SPAN_BASE) as usize;
        Some(LOCATED_TYPES[lo..hi].to_string())
    }

    /// A compiled document's operations state what each root field selects,
    /// inline fragments and the fragments it carries read on their type
    /// condition, exactly as the same document's text does
    /// ([`crate::graphql_selection`]).
    #[test]
    fn a_compiled_document_states_its_selection_as_its_text_does() {
        let text = "query Depot { depot { code ... on Depot { city } ...Place } } \
                    fragment Place on Depot { city zone }";
        let compiled = r#"export const DepotDocument = {"kind":"Document","definitions":[{"kind":"OperationDefinition","operation":"query","name":{"kind":"Name","value":"Depot"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"depot"},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"code"}},{"kind":"InlineFragment","typeCondition":{"kind":"NamedType","name":{"kind":"Name","value":"Depot"}},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"city"}}]}},{"kind":"FragmentSpread","name":{"kind":"Name","value":"Place"}}]}}]}},{"kind":"FragmentDefinition","name":{"kind":"Name","value":"Place"},"typeCondition":{"kind":"NamedType","name":{"kind":"Name","value":"Depot"}},"selectionSet":{"kind":"SelectionSet","selections":[{"kind":"Field","name":{"kind":"Name","value":"city"}},{"kind":"Field","name":{"kind":"Name","value":"zone"}}]}}]} as unknown as DocumentNode;"#;
        let (_, module) =
            crate::swc_scanner::parse_standalone_module(Path::new("graphql.ts"), compiled)
                .expect("parses");
        let object = module
            .body
            .iter()
            .find_map(|item| match item {
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => match &export.decl {
                    Decl::Var(var) => match var.decls[0].init.as_deref().map(unwrap_expression) {
                        Some(Expr::Object(object)) => Some(object),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .expect("the document object");
        let operations = object_operations(object);
        assert_eq!(operations, text_operations(text));
        let [depot] = operations[0].selections.as_slice() else {
            panic!("one root field: {operations:?}");
        };
        assert_eq!(depot.fields.len(), 1, "{depot:?}");
        assert_eq!(
            depot
                .on_types
                .iter()
                .map(|(on, set)| (on.as_deref(), set.fields.len()))
                .collect::<Vec<_>>(),
            vec![(Some("Depot"), 1), (Some("Depot"), 2)]
        );
    }

    /// A located type that is the result of the row's whole operation, one
    /// property per root field, is read at the row's field: the property under
    /// its response key, the field's alias when it has one. A type alias and an
    /// interface read the same.
    #[test]
    fn a_located_operation_result_is_read_at_the_rows_field() {
        let document = "query InvoicePage { invoice(id: 1) { id total } settings { prefix } }";
        assert_eq!(
            located_field_text("InvoicePageQuery", document, "invoice").as_deref(),
            Some("{ __typename: 'Invoice', id: string, total: number } | null")
        );
        assert_eq!(
            located_field_text("InvoicePageQuery", document, "settings").as_deref(),
            Some("{ __typename: 'Settings', prefix: string }")
        );
        assert_eq!(
            located_field_text("CustomerData", "query { customer { id name } }", "customer")
                .as_deref(),
            Some("{ id: string; name: string } | null")
        );
        assert_eq!(
            located_field_text("MineQuery", "query { mine: invoices { id } }", "invoices")
                .as_deref(),
            Some("Array<{ id: string }>"),
            "the property is keyed by the field's alias"
        );
    }

    /// A located type that is the field's payload keeps the symbol path, even
    /// when it carries a member named like the field: its own fields are not
    /// root fields.
    #[test]
    fn a_located_payload_type_is_not_read_at_a_member() {
        assert_eq!(
            located_field_text("NoteView", "query { note { id body } }", "note"),
            None
        );
        assert_eq!(
            located_field_text(
                "FolderView",
                "query { folder { id folder { id } } }",
                "folder"
            ),
            None,
            "a payload whose parent is a field of the same name is not the operation"
        );
    }

    /// Nothing is read when the located type does not show it is the
    /// operation's result: a type the module states as an intersection, a type
    /// missing one of the operation's root fields, a field the operation
    /// selects twice, or a name the module does not declare.
    #[test]
    fn a_located_type_this_pass_cannot_read_keeps_the_symbol_path() {
        assert_eq!(
            located_field_text("ReceiptQuery", "query { receipt { id } }", "receipt"),
            None
        );
        assert_eq!(
            located_field_text(
                "InvoicePageQuery",
                "query { invoice(id: 1) { id } settings { prefix } customer { id } }",
                "invoice"
            ),
            None,
            "the type has no property for the customer root field"
        );
        assert_eq!(
            located_field_text(
                "PairQuery",
                "query { first: invoice(id: 1) { id } second: invoice(id: 2) { id } }",
                "invoice"
            ),
            None,
            "a field selected under two keys has no one property"
        );
        assert_eq!(
            located_field_text("Missing", "query { note { id } }", "note"),
            None
        );
    }

    /// The located symbol resolves the way the symbol path resolves it: through
    /// the specifier the model gave, from the file the row sits in, or in that
    /// file when there is none. Only a row with an operation and a located
    /// symbol is read.
    #[test]
    fn a_located_operation_result_resolves_from_the_rows_file() {
        let root = tempfile::tempdir().expect("tempdir");
        write(root.path(), "src/generated/types.ts", LOCATED_TYPES);
        let page = write(
            root.path(),
            "src/pages/Page.tsx",
            "type PageQuery = { order: { id: string } | null };\nexport const page = 1;\n",
        );
        let operation = |document: &str| {
            let [operation]: [DocumentOperation; 1] =
                text_operations(document).try_into().expect("one operation");
            operation
        };
        let row = |field: &str, symbol: &str, source: Option<&str>, document: Option<&str>| {
            let mut op = site_consumer(
                OperationKey::graphql(GraphqlOperationKind::Query, field),
                Path::new(&page),
                3,
                &operation(document.unwrap_or("query { unused }")),
                None,
            );
            op.operation = document.map(operation);
            op.consumer_located_type_symbol = Some(symbol.to_string());
            op.consumer_located_type_source = source.map(str::to_string);
            op
        };
        let mut graphql = GraphqlExtraction {
            consumers: vec![
                row(
                    "invoice",
                    "InvoicePageQuery",
                    Some("../generated/types"),
                    Some("query { invoice(id: 1) { id } settings { prefix } }"),
                ),
                row("order", "PageQuery", None, Some("query { order { id } }")),
                row(
                    "note",
                    "NoteView",
                    Some("../generated/types"),
                    Some("query { note { id body } }"),
                ),
                row("order", "PageQuery", None, None),
            ],
            ..Default::default()
        };
        let modules = WorkspaceIndex::build_with_aliases(root.path(), None);

        assert_eq!(read_located_result_types(&mut graphql, &modules), 2);
        let text = |op: &GraphqlOp| {
            op.located_field_type.as_ref().map(|located| {
                let source = std::fs::read_to_string(&located.file).expect("module");
                let lo = (located.lo - SWC_SPAN_BASE) as usize;
                let hi = (located.hi - SWC_SPAN_BASE) as usize;
                (
                    located
                        .file
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    source[lo..hi].to_string(),
                )
            })
        };
        assert_eq!(
            text(&graphql.consumers[0]),
            Some((
                "types.ts".to_string(),
                "{ __typename: 'Invoice', id: string, total: number } | null".to_string()
            ))
        );
        assert_eq!(
            text(&graphql.consumers[1]),
            Some(("Page.tsx".to_string(), "{ id: string } | null".to_string()))
        );
        assert_eq!(text(&graphql.consumers[2]), None, "a payload type is kept");
        assert_eq!(
            text(&graphql.consumers[3]),
            None,
            "no operation, nothing read"
        );
    }
}
