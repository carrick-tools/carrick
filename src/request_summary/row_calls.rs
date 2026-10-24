//! The definition each call reaches, as the compiler's per-position rows say
//! (carrick#2413 slice D).
//!
//! Without rows, a call reaches what the call graph's resolution names
//! ([`CallSiteTargets`]): a member call is joined to a definition by its
//! name, and a receiver's type is read as written. With rows, each call has
//! a `call` row naming the declaration its callee resolves to, as the
//! compiler resolves it: the receiver's declared or inferred type, an
//! override, an overload, a re-export chain. That declaration joins the
//! definition the extractor keyed on what the declaration names
//! ([`FileIr::definition_lines`], [`DeclarationLines`]): the same file, the
//! line the function node or its holding declaration starts on, and a key
//! whose last part is the declared name (a named `export default` is
//! `default` to the compiler); an object entry joins the function its value
//! names.
//!
//! Each call is answered once, when the rows are bound ([`RowCalls`]).
//!
//! Two kinds of call stay on the call graph: a JSX element, which the rows
//! give no kind (carrick#2430), and a bare call of a function the same file
//! declares, whose binding SWC already names exactly.
//!
//! A call whose row says nothing is [`Reach::Unknown`], never a fallback to
//! the name join: the summary it sits in is not complete. The one exception
//! is a call through an import the compiler could not resolve: it knows
//! nothing of that callee, so the call reaches what the call graph names, as
//! without rows (carrick#2557).

use super::*;
use crate::position_rows::{FLAG_ANY, FLAG_ERROR_TYPE, Kind};
use crate::visitor::{FunctionDefinition, FunctionNodeType};

/// The declarations of a file no definition the extractor keys opens at,
/// read once per file.
#[derive(Debug, Default)]
pub(crate) struct DeclarationLines {
    /// Each function or method declared with no body, by its line, with its
    /// name: an overload's signature, which the declaration with a body
    /// after it implements. The compiler names an overloaded function by its
    /// first signature; the extractor keys it where its body is.
    bodyless: HashMap<u32, String>,
    /// Each method or property an interface or type literal declares, by
    /// its line, with its name: no body implements it here (carrick#2431's
    /// `interface_member` stop).
    pub(crate) signatures: HashMap<u32, String>,
    /// Each named `export default function name()` or `export default class
    /// Name`, by the line the declaration starts on, with the name it
    /// declares. The compiler names the symbol by what the module publishes
    /// it as, `default`; the extractor keys it by the declared name.
    defaults: HashMap<u32, String>,
    /// Each object-literal entry whose value is a name (`{ fn }`, `{ name:
    /// impl }`, a CommonJS export table among them), by the line the entry
    /// starts on: the entry's name and the name its value reads. The
    /// compiler names the entry a call through the object reaches; the
    /// function is the one the value names.
    entries: HashMap<u32, Vec<(String, String)>>,
    /// The line each declaration that holds a function starts on, by the
    /// function node's span start (discovery numbering): a declarator
    /// (`const x =\n  () =>`), a class property with its decorators
    /// (`@action\n  load = () =>`), an object entry. The compiler starts the
    /// declaration there; the extractor places the function node.
    holders: HashMap<u32, u32>,
    /// Each definition the extractor keyed whose holder starts on another
    /// line than the extractor's, by the holder's line ([`Self::place`]).
    starts: HashMap<u32, Vec<String>>,
    /// Each name a function's parameter binds (a destructured one's names
    /// included), by the line the parameter starts on and the line the name
    /// is written on: where a compiler row places a parameter's declaration
    /// (carrick#2474's receiver kind).
    pub(crate) params: HashSet<(u32, String)>,
}

/// Each name `pat` binds, with where it is written.
fn bound_names(pat: &Pat, out: &mut Vec<(Span, String)>) {
    match pat {
        Pat::Ident(ident) => out.push((ident.id.span, ident.id.sym.to_string())),
        Pat::Array(array) => {
            for element in array.elems.iter().flatten() {
                bound_names(element, out);
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::KeyValue(entry) => bound_names(&entry.value, out),
                    ObjectPatProp::Assign(assign) => {
                        out.push((assign.key.id.span, assign.key.id.sym.to_string()))
                    }
                    ObjectPatProp::Rest(rest) => bound_names(&rest.arg, out),
                }
            }
        }
        Pat::Assign(assign) => bound_names(&assign.left, out),
        Pat::Rest(rest) => bound_names(&rest.arg, out),
        _ => {}
    }
}

/// Where the function an initialiser or a property value is starts, when it
/// is one: an arrow, or a `function` expression's function.
fn function_start(expr: &Expr) -> Option<u32> {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Arrow(arrow) => Some(arrow.span.lo.0),
        Expr::Fn(expression) => Some(expression.function.span.lo.0),
        _ => None,
    }
}

