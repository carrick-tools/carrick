extern crate swc_common;
extern crate swc_ecma_parser;

use crate::receiver_origin::origin_root;
use crate::receiver_type::{
    ReceiverTypes, annotated_type_ident, class_field_types, constructed_type_ident,
};
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
    /// Explicit `?` on the parameter itself, not on a destructured member.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_optional: bool,
    /// A top-level initializer. This does not make a parameter before a
    /// required parameter omittable; callers must still supply that position.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_default: bool,
    /// Exact initializer source, when its span can be read. Never evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_rest: bool,
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

impl Spanned for FunctionNodeType {
    fn span(&self) -> swc_common::Span {
        match self {
            Self::ArrowFunction(arrow) => arrow.span,
            Self::FunctionDeclaration(function) => function.function.span,
            Self::FunctionExpression(function) => function.function.span,
            Self::Placeholder => swc_common::DUMMY_SP,
        }
    }
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
    /// What this function switches on, when it reads one request field and
    /// answers differently for each literal value of it (carrick#831).
    ///
    /// A projection of the service-level `dispatch_tables` array, joined onto
    /// this row by `handler_name` + `line_number` — the array is the record
    /// (a table whose handler the function index never saw is still carried
    /// there), and this is the copy a reader of a function row finds without
    /// having to know the array exists.
    ///
    /// From the model, and `None` on every function that does not do this,
    /// which is nearly all of them. Skipped on the wire when absent: a
    /// function row is the bulk of the blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_table: Option<crate::dispatch::DispatchTable>,
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
    /// `this.field.foo(...)`, inside the named class: the receiver is a FIELD
    /// of that class, so the class body says what it is (carrick#782).
    ThisFieldMember {
        /// The class whose body the call site sits in.
        class: String,
        /// The field the receiver names — `#field` for a private one.
        field: String,
    },
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
    /// Exact AST call span, used only during discovery to assign ownership.
    /// A line can contain separate calls in separate nested functions.
    pub span: swc_common::Span,
    /// Lexically resolved receiver facts; never serialized.
    pub receiver: ReceiverBinding,
}

/// The binding visible at one member call, independently of its indexed owner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiverBinding {
    /// An import can be resolved through this file's import table.
    pub imported_name: Option<String>,
    /// An indexed member declared within this receiver's lexical binding.
    pub local_member: Option<String>,
    /// A declared class settles resolution even when that class is unavailable.
    pub declared: bool,
    pub origin: Option<String>,
}

/// SWC gives each lexical binding a distinct Id, including unknown and
/// destructured shadows. Keep those identities until receiver facts have
/// been attached to exact call spans, then discard the resolved AST.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LexicalBinding {
    receiver: ReceiverBinding,
    declared_id: Option<Id>,
    origin_root: Option<Id>,
    direct_span: Option<swc_common::Span>,
}

#[derive(Default)]
struct LexicalReceivers {
    bindings: HashMap<Id, LexicalBinding>,
    calls: HashMap<swc_common::Span, Id>,
}

impl LexicalReceivers {
    fn record(&mut self, id: Id, binding: LexicalBinding) {
        self.bindings
            .entry(id)
            .and_modify(|previous| {
                // Agreement is per fact: conflicting initializer origins
                // cannot erase an explicit class annotation that agrees.
                if previous.receiver.imported_name != binding.receiver.imported_name {
                    previous.receiver.imported_name = None;
                }
                if previous.declared_id != binding.declared_id {
                    previous.declared_id = None;
                }
                if previous.receiver.origin != binding.receiver.origin {
                    previous.receiver.origin = None;
                }
                if previous.origin_root != binding.origin_root {
                    previous.origin_root = None;
                }
                if previous.direct_span != binding.direct_span {
                    previous.direct_span = None;
                }
            })
            .or_insert(binding);
    }

    fn record_call(&mut self, expr: &Expr, span: swc_common::Span) {
        if let Expr::Member(member) = expr
            && let Expr::Ident(object) = &*member.obj
        {
            self.calls.insert(span, object.to_id());
        }
    }

