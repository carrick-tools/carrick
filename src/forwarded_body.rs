//! What the request body is at a call to a function the service declares,
//! which makes the request inside it (carrick#1782).
//!
//! Two passes state a row at such a call: the request summaries, restating a
//! request at a caller ([`crate::request_summary`]), and the imported-member
//! join, at a call through a member of another module
//! ([`crate::imported_request_member`]). The call is the function's call, so
//! its arguments are the function's parameters, and the payload a model row
//! folds onto the site is one of them. Read as the request body, a string a
//! member wraps in `{ body }` is judged against the route's object, and a
//! literal argument is read with nothing to hold it (`{ status: "approved" }`
//! widens to `{ status: string }` and fails against the route's union).
//!
//! What the body at the site is follows from what the function does with its
//! parameters:
//!
//! - It sends one of them unchanged (`body: JSON.stringify(body)`), and that
//!   parameter's declaration states a type: the site sends the argument in
//!   that position, and its type is read from the declaration, where a literal
//!   the caller writes is checked against it rather than read alone
//!   ([`CallBody::Param`]).
//! - It builds the body itself, sends none, or assigns the parameter again
//!   before sending it: its own request line states what it sends, and the
//!   site states no request type ([`CallBody::Built`]).
//! - It sends a parameter whose declaration states nothing (no annotation, a
//!   top type, or a type the caller chooses through a type parameter), or the
//!   options object a caller writes the body into: the site's own payload is
//!   the only statement of the body there is, so the row carries no
//!   [`CallBody`] and the type layer reads the site as it reads a request's
//!   own line.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use swc_common::{SourceMap, Span};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::binding_scope::{BindingKey, ident_key};
use crate::swc_scanner::SWC_SPAN_BASE;

/// What the request body is at a call to a function the service declares
/// (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallBody {
    /// The function builds the body itself, or sends none: what the call
    /// passes is not the body, and the site states no request type.
    Built,
    /// The function sends this parameter unchanged as the body, and the
    /// declaration states its type: the site's body is the argument in that
    /// position, typed by the declaration.
    Param(DeclaredParam),
}

/// Where a parameter's name is written in its function's declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeclaredParam {
    /// The declaring file, as the source map holds it.
    pub file: String,
    /// The name's span within that file, in SWC's numbering (from
    /// [`SWC_SPAN_BASE`]), the numbering every row's span carries.
    pub span_start: u32,
    pub span_end: u32,
    /// 1-based line the name is written on.
    pub line: u32,
}

/// One parameter, as a body sent through it is read.
#[derive(Debug, Clone)]
pub(crate) struct ParamDecl {
    /// The binding the parameter introduces.
    pub(crate) key: BindingKey,
    /// Where its name is written.
    at: DeclaredParam,
    /// The declaration carries a type annotation that is not a top type
    /// (`any`, `unknown`).
    annotated: bool,
    /// Every type name the annotation refers to, by the first segment of the
    /// name (`T` in `T["body"]`, `Api` in `Api.Body`).
    mentions: BTreeSet<String>,
    /// The body assigns the parameter again, so what it sends through it is
    /// not what the caller passed.
    assigned_again: bool,
}

impl ParamDecl {
    /// Every plain (or defaulted) identifier parameter in `params`, by
    /// position, with `body` read for assignments to it. A destructured or
    /// rest parameter binds no single value a body could be sent as.
    pub(crate) fn all<'p, B: VisitWith<Assigns>>(
        params: impl IntoIterator<Item = &'p Pat>,
        body: Option<&B>,
        source_map: &SourceMap,
    ) -> Vec<Option<ParamDecl>> {
        params
            .into_iter()
            .map(|pat| {
                let mut decl = Self::of(pat, source_map)?;
                decl.assigned_again = body.is_some_and(|body| assigns(body, &decl.key));
                Some(decl)
            })
            .collect()
    }

    fn of(pat: &Pat, source_map: &SourceMap) -> Option<ParamDecl> {
        let binding = match pat {
            Pat::Ident(binding) => binding,
            // `body: Body = {}`: the binding is the left side.
            Pat::Assign(assign) => match &*assign.left {
                Pat::Ident(binding) => binding,
                _ => return None,
            },
            _ => return None,
        };
        let annotation = binding.type_ann.as_ref().map(|ann| &*ann.type_ann);
        let mut mentions = TypeNames::default();
        if let Some(annotation) = annotation {
            annotation.visit_with(&mut mentions);
        }
        Some(ParamDecl {
            key: ident_key(&binding.id),
            at: declared_at(binding.id.span, source_map),
            annotated: annotation.is_some_and(|annotation| !is_top_type(annotation)),
            mentions: mentions.names,
            assigned_again: false,
        })
    }

    /// Whether the function's body assigns the parameter again.
    pub(crate) fn assigned_again(&self) -> bool {
        self.assigned_again
    }

    /// What a call's body is when the function sends this parameter as its
    /// body, with `type_params` the type parameters in scope at the
    /// declaration (the function's own and its class's). `None`: the
    /// declaration states nothing a type could be read from, and the site's
    /// own payload stays the reading.
    pub(crate) fn forwarded(&self, type_params: &HashSet<String>) -> Option<CallBody> {
        if self.assigned_again {
            return Some(CallBody::Built);
        }
        let caller_chooses = self.mentions.iter().any(|name| type_params.contains(name));
        (self.annotated && !caller_chooses).then(|| CallBody::Param(self.at.clone()))
    }
}