/// An object entry's name, when it is written as a name or a string.
fn prop_name(name: &PropName) -> Option<String> {
    match name {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(text) => Some(text.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

impl DeclarationLines {
    pub(crate) fn of(module: &Module, source_map: &Lrc<SourceMap>) -> Self {
        struct Lines<'a> {
            source_map: &'a Lrc<SourceMap>,
            out: DeclarationLines,
        }
        impl Lines<'_> {
            fn line(&self, span: Span) -> u32 {
                self.source_map.lookup_char_pos(span.lo).line as u32
            }
            /// Record the names a parameter starting at `start` binds.
            fn param(&mut self, start: Span, pat: &Pat) {
                let mut names = Vec::new();
                bound_names(pat, &mut names);
                let opens = self.line(start);
                for (span, name) in names {
                    let written = self.line(span);
                    self.out.params.insert((opens, name.clone()));
                    self.out.params.insert((written, name));
                }
            }
        }
        impl Visit for Lines<'_> {
            fn visit_param(&mut self, param: &Param) {
                self.param(param.span, &param.pat);
                param.visit_children_with(self);
            }
            fn visit_ts_param_prop(&mut self, prop: &TsParamProp) {
                let pat = match &prop.param {
                    TsParamPropParam::Ident(ident) => Pat::Ident(ident.clone()),
                    TsParamPropParam::Assign(assign) => Pat::Assign(assign.clone()),
                };
                self.param(prop.span, &pat);
                prop.visit_children_with(self);
            }
            fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
                for pat in &arrow.params {
                    self.param(pat.span(), pat);
                }
                arrow.visit_children_with(self);
            }
            fn visit_export_default_decl(&mut self, export: &ExportDefaultDecl) {
                let declared = match &export.decl {
                    DefaultDecl::Fn(FnExpr {
                        ident: Some(ident), ..
                    })
                    | DefaultDecl::Class(ClassExpr {
                        ident: Some(ident), ..
                    }) => Some(ident.sym.to_string()),
                    _ => None,
                };
                if let Some(name) = declared {
                    let line = self.line(export.span);
                    self.out.defaults.insert(line, name);
                }
                export.visit_children_with(self);
            }
            fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
                if let Some(start) = declarator.init.as_deref().and_then(function_start) {
                    let line = self.line(declarator.span);
                    self.out.holders.insert(start, line);
                }
                declarator.visit_children_with(self);
            }
            fn visit_class_prop(&mut self, prop: &ClassProp) {
                if let Some(start) = prop.value.as_deref().and_then(function_start) {
                    let opens = prop
                        .decorators
                        .iter()
                        .map(|decorator| decorator.span.lo)
                        .chain([prop.span.lo, prop.key.span().lo])
                        .min()
                        .unwrap_or(prop.span.lo);
                    let line = self.source_map.lookup_char_pos(opens).line as u32;
                    self.out.holders.insert(start, line);
                }
                prop.visit_children_with(self);
            }
            fn visit_object_lit(&mut self, object: &ObjectLit) {
                for prop in &object.props {
                    let PropOrSpread::Prop(prop) = prop else {
                        continue;
                    };
                    match &**prop {
                        Prop::Shorthand(ident) => {
                            let line = self.line(ident.span);
                            let name = ident.sym.to_string();
                            self.out
                                .entries
                                .entry(line)
                                .or_default()
                                .push((name.clone(), name));
                        }
                        Prop::KeyValue(entry) => {
                            let line = self.line(entry.key.span());
                            if let Some(start) = function_start(&entry.value) {
                                self.out.holders.insert(start, line);
                            } else if let (Some(name), Expr::Ident(value)) = (
                                prop_name(&entry.key),
                                crate::graphql_document_sites::unwrap_expression(&entry.value),
                            ) {
                                self.out
                                    .entries
                                    .entry(line)
                                    .or_default()
                                    .push((name, value.sym.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
                object.visit_children_with(self);
            }
            fn visit_fn_decl(&mut self, declaration: &FnDecl) {
                if declaration.function.body.is_none() {
                    let line = self.line(declaration.function.span);
                    self.out
                        .bodyless
                        .insert(line, declaration.ident.sym.to_string());
                }
                declaration.visit_children_with(self);
            }
            fn visit_class_method(&mut self, method: &ClassMethod) {
                if method.function.body.is_none()
                    && !method.is_abstract
                    && let PropName::Ident(name) = &method.key
                {
                    let line = self.line(method.span);
                    self.out.bodyless.insert(line, name.sym.to_string());
                }
                method.visit_children_with(self);
            }
            fn visit_ts_method_signature(&mut self, signature: &TsMethodSignature) {
                if let Expr::Ident(name) = &*signature.key {
                    let line = self.line(signature.span);
                    self.out.signatures.insert(line, name.sym.to_string());
                }
            }
            fn visit_ts_property_signature(&mut self, signature: &TsPropertySignature) {
                if let Expr::Ident(name) = &*signature.key {
                    let line = self.line(signature.span);
                    self.out.signatures.insert(line, name.sym.to_string());
                }
            }
        }
        let mut lines = Lines {
            source_map,
            out: DeclarationLines::default(),
        };
        module.visit_with(&mut lines);
        lines.out
    }

    /// Place each definition the extractor keyed at the line its holder
    /// starts on, joined by the function node's span: what a compiler row's
    /// declaration line names where the extractor's line differs.
    pub(crate) fn place<'d>(
        &mut self,
        definitions: impl Iterator<Item = (&'d str, &'d FunctionDefinition)>,
    ) {
        self.starts.clear();
        for (key, definition) in definitions {
            let Some(line) = node_start(definition).and_then(|start| self.holders.get(&start))
            else {
                continue;
            };
            if *line != definition.line_number {
                self.starts.entry(*line).or_default().push(key.to_string());
            }
        }
        for keys in self.starts.values_mut() {
            keys.sort();
            keys.dedup();
        }
    }
}

/// Where a definition's function node starts (discovery numbering), when it
/// has one.
pub(crate) fn node_start(definition: &FunctionDefinition) -> Option<u32> {
    match &definition.node_type {
        FunctionNodeType::ArrowFunction(arrow) => Some(arrow.span.lo.0),
        FunctionNodeType::FunctionDeclaration(declaration) => Some(declaration.function.span.lo.0),
        FunctionNodeType::FunctionExpression(expression) => Some(expression.function.span.lo.0),
        FunctionNodeType::FromPayload(_) | FunctionNodeType::Placeholder => None,
    }
}

/// Each line a definition opens on, as the extractor places it, with the
/// definitions there. The compiler starts a declaration on the same line,
/// its decorators included.
pub(crate) fn definition_lines<'d>(
    definitions: impl Iterator<Item = (&'d str, &'d FunctionDefinition)>,
) -> HashMap<u32, Vec<String>> {
    let mut at: HashMap<u32, Vec<String>> = HashMap::new();
    for (key, definition) in definitions {
        at.entry(definition.line_number)
            .or_default()
            .push(key.to_string());
    }
    for keys in at.values_mut() {
        keys.sort();
        keys.dedup();
    }
    at
}

/// Whether the definition key `key` names the symbol `symbol`: the key
/// itself, or its last part (`Users.list`, `Users.static.list`).
fn names(key: &str, symbol: &str) -> bool {
    key == symbol || key.rsplit('.').next() == Some(symbol)
}

/// What a call reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reach {
    /// A definition of a file of the service: the file as walked and its
    /// key.
    Function(PathBuf, String),
    /// A declaration the repository makes outside the service's files: a
    /// function of this repository, which this service does not read.
    OutsideService(PathBuf, String),
    /// No function of the repository: the default library, a package, or a
    /// declaration that is no definition (a class, a parameter, an
    /// interface's member).
    NoFunction,
    /// The rows do not say.
    Unknown(HoleReason),
}

impl Reach {
    /// The definition, as [`CallSiteTargets::target_at`] names one: any
    /// function of the repository.
    pub(crate) fn target(&self) -> Option<(PathBuf, String)> {
        match self {
            Reach::Function(file, key) | Reach::OutsideService(file, key) => {
                Some((file.clone(), key.clone()))
            }
            Reach::NoFunction | Reach::Unknown(_) => None,
        }
    }
}

/// The calls that stay on the call graph with rows, and what the rows
/// changed, for the run log.
#[derive(Debug, Default)]
pub(crate) struct RowCalls {
    /// (file, span start, span end), discovery numbering.
    kept: HashSet<(PathBuf, u32, u32)>,
    /// What the rows say each other call reaches, by (file, span start, span
    /// end), read once: a lookup converts positions from the file's start.
    answers: HashMap<(PathBuf, u32, u32), Reach>,
    counts: BTreeMap<String, usize>,
}

impl RowCalls {
    /// One line for the run log: how each call was answered, against what
    /// the call graph names.
    pub(crate) fn describe(&self) -> String {
        format!("call targets {:?}", self.counts)
    }
}

/// Every call a file's IR records, in every body: (file, the call, whether
/// a construction).
pub(crate) fn every_call(files: &HashMap<PathBuf, FileIr>) -> Vec<(&Path, &CallIr, bool)> {
    fn walk<'a>(file: &'a Path, ir: &'a FnIr, out: &mut Vec<(&'a Path, &'a CallIr, bool)>) {
        out.extend(
            ir.read_calls()
                .map(|(call, construction)| (file, call, construction)),
        );
        for inner in ir.nested.iter().chain(&ir.detached) {
            walk(file, inner, out);
        }
    }
    let mut out = Vec::new();
    let mut paths: Vec<&PathBuf> = files.keys().collect();
    paths.sort();
    for path in paths {
        let file = &files[path];
        let mut keys: Vec<&String> = file.functions.keys().collect();
        keys.sort();
        for key in keys {
            walk(path, &file.functions[key], &mut out);
        }
        walk(path, &file.module_level, &mut out);
    }
    out
}