    fn receiver(&self, id: &Id) -> ReceiverBinding {
        let Some(binding) = self.bindings.get(id) else {
            return ReceiverBinding::default();
        };
        let mut receiver = binding.receiver.clone();
        receiver.declared = binding.declared_id.is_some();
        let mut current = binding;
        let mut seen = HashSet::new();
        while receiver.origin.is_none() {
            let Some(root) = &current.origin_root else {
                break;
            };
            if !seen.insert(root) {
                break;
            }
            let Some(parent) = self.bindings.get(root) else {
                break;
            };
            receiver.origin = parent.receiver.origin.clone();
            current = parent;
        }
        receiver
    }
}

impl Visit for LexicalReceivers {
    fn visit_import_decl(&mut self, import: &ImportDecl) {
        for specifier in &import.specifiers {
            let (ident, type_only) = match specifier {
                ImportSpecifier::Named(named) => (&named.local, named.is_type_only),
                ImportSpecifier::Default(default) => (&default.local, false),
                ImportSpecifier::Namespace(namespace) => (&namespace.local, false),
            };
            self.record(
                ident.to_id(),
                LexicalBinding {
                    receiver: ReceiverBinding {
                        imported_name: Some(ident.sym.to_string()),
                        declared: false,
                        local_member: None,
                        origin: (!import.type_only && !type_only)
                            .then(|| import.src.value.to_string()),
                    },
                    ..Default::default()
                },
            );
        }
    }

    fn visit_class_decl(&mut self, class: &ClassDecl) {
        self.record(
            class.ident.to_id(),
            LexicalBinding {
                direct_span: Some(class.class.span),
                ..Default::default()
            },
        );
        class.visit_children_with(self);
    }

    fn visit_class_expr(&mut self, class: &ClassExpr) {
        if let Some(ident) = &class.ident {
            self.record(
                ident.to_id(),
                LexicalBinding {
                    direct_span: Some(class.class.span),
                    ..Default::default()
                },
            );
        }
        class.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            self.record(
                ident.id.to_id(),
                LexicalBinding {
                    declared_id: ident
                        .type_ann
                        .as_deref()
                        .and_then(annotated_type_ident)
                        .or_else(|| declarator.init.as_deref().and_then(constructed_type_ident))
                        .map(Ident::to_id),
                    origin_root: declarator
                        .init
                        .as_deref()
                        .and_then(origin_root)
                        .map(Ident::to_id),
                    direct_span: match declarator.init.as_deref() {
                        Some(Expr::Object(object)) => Some(object.span),
                        _ => None,
                    },
                    ..Default::default()
                },
            );
            // The declarator's binding was recorded above. Its initializer
            // may contain more declarations and calls, which still need a walk.
            declarator.init.visit_with(self);
        } else {
            declarator.visit_children_with(self);
        }
    }

    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.record(
            ident.id.to_id(),
            LexicalBinding {
                declared_id: ident
                    .type_ann
                    .as_deref()
                    .and_then(annotated_type_ident)
                    .map(Ident::to_id),
                ..Default::default()
            },
        );
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(expr) = &call.callee {
            self.record_call(expr, call.span);
        }
        call.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, call: &OptCall) {
        self.record_call(&call.callee, call.span);
        call.visit_children_with(self);
    }
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
/// Nested functions and arrows are walked too, preserving retrieval tokens
/// and calls in anonymous callbacks. Once all definitions are known, module
/// finalization keeps each call under its innermost indexed owner. Entering a
/// nested class clears the `this` context, so a `this.x()` written inside a class
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

    /// Record the callee of one call site. Four shapes are kept: a bare
    /// identifier, a member access on an identifier or on `this`, and
    /// `this.field.foo()` — a two-level chain whose root the enclosing class
    /// body declares (carrick#782).
    ///
    /// Everything else (`a.b.c()`, `super.x()`, `getHandler()()`) is
    /// deliberately dropped: it cannot be resolved to a definition without
    /// type information, and a guess here is exactly the false edge this pass
    /// exists to remove. `this.field` is the one two-level receiver the file
    /// itself makes a statement about.
    fn record(&mut self, callee: &Expr, span: swc_common::Span) {
        let line = self.line(span);
        match Self::unwrap_expr(callee) {
            Expr::Ident(ident) => self.out.push(CalleeRef {
                name: ident.sym.to_string(),
                shape: CalleeShape::Bare,
                line,
                span,
                receiver: ReceiverBinding::default(),
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
                                span,
                                receiver: ReceiverBinding::default(),
                            });
                        }
                    }
                    Expr::Ident(obj) => self.out.push(CalleeRef {
                        name,
                        shape: CalleeShape::Member(obj.sym.to_string()),
                        line,
                        span,
                        receiver: ReceiverBinding::default(),
                    }),
                    // `this.field.foo()`. Nothing wider: `a.b.c()` needs the
                    // VALUE of `a.b`, which no statement in the file gives.
                    Expr::Member(inner) => {
                        let Some(class) = self.enclosing_class.clone() else {
                            return;
                        };
                        if !matches!(Self::unwrap_expr(&inner.obj), Expr::This(_)) {
                            return;
                        }
                        let field = match &inner.prop {
                            MemberProp::Ident(prop) => prop.sym.to_string(),
                            MemberProp::PrivateName(prop) => format!("#{}", prop.name),
                            MemberProp::Computed(_) => return,
                        };
                        self.out.push(CalleeRef {
                            name,
                            shape: CalleeShape::ThisFieldMember { class, field },
                            line,
                            span,
                            receiver: ReceiverBinding::default(),
                        });
                    }
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolKind {
    Named,
    Default,
    Namespace,
}