/// The names a type-parameter list declares.
pub(crate) fn type_param_names(decl: Option<&TsTypeParamDecl>) -> HashSet<String> {
    decl.map(|decl| {
        decl.params
            .iter()
            .map(|param| param.name.sym.to_string())
            .collect()
    })
    .unwrap_or_default()
}

/// The identifier a body expression sends unchanged: `body`, or `body`
/// serialised (`JSON.stringify(body)`), through parentheses, non-null and
/// type assertions. `None` for anything the expression computes.
pub(crate) fn sent_ident(expr: &Expr) -> Option<&Ident> {
    match expr {
        Expr::Ident(ident) => Some(ident),
        Expr::Paren(paren) => sent_ident(&paren.expr),
        Expr::TsAs(as_expr) => sent_ident(&as_expr.expr),
        Expr::TsSatisfies(satisfies) => sent_ident(&satisfies.expr),
        Expr::TsNonNull(non_null) => sent_ident(&non_null.expr),
        Expr::TsTypeAssertion(assertion) => sent_ident(&assertion.expr),
        Expr::TsConstAssertion(assertion) => sent_ident(&assertion.expr),
        Expr::Call(call) if is_json_stringify(call) => match call.args.first() {
            Some(arg) if arg.spread.is_none() => sent_ident(&arg.expr),
            _ => None,
        },
        _ => None,
    }
}

/// `JSON.stringify(…)`, named through the standard `JSON` object.
fn is_json_stringify(call: &CallExpr) -> bool {
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    let Expr::Member(member) = &**callee else {
        return false;
    };
    matches!(&*member.obj, Expr::Ident(object) if object.sym == *"JSON")
        && matches!(&member.prop, MemberProp::Ident(prop) if prop.sym == *"stringify")
}

fn declared_at(span: Span, source_map: &SourceMap) -> DeclaredParam {
    let start = source_map.lookup_byte_offset(span.lo);
    let end = source_map.lookup_byte_offset(span.hi);
    DeclaredParam {
        file: start.sf.name.to_string(),
        span_start: start.pos.0 + SWC_SPAN_BASE,
        span_end: end.pos.0 + SWC_SPAN_BASE,
        line: u32::try_from(source_map.lookup_char_pos(span.lo).line).unwrap_or(0),
    }
}

/// `any` and `unknown` say nothing about the value they annotate.
fn is_top_type(ty: &TsType) -> bool {
    match ty {
        TsType::TsKeywordType(keyword) => matches!(
            keyword.kind,
            TsKeywordTypeKind::TsAnyKeyword | TsKeywordTypeKind::TsUnknownKeyword
        ),
        TsType::TsParenthesizedType(paren) => is_top_type(&paren.type_ann),
        _ => false,
    }
}

#[derive(Default)]
struct TypeNames {
    names: BTreeSet<String>,
}

impl Visit for TypeNames {
    fn visit_ts_type_ref(&mut self, type_ref: &TsTypeRef) {
        let mut name = &type_ref.type_name;
        while let TsEntityName::TsQualifiedName(qualified) = name {
            name = &qualified.left;
        }
        if let TsEntityName::Ident(ident) = name {
            self.names.insert(ident.sym.to_string());
        }
        type_ref.visit_children_with(self);
    }
}

/// Whether `node` assigns the binding `key` again.
pub(crate) fn assigns<N: VisitWith<Assigns>>(node: &N, key: &BindingKey) -> bool {
    let mut assigns = Assigns {
        key: key.clone(),
        found: false,
    };
    node.visit_with(&mut assigns);
    assigns.found
}

/// The walk behind [`assigns`]: an assignment whose target is the binding
/// (`x = …`, `x += …`, `[x] = …`) or an update of it (`x++`).
pub(crate) struct Assigns {
    key: BindingKey,
    found: bool,
}

impl Visit for Assigns {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        match &assign.left {
            AssignTarget::Simple(SimpleAssignTarget::Ident(binding)) => {
                self.found |= ident_key(&binding.id) == self.key;
            }
            AssignTarget::Pat(pat) => {
                let mut bound = BoundIn {
                    key: &self.key,
                    found: false,
                };
                pat.visit_with(&mut bound);
                self.found |= bound.found;
            }
            _ => {}
        }
        assign.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if let Expr::Ident(ident) = &*update.arg {
            self.found |= ident_key(ident) == self.key;
        }
        update.visit_children_with(self);
    }
}