pub(crate) fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

impl RequestSummaryInputs {
    /// The calls that stay on the call graph, and the run log's counts:
    /// read once, when the rows are bound ([`Self::linked`]). `None` once
    /// `deadline` passes (carrick#2564).
    pub(crate) fn row_calls(&self, deadline: std::time::Instant) -> Option<RowCalls> {
        let mut out = RowCalls::default();
        for (file, call, _) in every_call(&self.files) {
            if std::time::Instant::now() >= deadline {
                return None;
            }
            let graph = self.sites.target_at(file, call.site.lo, call.site.hi);
            let kept = call.jsx
                || (is_identifier(&call.callee_text)
                    && graph.is_some_and(|(target, _)| target == file));
            if kept {
                out.kept
                    .insert((file.to_path_buf(), call.site.lo, call.site.hi));
                *out.counts.entry("kept".to_string()).or_default() += 1;
                continue;
            }
            let reach = self.row_reach(file, call.site.lo, call.site.hi);
            let key = match (&reach, graph) {
                (Reach::Unknown(reason), Some(_)) => format!("graph_only/{}", hole_key(*reason)),
                (Reach::Unknown(reason), None) => format!("neither/{}", hole_key(*reason)),
                (reach, graph) => match (reach.target(), graph) {
                    (Some(rows), Some(graph)) if rows == *graph => "agree".to_string(),
                    (Some(_), Some(_)) => "differ".to_string(),
                    (Some(_), None) => "rows_only".to_string(),
                    (None, Some(_)) => "graph_only/no_function".to_string(),
                    (None, None) => "no_function".to_string(),
                },
            };
            *out.counts.entry(key).or_default() += 1;
            out.answers
                .insert((file.to_path_buf(), call.site.lo, call.site.hi), reach);
        }
        Some(out)
    }

