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
    lower(expr, shift, None, 0)
}

/// What each argument of a call states as text, by position, the way
/// [`text_of`] reads one (carrick#3504 B10-1a): the terms a caller hands a
/// parameter, kept for the text reader to read a parameter through its
/// callers. A spread is `None` and ends the list: it moves every position
/// from it on, as [`super::Resolver::arg_values`] ends its own.
///
/// `class` is where the class the call is written in starts, where it is
/// written in a member of one: there `this.<field>` is a
/// [`ValueAt::Field`], which the text reader reads through the class's
/// constructions (carrick#3678 S6-2), as the callee lowering does
/// (carrick#2452).
pub(in crate::request_summary) fn args_text(
    args: &[ExprOrSpread],
    shift: &dyn Fn(u32) -> u32,
    class: Option<u32>,
) -> Vec<Option<ValueAt>> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        if arg.spread.is_some() {
            out.push(None);
            break;
        }
        out.push(Some(lower(&arg.expr, shift, class, 0)));
    }
    out
}

fn lower(expr: &Expr, shift: &dyn Fn(u32) -> u32, class: Option<u32>, depth: usize) -> ValueAt {
    if depth > MAX_TEXT_DEPTH {
        return ValueAt::Unread;
    }
    let at = |span: Span| (shift(span.lo.0), shift(span.hi.0));
    let next = |expr: &Expr| lower(expr, shift, class, depth + 1);
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
        Expr::Cond(cond) => {
            let (lo, hi) = at(cond.span);
            ValueAt::Alt {
                lo,
                hi,
                branches: vec![next(&cond.cons), next(&cond.alt)],
                nullish: false,
            }
        }
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) => {
            let (lo, hi) = at(bin.span);
            ValueAt::Alt {
                lo,
                hi,
                branches: vec![next(&bin.left), next(&bin.right)],
                nullish: bin.op == BinaryOp::NullishCoalescing,
            }
        }
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
        // `this.<field>` in a member of a class: the field, by the class's
        // span start, as the callee lowering reads it. A private name stays
        // unread.
        Expr::Member(MemberExpr {
            span,
            obj,
            prop: MemberProp::Ident(field),
        }) if class.is_some()
            && matches!(
                crate::graphql_document_sites::unwrap_expression(obj),
                Expr::This(_)
            ) =>
        {
            let (lo, hi) = at(*span);
            ValueAt::Field {
                lo,
                hi,
                class: class.unwrap_or_default(),
                field: field.sym.to_string(),
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
            // A call of no built-in: the expression stays unread, at its span.
            let (lo, hi) = at(call.span);
            let unstated = ValueAt::Unstated { lo, hi };
            let Callee::Expr(callee) = &call.callee else {
                return unstated;
            };
            let Expr::Member(member) = &**callee else {
                return unstated;
            };
            let MemberProp::Ident(method) = &member.prop else {
                return unstated;
            };
            let method = method.sym.as_ref();
            if !BUILT_INS.contains(&method) || call.args.iter().any(|arg| arg.spread.is_some()) {
                return unstated;
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
            ValueAt::BuiltIn {
                lo,
                hi,
                method: method.to_string(),
                object: Box::new(object),
                args: call.args.iter().map(|arg| next(&arg.expr)).collect(),
            }
        }
        Expr::TaggedTpl(tagged) => {
            // `String.raw`: the template's raw text. Any other tag stays
            // unread, at its span.
            let (lo, hi) = at(tagged.span);
            let unstated = ValueAt::Unstated { lo, hi };
            let Expr::Member(tag) = &*tagged.tag else {
                return unstated;
            };
            let (Expr::Ident(object), MemberProp::Ident(prop)) = (&*tag.obj, &tag.prop) else {
                return unstated;
            };
            if object.sym != "String" || prop.sym != "raw" {
                return unstated;
            }
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
                let entry = match &**prop {
                    Prop::KeyValue(entry) => entry,
                    // `{ a }` writes the key `a`, read as the name `a`.
                    Prop::Shorthand(ident) => {
                        entries.push((
                            ValueAt::Lit(ident.sym.to_string()),
                            next(&Expr::Ident(ident.clone())),
                        ));
                        continue;
                    }
                    // A method or an accessor writes its key with a value no
                    // text reader states: the key is present (S5a).
                    Prop::Getter(getter) => {
                        entries.push((prop_key(&getter.key, &next), ValueAt::Unread));
                        continue;
                    }
                    Prop::Setter(setter) => {
                        entries.push((prop_key(&setter.key, &next), ValueAt::Unread));
                        continue;
                    }
                    Prop::Method(method) => {
                        entries.push((prop_key(&method.key, &next), ValueAt::Unread));
                        continue;
                    }
                    _ => continue,
                };
                entries.push((prop_key(&entry.key, &next), next(&entry.value)));
            }
            ValueAt::Entries(entries)
        }
        other => {
            let (lo, hi) = at(other.span());
            ValueAt::Unstated { lo, hi }
        }
    }
}

/// An object literal's key as the text reader states it.
fn prop_key(key: &PropName, next: &dyn Fn(&Expr) -> ValueAt) -> ValueAt {
    match key {
        PropName::Ident(name) => ValueAt::Lit(name.sym.to_string()),
        PropName::Str(text) => text
            .value
            .as_str()
            .map_or(ValueAt::Unread, |text| ValueAt::Lit(text.to_string())),
        PropName::Computed(computed) => next(&computed.expr),
        PropName::Num(_) | PropName::BigInt(_) => ValueAt::Unread,
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
    /// (`{ at = "/x" }`) is a choice, as `init.at ?? "/x"` is (carrick#3191):
    /// the member, or the default.
    pub(in crate::request_summary) fn text(
        &self,
        init: &Expr,
        shift: &dyn Fn(u32) -> u32,
    ) -> ValueAt {
        let member = ValueAt::Member {
            lo: shift(self.span.lo.0),
            hi: shift(self.span.hi.0),
            object: Box::new(text_of(init, shift)),
            key: Box::new(ValueAt::Lit(self.key.clone())),
        };
        match self.default {
            // The choice spans the property it is written as.
            Some(default) => ValueAt::Alt {
                lo: shift(self.span.lo.0),
                hi: shift(self.span.hi.0),
                branches: vec![member, text_of(default, shift)],
                nullish: false,
            },
            None => member,
        }
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
