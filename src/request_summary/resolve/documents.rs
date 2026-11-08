//! The calls that execute a GraphQL document, read through the value
//! resolver (step S9 of the value-resolver record, carrick#3504).
//!
//! A call executes a document when a value one of its arguments may hold is
//! a declaration the GraphQL reader accepts as a document: at any position,
//! under any key of an object literal written there (to the lowering's depth
//! cap), on either side of a fallback, through a binding the module writes
//! (the binding table), a field its class's constructions fill, or what an
//! own factory returns. Each name is read at the declaration the compiler's
//! row names, as the text reader reads one ([`super::text_reader`]): an
//! import, a re-export (`export *`) and an alias are the compiler's to
//! resolve, never a second import walk.
//!
//! A document a call's own parameter holds is its callers': the caller that
//! hands it on is placed (it passes the document itself), and the call
//! inside the wrapper places nothing. That call still executes documents
//! when every value its callers fill the parameter with is one, which is
//! what the HTTP-twin drop reads ([`DocumentCall::through_callers`]).
//!
//! Two forms the compiler states nothing for are read from discovery's own
//! import table ([`FileIr::bindings`]): the default import of a `.graphql`
//! file (the file is the document), and an import whose module is not on
//! disk (a gitignored generated module). Which documents a declaration
//! holds is the GraphQL reader's ([`DocumentLookup`]).

use super::*;

/// What a document argument names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum DocumentEnd {
    /// A declaration the compiler's row names: the file it is written in,
    /// the 1-based line its name is declared on, and the name.
    Declared {
        file: PathBuf,
        line: u32,
        symbol: String,
    },
    /// A GraphQL document file imported whole, `None` when the import names
    /// no file on disk.
    File(Option<PathBuf>),
}

/// One call whose arguments hold a GraphQL document, or an import the
/// compiler finds no declaration for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentCall {
    /// The file, as the service's inputs key it.
    pub(crate) file: PathBuf,
    /// The call's span start in SWC's own numbering: the key every
    /// candidate and model row carries.
    pub(crate) span_start: u32,
    /// The call's 1-based line.
    pub(crate) line: u32,
    /// The documents the call's own arguments hold.
    pub(crate) documents: Vec<DocumentEnd>,
    /// The call holds no document of its own, but its arguments hold a
    /// parameter of the function it is written in, and every value the
    /// callers fill that parameter with is a document.
    pub(crate) through_callers: bool,
    /// An argument is an import the compiler finds no declaration for.
    pub(crate) unresolved_import: bool,
    /// The callee, as one key across every file of the service: the
    /// declaration its row names (a member call's: the declaration of the
    /// name the member is read on, with the member), else the import it is
    /// read through, as written. `None` for any other callee.
    pub(crate) callee: Option<(String, String)>,
}

/// What the GraphQL reader answers for the pass.
pub(crate) trait DocumentLookup {
    /// Whether the declaration of `symbol`, its name on `line` of `file`, is
    /// a GraphQL document.
    fn is_document(&mut self, file: &Path, line: u32, symbol: &str) -> bool;
    /// The GraphQL document file `specifier` names from `importer`, when it
    /// is on disk.
    fn document_file(&mut self, importer: &Path, specifier: &str) -> Option<PathBuf>;
}

/// What one value reached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Found {
    #[default]
    Other,
    Document(DocumentEnd),
    Unresolved,
}

struct Documents<'r, 'a> {
    resolver: &'r Resolver<'a>,
    lookup: &'r mut dyn DocumentLookup,
}

impl<'a> Follow<'a> for Documents<'_, 'a> {
    type Reached = Found;

    fn export(
        &mut self,
        file: &'a Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        _: Invoke<'a>,
    ) -> Option<Found> {
        let inputs = self.resolver.inputs_for(file);
        let row = self.resolver.row_at(file, lo, hi, kind);
        if let Some(row) = row
            && let (Some(decl), Some(line), Some(symbol)) =
                (row.decl_file(), row.decl_line(), row.symbol())
            && !decl.starts_with("lib:")
            && let Some(positions) = inputs.positions.as_ref()
            && self
                .lookup
                .is_document(&positions.path_of(decl), line, symbol)
        {
            return Some(Found::Document(DocumentEnd::Declared {
                file: positions.path_of(decl),
                line,
                symbol: symbol.to_string(),
            }));
        }
        if kind != Kind::Ident {
            return None;
        }
        let binding = import_at(inputs, file, lo, hi)?;
        if binding.export == "default"
            && crate::graphql_document_sites::is_graphql_document_file(&binding.specifier)
        {
            let path = self.lookup.document_file(file, &binding.specifier);
            return Some(Found::Document(DocumentEnd::File(path)));
        }
        import_unresolved(inputs, file, row).then_some(Found::Unresolved)
    }