/// Whether a destructuring target writes the binding.
struct BoundIn<'k> {
    key: &'k BindingKey,
    found: bool,
}

impl Visit for BoundIn<'_> {
    fn visit_ident(&mut self, ident: &Ident) {
        self.found |= ident_key(ident) == *self.key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use swc_common::{
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };

    /// The parameters of the module's first function declaration, read as a
    /// forwarded body reads them, with the module's source map.
    fn params_of(source: &str) -> (Vec<Option<ParamDecl>>, HashSet<String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&path, &cm, &handler).expect("parsed module");
        let function = module
            .body
            .iter()
            .find_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Fn(decl))) => Some(decl.function.clone()),
                _ => None,
            })
            .expect("a function declaration");
        let params = ParamDecl::all(
            function.params.iter().map(|param| &param.pat),
            function.body.as_ref(),
            &cm,
        );
        (params, type_param_names(function.type_params.as_deref()))
    }

    fn forwarded(source: &str, index: usize) -> Option<CallBody> {
        let (params, type_params) = params_of(source);
        params[index].as_ref()?.forwarded(&type_params)
    }

    #[test]
    fn a_declared_parameter_is_read_where_its_name_is_written() {
        let source = "function create(id: string, body: CreateBody) {\n  return 1;\n}\n";
        let Some(CallBody::Param(at)) = forwarded(source, 1) else {
            panic!("a typed parameter is read from its declaration");
        };
        let start = source.find("body:").expect("name") as u32 + SWC_SPAN_BASE;
        assert_eq!((at.span_start, at.span_end, at.line), (start, start + 4, 1));
        assert!(at.file.ends_with("input.ts"), "{}", at.file);
    }

    #[test]
    fn a_defaulted_or_optional_parameter_is_read_too() {
        assert!(matches!(
            forwarded("function f(body: Body = {}) {}\n", 0),
            Some(CallBody::Param(_))
        ));
        assert!(matches!(
            forwarded("function f(body?: { cap?: string[] }) {}\n", 0),
            Some(CallBody::Param(_))
        ));
    }

    #[test]
    fn a_declaration_that_states_nothing_leaves_the_site_s_payload() {
        for source in [
            "function f(body) {}\n",
            "function f(body: any) {}\n",
            "function f(body: unknown) {}\n",
            "function f<T>(body: T) {}\n",
            "function f<T>(body: Partial<T>) {}\n",
            "function f<T>(body: T[\"payload\"]) {}\n",
        ] {
            assert_eq!(forwarded(source, 0), None, "{source}");
        }
    }

    #[test]
    fn a_class_type_parameter_is_the_caller_s_choice_too() {
        let (params, _) = params_of("function f(body: Payload) {}\n");
        let param = params[0].as_ref().expect("a plain parameter");
        let class_params: HashSet<String> = HashSet::from(["Payload".to_string()]);
        assert_eq!(param.forwarded(&class_params), None);
    }

    #[test]
    fn a_parameter_assigned_again_is_not_what_the_caller_passed() {
        for source in [
            "function f(body: Body) { body = { ...body, at: 1 }; }\n",
            "function f(body: Body) { if (x) { body ??= {}; } }\n",
            "function f(body: Body) { [body] = list; }\n",
        ] {
            assert_eq!(forwarded(source, 0), Some(CallBody::Built), "{source}");
        }
        // A binding of the same name a nested scope declares is not the
        // parameter.
        assert!(matches!(
            forwarded(
                "function f(body: Body) { const g = () => { let body = 1; body = 2; }; }\n",
                0
            ),
            Some(CallBody::Param(_))
        ));
    }

    #[test]
    fn a_destructured_or_rest_parameter_is_no_forwarded_body() {
        let (params, _) = params_of("function f({ body }: Opts, ...rest: Body[]) {}\n");
        assert!(params.iter().all(Option::is_none));
    }

    #[test]
    fn the_sent_identifier_is_read_through_serialisation_and_assertions() {
        let source = "function f(body: Body) {\n  send(JSON.stringify(body));\n  send((body as Body)!);\n  send(JSON.stringify({ body }));\n  send(JSON.stringify(body ?? {}));\n}\n";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.ts");
        std::fs::write(&path, source).expect("write file");
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&path, &cm, &handler).expect("parsed module");
        struct Sends(Vec<Option<String>>);
        impl Visit for Sends {
            fn visit_call_expr(&mut self, call: &CallExpr) {
                if matches!(&call.callee, Callee::Expr(callee) if matches!(&**callee, Expr::Ident(name) if name.sym == *"send"))
                {
                    self.0
                        .push(sent_ident(&call.args[0].expr).map(|ident| ident.sym.to_string()));
                }
                call.visit_children_with(self);
            }
        }
        let mut sends = Sends(Vec::new());
        module.visit_with(&mut sends);
        assert_eq!(
            sends.0,
            vec![
                Some("body".to_string()),
                Some("body".to_string()),
                None,
                None
            ]
        );
    }
}