/// One import fact: what a file imported, from where, under what local name.
///
/// Ordered and hashable so a whole-service sample of import facts can be
/// collected into a `BTreeSet` — the framework-detect body is built from one
/// (see `framework_detector::FrameworkDetectionInput`) and must be identical
/// across two scans of an unchanged checkout (carrick#954).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// Class name → the classes that class declares its own FIELDS to be
    /// ([`crate::receiver_type::class_field_types`], carrick#782). Keyed by
    /// class rather than by definition because a field belongs to the class,
    /// and every method of it sees the same field.
    pub field_types: HashMap<String, ReceiverTypes>,
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
    /// Members whose AST accessibility prevents callers outside the class hierarchy.
    inaccessible_members: HashSet<String>,
}

impl FunctionDefinitionExtractor {
    pub fn new(
        file_path: PathBuf,
        source_map: swc_common::sync::Lrc<swc_common::SourceMap>,
    ) -> Self {
        Self {
            function_definitions: HashMap::new(),
            callee_refs: HashMap::new(),
            field_types: HashMap::new(),
            current_file_path: file_path,
            source_map,
            exported_names: HashSet::new(),
            current_class: None,
            current_class_collisions: HashSet::new(),
            inaccessible_members: HashSet::new(),
        }
    }

    /// Each exact call site belongs to its smallest enclosing indexed
    /// function (carrick#931). Callbacks without a definition row keep their
    /// calls under the nearest indexed enclosing function.
    /// Retrieval tokens still include nested bodies.
    fn finalize_call_owners(&mut self) {
        let mut owners: HashMap<swc_common::Span, (u32, String)> = HashMap::new();
        for (key, calls) in &self.callee_refs {
            let Some(definition) = self.function_definitions.get(key) else {
                continue;
            };
            let span = definition.node_type.span();
            if span.is_dummy() {
                continue;
            }
            let candidate = (span.hi.0 - span.lo.0, key.clone());
            for call in calls {
                if call.span.is_dummy() {
                    continue;
                }
                owners
                    .entry(call.span)
                    .and_modify(|owner| {
                        // The key breaks ties deterministically if aliases
                        // index the same function body more than once.
                        if candidate < *owner {
                            *owner = candidate.clone();
                        }
                    })
                    .or_insert_with(|| candidate.clone());
            }
        }
        for (key, calls) in &mut self.callee_refs {
            calls.retain(|call| owners.get(&call.span).is_none_or(|(_, owner)| owner == key));
        }
    }

