//! The names lowering (carrick#3504 S2): an expression that states a name
//! (a route path, an event or topic name, a call's name argument) as the
//! [`ValueAt`] terms the text reader ([`super::Resolver::text_values`])
//! reads. Pure syntax: no row is read here, and nothing decides what a
//! name is by how it is spelled. A name or a member is kept as its span, the
//! compiler's row there says what it holds.
//!
//! This is the second lowering beside `values_in` (the callee lowering):
//! discovery and the receiver walk never see the variants it produces, and
//! every reader of the binding table treats them as unread. S3 merges the
//! two into one `lower`.

use swc_common::{DUMMY_SP, Span, Spanned};
use swc_ecma_ast::*;

use super::ValueAt;

/// How many expressions deep one lowering reads before it answers
/// [`ValueAt::Unread`]: a name written as a longer chain than a person
/// writes is not stated.
const MAX_TEXT_DEPTH: usize = 32;

/// The built-ins whose result over folded parts is a text.
const BUILT_INS: [&str; 4] = ["join", "concat", "replace", "replaceAll"];

/// What `expr` states as text, each span mapped through `shift` (the
/// numbering the term is read in).
pub(in crate::request_summary) fn text_of(expr: &Expr, shift: &dyn Fn(u32) -> u32) -> ValueAt {
    lower(expr, shift, 0)
}

fn lower(expr: &Expr, shift: &dyn Fn(u32) -> u32, depth: usize) -> ValueAt {
    if depth > MAX_TEXT_DEPTH {
        return ValueAt::Unread;
    }
    let at = |span: Span| (shift(span.lo.0), shift(span.hi.0));
    let next = |expr: &Expr| lower(expr, shift, depth + 1);
    match crate::request_summary::unwrap_value(expr) {
        Expr::Lit(Lit::Str(lit)) => lit
            .value
            .as_str()
            .map_or(ValueAt::Unread, |text| ValueAt::Lit(text.to_string())),
        Expr::Tpl(tpl) => template(tpl, &next, |quasi| {
            quasi
                .cooked
                .as_ref()
                .and_then(|cooked| cooked.as_str())
                .map(str::to_string)
        }),
        Expr::Bin(bin) if bin.op == BinaryOp::Add => {
            ValueAt::Concat(vec![next(&bin.left), next(&bin.right)])
        }
        Expr::Ident(ident) => {
            let (lo, hi) = at(ident.span);
            ValueAt::Read {
                lo,
                hi,
                member: false,
                holds: super::Holds::Nothing,
            }
        }
        Expr::Member(member) => {
            let key = match &member.prop {
                MemberProp::Ident(name) => ValueAt::Lit(name.sym.to_string()),
                MemberProp::Computed(computed) => next(&computed.expr),
                MemberProp::PrivateName(_) => return ValueAt::Unread,
            };
            let (lo, hi) = at(member.span);
            ValueAt::Member {
                lo,
                hi,
                object: Box::new(next(&member.obj)),
                key: Box::new(key),
            }
        }
        Expr::Call(call) => {
            let Callee::Expr(callee) = &call.callee else {
                return ValueAt::Unread;
            };
            let Expr::Member(member) = &**callee else {
                return ValueAt::Unread;
            };
            let MemberProp::Ident(method) = &member.prop else {
                return ValueAt::Unread;
            };
            let method = method.sym.as_ref();
            if !BUILT_INS.contains(&method) || call.args.iter().any(|arg| arg.spread.is_some()) {
                return ValueAt::Unread;
            }
            let object = if method == "join" {
                // Only an array literal written in place: every element
                // stated, no hole and no spread.
                match crate::request_summary::unwrap_value(&member.obj) {
                    Expr::Array(array) => {
                        let mut elements = Vec::with_capacity(array.elems.len());
                        for element in &array.elems {
                            match element {
                                Some(element) if element.spread.is_none() => {
                                    elements.push(next(&element.expr));
                                }
                                _ => return ValueAt::Unread,
                            }
                        }
                        ValueAt::Elements(elements)
                    }
                    _ => ValueAt::Unread,
                }
            } else {
                next(&member.obj)
            };
            let (lo, hi) = at(call.span);
            ValueAt::BuiltIn {
                lo,
                hi,
                method: method.to_string(),
                object: Box::new(object),
                args: call.args.iter().map(|arg| next(&arg.expr)).collect(),
            }
        }
        Expr::TaggedTpl(tagged) => {
            // `String.raw`: the template's raw text.
            let Expr::Member(tag) = &*tagged.tag else {
                return ValueAt::Unread;
            };
            let (Expr::Ident(object), MemberProp::Ident(prop)) = (&*tag.obj, &tag.prop) else {
                return ValueAt::Unread;
            };
            if object.sym != "String" || prop.sym != "raw" {
                return ValueAt::Unread;
            }
            let (lo, hi) = at(tagged.span);
            ValueAt::BuiltIn {
                lo,
                hi,
                method: "raw".to_string(),
                object: Box::new(template(&tagged.tpl, &next, |quasi| {
                    Some(quasi.raw.to_string())
                })),
                args: Vec::new(),
            }
        }
        Expr::Object(object) => {
            let mut entries = Vec::with_capacity(object.props.len());
            for prop in &object.props {
                // A spread may put any key in the object.
                let PropOrSpread::Prop(prop) = prop else {
                    return ValueAt::Unread;
                };
                let Prop::KeyValue(entry) = &**prop else {
                    continue;
                };
                let key = match &entry.key {
                    PropName::Ident(name) => ValueAt::Lit(name.sym.to_string()),
                    PropName::Str(text) => text
                        .value
                        .as_str()
                        .map_or(ValueAt::Unread, |text| ValueAt::Lit(text.to_string())),
                    PropName::Computed(computed) => next(&computed.expr),
                    PropName::Num(_) | PropName::BigInt(_) => ValueAt::Unread,
                };
                entries.push((key, next(&entry.value)));
            }
            ValueAt::Entries(entries)
        }
        _ => ValueAt::Unread,
    }
}

