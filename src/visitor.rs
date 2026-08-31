extern crate swc_common;
extern crate swc_ecma_parser;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
use swc_common::{SourceMapper, Spanned};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionArgument {
    #[allow(dead_code)]
    pub name: String,
    #[serde(skip)]
    pub type_ann: Option<TsTypeAnn>, // swc_ecma_ast::TsTypeAnn
    /// Raw TS source text of the type annotation, e.g. "Request<{id: string}>".
    /// `None` when the parameter has no annotation. Serialized so the cloud /
    /// MCP layer can surface function signatures. May be filled by the
    /// signature pass with a compiler-inferred type when no annotation exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_string: Option<String>,
    /// `true` when `type_string` came from a source annotation, `false` when
    /// it was inferred by the sidecar. Serves as a confidence signal to agents.
    #[serde(default)]
    pub is_explicit: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypeReference {
    pub file_path: PathBuf,
    #[allow(dead_code)]
    #[serde(skip)]
    pub type_ann: Option<Box<TsType>>,
    pub start_position: usize,
    pub composite_type_string: String,
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Json {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(HashMap<String, Json>),
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub enum FunctionNodeType {
    ArrowFunction(Box<ArrowExpr>),
    FunctionDeclaration(Box<FnDecl>),
    FunctionExpression(Box<FnExpr>),
    // Used for deserialization when AST data is not available
    #[default]
    Placeholder,
}

impl serde::Serialize for FunctionNodeType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            FunctionNodeType::ArrowFunction(_) => serializer.serialize_str("ArrowFunction"),
            FunctionNodeType::FunctionDeclaration(_) => {
                serializer.serialize_str("FunctionDeclaration")
            }
            FunctionNodeType::FunctionExpression(_) => {
                serializer.serialize_str("FunctionExpression")
            }
            FunctionNodeType::Placeholder => serializer.serialize_str("Placeholder"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for FunctionNodeType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "ArrowFunction" => Ok(FunctionNodeType::Placeholder),
            "FunctionDeclaration" => Ok(FunctionNodeType::Placeholder),
            "FunctionExpression" => Ok(FunctionNodeType::Placeholder),
            "Placeholder" => Ok(FunctionNodeType::Placeholder),
            _ => Ok(FunctionNodeType::Placeholder),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionDefinition {
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub file_path: PathBuf,
    pub node_type: FunctionNodeType,
    pub arguments: Vec<FunctionArgument>,
    /// Raw source text of function body (capped at 2000 chars)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_source: Option<String>,
    /// Whether the function is exported
    #[serde(default)]
    pub is_exported: bool,
    /// Start line number for navigation
    #[serde(default)]
    pub line_number: u32,
    /// Last line of the function body. Paired with `line_number` this lets a
    /// consumer read exactly the function instead of the whole file — the
    /// index locates code, and without an end line every hit still costs a
    /// full-file read to see thirty lines. 0 when the span is unavailable.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub end_line: u32,
    /// LLM-generated description of what this function intends to do
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Local functions called by this function (name, file_path, line_number)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<FunctionCallRef>,
    /// Raw TS source text of the return type annotation, e.g. "Promise<User>".
    /// `None` when the function has no annotated return type. May be filled by
    /// the signature pass with a compiler-inferred type when no annotation exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    /// `true` when `return_type` came from a source annotation, `false` when
    /// it was inferred by the sidecar.
    #[serde(default)]
    pub return_is_explicit: bool,
    /// One-line signature hint composed at scan time, e.g.
    /// "(token: string, opts?: VerifyOpts) => Promise<AuthResult>". `None`
    /// until the signature pass runs. The MCP layer surfaces this verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Literal retrieval tokens drawn from this function's own AST: parameter
    /// names, literal parameter defaults, identifiers and property names read
    /// in the body, and the string/numeric literals it contains
    /// (carrick-cloud#434). Deduplicated, first-occurrence order, capped — see
    /// `build_tokens` for the cap and the priority between the three groups.
    ///
    /// The cloud's lexical retrieval leg indexes name and intent only, so a
    /// question asked in code words cannot reach the function that holds
    /// `budgetBytes = 1500`: the default is dropped from the stored signature
    /// and the body is never stored. These tokens are the smallest thing that
    /// puts those words in the index without shipping source.
    ///
    /// Serde-defaulted and skipped when empty, so an index cached by an older
    /// scanner still deserialises and a function with nothing notable costs no
    /// payload bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<String>,
    /// Content hash of the exact inputs that produced `intent` (cache version +
    /// function body + callees' intents). Lets a later scan reuse the cached
    /// intent when nothing affecting it changed, and regenerate it when a
    /// callee's intent shifts. `None` until an intent is generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_input_hash: Option<String>,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// A reference to a called function, for navigating to its source via GitHub.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionCallRef {
    pub name: String,
    pub file_path: String,
    /// Line the CALLEE is defined on, in `file_path`.
    pub line_number: u32,
    /// Line the call is written on, in the CALLER's file. Zero when unknown.
    /// Serde-defaulted so an index cached by an older scanner still
    /// deserialises (it reports 0 until the next scan rewrites it).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub call_site_line: u32,
}

/// How a call site names its callee. Resolution differs per shape: a bare name
/// is a module-scope binding, `this.x` is scoped to the enclosing class, and
/// `obj.x` means nothing until `obj` itself is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalleeShape {
    /// `foo(...)`
    Bare,
    /// `this.foo(...)` / `this.#foo(...)`, inside the named class.
    ThisMember(String),
    /// `obj.foo(...)`, where `obj` is a plain identifier.
    Member(String),
}

/// One call site inside a function body, as written in the AST.
///
/// Collected structurally, so a name that appears only in a string literal, a
/// template literal or a comment is never recorded (#581). Resolution to a
/// definition happens later, in [`crate::call_graph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalleeRef {
    /// The called member/binding name — `foo` in all three shapes.
    pub name: String,
    pub shape: CalleeShape,
    /// Line the call is written on, in the caller's file.
    pub line: u32,
}

/// Most retrieval tokens one function may contribute (see
/// `FunctionDefinition::tokens`).
///
/// Chosen against a measurement rather than picked: over a 1,061-function
/// TypeScript service the mean function emits 20 tokens and 246 bytes of
/// serialised JSON, and 1% of functions reach 120. So the cap costs a
/// thousand-function repo well under a megabyte on an upload path that
/// already stages anything sizeable through S3, while binding the tail. The
/// functions that do hit it are the long switch-heavy bodies whose hundredth
/// identifier carries no retrieval signal anyway.
const MAX_FUNCTION_TOKENS: usize = 120;

/// Ceiling for parameters plus body identifiers together, leaving the rest of
/// `MAX_FUNCTION_TOKENS` for literals. Without a reserved floor a long body
/// spends the whole budget on identifiers and its literals never appear —
/// which would lose exactly the case this field exists for (a query naming a
/// constant) on exactly the large functions where retrieval matters most.
const IDENTIFIER_TOKEN_CEILING: usize = 90;

/// Longest literal kept as a token. A longer string is dropped rather than
/// truncated: half a sentence is a term nobody will ever type. Identifiers are
/// not length-capped — a parameter name must survive whatever its length.
const MAX_LITERAL_TOKEN_LEN: usize = 40;

/// Trim a string-ish literal into a token, or drop it. `None` for anything
/// empty, whitespace-only, or longer than `MAX_LITERAL_TOKEN_LEN`.
fn string_literal_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_LITERAL_TOKEN_LEN {
        return None;
    }
    Some(trimmed.to_string())
}

/// A literal's token form, or `None` for kinds that carry no retrieval signal
/// (booleans, `null`, regexes, JSX text).
///
/// Numbers keep their source text when the parser preserved it, so `0x1f` and
/// `1_500` index as written rather than as a reconstruction of their value.
fn literal_token(lit: &Lit) -> Option<String> {
    match lit {
        Lit::Str(s) => string_literal_token(&s.value),
        Lit::Num(n) => Some(match &n.raw {
            Some(raw) => raw.to_string(),
            None if n.value.fract() == 0.0 && n.value.abs() < 1e15 => {
                format!("{}", n.value as i64)
            }
            None => format!("{}", n.value),
        }),
        Lit::BigInt(b) => Some(match &b.raw {
            Some(raw) => raw.to_string(),
            None => b.value.to_string(),
        }),
        _ => None,
    }
}

/// The token for a literal used as a default value, if it is one. Unwraps the
/// type-level and parenthesis wrappers, and reads `-1` as a literal rather
/// than as an operator applied to `1`.
fn default_value_token(expr: &Expr) -> Option<String> {
    match CalleeCollector::unwrap_expr(expr) {
        Expr::Lit(lit) => literal_token(lit),
        Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
            match CalleeCollector::unwrap_expr(&unary.arg) {
                Expr::Lit(lit @ (Lit::Num(_) | Lit::BigInt(_))) => {
                    literal_token(lit).map(|t| format!("-{t}"))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Every name a parameter pattern binds, plus its literal defaults, in source
/// order. Destructuring is walked to its leaves, so `{ budgetBytes = 1500 }`
/// contributes both parts exactly as a plain defaulted parameter does.
fn collect_pat_tokens(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Ident(ident) => out.push(ident.id.sym.to_string()),
        Pat::Rest(rest) => collect_pat_tokens(&rest.arg, out),
        Pat::Assign(assign) => {
            collect_pat_tokens(&assign.left, out);
            if let Some(token) = default_value_token(&assign.right) {
                out.push(token);
            }
        }
        Pat::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                collect_pat_tokens(elem, out);
            }
        }
        Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ObjectPatProp::Assign(assign) => {
                        out.push(assign.key.sym.to_string());
                        if let Some(value) = &assign.value
                            && let Some(token) = default_value_token(value)
                        {
                            out.push(token);
                        }
                    }
                    ObjectPatProp::KeyValue(kv) => {
                        match &kv.key {
                            PropName::Ident(ident) => out.push(ident.sym.to_string()),
                            PropName::Str(s) => {
                                if let Some(token) = string_literal_token(&s.value) {
                                    out.push(token);
                                }
                            }
                            _ => {}
                        }
                        collect_pat_tokens(&kv.value, out);
                    }
                    ObjectPatProp::Rest(rest) => collect_pat_tokens(&rest.arg, out),
                }
            }
        }
        _ => {}
    }
}