    /// Resolve lexical identities on a private clone so scope marks cannot
    /// affect the AST retained for signatures or other extraction passes.
    fn finalize_receiver_bindings(&mut self, module: &Module) {
        use swc_common::{GLOBALS, Globals, Mark};
        use swc_ecma_transforms_base::resolver;
        use swc_ecma_visit::VisitMutWith;

        // parse_file already resolves its AST; standalone parsing does not.
        // Marks belong to the Globals that created them, so neither those
        // marks nor their numeric indexes can be reused in a fresh resolver.
        struct ClearContexts;
        impl swc_ecma_visit::VisitMut for ClearContexts {
            fn visit_mut_syntax_context(&mut self, context: &mut swc_common::SyntaxContext) {
                *context = swc_common::SyntaxContext::empty();
            }
        }
        let mut resolved = module.clone();
        resolved.visit_mut_with(&mut ClearContexts);
        let mut lexical = LexicalReceivers::default();
        GLOBALS.set(&Globals::new(), || {
            resolved.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), true));
            resolved.visit_with(&mut lexical);
        });
        for call in self.callee_refs.values_mut().flatten() {
            if let Some(id) = lexical.calls.get(&call.span) {
                call.receiver = lexical.receiver(id);
                let target_id = lexical
                    .bindings
                    .get(id)
                    .and_then(|binding| binding.declared_id.as_ref())
                    .unwrap_or(id);
                if call.receiver.declared {
                    call.receiver.imported_name = lexical
                        .bindings
                        .get(target_id)
                        .and_then(|binding| binding.receiver.imported_name.clone());
                }
                if let Some(span) = lexical
                    .bindings
                    .get(target_id)
                    .and_then(|binding| binding.direct_span)
                {
                    // A same-named class or object in another scope cannot
                    // supply this member. Require the indexed body to lie
                    // within the actual receiver declaration.
                    for key in [
                        format!("{}.{}", target_id.0, call.name),
                        format!("{}.static.{}", target_id.0, call.name),
                    ] {
                        if let Some(definition) = self.function_definitions.get(&key) {
                            let member_span = definition.node_type.span();
                            if span.lo <= member_span.lo && member_span.hi <= span.hi {
                                call.receiver.local_member = Some(key);
                                break;
                            }
                        }
                    }
                }
            }
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

    /// Record what one class declares its fields to be. Two classes of one
    /// name in a single file state two different things about the same key,
    /// so the table is emptied rather than picked between.
    fn record_field_types(&mut self, name: &str, class: &Class) {
        let fields = class_field_types(class);
        match self.field_types.get(name) {
            Some(existing) if *existing == fields => {}
            Some(_) => {
                self.field_types
                    .insert(name.to_string(), ReceiverTypes::new());
            }
            None => {
                self.field_types.insert(name.to_string(), fields);
            }
        }
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
                let (name, _) = self.pat_name_and_type(&rest.arg);
                let rest_name = format!("...{name}");
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

    /// Capture the top-level parameter pattern independently of its binding members.
    fn build_argument(&self, pat: &Pat) -> FunctionArgument {
        let (name, type_ann) = self.pat_name_and_type(pat);
        let is_optional = match pat {
            Pat::Ident(ident) => ident.id.optional,
            Pat::Object(object) => object.optional,
            Pat::Array(array) => array.optional,
            _ => false,
        };
        let default_value = match pat {
            Pat::Assign(assign) => self.source_map.span_to_snippet(assign.right.span()).ok(),
            _ => None,
        };
        let type_string = type_ann.as_ref().and_then(|t| self.type_ann_to_string(t));
        FunctionArgument {
            name,
            type_ann,
            is_explicit: type_string.is_some(),
            type_string,
            is_optional,
            has_default: matches!(pat, Pat::Assign(_)),
            default_value,
            is_rest: matches!(pat, Pat::Rest(_)),
        }
    }

    /// Extract function arguments with their type annotations
    fn extract_arguments(&self, params: &[Param]) -> Vec<FunctionArgument> {
        params
            .iter()
            .map(|param| self.build_argument(&param.pat))
            .collect()
    }

    /// Extract arguments from arrow function parameters
    fn extract_arrow_arguments(&self, params: &[Pat]) -> Vec<FunctionArgument> {
        params.iter().map(|pat| self.build_argument(pat)).collect()
    }

    /// Mark functions as exported based on collected export names.
    /// Call this after `module.visit_with()` completes.
    /// A class member (`Class.member`) is exported only when its class is
    /// exported and the member is accessible outside the class hierarchy.
    pub fn finalize_exports(&mut self) {
        for (name, def) in self.function_definitions.iter_mut() {
            let exported = self.exported_names.contains(name)
                || name
                    .split_once('.')
                    .is_some_and(|(class_name, _)| self.exported_names.contains(class_name));
            if self.inaccessible_members.contains(name) {
                def.is_exported = false;
            } else if exported {
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
                ClassMember::PrivateProp(p) => (Some(format!("#{}", p.key.name)), p.is_static),
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
                dispatch_table: None,
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
                dispatch_table: None,
            },
        );
    }
    /// Index the function-valued members of an object literal the module
    /// EXPORTS, keyed `owner.member` — the same key space class members use.
    ///
    /// A module whose surface is an object rather than a set of bindings is an
    /// ordinary shape, not a framework's: `export default { async fetch(req)
    /// {} }` is the whole of a request handler on more than one runtime, and
    /// `export const handlers = { list, create }` is a route table. Neither is
    /// a `FnDecl`, a `VarDeclarator` or a class member, so before carrick#830
    /// a file whose only functions were written this way produced no function
    /// definition at all: no signature, no intent, nothing in the index for
    /// anything else to point at.
    ///
    /// Bounded to the export surface on purpose. An object literal passed as an
    /// argument, or bound to a local the module keeps to itself, is a value the
    /// module uses rather than one it offers, and collecting every callback in
    /// every options object would flood the function index — and its intents —
    /// with things nothing can call.
    fn collect_exported_object_members(&mut self, owner: &str, object: &ObjectLit, depth: usize) {
        // A route table nests (`{ v1: { list() {} } }`); a deep object is data.
        const MAX_DEPTH: usize = 3;
        if depth > MAX_DEPTH {
            return;
        }
        for prop in &object.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            match &**prop {
                Prop::Method(method) => {
                    if let Some(key) = Self::prop_name_to_string(&method.key) {
                        self.insert_method_definition(format!("{owner}.{key}"), &method.function);
                    }
                }
                Prop::KeyValue(kv) => {
                    let Some(key) = Self::prop_name_to_string(&kv.key) else {
                        continue;
                    };
                    let name = format!("{owner}.{key}");
                    match &*kv.value {
                        Expr::Arrow(arrow) => self.insert_arrow_definition(name, arrow),
                        Expr::Fn(fn_expr) => self.insert_method_definition(name, &fn_expr.function),
                        Expr::Object(nested) => {
                            self.collect_exported_object_members(&name, nested, depth + 1)
                        }
                        // `{ list: listThings }` names a function the module
                        // declared elsewhere. It has a definition already, at
                        // its own key; what the table adds is that the module
                        // OFFERS it. A second row for the same body would
                        // double the function count and re-bill its intent.
                        Expr::Ident(ident) => {
                            self.exported_names.insert(ident.sym.to_string());
                        }
                        _ => {}
                    }
                }
                // `{ list, create }`: the shorthand form of the line above.
                Prop::Shorthand(ident) => {
                    self.exported_names.insert(ident.sym.to_string());
                }
                _ => {}
            }
        }
    }

    /// The export a CommonJS assignment writes to, if it writes to one.
    ///
    /// `exports.handler` and `module.exports.handler` are the named export
    /// `handler`, keyed like any other named export. `module.exports` is the
    /// default export, keyed `default` exactly as `export default` is, so a
    /// member of it lands on `default.<member>` on either module system.
    fn cjs_export_key(target: &AssignTarget) -> Option<String> {
        let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = target else {
            return None;
        };
        let MemberProp::Ident(prop) = &member.prop else {
            return None;
        };
        match &*member.obj {
            // `module.exports = …`
            Expr::Ident(object) if &*object.sym == "module" && &*prop.sym == "exports" => {
                Some("default".to_string())
            }
            // `exports.handler = …`
            Expr::Ident(object) if &*object.sym == "exports" => Some(prop.sym.to_string()),
            // `module.exports.handler = …`
            Expr::Member(inner) => {
                let Expr::Ident(root) = &*inner.obj else {
                    return None;
                };
                let MemberProp::Ident(middle) = &inner.prop else {
                    return None;
                };
                (&*root.sym == "module" && &*middle.sym == "exports").then(|| prop.sym.to_string())
            }
            _ => None,
        }
    }

    /// Record what a CommonJS export assignment puts on the module's surface.
    ///
    /// A function value is a definition under the exported name. An object is
    /// a member bag, read exactly as the ESM one is. An identifier names a
    /// function the module declared elsewhere: that already has a definition at
    /// its own key, and what the assignment adds is that the module offers it.
    fn record_cjs_export(&mut self, name: String, value: &Expr) {
        match value {
            Expr::Arrow(arrow) => {
                self.insert_arrow_definition(name.clone(), arrow);
                self.exported_names.insert(name);
            }
            Expr::Fn(fn_expr) => {
                self.insert_method_definition(name.clone(), &fn_expr.function);
                self.exported_names.insert(name);
            }
            Expr::Object(object) => {
                self.exported_names.insert(name.clone());
                self.collect_exported_object_members(&name, object, 0);
            }
            Expr::Ident(ident) => {
                self.exported_names.insert(ident.sym.to_string());
            }
            _ => {}
        }
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
                        let name = ident.id.sym.to_string();
                        self.exported_names.insert(name.clone());
                        // `export const handlers = { list, create() {} }`: an
                        // exported object's function members are part of the
                        // module's surface (carrick#830).
                        if let Some(init) = &decl.init
                            && let Expr::Object(object) = &**init
                        {
                            self.collect_exported_object_members(&name, object, 0);
                        }
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
                            dispatch_table: None,
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
        match &*export.expr {
            Expr::Ident(ident) => {
                self.exported_names.insert(ident.sym.to_string());
            }
            // `export default { async fetch(request) { … } }`: the module's
            // whole surface is one object, and its members are its functions
            // (carrick#830). `default` is what the module exports them under,
            // and naming them for it keeps the key the same on every host.
            Expr::Object(object) => {
                self.exported_names.insert("default".to_string());
                self.collect_exported_object_members("default", object, 0);
            }
            _ => {}
        }
        export.visit_children_with(self);
    }

    /// Track the CommonJS export surface: `exports.handler = …`,
    /// `module.exports.other = …`, `module.exports = { … }` and
    /// `module.exports = fn` (carrick#863). It is how every `.js` lambda states
    /// its entry point, and none of those shapes is a declaration, a
    /// function-initialised variable or a class member, so none of them
    /// produced a definition.
    ///
    /// Not restricted to the top level. An export written inside a branch or a
    /// setup function is still what the module offers, and the key comes from
    /// the exports member rather than from a binding name, so it means the same
    /// thing wherever it is written.
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let Some(name) = Self::cjs_export_key(&assign.left) {
            self.record_cjs_export(name, &assign.right);
        }
        assign.visit_children_with(self);
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
                dispatch_table: None,
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
                                dispatch_table: None,
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
                                dispatch_table: None,
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

    /// Finalize ownership and lexical receivers after discovering every body.
    fn visit_module(&mut self, module: &Module) {
        module.visit_children_with(self);
        self.finalize_call_owners();
        self.finalize_receiver_bindings(module);
    }

    /// Track the enclosing class name so members can be indexed as `Class.member`.
    fn visit_class_decl(&mut self, class: &ClassDecl) {
        self.record_field_types(class.ident.sym.as_ref(), &class.class);
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
            Some(ident) => {
                self.record_field_types(ident.sym.as_ref(), &class.class);
                self.current_class.replace(ident.sym.to_string())
            }
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
            if matches!(
                method.accessibility,
                Some(Accessibility::Private | Accessibility::Protected)
            ) {
                self.inaccessible_members.insert(name.clone());
            }
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
            self.inaccessible_members.insert(name.clone());
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
            if matches!(
                prop.accessibility,
                Some(Accessibility::Private | Accessibility::Protected)
            ) {
                self.inaccessible_members.insert(name.clone());
            }
            match &**init {
                Expr::Arrow(arrow) => self.insert_arrow_definition(name, arrow),
                Expr::Fn(fn_expr) => self.insert_method_definition(name, &fn_expr.function),
                _ => {}
            }
        }
        prop.visit_children_with(self);
    }

    /// ECMAScript private fields use a distinct AST node from TypeScript's
    /// accessibility-modified class properties, but retain the same callable facts.
    fn visit_private_prop(&mut self, prop: &PrivateProp) {
        if let Some(class_name) = self.current_class.clone()
            && let Some(init) = &prop.value
        {
            let member_name = format!("#{}", prop.key.name);
            let name = self.member_key(&class_name, &member_name, prop.is_static);
            self.inaccessible_members.insert(name.clone());
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
                                    dispatch_table: None,
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
                                    dispatch_table: None,
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
    fn parameter_facts_survive_signature_projection_and_storage() {
        let mut defs = extract(
            r#"
            export function plain(required: string, optional?: string): void {}
            export const arrow = (ordinal: number = 0, label: string, ...args: string[]): void => {};
            export class Service {
                method({ count = 1 }: { count?: number } = {}, [first]: string[] = ["a"]): void {}
            }
            export function nested({ count = 1 }: { count?: number }): void {}
            export function defaults(value = makeValue(1, "x")): void {}
            export function tupleRest(...[first, second]: [string, number]): void {}
            export function callbackDefault(callback: () => number = () => 0, label: string, ordinal: number = 0): void {}
            export function multilineDefault(value: string = `first
second`): void {}
        "#,
        );
        crate::signature_pass::populate_function_signatures(None, &mut defs, "/tmp");
        assert_eq!(
            defs["plain"].signature.as_deref(),
            Some("(required: string, optional?: string) => void")
        );
        assert_eq!(
            defs["arrow"].signature.as_deref(),
            Some("(ordinal: (number) | undefined, label: string, ...args: string[]) => void")
        );
        assert_eq!(
            defs["Service.method"].signature.as_deref(),
            Some("({ count }?: { count?: number }, [first]?: string[]) => void")
        );
        assert_eq!(
            defs["defaults"].signature.as_deref(),
            Some("(value?) => void")
        );

        assert_eq!(
            defs["tupleRest"].signature.as_deref(),
            Some("(...[first, second]: [string, number]) => void")
        );
        assert!(defs["tupleRest"].arguments[0].is_rest);

        assert_eq!(
            defs["callbackDefault"].signature.as_deref(),
            Some("(callback: (() => number) | undefined, label: string, ordinal?: number) => void")
        );
        assert_eq!(
            defs["multilineDefault"].signature.as_deref(),
            Some("(value?: string) => void")
        );
        assert_eq!(
            defs["multilineDefault"].arguments[0]
                .default_value
                .as_deref(),
            Some("`first\nsecond`")
        );

        let mut blob: crate::cloud_storage::CloudRepoData =
            serde_json::from_value(serde_json::json!({
                "repo_name": "example/service", "endpoints": [], "calls": [], "mounts": [],
                "apps": {}, "imported_handlers": [], "function_definitions": {},
                "last_updated": "2026-01-01T00:00:00Z", "commit_hash": "abc"
            }))
            .unwrap();
        blob.function_definitions = defs;
        let wire = serde_json::to_value(&blob).unwrap();
        let functions = &wire["function_definitions"];
        assert_eq!(
            functions["plain"]["arguments"][0],
            serde_json::json!({
                "name": "required", "type_string": "string", "is_explicit": true
            })
        );
        assert_eq!(
            functions["plain"]["arguments"][1],
            serde_json::json!({
                "name": "optional", "type_string": "string", "is_explicit": true, "is_optional": true
            })
        );
        assert_eq!(
            functions["arrow"]["arguments"][0],
            serde_json::json!({
                "name": "ordinal", "type_string": "number", "is_explicit": true,
                "has_default": true, "default_value": "0"
            })
        );
        assert_eq!(
            functions["arrow"]["arguments"][2],
            serde_json::json!({
                "name": "...args", "type_string": "string[]", "is_explicit": true, "is_rest": true
            })
        );
        assert!(
            functions["nested"]["arguments"][0]
                .get("has_default")
                .is_none()
        );
        assert!(
            functions["nested"]["arguments"][0]
                .get("is_optional")
                .is_none()
        );
        let stored = serde_json::to_vec(&blob).unwrap();
        let restored: crate::cloud_storage::CloudRepoData =
            serde_json::from_slice(&stored).unwrap();
        for (name, def) in &restored.function_definitions {
            assert_eq!(
                serde_json::to_value(&def.arguments).unwrap(),
                wire["function_definitions"][name]["arguments"]
            );
            assert_eq!(
                def.signature.as_deref(),
                wire["function_definitions"][name]["signature"].as_str()
            );
        }

        // Old records still read and stay sparse on write. No initializer or
        // optionality may be invented from a type or the argument name.
        let mut old = wire;
        for def in old["function_definitions"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            for arg in def["arguments"].as_array_mut().unwrap() {
                for field in ["is_optional", "has_default", "default_value", "is_rest"] {
                    arg.as_object_mut().unwrap().remove(field);
                }
            }
        }
        let restored: crate::cloud_storage::CloudRepoData =
            serde_json::from_value(old.clone()).unwrap();
        for (name, def) in &restored.function_definitions {
            assert_eq!(
                serde_json::to_value(&def.arguments).unwrap(),
                old["function_definitions"][name]["arguments"]
            );
            assert!(def.arguments.iter().all(|arg| !arg.is_optional
                && !arg.has_default
                && !arg.is_rest
                && arg.default_value.is_none()));
        }
    }

    #[test]
    fn call_ownership_preserves_distinct_spans_and_nested_tokens() {
        let (cm, module) = parse_ts(
            "function target() {}\n\
             function outer() { const label = 'café'; function nested() { target(); target?.(); } target(); }",
        );
        let mut extractor = FunctionDefinitionExtractor::new(PathBuf::from("test.ts"), cm);
        module.visit_with(&mut extractor);

        assert_eq!(extractor.callee_refs["outer"].len(), 1);
        assert_eq!(extractor.callee_refs["nested"].len(), 2);
        let spans: HashSet<_> = extractor
            .callee_refs
            .values()
            .flatten()
            .map(|call| {
                assert_eq!(call.name, "target");
                assert_eq!(call.line, 2);
                call.span
            })
            .collect();
        assert_eq!(spans.len(), 3);
        assert!(
            extractor.function_definitions["outer"]
                .tokens
                .contains(&"target".to_string())
        );
        assert!(
            extractor.function_definitions["outer"]
                .tokens
                .contains(&"café".to_string())
        );
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
    fn class_member_accessibility_controls_exports() {
        for export in [
            "export class Surface",
            "export default class Surface",
            "class Surface",
        ] {
            let source = format!(
                r#"{export} {{
                constructor() {{}}
                public run() {{}}
                implicit() {{}}
                private hidden() {{}}
                protected inherited() {{}}
                #secret() {{}}
                public static create() {{}}
                private static hiddenStatic() {{}}
                protected static inheritedStatic() {{}}
                static #secretStatic() {{}}
                public get readable() {{ return 1; }}
                private get hiddenValue() {{ return 1; }}
                protected get inheritedValue() {{ return 1; }}
                get #secretValue() {{ return 1; }}
                set readable(value: number) {{}}
                private set writeOnly(value: number) {{}}
                public arrow = () => 1;
                private hiddenArrow = () => 1;
                protected inheritedArrow = () => 1;
                private static hiddenFn = function() {{ return 1; }};
                #secretArrow = () => 1;
                static #secretFn = function() {{ return 1; }};
                public static shared() {{}}
                private shared() {{}}
            }}"#
            );
            let defs = extract(&source);
            for member in [
                "run",
                "implicit",
                "create",
                "readable",
                "arrow",
                "static.shared",
            ] {
                let def = &defs[&format!("Surface.{member}")];
                assert_eq!(
                    def.is_exported,
                    export.starts_with("export"),
                    "{export}: {member}"
                );
            }
            for member in [
                "hidden",
                "inherited",
                "#secret",
                "hiddenStatic",
                "inheritedStatic",
                "#secretStatic",
                "hiddenValue",
                "inheritedValue",
                "#secretValue",
                "hiddenArrow",
                "inheritedArrow",
                "hiddenFn",
                "#secretArrow",
                "#secretFn",
                "shared",
            ] {
                let def = &defs[&format!("Surface.{member}")];
                assert!(
                    !def.is_exported,
                    "{export}: {member} must remain inaccessible"
                );
                assert_eq!(serde_json::to_value(def).unwrap()["is_exported"], false);
            }
            // Constructors and setters have no standalone row in the current index.
            assert!(!defs.contains_key("Surface.constructor"));
            assert!(!defs.contains_key("Surface.writeOnly"));
            assert_eq!(defs.len(), 21);
        }
        let defs =
            extract("class Surface { private hidden() {} public run() {} } export { Surface };");
        assert!(!defs["Surface.hidden"].is_exported);
        assert!(defs["Surface.run"].is_exported);
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