/// A template's parts joined: each quasi's text (`quasi_text`), each
/// expression's. A quasi with no text makes the whole template unread.
fn template(
    tpl: &Tpl,
    next: &dyn Fn(&Expr) -> ValueAt,
    quasi_text: impl Fn(&TplElement) -> Option<String>,
) -> ValueAt {
    let mut parts = Vec::with_capacity(tpl.quasis.len() + tpl.exprs.len());
    for (index, quasi) in tpl.quasis.iter().enumerate() {
        let Some(text) = quasi_text(quasi) else {
            return ValueAt::Unread;
        };
        parts.push(ValueAt::Lit(text));
        if let Some(expr) = tpl.exprs.get(index) {
            parts.push(next(expr));
        }
    }
    ValueAt::Concat(parts)
}

/// An enum's members as entries, in source order: each member's name and
/// what its initialiser states (a member with none is unread).
pub(in crate::request_summary) fn enum_text(
    decl: &TsEnumDecl,
    shift: &dyn Fn(u32) -> u32,
) -> ValueAt {
    let mut entries = Vec::with_capacity(decl.members.len());
    for member in &decl.members {
        let key = match &member.id {
            TsEnumMemberId::Ident(ident) => ValueAt::Lit(ident.sym.to_string()),
            TsEnumMemberId::Str(text) => text
                .value
                .as_str()
                .map_or(ValueAt::Unread, |text| ValueAt::Lit(text.to_string())),
        };
        let value = member
            .init
            .as_deref()
            .map_or(ValueAt::Unread, |init| text_of(init, shift));
        entries.push((key, value));
    }
    ValueAt::Entries(entries)
}

/// One plain-key property of an object pattern declarator
/// (`const { a, b: name, c = '/x' } = init`, carrick#3504 S2b-3): the name
/// it binds, the key it reads, its span, and its default. A rest element, a
/// nested pattern and a computed key are not plain keys.
pub(in crate::request_summary) struct PatternMember<'p> {
    pub(in crate::request_summary) bound: &'p BindingIdent,
    pub(in crate::request_summary) key: String,
    pub(in crate::request_summary) span: Span,
    pub(in crate::request_summary) default: Option<&'p Expr>,
}

/// The plain-key properties of `pattern`, in source order.
pub(in crate::request_summary) fn pattern_members(pattern: &ObjectPat) -> Vec<PatternMember<'_>> {
    let mut out = Vec::new();
    for prop in &pattern.props {
        match prop {
            ObjectPatProp::Assign(assign) => out.push(PatternMember {
                bound: &assign.key,
                key: assign.key.id.sym.to_string(),
                span: assign.span,
                default: assign.value.as_deref(),
            }),
            ObjectPatProp::KeyValue(entry) => {
                let key = match &entry.key {
                    PropName::Ident(name) => name.sym.to_string(),
                    PropName::Str(text) => match text.value.as_str() {
                        Some(text) => text.to_string(),
                        None => continue,
                    },
                    _ => continue,
                };
                let (bound, default) = match &*entry.value {
                    Pat::Ident(bound) => (bound, None),
                    Pat::Assign(assign) => match &*assign.left {
                        Pat::Ident(bound) => (bound, Some(&*assign.right)),
                        _ => continue,
                    },
                    _ => continue,
                };
                out.push(PatternMember {
                    bound,
                    key,
                    span: entry.key.span().with_hi(entry.value.span().hi),
                    default,
                });
            }
            ObjectPatProp::Rest(_) => {}
        }
    }
    out
}

impl PatternMember<'_> {
    /// What the bound name states as text: the member of `init` the key
    /// names, read where the property is written. A property with a default
    /// is unread (`None`) until the names lowering reads `??`.
    pub(in crate::request_summary) fn text(
        &self,
        init: &Expr,
        shift: &dyn Fn(u32) -> u32,
    ) -> Option<ValueAt> {
        if self.default.is_some() {
            return None;
        }
        Some(ValueAt::Member {
            lo: shift(self.span.lo.0),
            hi: shift(self.span.hi.0),
            object: Box::new(text_of(init, shift)),
            key: Box::new(ValueAt::Lit(self.key.clone())),
        })
    }

    /// The member read `init.key` the property stands for, spanning the
    /// property, for the value readers to lower as they do any member read.
    pub(in crate::request_summary) fn read(&self, init: &Expr) -> Expr {
        Expr::Member(MemberExpr {
            span: self.span,
            obj: Box::new(init.clone()),
            prop: MemberProp::Ident(IdentName::new(self.key.as_str().into(), DUMMY_SP)),
        })
    }
}