/// Fuse the three token groups into the field's final value: parameters and
/// their defaults first, then body identifiers, then literals, deduplicated on
/// first occurrence so the result is byte-identical across extractions of the
/// same source.
///
/// Priority is the cap's tie-breaker, not a ranking the cloud sees — a
/// truncated function keeps the parameters that name it over the hundredth
/// identifier of its body. See `IDENTIFIER_TOKEN_CEILING` for the floor that
/// keeps literals reachable.
fn build_tokens(params: &[String], identifiers: &[String], literals: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for (group, ceiling) in [
        (params, MAX_FUNCTION_TOKENS),
        (identifiers, IDENTIFIER_TOKEN_CEILING),
        (literals, MAX_FUNCTION_TOKENS),
    ] {
        for token in group {
            if out.len() >= ceiling {
                break;
            }
            if seen.insert(token.clone()) {
                out.push(token.clone());
            }
        }
    }
    out
}

/// Walks a function body and records every call expression's callee, plus the
/// raw material for `FunctionDefinition::tokens` — every identifier and
/// property name read, and every string or numeric literal written.
///
/// Nested functions and arrows are walked too: their calls sit inside the
/// enclosing function's body and count as its dependencies. Entering a nested
/// class clears the `this` context, so a `this.x()` written inside a class
/// declared in the body resolves to nothing rather than to the outer class.
struct CalleeCollector<'a> {
    source_map: &'a swc_common::SourceMap,
    enclosing_class: Option<String>,
    out: Vec<CalleeRef>,
    /// Identifiers and property names, in source order, before dedupe.
    identifiers: Vec<String>,
    /// String and numeric literals, in source order, before dedupe.
    literals: Vec<String>,
}