    /// What the call spanning exactly `lo..hi` (discovery numbering) in
    /// `file` reaches: from the rows when the run walks them, from the call
    /// graph otherwise.
    pub(crate) fn reach(&self, file: &Path, lo: u32, hi: u32) -> Reach {
        // FROZEN (old path, 2026-10-21): this call-graph answer only (call
        // targets joined by name; every summaries read of `CallSiteTargets`
        // goes through it). Deletions and safety fixes only. Fixes for wrong or
        // missing rows go in the compiler's position rows (the `call` row's
        // declaration, carrick#2413 slice D). Delete at carrick#2413 slice F,
        // once the unresolved-import fallback below (#2573) and the rows-join
        // budget fallback (#2574) count zero (carrick#2413).
        let graph = || match self.sites.target_at(file, lo, hi) {
            Some((target, key)) if self.files.contains_key(target) => {
                Reach::Function(target.clone(), key.clone())
            }
            Some((target, key)) => Reach::OutsideService(target.clone(), key.clone()),
            None => Reach::NoFunction,
        };
        let Some(row_calls) = &self.row_calls else {
            return graph();
        };
        let at = (file.to_path_buf(), lo, hi);
        if row_calls.kept.contains(&at) {
            return graph();
        }
        match self.rows_say(file, lo, hi) {
            // The import the call goes through did not resolve: the compiler
            // knows nothing of the callee, so its rows hold nothing against
            // the call graph, which is read as without rows (carrick#2557).
            Reach::Unknown(HoleReason::UnresolvedImport) => graph(),
            reach => reach,
        }
    }

    /// What the rows alone say the call at `lo..hi` in `file` reaches.
    fn rows_say(&self, file: &Path, lo: u32, hi: u32) -> Reach {
        let at = (file.to_path_buf(), lo, hi);
        match self
            .row_calls
            .as_ref()
            .and_then(|row_calls| row_calls.answers.get(&at))
        {
            Some(reach) => reach.clone(),
            None => self.row_reach(file, lo, hi),
        }
    }

    /// Whether [`Self::reach`] reads the call at `lo..hi` in `file` from the
    /// call graph because the rows cannot resolve the import it goes
    /// through, and the call graph follows it into a function of the
    /// repository (carrick#2557).
    pub(crate) fn fell_back(&self, file: &Path, lo: u32, hi: u32) -> bool {
        let Some(row_calls) = &self.row_calls else {
            return false;
        };
        !row_calls.kept.contains(&(file.to_path_buf(), lo, hi))
            && self.rows_say(file, lo, hi) == Reach::Unknown(HoleReason::UnresolvedImport)
            && self.sites.target_at(file, lo, hi).is_some()
    }

    /// [`Self::reach`]'s definition, as [`CallSiteTargets::target_at`]
    /// names one.
    pub(crate) fn call_target(&self, file: &Path, lo: u32, hi: u32) -> Option<(PathBuf, String)> {
        self.reach(file, lo, hi).target()
    }

    /// Every call of the service's files whose callee [`Self::reach`] names
    /// as a function another file of the service declares, or a function
    /// another service declares ([`crate::call_through`]'s leaf calls). A construction (`new
    /// Client()`) is left out: it builds a value and sends nothing itself.
    /// So is a JSX element: it renders a component, which is no call the
    /// source writes. Sorted by file and span, each call once.
    pub(crate) fn calls_into_other_files(&self) -> Vec<CallIntoFile> {
        let mut out: Vec<CallIntoFile> = every_call(&self.files)
            .into_iter()
            .filter(|(_, call, construction)| !construction && !call.jsx)
            .filter_map(|(file, call, _)| {
                let (target, key, boundary) = match self.reach(file, call.site.lo, call.site.hi) {
                    Reach::Function(target, key) if target != file => (target, key, false),
                    Reach::OutsideService(target, key) => (target, key, true),
                    _ => return None,
                };
                Some(CallIntoFile {
                    file: file.to_path_buf(),
                    span_start: call.site.span_start,
                    span_end: call.site.span_end,
                    line: call.site.line,
                    target,
                    key,
                    boundary,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (&a.file, a.span_start, a.span_end).cmp(&(&b.file, b.span_start, b.span_end))
        });
        out.dedup_by(|a, b| {
            (&a.file, a.span_start, a.span_end) == (&b.file, b.span_start, b.span_end)
        });
        out
    }
}

impl RequestSummaryInputs {
    /// The files, outside this service's own, that its calls' callees are
    /// declared in ([`Reach::OutsideService`]): the files of the other
    /// services of the run a walk from this service may enter
    /// (carrick#2618). Sorted.
    pub(crate) fn outside_declarations(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = every_call(&self.files)
            .into_iter()
            .filter_map(
                |(file, call, _)| match self.reach(file, call.site.lo, call.site.hi) {
                    Reach::OutsideService(declaring, _) => Some(declaring),
                    _ => None,
                },
            )
            .collect();
        files.sort();
        files.dedup();
        files
    }
}

/// One call [`RequestSummaryInputs::calls_into_other_files`] lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallIntoFile {
    /// The file as walked.
    pub(crate) file: PathBuf,
    /// The call's span in SWC's own numbering ([`SWC_SPAN_BASE`]), and the
    /// line it opens on.
    pub(crate) span_start: u32,
    pub(crate) span_end: u32,
    pub(crate) line: u32,
    /// The definition the callee resolves to: the file as walked and its key.
    pub(crate) target: PathBuf,
    pub(crate) key: String,
    /// The definition is not one of this service's files: a call across the
    /// service boundary, into a function another service declares.
    pub(crate) boundary: bool,
}

impl RequestSummaryInputs {
    /// What the call row at `lo..hi` in `file` names.
    fn row_reach(&self, file: &Path, lo: u32, hi: u32) -> Reach {
        self.row_names(file, lo, hi, Kind::Call)
    }