    fn definition(&mut self, _: PathBuf, _: String, _: Invoke<'a>) -> Found {
        Found::Other
    }

    fn written(&mut self, _: &'a Path, _: &'a FnIr, _: Invoke<'a>, _: Option<Maker<'a>>) -> Found {
        Found::Other
    }
}

/// Whether the compiler names no declaration for a use of a name `file`
/// imports: no row, or a row naming the importing file itself (an import
/// whose module is not on disk names its own import line, with the error
/// type). A declaration whose own type is the error type is still one.
fn import_unresolved(inputs: &RequestSummaryInputs, file: &Path, row: Option<&Row>) -> bool {
    let Some(declared) = row.and_then(Row::decl_file) else {
        return true;
    };
    inputs
        .positions
        .as_ref()
        .is_some_and(|positions| positions.path_of(declared) == file)
}

/// The import binding read at `lo..hi` in `file`, from discovery's import
/// table.
fn import_at<'i>(
    inputs: &'i RequestSummaryInputs,
    file: &Path,
    lo: u32,
    hi: u32,
) -> Option<&'i ImportBinding> {
    let ir = inputs.files.get(file)?;
    let (name, _) = ir
        .import_uses
        .iter()
        .find(|(_, spans)| spans.contains(&(lo, hi)))?;
    ir.bindings.get(name).filter(|binding| !binding.namespace)
}

/// A call whose own arguments hold no document but a parameter, kept for
/// the fan-out to its callers.
struct Pending<'a> {
    at: (&'a Path, Option<&'a str>, u8),
    call: &'a CallIr,
    params: Vec<(u8, usize, Vec<String>)>,
    unresolved_import: bool,
}

/// Every call of the service whose arguments hold a GraphQL document, or
/// an import no declaration answers, in file order. Empty without the
/// compiler's rows: no name is read by its spelling.
pub(crate) fn document_calls(
    inputs: &RequestSummaryInputs,
    siblings: &[&RequestSummaryInputs],
    lookup: &mut dyn DocumentLookup,
) -> Vec<DocumentCall> {
    if inputs.positions.is_none() {
        tracing::debug!(
            "GraphQL document calls: no compiler rows for this service, so no call is read as \
             executing a document"
        );
        return Vec::new();
    }
    let resolver = Resolver::new(inputs, siblings);
    let mut follow = Documents {
        resolver: &resolver,
        lookup,
    };
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
    paths.sort();
    for path in paths {
        let ir = &inputs.files[path];
        let mut keys: Vec<&String> = ir.functions.keys().collect();
        keys.sort();
        for key in keys {
            read_body(
                &mut follow,
                (path, Some(key.as_str()), 0),
                &ir.functions[key],
                &mut out,
                &mut pending,
            );
        }
        read_body(
            &mut follow,
            (path, None, 0),
            &ir.module_level,
            &mut out,
            &mut pending,
        );
    }
    // A wrapper's parameter is read at its callers only in a service where
    // some call is seen executing a document.
    if out
        .iter()
        .any(|call: &DocumentCall| !call.documents.is_empty())
    {
        for wait in pending {
            let invoke = Invoke {
                file: wait.at.0,
                call: wait.call,
                construction: false,
            };
            let mut held = Vec::new();
            for (depth, index, keys) in &wait.params {
                held.extend(resolver.callers_pass(
                    &mut follow,
                    wait.at,
                    (*depth, *index, keys),
                    invoke,
                    &mut Vec::new(),
                ));
            }
            let through_callers = !held.is_empty()
                && held
                    .iter()
                    .all(|held| matches!(held, Held::Reached(Found::Document(_))));
            if through_callers || wait.unresolved_import {
                out.push(document_call(
                    &resolver,
                    wait.at.0,
                    wait.call,
                    Vec::new(),
                    through_callers,
                    wait.unresolved_import,
                ));
            }
        }
    } else {
        for wait in pending.into_iter().filter(|wait| wait.unresolved_import) {
            out.push(document_call(
                &resolver,
                wait.at.0,
                wait.call,
                Vec::new(),
                false,
                true,
            ));
        }
    }
    out.sort_by(|a, b| (&a.file, a.span_start).cmp(&(&b.file, b.span_start)));
    // The value resolver's step census (carrick#3504 S0), for this pass.
    if std::env::var_os("CARRICK_RESOLVER_CENSUS").is_some() {
        let census = resolver.census();
        crate::errln!(
            "carrick-resolver-census pass=graphql-documents files={} queries={} max_query_steps={} steps={} capped_queries={}",
            inputs.files.len(),
            census.queries,
            census.max_query_steps,
            census.steps,
            census.capped_queries
        );
    }
    out
}