impl CalleeCollector<'_> {
    /// Strip the wrappers that sit between a callee position and the
    /// identifier it names — `(foo)()`, `foo!()`, `(foo as F)()`.
    fn unwrap_expr(expr: &Expr) -> &Expr {
        match expr {
            Expr::Paren(paren) => Self::unwrap_expr(&paren.expr),
            Expr::TsNonNull(non_null) => Self::unwrap_expr(&non_null.expr),
            Expr::TsAs(as_expr) => Self::unwrap_expr(&as_expr.expr),
            Expr::TsSatisfies(sat) => Self::unwrap_expr(&sat.expr),
            Expr::TsTypeAssertion(assertion) => Self::unwrap_expr(&assertion.expr),
            other => other,
        }
    }

    fn line(&self, span: swc_common::Span) -> u32 {
        if span.is_dummy() {
            return 0;
        }
        self.source_map.lookup_char_pos(span.lo).line as u32
    }

    /// Record the callee of one call site. Anything that is not a bare
    /// identifier or a single-level member access on an identifier or `this`
    /// (`a.b.c()`, `super.x()`, `getHandler()()`) is deliberately dropped: it
    /// cannot be resolved to a definition without type information, and a
    /// guess here is exactly the false edge this pass exists to remove.
    fn record(&mut self, callee: &Expr, span: swc_common::Span) {
        let line = self.line(span);
        match Self::unwrap_expr(callee) {
            Expr::Ident(ident) => self.out.push(CalleeRef {
                name: ident.sym.to_string(),
                shape: CalleeShape::Bare,
                line,
            }),
            Expr::Member(member) => {
                let name = match &member.prop {
                    MemberProp::Ident(prop) => prop.sym.to_string(),
                    MemberProp::PrivateName(prop) => format!("#{}", prop.name),
                    MemberProp::Computed(_) => return,
                };
                match Self::unwrap_expr(&member.obj) {
                    Expr::This(_) => {
                        if let Some(class) = self.enclosing_class.clone() {
                            self.out.push(CalleeRef {
                                name,
                                shape: CalleeShape::ThisMember(class),
                                line,
                            });
                        }
                    }
                    Expr::Ident(obj) => self.out.push(CalleeRef {
                        name,
                        shape: CalleeShape::Member(obj.sym.to_string()),
                        line,
                    }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Visit for CalleeCollector<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(expr) = &call.callee {
            self.record(expr, call.span);
        }
        call.visit_children_with(self);
    }

    /// `a?.b()` and `foo?.()` parse as an optional call, not a `CallExpr`.
    fn visit_opt_call(&mut self, call: &OptCall) {
        self.record(&call.callee, call.span);
        call.visit_children_with(self);
    }

    fn visit_class(&mut self, class: &Class) {
        let prev = self.enclosing_class.take();
        class.visit_children_with(self);
        self.enclosing_class = prev;
    }

    /// Every binding, reference and type name written in the body.
    fn visit_ident(&mut self, ident: &Ident) {
        self.identifiers.push(ident.sym.to_string());
        ident.visit_children_with(self);
    }

    /// Property positions — `a.budgetBytes`, `{ budgetBytes: 1 }` — parse as
    /// `IdentName`, not `Ident`, so they need their own arm to be seen.
    fn visit_ident_name(&mut self, ident: &IdentName) {
        self.identifiers.push(ident.sym.to_string());
        ident.visit_children_with(self);
    }

    fn visit_lit(&mut self, lit: &Lit) {
        if let Some(token) = literal_token(lit) {
            self.literals.push(token);
        }
        lit.visit_children_with(self);
    }

    /// Template literals carry paths, URLs and header names as often as plain
    /// string literals do, and their static chunks are literals by any other
    /// name. The interpolations are ordinary expressions and are picked up by
    /// the arms above.
    fn visit_tpl(&mut self, tpl: &Tpl) {
        for quasi in &tpl.quasis {
            let raw = match &quasi.cooked {
                Some(cooked) => cooked.as_str(),
                None => quasi.raw.as_str(),
            };
            if let Some(token) = string_literal_token(raw) {
                self.literals.push(token);
            }
        }
        tpl.visit_children_with(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OwnerType {
    App(String),
    Router(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mount {
    pub parent: OwnerType, // App or Router doing the .use
    pub child: OwnerType,  // Router being mounted
    pub prefix: String,    // Path prefix for this mount
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Named,
    Default,
    Namespace,
}

#[derive(Debug, Clone)]
pub struct ImportedSymbol {
    #[allow(dead_code)]
    pub local_name: String,
    #[allow(dead_code)]
    pub imported_name: String,
    pub source: String,
    pub kind: SymbolKind,
}

/// Lightweight extractor focused only on import symbols.
/// Used by the multi-agent pipeline for import resolution.
#[derive(Debug)]
pub struct ImportSymbolExtractor {
    pub imported_symbols: HashMap<String, ImportedSymbol>,
}

impl ImportSymbolExtractor {
    pub fn new() -> Self {
        Self {
            imported_symbols: HashMap::new(),
        }
    }
}

impl Default for ImportSymbolExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Visit for ImportSymbolExtractor {
    fn visit_import_decl(&mut self, import: &ImportDecl) {
        let source = import.src.value.to_string();

        for specifier in &import.specifiers {
            match specifier {
                ImportSpecifier::Named(named) => {
                    let local_name = named.local.sym.to_string();
                    let imported_name = match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(str)) => str.value.to_string(),
                        None => local_name.clone(),
                    };
                    self.imported_symbols.insert(
                        local_name.to_string(),
                        ImportedSymbol {
                            local_name: local_name.clone(),
                            imported_name,
                            source: source.clone(),
                            kind: SymbolKind::Named,
                        },
                    );
                }
                ImportSpecifier::Default(default) => {
                    let local_name = default.local.sym.to_string();
                    self.imported_symbols.insert(
                        local_name.to_string(),
                        ImportedSymbol {
                            local_name: local_name.clone(),
                            imported_name: local_name,
                            source: source.clone(),
                            kind: SymbolKind::Default,
                        },
                    );
                }
                ImportSpecifier::Namespace(namespace) => {
                    let local_name = namespace.local.sym.to_string();
                    self.imported_symbols.insert(
                        local_name.to_string(),
                        ImportedSymbol {
                            local_name: local_name.clone(),
                            imported_name: local_name,
                            source: source.clone(),
                            kind: SymbolKind::Namespace,
                        },
                    );
                }
            }
        }
    }
}

/// Extracts locally-declared type symbols for validation. Collects every
/// declaration form that a payload type annotation can legitimately name:
/// type aliases, interfaces, classes, and enums. Classes and enums matter for
/// pub/sub payloads in particular, where event payloads are often declared as
/// local event classes; missing them would make the AST reject check null
/// legitimate same-file symbols.
#[derive(Debug, Default)]
pub struct TypeSymbolExtractor {
    pub type_symbols: HashSet<String>,
}

impl TypeSymbolExtractor {
    pub fn new() -> Self {
        Self {
            type_symbols: HashSet::new(),
        }
    }
}

impl Visit for TypeSymbolExtractor {
    fn visit_ts_type_alias_decl(&mut self, alias: &TsTypeAliasDecl) {
        self.type_symbols.insert(alias.id.sym.to_string());
        alias.visit_children_with(self);
    }

    fn visit_ts_interface_decl(&mut self, interface: &TsInterfaceDecl) {
        self.type_symbols.insert(interface.id.sym.to_string());
        interface.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, class: &ClassDecl) {
        self.type_symbols.insert(class.ident.sym.to_string());
        class.visit_children_with(self);
    }

    fn visit_ts_enum_decl(&mut self, ts_enum: &TsEnumDecl) {
        self.type_symbols.insert(ts_enum.id.sym.to_string());
        ts_enum.visit_children_with(self);
    }
}

/// Extractor for function definitions with type annotations.
/// Used to extract handler functions for type resolution in the multi-agent pipeline.
pub struct FunctionDefinitionExtractor {
    pub function_definitions: HashMap<String, FunctionDefinition>,
    /// Definition key → the call sites inside that definition's body, as
    /// written in the AST. Kept beside `function_definitions` rather than on
    /// `FunctionDefinition` because it is scan-local: `crate::call_graph`
    /// consumes it immediately after discovery to populate
    /// `FunctionDefinition::calls`, and nothing downstream sees it.
    pub callee_refs: HashMap<String, Vec<CalleeRef>>,
    current_file_path: PathBuf,
    source_map: swc_common::sync::Lrc<swc_common::SourceMap>,
    /// Names of functions that are exported (populated by visit_export_decl / visit_named_export)
    exported_names: HashSet<String>,
    /// Name of the class whose body is currently being visited. Class members
    /// are indexed as `Class.member`; None inside anonymous class expressions,
    /// whose members cannot be given a stable name.
    current_class: Option<String>,
    /// Member names of the current class that exist as BOTH a static and an
    /// instance member — the one case where `Class.member` would collide on a
    /// single key. The static side is keyed `Class.static.member` instead.
    current_class_collisions: HashSet<String>,
}

impl FunctionDefinitionExtractor {
    pub fn new(
        file_path: PathBuf,
        source_map: swc_common::sync::Lrc<swc_common::SourceMap>,
    ) -> Self {
        Self {
            function_definitions: HashMap::new(),
            callee_refs: HashMap::new(),
            current_file_path: file_path,
            source_map,
            exported_names: HashSet::new(),
            current_class: None,
            current_class_collisions: HashSet::new(),
        }
    }

    /// Walk one function body once, for both of the things a body is read for:
    /// its call sites (with the enclosing class, if any, so `this.x()` can be
    /// resolved later) and its retrieval tokens. `node` is a function body,
    /// never the whole file, so only that function's own material is recorded.
    fn walk_body<'a, N>(&'a self, node: &N) -> CalleeCollector<'a>
    where
        N: VisitWith<CalleeCollector<'a>>,
    {
        let mut collector = CalleeCollector {
            source_map: &self.source_map,
            enclosing_class: self.current_class.clone(),
            out: Vec::new(),
            identifiers: Vec::new(),
            literals: Vec::new(),
        };
        node.visit_with(&mut collector);
        collector
    }

    /// Record the call sites of a `Function` node (declaration, method,
    /// function expression) under `key`, and return its retrieval tokens (see
    /// `FunctionDefinition::tokens`). A body-less node — an overload signature,
    /// an abstract method — still yields its parameter tokens.
    fn record_fn_callees(&mut self, key: &str, function: &Function) -> Vec<String> {
        let mut params = Vec::new();
        for param in &function.params {
            collect_pat_tokens(&param.pat, &mut params);
        }
        let body = function.body.as_ref().map(|body| self.walk_body(body));
        let (refs, tokens) = match body {
            Some(collector) => (
                collector.out,
                build_tokens(&params, &collector.identifiers, &collector.literals),
            ),
            None => (Vec::new(), build_tokens(&params, &[], &[])),
        };
        self.callee_refs.insert(key.to_string(), refs);
        tokens
    }

    /// Record the call sites of an arrow function under `key`, and return its
    /// retrieval tokens.
    fn record_arrow_callees(&mut self, key: &str, arrow: &ArrowExpr) -> Vec<String> {
        let mut params = Vec::new();
        for pat in &arrow.params {
            collect_pat_tokens(pat, &mut params);
        }
        let collector = self.walk_body(&*arrow.body);
        let tokens = build_tokens(&params, &collector.identifiers, &collector.literals);
        self.callee_refs.insert(key.to_string(), collector.out);
        tokens
    }

    /// Extract source text from a span, capped at 2000 chars
    fn extract_source(&self, span: swc_common::Span) -> Option<String> {
        if span.is_dummy() {
            return None;
        }
        let src = self.source_map.span_to_snippet(span).ok()?;
        if src.len() > 2000 {
            if let Some((idx, _)) = src.char_indices().nth(2000) {
                Some(format!("{}...", &src[..idx]))
            } else {
                Some(src)
            }
        } else {
            Some(src)
        }
    }

    /// Get the line number for a span
    fn line_number(&self, span: swc_common::Span) -> u32 {
        if span.is_dummy() {
            return 0;
        }
        self.source_map.lookup_char_pos(span.lo).line as u32
    }

    /// Get the last line of a span (see `FunctionDefinition::end_line`).
    fn end_line(&self, span: swc_common::Span) -> u32 {
        if span.is_dummy() {
            return 0;
        }
        self.source_map.lookup_char_pos(span.hi).line as u32
    }

    /// Render a `TsTypeAnn` as raw TS source text (the text between `:` and
    /// the next token in the source file). Falls back to `None` if the span
    /// can't be resolved.
    fn type_ann_to_string(&self, type_ann: &TsTypeAnn) -> Option<String> {
        self.source_map
            .span_to_snippet(type_ann.type_ann.span())
            .ok()
    }

    /// Resolve a parameter `Pat` to its `(name, type annotation)`.
    ///
    /// Handles plain identifiers, rest params, defaulted params
    /// (`role: "a" | "b" = "x"`, annotation on the inner `left`), and
    /// object/array destructuring (`{ id }: { id: string }`, annotation on the
    /// pattern's own `type_ann`). Anything else falls through to the unnamed
    /// placeholder.
    fn pat_name_and_type(&self, pat: &Pat) -> (String, Option<TsTypeAnn>) {
        match pat {
            Pat::Ident(ident) => (
                ident.id.sym.to_string(),
                ident.type_ann.as_ref().map(|t| *t.clone()),
            ),
            Pat::Rest(rest) => {
                let rest_name = match &*rest.arg {
                    Pat::Ident(ident) => format!("...{}", ident.id.sym),
                    _ => "...rest".to_string(),
                };
                (rest_name, rest.type_ann.as_ref().map(|t| *t.clone()))
            }
            // A defaulted param (`role = "x"`); the annotation, if any, is on
            // the inner left pattern. Recurse so name and type are preserved.
            Pat::Assign(assign) => self.pat_name_and_type(&assign.left),
            // Destructuring params carry their annotation on the pattern's own
            // `type_ann`; recover it and reconstruct a readable binding name so the
            // param is named and (when annotated) reported explicit.
            Pat::Object(obj) => (
                Self::object_pat_name(obj),
                obj.type_ann.as_ref().map(|t| *t.clone()),
            ),
            Pat::Array(arr) => (
                Self::array_pat_name(arr),
                arr.type_ann.as_ref().map(|t| *t.clone()),
            ),
            _ => ("param".to_string(), None),
        }
    }

    /// Reconstruct a readable name for an object-destructuring param, e.g.
    /// `{ id, name }`. Nested value patterns collapse to their key.
    fn object_pat_name(obj: &ObjectPat) -> String {
        let keys: Vec<String> = obj
            .props
            .iter()
            .map(|prop| match prop {
                ObjectPatProp::Assign(a) => a.key.sym.to_string(),
                ObjectPatProp::KeyValue(kv) => match &kv.key {
                    PropName::Ident(i) => i.sym.to_string(),
                    PropName::Str(s) => s.value.to_string(),
                    _ => "_".to_string(),
                },
                ObjectPatProp::Rest(rest) => match &*rest.arg {
                    Pat::Ident(i) => format!("...{}", i.id.sym),
                    _ => "...rest".to_string(),
                },
            })
            .collect();
        format!("{{ {} }}", keys.join(", "))
    }

    /// Reconstruct a readable name for an array-destructuring param, e.g.
    /// `[a, b]`. Elisions and nested patterns collapse to `_`.
    fn array_pat_name(arr: &ArrayPat) -> String {
        let elems: Vec<String> = arr
            .elems
            .iter()
            .map(|elem| match elem {
                Some(Pat::Ident(i)) => i.id.sym.to_string(),
                Some(Pat::Rest(rest)) => match &*rest.arg {
                    Pat::Ident(i) => format!("...{}", i.id.sym),
                    _ => "...rest".to_string(),
                },
                _ => "_".to_string(),
            })
            .collect();
        format!("[{}]", elems.join(", "))
    }

    /// Build a `FunctionArgument` from a resolved `(name, type annotation)`.
    fn build_argument(&self, name: String, type_ann: Option<TsTypeAnn>) -> FunctionArgument {
        let type_string = type_ann.as_ref().and_then(|t| self.type_ann_to_string(t));
        FunctionArgument {
            name,
            type_ann,
            is_explicit: type_string.is_some(),
            type_string,
        }
    }

    /// Extract function arguments with their type annotations
    fn extract_arguments(&self, params: &[Param]) -> Vec<FunctionArgument> {
        params
            .iter()
            .map(|param| {
                let (name, type_ann) = self.pat_name_and_type(&param.pat);
                self.build_argument(name, type_ann)
            })
            .collect()
    }

    /// Extract arguments from arrow function parameters
    fn extract_arrow_arguments(&self, params: &[Pat]) -> Vec<FunctionArgument> {
        params
            .iter()
            .map(|pat| {
                let (name, type_ann) = self.pat_name_and_type(pat);
                self.build_argument(name, type_ann)
            })
            .collect()
    }

    /// Mark functions as exported based on collected export names.
    /// Call this after `module.visit_with()` completes.
    /// A class member (`Class.member`) is exported iff its class is.
    pub fn finalize_exports(&mut self) {
        for (name, def) in self.function_definitions.iter_mut() {
            let exported = self.exported_names.contains(name)
                || name
                    .split_once('.')
                    .is_some_and(|(class_name, _)| self.exported_names.contains(class_name));
            if exported {
                def.is_exported = true;
            }
        }
    }

    /// Resolve a class-member key to its name, if it has a statically-known one.
    /// String-literal names containing `.` are rejected: they would make the
    /// `Class.member` key ambiguous for everything that parses it
    /// (`finalize_exports`, the intent generator's class-evidence check).
    fn prop_name_to_string(key: &PropName) -> Option<String> {
        match key {
            PropName::Ident(ident) => Some(ident.sym.to_string()),
            PropName::Str(s) if !s.value.contains('.') => Some(s.value.to_string()),
            _ => None,
        }
    }

    /// Member names defined as BOTH a static and an instance member of the
    /// class — the one shape where `Class.member` would collide on a single
    /// map key. Private members are tracked with their `#` prefix, so they
    /// can never collide with a same-named public member.
    fn static_instance_collisions(class: &Class) -> HashSet<String> {
        let mut static_names = HashSet::new();
        let mut instance_names = HashSet::new();
        for member in &class.body {
            let (name, is_static) = match member {
                ClassMember::Method(m) => (Self::prop_name_to_string(&m.key), m.is_static),
                ClassMember::ClassProp(p) => (Self::prop_name_to_string(&p.key), p.is_static),
                ClassMember::PrivateMethod(m) => (Some(format!("#{}", m.key.name)), m.is_static),
                _ => continue,
            };
            if let Some(name) = name {
                if is_static {
                    static_names.insert(name);
                } else {
                    instance_names.insert(name);
                }
            }
        }
        static_names
            .intersection(&instance_names)
            .cloned()
            .collect()
    }

    /// Key for a class member. The instance member keeps `Class.member`; when
    /// a same-named static member also exists, the static side is keyed
    /// `Class.static.member` so neither definition silently overwrites the
    /// other. Parsers of these keys take the class from the FIRST `.` and the
    /// member from the LAST, so both forms resolve correctly.
    fn member_key(&self, class_name: &str, member_name: &str, is_static: bool) -> String {
        if is_static && self.current_class_collisions.contains(member_name) {
            format!("{class_name}.static.{member_name}")
        } else {
            format!("{class_name}.{member_name}")
        }
    }

    /// Insert a definition for a class member backed by a `Function` node
    /// (methods, private methods, and function-expression class props). The
    /// node is wrapped as an anonymous `FnExpr` so every downstream consumer
    /// of `FunctionNodeType` works unchanged.
    fn insert_method_definition(&mut self, name: String, function: &Function) {
        let tokens = self.record_fn_callees(&name, function);
        let arguments = self.extract_arguments(&function.params);
        let body_source = function
            .body
            .as_ref()
            .and_then(|b| self.extract_source(b.span));
        let line_number = self.line_number(function.span);
        let end_line = self.end_line(function.span);
        let return_type = function
            .return_type
            .as_ref()
            .and_then(|t| self.type_ann_to_string(t));
        self.function_definitions.insert(
            name.clone(),
            FunctionDefinition {
                name,
                file_path: self.current_file_path.clone(),
                node_type: FunctionNodeType::FunctionExpression(Box::new(FnExpr {
                    ident: None,
                    function: Box::new(function.clone()),
                })),
                arguments,
                body_source,
                is_exported: false, // Updated in a post-pass
                line_number,
                end_line,
                intent: None,
                calls: vec![],
                tokens,
                return_is_explicit: return_type.is_some(),
                return_type,
                signature: None,
                intent_input_hash: None,
            },
        );
    }

    /// Insert a definition for an arrow-initialized class prop
    /// (`handle = () => { ... }`).
    fn insert_arrow_definition(&mut self, name: String, arrow: &ArrowExpr) {
        let tokens = self.record_arrow_callees(&name, arrow);
        let arguments = self.extract_arrow_arguments(&arrow.params);
        let body_source = self.extract_source(arrow.span);
        let line_number = self.line_number(arrow.span);
        let end_line = self.end_line(arrow.span);
        let return_type = arrow
            .return_type
            .as_ref()
            .and_then(|t| self.type_ann_to_string(t));
        self.function_definitions.insert(
            name.clone(),
            FunctionDefinition {
                name,
                file_path: self.current_file_path.clone(),
                node_type: FunctionNodeType::ArrowFunction(Box::new(arrow.clone())),
                arguments,
                body_source,
                is_exported: false, // Updated in a post-pass
                line_number,
                end_line,
                intent: None,
                calls: vec![],
                tokens,
                return_is_explicit: return_type.is_some(),
                return_type,
                signature: None,
                intent_input_hash: None,
            },
        );
    }
}

impl Visit for FunctionDefinitionExtractor {
    /// Track `export function foo() {}` and `export const bar = ...`
    fn visit_export_decl(&mut self, export: &ExportDecl) {
        match &export.decl {
            Decl::Fn(fn_decl) => {
                self.exported_names.insert(fn_decl.ident.sym.to_string());
            }
            Decl::Var(var_decl) => {
                for decl in &var_decl.decls {
                    if let Pat::Ident(ident) = &decl.name {
                        self.exported_names.insert(ident.id.sym.to_string());
                    }
                }
            }
            Decl::Class(class_decl) => {
                self.exported_names.insert(class_decl.ident.sym.to_string());
            }
            _ => {}
        }
        // Continue visiting so visit_fn_decl / visit_var_declarator fire
        export.visit_children_with(self);
    }

    /// Track `export default function foo() {}` and `export default class Foo {}`
    fn visit_export_default_decl(&mut self, export: &ExportDefaultDecl) {
        match &export.decl {
            DefaultDecl::Fn(fn_expr) => {
                if let Some(ident) = &fn_expr.ident {
                    let name = ident.sym.to_string();
                    self.exported_names.insert(name.clone());
                    // Capture the function since visit_fn_decl won't fire for default exports
                    let tokens = self.record_fn_callees(&name, &fn_expr.function);
                    let arguments = self.extract_arguments(&fn_expr.function.params);
                    let body_source = fn_expr
                        .function
                        .body
                        .as_ref()
                        .and_then(|b| self.extract_source(b.span));
                    let line_number = self.line_number(fn_expr.function.span);
                    let end_line = self.end_line(fn_expr.function.span);
                    let return_type = fn_expr
                        .function
                        .return_type
                        .as_ref()
                        .and_then(|t| self.type_ann_to_string(t));
                    self.function_definitions.insert(
                        name.clone(),
                        FunctionDefinition {
                            name,
                            file_path: self.current_file_path.clone(),
                            node_type: FunctionNodeType::FunctionExpression(Box::new(
                                fn_expr.clone(),
                            )),
                            arguments,
                            body_source,
                            is_exported: true,
                            line_number,
                            end_line,
                            intent: None,
                            calls: vec![],
                            tokens,
                            return_is_explicit: return_type.is_some(),
                            return_type,
                            signature: None,
                            intent_input_hash: None,
                        },
                    );
                }
            }
            DefaultDecl::Class(class_expr) => {
                if let Some(ident) = &class_expr.ident {
                    self.exported_names.insert(ident.sym.to_string());
                }
            }
            _ => {}
        }
        export.visit_children_with(self);
    }

    /// Track `export default foo` (expression)
    fn visit_export_default_expr(&mut self, export: &ExportDefaultExpr) {
        if let Expr::Ident(ident) = &*export.expr {
            self.exported_names.insert(ident.sym.to_string());
        }
        export.visit_children_with(self);
    }

    /// Track `export { foo, bar }` named exports
    fn visit_named_export(&mut self, export: &NamedExport) {
        // Only track re-exports from local scope (no `from` source)
        if export.src.is_none() {
            for spec in &export.specifiers {
                if let ExportSpecifier::Named(named) = spec {
                    let name = match &named.orig {
                        ModuleExportName::Ident(ident) => ident.sym.to_string(),
                        ModuleExportName::Str(s) => s.value.to_string(),
                    };
                    self.exported_names.insert(name);
                }
            }
        }
    }

    fn visit_fn_decl(&mut self, fn_decl: &FnDecl) {
        let name = fn_decl.ident.sym.to_string();
        let tokens = self.record_fn_callees(&name, &fn_decl.function);
        let arguments = self.extract_arguments(&fn_decl.function.params);
        let body_source = fn_decl
            .function
            .body
            .as_ref()
            .and_then(|b| self.extract_source(b.span));
        let line_number = self.line_number(fn_decl.function.span);
        let end_line = self.end_line(fn_decl.function.span);
        let return_type = fn_decl
            .function
            .return_type
            .as_ref()
            .and_then(|t| self.type_ann_to_string(t));

        self.function_definitions.insert(
            name.clone(),
            FunctionDefinition {
                name,
                file_path: self.current_file_path.clone(),
                node_type: FunctionNodeType::FunctionDeclaration(Box::new(fn_decl.clone())),
                arguments,
                body_source,
                is_exported: false, // Updated in a post-pass
                line_number,
                end_line,
                intent: None,
                calls: vec![],
                tokens,
                return_is_explicit: return_type.is_some(),
                return_type,
                signature: None,
                intent_input_hash: None,
            },
        );

        // Continue visiting child nodes
        fn_decl.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, var_decl: &VarDeclarator) {
        // Handle: const myHandler = (req, res) => { ... }
        // Or: const myHandler = function(req, res) { ... }
        if let Pat::Ident(ident) = &var_decl.name {
            let name = ident.id.sym.to_string();

            if let Some(init) = &var_decl.init {
                match &**init {
                    Expr::Arrow(arrow) => {
                        let tokens = self.record_arrow_callees(&name, arrow);
                        let arguments = self.extract_arrow_arguments(&arrow.params);
                        let body_source = self.extract_source(arrow.span);
                        let line_number = self.line_number(arrow.span);
                        let end_line = self.end_line(arrow.span);
                        let return_type = arrow
                            .return_type
                            .as_ref()
                            .and_then(|t| self.type_ann_to_string(t));
                        self.function_definitions.insert(
                            name.clone(),
                            FunctionDefinition {
                                name,
                                file_path: self.current_file_path.clone(),
                                node_type: FunctionNodeType::ArrowFunction(Box::new(arrow.clone())),
                                arguments,
                                body_source,
                                is_exported: false,
                                line_number,
                                end_line,
                                intent: None,
                                calls: vec![],
                                tokens,
                                return_is_explicit: return_type.is_some(),
                                return_type,
                                signature: None,
                                intent_input_hash: None,
                            },
                        );
                    }
                    Expr::Fn(fn_expr) => {
                        let tokens = self.record_fn_callees(&name, &fn_expr.function);
                        let arguments = self.extract_arguments(&fn_expr.function.params);
                        let body_source = fn_expr
                            .function
                            .body
                            .as_ref()
                            .and_then(|b| self.extract_source(b.span));
                        let line_number = self.line_number(fn_expr.function.span);
                        let end_line = self.end_line(fn_expr.function.span);
                        let return_type = fn_expr
                            .function
                            .return_type
                            .as_ref()
                            .and_then(|t| self.type_ann_to_string(t));
                        self.function_definitions.insert(
                            name.clone(),
                            FunctionDefinition {
                                name,
                                file_path: self.current_file_path.clone(),
                                node_type: FunctionNodeType::FunctionExpression(Box::new(
                                    fn_expr.clone(),
                                )),
                                arguments,
                                body_source,
                                is_exported: false,
                                line_number,
                                end_line,
                                intent: None,
                                calls: vec![],
                                tokens,
                                return_is_explicit: return_type.is_some(),
                                return_type,
                                signature: None,
                                intent_input_hash: None,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }

        // Continue visiting child nodes
        var_decl.visit_children_with(self);
    }

    /// Track the enclosing class name so members can be indexed as `Class.member`.
    fn visit_class_decl(&mut self, class: &ClassDecl) {
        let prev = self.current_class.replace(class.ident.sym.to_string());
        let prev_collisions = std::mem::replace(
            &mut self.current_class_collisions,
            Self::static_instance_collisions(&class.class),
        );
        class.visit_children_with(self);
        self.current_class = prev;
        self.current_class_collisions = prev_collisions;
    }

    /// Class expressions (`const Foo = class { ... }`, `export default class Foo`).
    /// An anonymous class expression clears the tracked name: its members have
    /// no stable qualified name and must not be attributed to an outer class.
    fn visit_class_expr(&mut self, class: &ClassExpr) {
        let prev = match &class.ident {
            Some(ident) => self.current_class.replace(ident.sym.to_string()),
            None => self.current_class.take(),
        };
        let prev_collisions = std::mem::replace(
            &mut self.current_class_collisions,
            Self::static_instance_collisions(&class.class),
        );
        class.visit_children_with(self);
        self.current_class = prev;
        self.current_class_collisions = prev_collisions;
    }

    /// Index class methods (static and instance) as `Class.method` (see
    /// `member_key` for the static/instance collision case).
    /// Getters are included (`Class.name` — they carry real logic surprisingly
    /// often); setters are skipped so a getter/setter pair doesn't collide on
    /// one key.
    fn visit_class_method(&mut self, method: &ClassMethod) {
        if let Some(class_name) = self.current_class.clone()
            && !matches!(method.kind, MethodKind::Setter)
            && let Some(member_name) = Self::prop_name_to_string(&method.key)
        {
            let name = self.member_key(&class_name, &member_name, method.is_static);
            self.insert_method_definition(name, &method.function);
        }
        method.visit_children_with(self);
    }

    /// Index private methods as `Class.#method`.
    fn visit_private_method(&mut self, method: &PrivateMethod) {
        if let Some(class_name) = self.current_class.clone()
            && !matches!(method.kind, MethodKind::Setter)
        {
            let member_name = format!("#{}", method.key.name);
            let name = self.member_key(&class_name, &member_name, method.is_static);
            self.insert_method_definition(name, &method.function);
        }
        method.visit_children_with(self);
    }

    /// Index function-valued class props (`handle = () => { ... }`) as
    /// `Class.prop` — the common controller idiom that is not a
    /// `VarDeclarator` and so never reaches `visit_var_declarator`.
    fn visit_class_prop(&mut self, prop: &ClassProp) {
        if let Some(class_name) = self.current_class.clone()
            && let Some(member_name) = Self::prop_name_to_string(&prop.key)
            && let Some(init) = &prop.value
        {
            let name = self.member_key(&class_name, &member_name, prop.is_static);
            match &**init {
                Expr::Arrow(arrow) => self.insert_arrow_definition(name, arrow),
                Expr::Fn(fn_expr) => self.insert_method_definition(name, &fn_expr.function),
                _ => {}
            }
        }
        prop.visit_children_with(self);
    }

    /// Capture anonymous closures passed as arguments to method calls.
    /// e.g. `app.get("/users", async (req, res) => { ... })` → name: "GET_users_handler"
    /// e.g. `emitter.on("data", (chunk) => { ... })` → name: "on_data_handler"
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Some((method_name, first_str_arg)) = extract_call_context(call) {
            // Look for function/arrow arguments (skip the first string arg)
            for arg in &call.args {
                match &*arg.expr {
                    Expr::Arrow(arrow) => {
                        let synthetic_name =
                            derive_handler_name(&method_name, first_str_arg.as_deref());
                        // Don't overwrite named functions already captured
                        if !self.function_definitions.contains_key(&synthetic_name) {
                            let tokens = self.record_arrow_callees(&synthetic_name, arrow);
                            let arguments = self.extract_arrow_arguments(&arrow.params);
                            let body_source = self.extract_source(arrow.span);
                            let line_number = self.line_number(arrow.span);
                            let end_line = self.end_line(arrow.span);
                            let return_type = arrow
                                .return_type
                                .as_ref()
                                .and_then(|t| self.type_ann_to_string(t));
                            self.function_definitions.insert(
                                synthetic_name.clone(),
                                FunctionDefinition {
                                    name: synthetic_name,
                                    file_path: self.current_file_path.clone(),
                                    node_type: FunctionNodeType::ArrowFunction(Box::new(
                                        arrow.clone(),
                                    )),
                                    arguments,
                                    body_source,
                                    is_exported: false,
                                    line_number,
                                    end_line,
                                    intent: None,
                                    calls: vec![],
                                    tokens,
                                    return_is_explicit: return_type.is_some(),
                                    return_type,
                                    signature: None,
                                    intent_input_hash: None,
                                },
                            );
                        }
                    }
                    Expr::Fn(fn_expr) => {
                        let synthetic_name =
                            derive_handler_name(&method_name, first_str_arg.as_deref());
                        if !self.function_definitions.contains_key(&synthetic_name) {
                            let tokens = self.record_fn_callees(&synthetic_name, &fn_expr.function);
                            let arguments = self.extract_arguments(&fn_expr.function.params);
                            let body_source = fn_expr
                                .function
                                .body
                                .as_ref()
                                .and_then(|b| self.extract_source(b.span));
                            let line_number = self.line_number(fn_expr.function.span);
                            let end_line = self.end_line(fn_expr.function.span);
                            let return_type = fn_expr
                                .function
                                .return_type
                                .as_ref()
                                .and_then(|t| self.type_ann_to_string(t));
                            self.function_definitions.insert(
                                synthetic_name.clone(),
                                FunctionDefinition {
                                    name: synthetic_name,
                                    file_path: self.current_file_path.clone(),
                                    node_type: FunctionNodeType::FunctionExpression(Box::new(
                                        fn_expr.clone(),
                                    )),
                                    arguments,
                                    body_source,
                                    is_exported: false,
                                    line_number,
                                    end_line,
                                    intent: None,
                                    calls: vec![],
                                    tokens,
                                    return_is_explicit: return_type.is_some(),
                                    return_type,
                                    signature: None,
                                    intent_input_hash: None,
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        // Continue visiting child nodes
        call.visit_children_with(self);
    }
}

/// Extract the method name and optional first string argument from a call expression.
/// e.g. `app.get("/users", handler)` → Some(("get", Some("/users")))
/// e.g. `emitter.on("data", handler)` → Some(("on", Some("data")))
/// e.g. `doSomething(handler)` → Some(("doSomething", None))
fn extract_call_context(call: &CallExpr) -> Option<(String, Option<String>)> {
    let method_name = match &call.callee {
        Callee::Expr(expr) => match &**expr {
            Expr::Member(member) => {
                if let MemberProp::Ident(ident) = &member.prop {
                    Some(ident.sym.to_string())
                } else {
                    None
                }
            }
            Expr::Ident(ident) => Some(ident.sym.to_string()),
            _ => None,
        },
        _ => None,
    }?;

    // Get the first string argument if present
    let first_str = call.args.first().and_then(|arg| match &*arg.expr {
        Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
        Expr::Tpl(tpl) => {
            // Template literal — extract the first quasi
            tpl.quasis.first().map(|q| q.raw.to_string())
        }
        _ => None,
    });

    // Only capture if there's at least one function argument
    let has_fn_arg = call
        .args
        .iter()
        .any(|arg| matches!(&*arg.expr, Expr::Arrow(_) | Expr::Fn(_)));

    if has_fn_arg {
        Some((method_name, first_str))
    } else {
        None
    }
}

/// Derive a handler name from the method and path/event.
/// e.g. ("get", Some("/users/:id")) → "get_users_id_handler"
/// e.g. ("on", Some("data")) → "on_data_handler"
/// e.g. ("use", None) → "use_handler"
fn derive_handler_name(method: &str, first_arg: Option<&str>) -> String {
    let base = match first_arg {
        Some(arg) => {
            let cleaned: String = arg
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let trimmed = cleaned.trim_matches('_');
            if trimmed.is_empty() {
                method.to_string()
            } else {
                format!("{}_{}", method, trimmed)
            }
        }
        None => method.to_string(),
    };
    format!("{}_handler", base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    fn parse_ts(source: &str) -> (Lrc<SourceMap>, Module) {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");
        (cm, module)
    }

    fn extract(source: &str) -> HashMap<String, FunctionDefinition> {
        let (cm, module) = parse_ts(source);
        let mut extractor = FunctionDefinitionExtractor::new(PathBuf::from("test.ts"), cm);
        module.visit_with(&mut extractor);
        extractor.finalize_exports();
        extractor.function_definitions
    }

    #[test]
    fn type_symbol_extractor_collects_all_type_declaration_forms() {
        let (_cm, module) = parse_ts(
            "type Alias = string;\n\
             interface Shape { x: number }\n\
             export class OrderPlacedEvent { id: string; }\n\
             class LocalEvent { n: number; }\n\
             export enum OrderStatus { Placed }\n\
             const enum Inline { A }\n",
        );
        let mut extractor = TypeSymbolExtractor::new();
        module.visit_with(&mut extractor);
        for symbol in [
            "Alias",
            "Shape",
            "OrderPlacedEvent",
            "LocalEvent",
            "OrderStatus",
            "Inline",
        ] {
            assert!(
                extractor.type_symbols.contains(symbol),
                "should collect {symbol}"
            );
        }
    }

    #[test]
    fn captures_body_source_for_function_declaration() {
        let defs = extract("function greet(name: string) { return `Hello ${name}`; }");
        let def = defs.get("greet").expect("should find greet");
        assert!(def.body_source.is_some(), "should have body_source");
        assert!(
            def.body_source.as_ref().unwrap().contains("Hello"),
            "body should contain function text"
        );
    }

    #[test]
    fn captures_body_source_for_arrow_function() {
        let defs = extract("const add = (a: number, b: number) => { return a + b; };");
        let def = defs.get("add").expect("should find add");
        assert!(def.body_source.is_some(), "should have body_source");
        assert!(
            def.body_source.as_ref().unwrap().contains("a + b"),
            "body should contain arrow text"
        );
    }

    #[test]
    fn detects_export_function_declaration() {
        let defs = extract("export function hello() { return 1; }");
        let def = defs.get("hello").expect("should find hello");
        assert!(def.is_exported, "export function should be marked exported");
    }

    #[test]
    fn detects_export_default_function() {
        let defs = extract("export default function main() { return 1; }");
        let def = defs.get("main").expect("should find main");
        assert!(
            def.is_exported,
            "export default function should be marked exported"
        );
    }

    #[test]
    fn detects_export_default_expression() {
        let defs = extract("function setup() { return 1; }\nexport default setup;");
        let def = defs.get("setup").expect("should find setup");
        assert!(
            def.is_exported,
            "export default expr should be marked exported"
        );
    }

    #[test]
    fn detects_export_const_arrow() {
        let defs = extract("export const foo = () => { return 42; };");
        let def = defs.get("foo").expect("should find foo");
        assert!(
            def.is_exported,
            "export const arrow should be marked exported"
        );
    }

    #[test]
    fn detects_named_export() {
        let defs =
            extract("function bar() { return 1; }\nfunction baz() { return 2; }\nexport { bar };");
        let bar = defs.get("bar").expect("should find bar");
        let baz = defs.get("baz").expect("should find baz");
        assert!(
            bar.is_exported,
            "bar should be marked exported via named export"
        );
        assert!(!baz.is_exported, "baz should NOT be exported");
    }

    #[test]
    fn non_exported_function_is_not_marked() {
        let defs = extract("function internal() { return 'private'; }");
        let def = defs.get("internal").expect("should find internal");
        assert!(
            !def.is_exported,
            "non-exported function should not be marked"
        );
    }

    #[test]
    fn captures_line_number() {
        let defs = extract("function first() { return 1; }");
        let def = defs.get("first").expect("should find first");
        assert!(def.line_number > 0, "line_number should be positive");
    }

    #[test]
    fn caps_body_source_at_2000_chars() {
        // Generate a function with a body > 2000 chars
        let long_body = "x".repeat(2500);
        let source = format!(
            "function big() {{ const s = \"{}\"; return s; }}",
            long_body
        );
        let defs = extract(&source);
        let def = defs.get("big").expect("should find big");
        let body = def.body_source.as_ref().expect("should have body_source");
        assert!(
            body.len() <= 2003, // 2000 + "..."
            "body_source should be capped (got {} chars)",
            body.len()
        );
        assert!(body.ends_with("..."), "capped body should end with ...");
    }

    #[test]
    fn captures_anonymous_arrow_in_method_call() {
        let defs = extract(
            r#"
            const app = { get: (path: string, handler: any) => {} };
            app.get("/users", (req: any, res: any) => { res.json({ id: 1 }); });
            "#,
        );
        // "/users" → "_users" → trimmed → "users" → "get_users_handler"
        let handler_keys: Vec<_> = defs.keys().filter(|k| k.contains("handler")).collect();
        assert!(
            !handler_keys.is_empty(),
            "should have captured at least one handler, got keys: {:?}",
            defs.keys().collect::<Vec<_>>()
        );
        let def = defs
            .get("get_users_handler")
            .expect("should capture route handler");
        assert!(def.body_source.is_some());
        assert!(def.body_source.as_ref().unwrap().contains("res.json"));
    }

    #[test]
    fn captures_anonymous_fn_in_method_call() {
        let defs = extract(
            r#"
            const router = { post: (path: string, handler: any) => {} };
            router.post("/orders", function(req: any, res: any) { res.send("ok"); });
            "#,
        );
        let def = defs
            .get("post_orders_handler")
            .expect("should capture route handler");
        assert!(def.body_source.is_some());
    }

    #[test]
    fn anonymous_handler_has_line_number() {
        let defs = extract(
            r#"
            const app = { get: (path: string, handler: any) => {} };
            app.get("/health", () => { return "ok"; });
            "#,
        );
        let def = defs
            .get("get_health_handler")
            .expect("should capture handler");
        assert!(def.line_number > 0);
    }

    #[test]
    fn does_not_capture_non_function_args() {
        let defs = extract(
            r#"
            const app = { get: (path: string) => {} };
            app.get("/static");
            "#,
        );
        // No function arg → no handler captured
        assert!(
            !defs.keys().any(|k| k.contains("handler")),
            "should not capture calls without function args"
        );
    }

    #[test]
    fn captures_argument_type_strings_on_function_declaration() {
        let defs = extract("function greet(name: string, count: number) { return name; }");
        let def = defs.get("greet").expect("should find greet");
        assert_eq!(def.arguments.len(), 2);
        assert_eq!(def.arguments[0].name, "name");
        assert_eq!(def.arguments[0].type_string.as_deref(), Some("string"));
        assert_eq!(def.arguments[1].name, "count");
        assert_eq!(def.arguments[1].type_string.as_deref(), Some("number"));
    }

    #[test]
    fn captures_argument_type_strings_on_arrow_function() {
        let defs = extract("const add = (a: number, b: number) => a + b;");
        let def = defs.get("add").expect("should find add");
        assert_eq!(def.arguments[0].type_string.as_deref(), Some("number"));
        assert_eq!(def.arguments[1].type_string.as_deref(), Some("number"));
    }

    #[test]
    fn captures_complex_generic_argument_type() {
        let defs =
            extract("function handle(req: Request<{ id: string }>, res: Response) { return; }");
        let def = defs.get("handle").expect("should find handle");
        assert_eq!(
            def.arguments[0].type_string.as_deref(),
            Some("Request<{ id: string }>")
        );
        assert_eq!(def.arguments[1].type_string.as_deref(), Some("Response"));
    }

    #[test]
    fn argument_without_annotation_has_no_type_string() {
        let defs = extract("function bare(x) { return x; }");
        let def = defs.get("bare").expect("should find bare");
        assert!(def.arguments[0].type_string.is_none());
    }

    #[test]
    fn recovers_defaulted_union_param_on_function_declaration() {
        let defs = extract(
            r#"function pick(role: "producer" | "consumer" = "producer") { return role; }"#,
        );
        let def = defs.get("pick").expect("should find pick");
        assert_eq!(def.arguments.len(), 1);
        assert_eq!(def.arguments[0].name, "role");
        assert_eq!(
            def.arguments[0].type_string.as_deref(),
            Some(r#""producer" | "consumer""#)
        );
        assert!(
            def.arguments[0].is_explicit,
            "defaulted param with an annotation should be explicit"
        );
    }

    #[test]
    fn recovers_defaulted_union_param_on_arrow_function() {
        let defs = extract(r#"const pick = (role: "producer" | "consumer" = "producer") => role;"#);
        let def = defs.get("pick").expect("should find pick");
        assert_eq!(def.arguments.len(), 1);
        assert_eq!(def.arguments[0].name, "role");
        assert_eq!(
            def.arguments[0].type_string.as_deref(),
            Some(r#""producer" | "consumer""#)
        );
        assert!(
            def.arguments[0].is_explicit,
            "defaulted param with an annotation should be explicit"
        );
    }

    #[test]
    fn recovers_defaulted_param_name_without_annotation() {
        // A defaulted param with no annotation keeps its name but stays
        // implicit (faithful: there is no declared type to recover).
        let defs = extract(r#"function pick(role = "producer") { return role; }"#);
        let def = defs.get("pick").expect("should find pick");
        assert_eq!(def.arguments[0].name, "role");
        assert!(def.arguments[0].type_string.is_none());
        assert!(!def.arguments[0].is_explicit);
    }

    #[test]
    fn recovers_object_destructured_param_on_function_declaration() {
        let defs = extract(r#"function f({ id }: { id: string }) { return id; }"#);
        let def = defs.get("f").expect("should find f");
        assert_eq!(def.arguments.len(), 1);
        assert_eq!(def.arguments[0].name, "{ id }");
        assert!(def.arguments[0].is_explicit);
        assert!(
            def.arguments[0]
                .type_string
                .as_deref()
                .is_some_and(|t| t.contains("id") && t.contains("string")),
            "object param should keep its annotation, got {:?}",
            def.arguments[0].type_string
        );
    }

    #[test]
    fn recovers_object_destructured_param_on_arrow_function() {
        let defs = extract(r#"const f = ({ id, name }: { id: string; name: string }) => id;"#);
        let def = defs.get("f").expect("should find f");
        assert_eq!(def.arguments[0].name, "{ id, name }");
        assert!(def.arguments[0].is_explicit);
    }

    #[test]
    fn recovers_array_destructured_param() {
        let defs = extract(r#"function f([a, b]: [number, number]) { return a + b; }"#);
        let def = defs.get("f").expect("should find f");
        assert_eq!(def.arguments[0].name, "[a, b]");
        assert!(def.arguments[0].is_explicit);
        assert!(
            def.arguments[0]
                .type_string
                .as_deref()
                .is_some_and(|t| t.contains("number")),
            "array param should keep its tuple annotation, got {:?}",
            def.arguments[0].type_string
        );
    }

    #[test]
    fn annotated_argument_is_marked_explicit() {
        let defs = extract("function greet(name: string) { return name; }");
        let def = defs.get("greet").expect("should find greet");
        assert!(
            def.arguments[0].is_explicit,
            "annotated param should be explicit"
        );
    }

    #[test]
    fn unannotated_argument_is_not_explicit() {
        let defs = extract("function bare(x) { return x; }");
        let def = defs.get("bare").expect("should find bare");
        assert!(
            !def.arguments[0].is_explicit,
            "unannotated param should not be explicit at parse time"
        );
    }

    #[test]
    fn annotated_return_is_marked_explicit() {
        let defs = extract("function greet(name: string): string { return name; }");
        let def = defs.get("greet").expect("should find greet");
        assert!(
            def.return_is_explicit,
            "annotated return should be explicit"
        );
    }

    #[test]
    fn unannotated_return_is_not_explicit() {
        let defs = extract("function bare() { return 1; }");
        let def = defs.get("bare").expect("should find bare");
        assert!(
            !def.return_is_explicit,
            "unannotated return should not be explicit at parse time"
        );
    }

    #[test]
    fn captures_rest_argument_type_on_function_declaration() {
        let defs = extract("function variadic(...args: string[]) { return args; }");
        let def = defs.get("variadic").expect("should find variadic");
        assert_eq!(def.arguments[0].name, "...args");
        assert_eq!(def.arguments[0].type_string.as_deref(), Some("string[]"));
    }

    #[test]
    fn captures_rest_argument_type_on_arrow_function() {
        let defs = extract("const variadic = (...args: number[]) => args;");
        let def = defs.get("variadic").expect("should find variadic");
        assert_eq!(def.arguments[0].name, "...args");
        assert_eq!(def.arguments[0].type_string.as_deref(), Some("number[]"));
    }

    #[test]
    fn captures_return_type_on_function_declaration() {
        let defs = extract("function greet(name: string): string { return name; }");
        let def = defs.get("greet").expect("should find greet");
        assert_eq!(def.return_type.as_deref(), Some("string"));
    }

    #[test]
    fn captures_return_type_on_arrow_function() {
        let defs = extract("const add = (a: number, b: number): number => a + b;");
        let def = defs.get("add").expect("should find add");
        assert_eq!(def.return_type.as_deref(), Some("number"));
    }

    #[test]
    fn captures_return_type_on_function_expression() {
        let defs = extract("const greet = function(name: string): string { return name; };");
        let def = defs.get("greet").expect("should find greet");
        assert_eq!(def.return_type.as_deref(), Some("string"));
    }

    #[test]
    fn captures_promise_return_type() {
        let defs =
            extract("async function fetchUser(id: string): Promise<User> { return null as any; }");
        let def = defs.get("fetchUser").expect("should find fetchUser");
        assert_eq!(def.return_type.as_deref(), Some("Promise<User>"));
    }

    #[test]
    fn no_return_type_when_unannotated() {
        let defs = extract("function bare() { return 1; }");
        let def = defs.get("bare").expect("should find bare");
        assert!(def.return_type.is_none());
    }

    #[test]
    fn captures_return_type_on_anonymous_handler() {
        let defs = extract(
            r#"
            const app = { get: (path: string, handler: any) => {} };
            app.get("/users", (req: Request, res: Response): void => { res.json({}); });
            "#,
        );
        let def = defs
            .get("get_users_handler")
            .expect("should capture handler");
        assert_eq!(def.arguments[0].type_string.as_deref(), Some("Request"));
        assert_eq!(def.arguments[1].type_string.as_deref(), Some("Response"));
        assert_eq!(def.return_type.as_deref(), Some("void"));
    }

    #[test]
    fn captures_return_type_on_export_default_function() {
        let defs = extract("export default function main(): number { return 1; }");
        let def = defs.get("main").expect("should find main");
        assert_eq!(def.return_type.as_deref(), Some("number"));
    }

    #[test]
    fn function_definition_serializes_types_to_json() {
        let defs = extract("function greet(name: string): Promise<string> { return name as any; }");
        let def = defs.get("greet").expect("should find greet");
        let json = serde_json::to_value(def).expect("serialize");
        assert_eq!(json["return_type"], "Promise<string>");
        assert_eq!(json["arguments"][0]["name"], "name");
        assert_eq!(json["arguments"][0]["type_string"], "string");
    }

    #[test]
    fn captures_end_line_for_each_function_form() {
        // Line numbers are 1-based and the leading newline makes them easy to
        // read off directly: `alpha` opens on 2 and closes on 5.
        let defs = extract(
            "\n\
             function alpha(a: number): number {\n\
             \x20 const b = a + 1;\n\
             \x20 return b;\n\
             }\n\
             const beta = (x: number) => {\n\
             \x20 return x * 2;\n\
             };\n\
             const gamma = function (y: number) {\n\
             \x20 return y - 1;\n\
             };\n\
             function delta() { return 0; }\n",
        );

        for (name, start, end) in [
            ("alpha", 2, 5),
            ("beta", 6, 8),
            ("gamma", 9, 11),
            // Single-line function: start and end are the same line, which is
            // the case that would expose an off-by-one from `span.hi`.
            ("delta", 12, 12),
        ] {
            let def = defs.get(name).unwrap_or_else(|| panic!("captured {name}"));
            assert_eq!(def.line_number, start, "{name} line_number");
            assert_eq!(def.end_line, end, "{name} end_line");
            assert!(def.end_line >= def.line_number, "{name} end after start");
        }
    }

    #[test]
    fn end_line_is_omitted_from_json_when_unset() {
        // `skip_serializing_if` is what keeps existing index blobs byte-stable
        // for definitions whose span could not be resolved.
        let defs = extract("function greet(): void {}");
        let mut def = defs.get("greet").expect("should find greet").clone();
        assert!(def.end_line > 0, "a real span should populate end_line");

        let json = serde_json::to_value(&def).expect("serialize");
        assert_eq!(json["end_line"], def.end_line);

        def.end_line = 0;
        let json = serde_json::to_value(&def).expect("serialize");
        assert!(json.get("end_line").is_none(), "0 must not serialize");
    }

    #[test]
    fn derive_handler_name_works() {
        assert_eq!(
            super::derive_handler_name("get", Some("/users/:id")),
            "get_users__id_handler"
        );
        assert_eq!(
            super::derive_handler_name("on", Some("data")),
            "on_data_handler"
        );
        assert_eq!(super::derive_handler_name("use", None), "use_handler");
    }

    #[test]
    fn captures_static_and_instance_class_methods() {
        let defs = extract(
            "export class OrderPresenter {\n\
               static isFinished(status: string): boolean { return status === \"DONE\"; }\n\
               public static async findOrder(id: string): Promise<string> { return id; }\n\
               format(count: number): string { return `count: ${count}`; }\n\
             }",
        );
        let is_finished = defs
            .get("OrderPresenter.isFinished")
            .expect("static method should be indexed");
        assert!(
            is_finished.is_exported,
            "member of an exported class is exported"
        );
        assert_eq!(is_finished.return_type.as_deref(), Some("boolean"));
        assert_eq!(is_finished.arguments.len(), 1);
        assert_eq!(is_finished.arguments[0].name, "status");
        assert_eq!(
            is_finished.arguments[0].type_string.as_deref(),
            Some("string")
        );
        assert!(is_finished.body_source.as_ref().unwrap().contains("DONE"));
        assert!(is_finished.line_number > 0);
        assert!(is_finished.end_line >= is_finished.line_number);
        assert!(
            defs.contains_key("OrderPresenter.findOrder"),
            "public static async method should be indexed"
        );
        assert!(
            defs.contains_key("OrderPresenter.format"),
            "instance method should be indexed"
        );
    }

    #[test]
    fn members_of_unexported_class_are_not_exported() {
        let defs = extract("class Local { run(): number { return 1; } }");
        let def = defs
            .get("Local.run")
            .expect("instance method should be indexed");
        assert!(!def.is_exported);
    }

    #[test]
    fn captures_arrow_class_props_and_private_methods() {
        let defs = extract(
            "export class JobController {\n\
               handle = async (req: string): Promise<void> => { console.log(req); };\n\
               #reload(): void { console.log(\"reloading\"); }\n\
             }",
        );
        let handle = defs
            .get("JobController.handle")
            .expect("arrow-valued class prop should be indexed");
        assert_eq!(handle.arguments.len(), 1);
        assert_eq!(handle.arguments[0].name, "req");
        assert!(handle.is_exported);
        assert!(
            defs.contains_key("JobController.#reload"),
            "private method should be indexed"
        );
    }

    #[test]
    fn getter_is_indexed_and_setter_is_skipped() {
        let defs = extract(
            "class Run {\n\
               get finished(): boolean { return this.status === \"DONE\"; }\n\
               set finished(v: boolean) { console.log(v); }\n\
             }",
        );
        let def = defs.get("Run.finished").expect("getter should be indexed");
        assert_eq!(def.return_type.as_deref(), Some("boolean"));
        assert!(
            def.body_source.as_ref().unwrap().contains("this.status"),
            "getter body, not setter body, should win the key"
        );
    }

    #[test]
    fn default_exported_class_members_are_exported() {
        let defs = extract("export default class Worker { run(): number { return 1; } }");
        let def = defs.get("Worker.run").expect("method should be indexed");
        assert!(def.is_exported);
    }

    #[test]
    fn anonymous_class_expression_members_are_skipped() {
        let defs = extract("const Foo = class { run(): number { return 1; } };");
        assert!(
            defs.is_empty(),
            "anonymous class members have no stable name; got {:?}",
            defs.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn module_functions_inside_and_after_class_are_unaffected() {
        let defs = extract(
            "class Svc { run(): number { return helper(); } }\n\
             function helper(): number { return 2; }",
        );
        assert!(defs.contains_key("Svc.run"));
        let helper = defs.get("helper").expect("module function still indexed");
        assert!(!helper.name.contains('.'));
    }

    #[test]
    fn static_instance_collision_gets_distinct_keys() {
        let defs = extract(
            "export class Counter {\n\
               static create(seed: number): Counter { return new Counter(); }\n\
               create(step: number): number { return step + 1; }\n\
             }",
        );
        let instance = defs
            .get("Counter.create")
            .expect("instance member keeps the plain key");
        assert_eq!(instance.arguments[0].name, "step");
        let stat = defs
            .get("Counter.static.create")
            .expect("colliding static member gets the qualified key");
        assert_eq!(stat.arguments[0].name, "seed");
        assert!(
            stat.is_exported && instance.is_exported,
            "export propagation parses the class from the FIRST dot"
        );
    }

    #[test]
    fn non_colliding_static_keeps_plain_key() {
        let defs = extract(
            "class Presenter { static isFinished(s: string): boolean { return s === \"DONE\"; } }",
        );
        assert!(defs.contains_key("Presenter.isFinished"));
        assert!(!defs.keys().any(|k| k.contains(".static.")));
    }

    #[test]
    fn string_literal_member_containing_dot_is_skipped() {
        let defs = extract(
            "class Api {\n\
               \"a.b\"(): number { return 1; }\n\
               \"plain\"(): number { return 2; }\n\
             }",
        );
        assert!(
            defs.contains_key("Api.plain"),
            "dot-free string keys are indexed"
        );
        assert_eq!(
            defs.len(),
            1,
            "a string key containing '.' would break the Class.member invariant; got {:?}",
            defs.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn nestjs_controller_fixture_yields_class_methods() {
        // In-tree reproduction of carrick#483: this fixture previously
        // produced zero function definitions.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/nestjs-api/users.controller.ts"
        ))
        .expect("fixture should exist");
        let defs = extract(&source);
        for name in [
            "UsersController.findAll",
            "UsersController.findOne",
            "UsersController.create",
        ] {
            assert!(
                defs.contains_key(name),
                "missing {name}; got {:?}",
                defs.keys().collect::<Vec<_>>()
            );
        }
        assert!(
            defs.get("UsersController.findAll").unwrap().is_exported,
            "exported controller's methods are exported"
        );
    }

    /// The motivating case for `tokens` (carrick-cloud#434): the constant a
    /// question is asked in lives in a default parameter value, which the
    /// stored signature drops and the stripped body never carried.
    #[test]
    fn literal_default_param_yields_both_the_name_and_the_value() {
        let defs = extract(
            "export function formatOrientation(indexMd: string, budgetBytes = 1500) {\n\
             \x20 return indexMd.slice(0, budgetBytes);\n\
             }\n",
        );
        let tokens = &defs.get("formatOrientation").expect("definition").tokens;
        assert!(
            tokens.contains(&"budgetBytes".to_string()),
            "parameter name missing: {tokens:?}"
        );
        assert!(
            tokens.contains(&"1500".to_string()),
            "literal default missing: {tokens:?}"
        );
        // Parameters come first, so a truncated function keeps them.
        assert_eq!(tokens[0], "indexMd", "params lead the vec: {tokens:?}");
    }

    /// Destructured defaults are the same shape written differently, and the
    /// options-bag idiom puts them there far more often than in a plain param.
    #[test]
    fn destructured_default_yields_both_the_name_and_the_value() {
        let defs = extract(
            "export const trim = ({ maxBytes = 2048, label = \"orientation\" }) => maxBytes;\n",
        );
        let tokens = &defs.get("trim").expect("definition").tokens;
        for expected in ["maxBytes", "2048", "label", "orientation"] {
            assert!(
                tokens.contains(&expected.to_string()),
                "{expected} missing: {tokens:?}"
            );
        }
    }

    /// Negative defaults read as one literal, not as an operator applied to a
    /// number, so `-1` is queryable as written.
    #[test]
    fn negative_literal_default_keeps_its_sign() {
        let defs = extract("function seek(offset = -1) { return offset; }\n");
        let tokens = &defs.get("seek").expect("definition").tokens;
        assert!(
            tokens.contains(&"-1".to_string()),
            "signed default missing: {tokens:?}"
        );
    }

    #[test]
    fn body_identifiers_and_property_names_are_collected() {
        let defs = extract(
            "function readBudget(config) {\n\
             \x20 const ceiling = config.budgetBytes;\n\
             \x20 return clampToCeiling(ceiling);\n\
             }\n",
        );
        let tokens = &defs.get("readBudget").expect("definition").tokens;
        for expected in ["config", "ceiling", "budgetBytes", "clampToCeiling"] {
            assert!(
                tokens.contains(&expected.to_string()),
                "{expected} missing: {tokens:?}"
            );
        }
    }

    #[test]
    fn string_and_numeric_literals_are_collected_within_the_length_cap() {
        let long = "x".repeat(MAX_LITERAL_TOKEN_LEN + 1);
        let source = format!(
            "function emit() {{\n\
             \x20 const header = \"x-carrick-run\";\n\
             \x20 const padded = \"  spaced  \";\n\
             \x20 const blank = \"\";\n\
             \x20 const whitespace = \"   \";\n\
             \x20 const oversized = \"{long}\";\n\
             \x20 const path = `/v1/orientation`;\n\
             \x20 return [header, padded, blank, whitespace, oversized, path, 1500, 0.5];\n\
             }}\n"
        );
        let defs = extract(&source);
        let tokens = &defs.get("emit").expect("definition").tokens;
        for expected in ["x-carrick-run", "spaced", "/v1/orientation", "1500", "0.5"] {
            assert!(
                tokens.contains(&expected.to_string()),
                "{expected} missing: {tokens:?}"
            );
        }
        for rejected in ["", "   ", "  spaced  ", long.as_str()] {
            assert!(
                !tokens.contains(&rejected.to_string()),
                "{rejected:?} should not be a token: {tokens:?}"
            );
        }
    }

    #[test]
    fn tokens_are_deduplicated_and_byte_stable_across_extractions() {
        let source = "function repeat(name) {\n\
                      \x20 log(name, \"retry\");\n\
                      \x20 log(name, \"retry\");\n\
                      \x20 return name;\n\
                      }\n";
        let first = extract(source);
        let second = extract(source);
        let tokens = &first.get("repeat").expect("definition").tokens;
        let mut unique = tokens.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), tokens.len(), "duplicate tokens: {tokens:?}");
        assert_eq!(
            tokens,
            &second.get("repeat").expect("definition").tokens,
            "two extractions of one source must be byte-equal"
        );
    }

    #[test]
    fn a_function_with_nothing_notable_has_no_tokens_and_serialises_none() {
        let defs = extract("function noop() {}\n");
        let def = defs.get("noop").expect("definition");
        assert!(def.tokens.is_empty(), "unexpected tokens: {:?}", def.tokens);
        let json = serde_json::to_value(def).expect("serialize");
        assert!(
            json.get("tokens").is_none(),
            "an empty vec must serialise away, not ship as []"
        );
    }

    /// A body big enough to exhaust the budget must still surface its
    /// literals — the identifier ceiling exists precisely so the constant a
    /// question names is not crowded out by the four hundredth local.
    #[test]
    fn cap_is_enforced_and_reserves_room_for_literals() {
        let mut body = String::new();
        for i in 0..400 {
            body.push_str(&format!("  const local{i} = other{i};\n"));
        }
        // The literal a question would name, written AFTER the identifier
        // flood that would otherwise consume the whole budget.
        body.push_str("  const marker = \"needle-token\";\n");
        for i in 0..200 {
            body.push_str(&format!("  send({});\n", 9000 + i));
        }
        let source = format!("function pathological(seed) {{\n{body}}}\n");
        let defs = extract(&source);
        let tokens = &defs.get("pathological").expect("definition").tokens;
        assert_eq!(
            tokens.len(),
            MAX_FUNCTION_TOKENS,
            "cap not enforced: {}",
            tokens.len()
        );
        assert_eq!(tokens[0], "seed", "params keep priority under the cap");
        assert!(
            tokens.contains(&"needle-token".to_string()),
            "literal crowded out by identifiers: {tokens:?}"
        );
    }
}