    /// The definition the row of `kind` at `lo..hi` in `file` names: for a
    /// name or a member read as a value (carrick#2432), the function it
    /// holds when it holds one of the repository's.
    pub(super) fn row_names(&self, file: &Path, lo: u32, hi: u32, kind: Kind) -> Reach {
        let Some(positions) = &self.positions else {
            return Reach::Unknown(HoleReason::RowsMissing);
        };
        let Some(ir) = self.files.get(file) else {
            return Reach::Unknown(HoleReason::RowsMissing);
        };
        let row = match positions.lookup(file, ir.swc_start(lo), ir.swc_start(hi), kind) {
            Ok(row) => row,
            Err(reason) => return Reach::Unknown(reason),
        };
        let unresolved = row.flags() & FLAG_ERROR_TYPE != 0;
        let Some(symbol) = row.symbol() else {
            return Reach::Unknown(if row.flags() & FLAG_ANY != 0 {
                HoleReason::UntypedReceiver
            } else {
                HoleReason::UnresolvedName
            });
        };
        let Some(decl_file) = row.decl_file() else {
            return Reach::NoFunction;
        };
        if decl_file.starts_with("lib:")
            || decl_file.starts_with("pkg:")
            || decl_file.split('/').any(|part| part == "node_modules")
        {
            return Reach::NoFunction;
        }
        let declaring = positions.path_of(decl_file);
        let Some(declared) = self.files.get(&declaring) else {
            return Reach::OutsideService(declaring, symbol.to_string());
        };
        match join(declared, row.decl_line(), symbol) {
            // A call whose result did not resolve, at a declaration that is
            // no definition: an import of nothing, as the import line is.
            Join::None if unresolved => Reach::Unknown(if declaring == file {
                HoleReason::UnresolvedImport
            } else {
                HoleReason::UnresolvedName
            }),
            joined => reach_of_join(declaring, joined),
        }
    }