/// Reads every call written in `body` and in the functions written inside
/// it. `at` is the file, the definition the body is in, and how many
/// functions deep inside it the body is.
fn read_body<'a>(
    follow: &mut Documents<'_, 'a>,
    at: (&'a Path, Option<&'a str>, u8),
    body: &'a FnIr,
    out: &mut Vec<DocumentCall>,
    pending: &mut Vec<Pending<'a>>,
) {
    for call in body.calls.iter().chain(&body.optional_calls) {
        if call.jsx {
            continue;
        }
        read_call(follow, at, call, out, pending);
    }
    for inner in body.nested.iter().chain(&body.detached) {
        read_body(
            follow,
            (at.0, at.1, at.2.saturating_add(1)),
            inner,
            out,
            pending,
        );
    }
}

fn read_call<'a>(
    follow: &mut Documents<'_, 'a>,
    at: (&'a Path, Option<&'a str>, u8),
    call: &'a CallIr,
    out: &mut Vec<DocumentCall>,
    pending: &mut Vec<Pending<'a>>,
) {
    let file = at.0;
    let invoke = Invoke {
        file,
        call,
        construction: false,
    };
    let mut leaves = Vec::new();
    for values in &call.arg_values {
        for value in values {
            leaves_of(value, &mut leaves);
        }
    }
    let mut documents = Vec::new();
    let mut params = Vec::new();
    let mut unresolved_import = false;
    for value in leaves {
        if let ValueAt::Param { depth, index, keys } = value {
            params.push((*depth, *index, keys.clone()));
            continue;
        }
        if !matches!(
            value,
            ValueAt::Read { .. } | ValueAt::Field { .. } | ValueAt::Returned { .. }
        ) {
            continue;
        }
        let resolver = follow.resolver;
        for held in resolver.read_value(follow, file, value, invoke, false) {
            match held {
                Held::Reached(Found::Document(end)) => {
                    if !documents.contains(&end) {
                        documents.push(end);
                    }
                }
                Held::Reached(Found::Unresolved) => unresolved_import = true,
                Held::Param(depth, index, keys) => params.push((depth, index, keys)),
                Held::Reached(Found::Other) | Held::Callback => {}
            }
        }
    }
    if !documents.is_empty() {
        out.push(document_call(
            follow.resolver,
            file,
            call,
            documents,
            false,
            unresolved_import,
        ));
    } else if !params.is_empty() || unresolved_import {
        pending.push(Pending {
            at,
            call,
            params,
            unresolved_import,
        });
    }
}

/// The values an argument holds, each key of an object literal written
/// there read through to its own values.
fn leaves_of<'v>(value: &'v ValueAt, out: &mut Vec<&'v ValueAt>) {
    match value {
        ValueAt::Object { fields, .. } => {
            for values in fields.values() {
                for value in values {
                    leaves_of(value, out);
                }
            }
        }
        other => out.push(other),
    }
}

fn document_call(
    resolver: &Resolver<'_>,
    file: &Path,
    call: &CallIr,
    documents: Vec<DocumentEnd>,
    through_callers: bool,
    unresolved_import: bool,
) -> DocumentCall {
    DocumentCall {
        file: file.to_path_buf(),
        span_start: call.site.span_start,
        line: call.site.line,
        documents,
        through_callers,
        unresolved_import,
        callee: callee_key(resolver, file, call),
    }
}

/// The callee of `call` as one key across the service's files
/// ([`DocumentCall::callee`]): a name (`useQuery`) or a member of a name
/// (`client.query`).
fn callee_key(resolver: &Resolver<'_>, file: &Path, call: &CallIr) -> Option<(String, String)> {
    let is_name = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    };
    let text = call.callee_text.as_str();
    let (root, member) = match text.split_once('.') {
        Some((root, member)) => (root, Some(member)),
        None => (text, None),
    };
    if !is_name(root) || member.is_some_and(|member| !is_name(member)) {
        return None;
    }
    let row = match member {
        None => resolver.row_at(file, call.site.lo, call.site.hi, Kind::Call),
        Some(_) => match call.receiver_spans.as_slice() {
            [(lo, hi, false)] => resolver.row_at(file, *lo, *hi, Kind::Ident),
            _ => return None,
        },
    };
    let suffix = member
        .map(|member| format!(".{member}"))
        .unwrap_or_default();
    let inputs = resolver.inputs_for(file);
    let imported = inputs.files.get(file).and_then(|ir| ir.bindings.get(root));
    if let Some(row) = row
        && let (Some(decl), Some(line), Some(symbol)) =
            (row.decl_file(), row.decl_line(), row.symbol())
        && !(imported.is_some() && import_unresolved(inputs, file, Some(row)))
    {
        return Some((decl.to_string(), format!("{line}:{symbol}{suffix}")));
    }
    let binding = imported?;
    Some((
        binding.specifier.clone(),
        format!("{}{suffix}", binding.export),
    ))
}