    /// The definition of `file` the declaration of `symbol` at `line` is,
    /// when the service holds `file`: what [`Self::row_names`] reads of a
    /// declaration in one of its files, for a call another service of the
    /// run makes (carrick#2618). `NoFunction` when the service holds no such
    /// file or the declaration is no definition.
    pub(crate) fn definition_at(&self, file: &Path, line: Option<u32>, symbol: &str) -> Reach {
        match self.files.get(file) {
            Some(declared) => reach_of_join(file.to_path_buf(), join(declared, line, symbol)),
            None => Reach::NoFunction,
        }
    }
}

/// What a declaration in `declaring` that [`join`] read is.
fn reach_of_join(declaring: PathBuf, joined: Join) -> Reach {
    match joined {
        Join::One(key) => Reach::Function(declaring, key),
        Join::Several => Reach::Unknown(HoleReason::RowsMissing),
        Join::Unfollowed => Reach::Unknown(HoleReason::UnreadExpression),
        Join::None => Reach::NoFunction,
    }
}

enum Join {
    One(String),
    Several,
    None,
    /// An object entry whose value names no definition of the file: an
    /// import, or a binding this pass does not follow.
    Unfollowed,
}

/// The definition of `declared` the compiler's declaration of `symbol` at
/// `line` is, joined on what the declaration names:
///
/// - the one opening there whose key names the symbol, where the extractor
///   places its function node or where the declaration holding it starts
///   (a declarator, a decorated property); a named `export default`
///   declaration names its own name, not `default`;
/// - for an object entry (`{ fn }`, `{ name: impl }`), the function its
///   value names;
/// - for an overload's signature, the first definition of that name after
///   it, which is the overloads' body.
fn join(declared: &FileIr, line: Option<u32>, symbol: &str) -> Join {
    let Some(line) = line else {
        return Join::None;
    };
    let lines = &declared.declaration_lines;
    let symbol = match lines.defaults.get(&line) {
        Some(name) if symbol == "default" => name.as_str(),
        _ => symbol,
    };
    let mut at: Vec<&String> = declared
        .definition_lines
        .get(&line)
        .into_iter()
        .flatten()
        .chain(lines.starts.get(&line).into_iter().flatten())
        .filter(|key| names(key, symbol))
        .collect();
    at.sort();
    at.dedup();
    match at.as_slice() {
        [one] => return Join::One((*one).clone()),
        [_, _, ..] => return Join::Several,
        [] => {}
    }
    if let Some((_, value)) = lines
        .entries
        .get(&line)
        .into_iter()
        .flatten()
        .find(|(name, _)| name == symbol)
    {
        return if declared.definitions.contains_key(value) {
            Join::One(value.clone())
        } else {
            Join::Unfollowed
        };
    }
    if declared
        .declaration_lines
        .bodyless
        .get(&line)
        .map(String::as_str)
        != Some(symbol)
    {
        return Join::None;
    }
    let implementation = declared
        .definition_lines
        .iter()
        .filter(|(opens, _)| **opens > line)
        .flat_map(|(opens, keys)| keys.iter().map(move |key| (*opens, key)))
        .filter(|(_, key)| names(key, symbol))
        .min();
    match implementation {
        Some((_, key)) => Join::One(key.clone()),
        None => Join::None,
    }
}

pub(crate) fn hole_key(reason: HoleReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::fixture_walk::{NODENEXT, Walked, walked};
    use super::*;

    fn calls(edit: &[(&str, &str)], with_rows: bool) -> Option<Walked> {
        walked(
            "position-rows-calls",
            "services/calls",
            "calls",
            edit,
            with_rows,
        )
    }

    const CALLERS: &str = "services/calls/src/callers.ts";

    /// The URLs each line of `callers.ts` states a summary row for.
    fn stated(walked: &Walked) -> BTreeMap<u32, Vec<String>> {
        stated_in(walked, CALLERS)
    }

    /// The URLs each line of `file` (repo-relative) states a summary row
    /// for.
    fn stated_in(walked: &Walked, file: &str) -> BTreeMap<u32, Vec<String>> {
        let index = summarize(&walked.inputs, &LibrarySemantics::default());
        let path = walked.path(file);
        let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for rows in index.rows(&path).into_iter().flat_map(BTreeMap::values) {
            for row in rows {
                out.entry(row.line).or_default().push(row.target.clone());
            }
        }
        for targets in out.values_mut() {
            targets.sort();
        }
        out
    }

    /// What each call written in `callers.ts` reaches, by line, as
    /// `file:key` or the outcome's name.
    fn reached(walked: &Walked) -> BTreeMap<u32, Vec<String>> {
        reached_in(walked, CALLERS)
    }

    /// What each call written in `file` (repo-relative) reaches, by line.
    fn reached_in(walked: &Walked, file: &str) -> BTreeMap<u32, Vec<String>> {
        let path = walked.path(file);
        let mut out: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for (file, call, _) in every_call(&walked.inputs.files) {
            if file != path {
                continue;
            }
            let text = match walked.inputs.reach(file, call.site.lo, call.site.hi) {
                Reach::Function(target, key) => {
                    format!("{}:{key}", target.file_name().unwrap().to_string_lossy())
                }
                Reach::OutsideService(target, key) => format!(
                    "outside {}:{key}",
                    target.file_name().unwrap().to_string_lossy()
                ),
                Reach::NoFunction => "no_function".to_string(),
                Reach::Unknown(reason) => format!("unknown {}", hole_key(reason)),
            };
            out.entry(call.site.line).or_default().push(text);
        }
        for texts in out.values_mut() {
            texts.sort();
        }
        out
    }

    /// Every summary row `walked` states, in every file, with the copied
    /// repo's root taken out so two walks compare.
    fn every_row(walked: &Walked, index: &RequestSummaryIndex) -> Vec<String> {
        let root = walked.repo.to_string_lossy().to_string();
        let mut out: Vec<String> = walked
            .inputs
            .files
            .keys()
            .filter_map(|path| index.rows(path))
            .flat_map(BTreeMap::values)
            .flatten()
            .map(|row| format!("{row:?}").replace(&root, ""))
            .collect();
        out.sort();
        out
    }

    /// carrick#1604: the wrapper fixture's service is rooted above the
    /// `web/` folder that holds its tsconfig. Each file is typed under that
    /// config, so the rows resolve every import and no call is read around
    /// them, and the summaries state the rows the call graph alone states.
    #[test]
    fn a_service_whose_config_sits_below_its_root_is_read_through_the_rows() {
        let Some(off) = walked("position-rows-wrapper", ".", "web", &[], false) else {
            return;
        };
        let without = summarize(&off.inputs, &LibrarySemantics::default());
        let on = walked("position-rows-wrapper", ".", "web", &[], true).unwrap();
        let with = summarize(&on.inputs, &LibrarySemantics::default());
        assert!(with.row_count() > 0);
        assert_eq!(with.fell_back(), 0);
        assert_eq!(with.unfollowed_count(), 0, "{:?}", with.unfollowed());
        // The call graph writes a path through the service root as `/./`,
        // the rows as `/`: the same file.
        let plain = |rows: Vec<String>| -> Vec<String> {
            let mut rows: Vec<String> = rows.iter().map(|row| row.replace("/./", "/")).collect();
            rows.sort();
            rows
        };
        assert_eq!(
            plain(every_row(&on, &with)),
            plain(every_row(&off, &without))
        );
    }

    /// carrick#2557: a service rooted above the directory that holds its
    /// tsconfig, whose components call service functions that call a shared
    /// fetch wrapper. The call graph follows every import; the compiler's
    /// rows, walked under a config whose resolution cannot read the
    /// extensionless imports, resolve none of them. An import the compiler cannot resolve tells it nothing about
    /// the callee, so each such call is read from the call graph as without
    /// rows, and counted as such: the summaries state the same rows.
    #[test]
    fn a_call_through_an_unresolved_import_is_read_from_the_call_graph() {
        let edit = [NODENEXT];
        let Some(off) = walked("position-rows-wrapper", ".", "web", &edit, false) else {
            return;
        };
        let without = summarize(&off.inputs, &LibrarySemantics::default());
        assert!(
            without.row_count() > 0,
            "the call graph follows the wrapper"
        );
        assert_eq!(without.fell_back(), 0, "no rows, nothing read around them");

        let on = walked("position-rows-wrapper", ".", "web", &edit, true).unwrap();
        let with = summarize(&on.inputs, &LibrarySemantics::default());
        assert_eq!(every_row(&on, &with), every_row(&off, &without));
        assert_eq!(with.silent_count(), without.silent_count());
        assert_eq!(with.undetermined, without.undetermined);
        // Each call into a function another module declares: 9 in the
        // components (7 into the services, 2 straight into the wrapper) and
        // 8 in the services. The wrapper's own calls of its token helper are
        // bare calls of a function the same file declares, which stay on the
        // call graph.
        assert_eq!(with.fell_back(), 17);
        assert_eq!(with.unfollowed_count(), 0, "{:?}", with.unfollowed());
    }

    const PROBE_FILE: &str = "web/src/components/Sync.ts";
    const PROBE: &str = "import * as users from \"../services/users\";\n\nexport async function sync() {\n  return users.listUsers();\n}\n";
    const PROBE_REASON: &str = "unresolved_name";

    /// carrick#2557: only an unresolved import is read from the call graph.
    /// A call the rows cannot follow for any other reason is still no
    /// callee: the summaries state no row there, and the call is counted
    /// under that reason.
    #[test]
    fn a_call_the_rows_cannot_follow_for_another_reason_states_no_row() {
        let edit = [NODENEXT, (PROBE_FILE, PROBE)];
        let Some(off) = walked("position-rows-wrapper", ".", "web", &edit, false) else {
            return;
        };
        let without = summarize(&off.inputs, &LibrarySemantics::default());
        let on = walked("position-rows-wrapper", ".", "web", &edit, true).unwrap();
        assert_eq!(
            reached_in(&on, PROBE_FILE),
            BTreeMap::from([(4, vec![format!("unknown {PROBE_REASON}")])])
        );
        let with = summarize(&on.inputs, &LibrarySemantics::default());
        assert!(
            !stated_in(&off, PROBE_FILE).is_empty(),
            "the call graph follows it"
        );
        assert_eq!(stated_in(&on, PROBE_FILE), BTreeMap::new());
        assert_eq!(
            with.unfollowed().clone(),
            BTreeMap::from([(PROBE_REASON.to_string(), 1)])
        );
        assert_eq!(without.unfollowed_count(), 0);
    }

    fn line(n: u32, target: &str) -> (u32, Vec<String>) {
        (n, vec![target.to_string()])
    }

    /// carrick#2413 slice D: with rows, each call reaches the declaration
    /// the compiler resolves its callee to. The receiver's type picks one of
    /// two classes' `list`, an override is the subclass's, an overload's
    /// first signature joins the implementation, a decorated method joins
    /// at its decorator's line, a chain's outer call is its own, and an
    /// `export *` chain is followed. A call through an `any` receiver says
    /// nothing, so its body is not complete, and a function of a sibling
    /// package is outside the service, as without rows.
    #[test]
    fn with_rows_a_call_reaches_the_declaration_the_compiler_names() {
        let Some(on) = calls(&[], true) else {
            return;
        };
        let texts = |items: &[&str]| items.iter().map(|item| item.to_string()).collect();
        assert_eq!(
            reached(&on),
            BTreeMap::from([
                (9, texts(&["no_function"])),
                (13, texts(&["models.ts:Users.list"])),
                (17, texts(&["models.ts:Child.load"])),
                (21, texts(&["overload.ts:readPath"])),
                (25, texts(&["unknown untyped_receiver"])),
                (29, texts(&["helpers.ts:ping"])),
                (33, texts(&["reports.ts:Reports.fetchAll"])),
                (37, texts(&["outside index.ts:remote"])),
                (41, texts(&["callers.ts:users", "models.ts:Users.list"])),
            ])
        );
        assert_eq!(
            stated(&on),
            BTreeMap::from([
                line(13, "/users"),
                line(17, "/child"),
                line(21, "/a"),
                line(29, "/ping"),
                line(33, "/reports"),
                line(41, "/users"),
            ])
        );
    }

    /// The specifier each library site in `kit-user.ts` is read through.
    fn kit_sites(walked: &Walked) -> Vec<(u32, String)> {
        let path = walked.path("services/calls/src/kit-user.ts");
        let mut sites: Vec<(u32, String)> = super::super::library_sites(&walked.inputs)
            .sites
            .into_iter()
            .filter(|site| site.file == path)
            .map(|site| (site.line, site.specifier))
            .collect();
        sites.sort();
        sites
    }

    /// The end of the chain from the `/kit-health` comparison in
    /// `server.ts`.
    fn kit_health_end(walked: &Walked) -> serde_json::Value {
        let chains = super::super::guard_chains(&walked.inputs, &walked.repo);
        let chain = chains
            .iter()
            .find(|chain| chain.literal == "/kit-health")
            .expect("a chain for /kit-health");
        serde_json::to_value(&chain.end).unwrap()
    }

    /// carrick#2413 slice E: an import whose rows name a declaration of the
    /// repository is no package's client, whatever its specifier says: a
    /// tsconfig alias to in-repo code reads as a package without rows and as
    /// nothing with them. An external import's end names the package the
    /// compiler says its export is declared in.
    #[test]
    fn with_rows_a_package_is_the_one_the_declaration_is_in() {
        let Some(off) = calls(&[], false) else {
            return;
        };
        let on = calls(&[], true).unwrap();
        assert_eq!(
            kit_sites(&off),
            vec![(5, "http-kit".to_string()), (9, "fixture-kit".to_string())]
        );
        assert_eq!(kit_sites(&on), vec![(9, "fixture-kit".to_string())]);
        assert_eq!(
            kit_health_end(&off),
            serde_json::json!({
                "kind": "external_import", "specifier": "fixture-kit",
                "export": "serve", "steps": []
            })
        );
        assert_eq!(
            kit_health_end(&on),
            serde_json::json!({
                "kind": "external_import", "specifier": "fixture-kit",
                "export": "serve", "steps": [], "package": "fixture-kit"
            })
        );
    }

    /// Without rows, the call graph's answer stands, byte for byte: the
    /// lines it cannot follow state nothing.
    #[test]
    fn without_rows_the_call_graph_answers() {
        let Some(off) = calls(&[], false) else {
            return;
        };
        assert!(off.inputs.row_calls.is_none());
        assert_eq!(
            stated(&off),
            BTreeMap::from([
                line(17, "/child"),
                line(21, "/a"),
                line(29, "/ping"),
                line(33, "/reports"),
            ])
        );
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// The calls service with `allowJs`, so its CommonJS files are in the
    /// program.
    const ALLOW_JS: &str = r#"{
  "compilerOptions": {
    "target": "es2022",
    "module": "esnext",
    "moduleResolution": "bundler",
    "strict": true,
    "skipLibCheck": true,
    "experimentalDecorators": true,
    "allowJs": true,
    "baseUrl": ".",
    "paths": {
      "@calls/remote": ["../../packages/remote/src/index.ts"],
      "http-kit": ["src/vendor/http-kit.ts"]
    },
    "lib": ["es2022", "dom"],
    "types": []
  },
  "include": ["src/**/*"]
}
"#;

    /// Declarations whose compiler row names another thing than the key the
    /// extractor gives the definition, each written apart from the calls.
    const SHAPES: &[(&str, &str)] = &[
        ("tsconfig.json", ALLOW_JS),
        (
            "src/load-users.ts",
            "export default async function loadUsers(): Promise<Response> {\n  return fetch(\"/users-default\");\n}\n",
        ),
        (
            "src/books.ts",
            "export const loadBooks =\n  async (): Promise<Response> => fetch(\"/books\");\n",
        ),
        (
            "src/store.ts",
            "function action(_target: unknown, _key: string): void {}\n\nexport class Store {\n  @action\n  load = async (): Promise<Response> => fetch(\"/store\");\n\n  refresh(): Promise<Response> {\n    return this.load();\n  }\n}\n",
        ),
        (
            "src/api.ts",
            "function list(): Promise<Response> {\n  return fetch(\"/list\");\n}\n\nexport default {\n  list,\n};\n",
        ),
        (
            "src/shapes.ts",
            "import loadUsers from \"./load-users\";\nimport { loadBooks } from \"./books\";\nimport { Store } from \"./store\";\nimport api from \"./api\";\n\nexport function viaDefault(): Promise<Response> {\n  return loadUsers();\n}\n\nexport function viaDeclarator(): Promise<Response> {\n  return loadBooks();\n}\n\nexport function viaDecorated(store: Store): Promise<Response> {\n  return store.load();\n}\n\nexport function viaEntry(): Promise<Response> {\n  return api.list();\n}\n",
        ),
        (
            "src/totals.js",
            "function computeTotal(items) {\n  return fetch(\"/total\");\n}\n\nfunction otherImpl() {\n  return fetch(\"/other\");\n}\n\nmodule.exports = {\n  computeTotal,\n  other: otherImpl,\n};\n",
        ),
        (
            "src/orders.js",
            "const totals = require(\"./totals\");\nconst { other: renamed } = require(\"./totals\");\n\nfunction readTotal() {\n  return totals.computeTotal([]);\n}\n\nfunction readOther() {\n  return renamed();\n}\n\nmodule.exports = { readTotal, readOther };\n",
        ),
    ];

    /// The compiler names a declaration by what the module publishes it as,
    /// or where the declaration holding the function starts; the join reads
    /// what that declaration names. A named `export default function` is
    /// `default` to the compiler and its name to the extractor. A CommonJS
    /// export table entry (`{ fn }`, `{ name: impl }`) and an object
    /// literal's entry name the entry, and the function is the one its value
    /// names. A declarator split over two lines and a decorated arrow
    /// property start a line above the arrow the extractor places.
    #[test]
    fn with_rows_a_call_reaches_what_its_declaration_names() {
        let Some(on) = calls(SHAPES, true) else {
            return;
        };
        assert_eq!(
            reached_in(&on, "services/calls/src/shapes.ts"),
            BTreeMap::from([
                (7, texts(&["load-users.ts:loadUsers"])),
                (11, texts(&["books.ts:loadBooks"])),
                (15, texts(&["store.ts:Store.load"])),
                (19, texts(&["api.ts:list"])),
            ])
        );
        assert_eq!(
            reached_in(&on, "services/calls/src/store.ts"),
            BTreeMap::from([
                (5, texts(&["no_function"])),
                (8, texts(&["store.ts:Store.load"])),
            ])
        );
        assert_eq!(
            reached_in(&on, "services/calls/src/orders.js"),
            BTreeMap::from([
                (1, texts(&["no_function"])),
                (2, texts(&["no_function"])),
                (5, texts(&["totals.js:computeTotal"])),
                (9, texts(&["totals.js:otherImpl"])),
            ])
        );
        assert_eq!(
            stated_in(&on, "services/calls/src/shapes.ts"),
            BTreeMap::from([
                line(7, "/users-default"),
                line(11, "/books"),
                line(15, "/store"),
                line(19, "/list"),
            ])
        );
    }

    /// A call the rows cannot say the callee of (an `any` receiver, a
    /// `require` the compiler does not resolve) is not composed through the
    /// rows, and is still the request its shape states, as without rows.
    #[test]
    fn with_rows_an_unreadable_call_is_still_a_request() {
        let untyped = [
            (
                "src/untyped.ts",
                "const kit = require(\"untyped-kit\");\n\nexport function readUntyped(client: any): Promise<unknown> {\n  return client.get(\"/untyped\");\n}\n\nexport function readRequired(): Promise<unknown> {\n  return kit.get(\"/required\");\n}\n",
            ),
            (
                "src/untyped-user.ts",
                "import { readRequired, readUntyped } from \"./untyped\";\n\nexport function loadUntyped(): Promise<unknown> {\n  return readUntyped({});\n}\n\nexport function loadRequired(): Promise<unknown> {\n  return readRequired();\n}\n",
            ),
        ];
        let Some(on) = calls(&untyped, true) else {
            return;
        };
        let off = calls(&untyped, false).unwrap();
        let reached = reached_in(&on, "services/calls/src/untyped.ts");
        for line in [4, 8] {
            assert!(
                reached[&line]
                    .iter()
                    .all(|text| text.starts_with("unknown")),
                "line {line}: {reached:?}"
            );
        }
        let user = "services/calls/src/untyped-user.ts";
        let stated = stated_in(&on, user);
        assert_eq!(stated, stated_in(&off, user));
        assert_eq!(
            stated,
            BTreeMap::from([line(4, "/untyped"), line(8, "/required")])
        );
    }

    /// Each call's target is read from the rows once: asking again looks no
    /// row up.
    #[test]
    fn with_rows_each_call_is_looked_up_once() {
        let Some(on) = calls(&[], true) else {
            return;
        };
        let positions = on.inputs.positions.as_ref().unwrap();
        let before = positions.lookups();
        for _ in 0..3 {
            for (file, call, _) in every_call(&on.inputs.files) {
                on.inputs.reach(file, call.site.lo, call.site.hi);
            }
        }
        assert_eq!(positions.lookups(), before);
    }
}
