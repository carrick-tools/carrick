//! The values a call's callee may hold, read where each is written, for
//! discovery's value walk (carrick#2432 slice A).
//!
//! A callee that is a value rather than a definition (a parameter, a key of
//! one, a parameter's default, a local set to `a ?? b`, a function an
//! in-repo factory returns) is followed through what holds it: what each
//! caller passes at that position and key, the default when a caller passes
//! nothing there, each side of a fallback. This file reads those values out
//! of the source; [`super::discovery`] follows them. Nothing here decides
//! what a value is by its name: a name or a member is kept as its span, and
//! the compiler's row there says what it holds.

use super::*;

/// How many object literals deep an argument's keys are read.
const MAX_VALUE_DEPTH: usize = 4;

/// One value a callee, an argument, a parameter's default or a `return` may
/// hold, read where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ValueAt {
    /// A parameter, `depth` functions out from the body that reads it (0: its
    /// own), at `keys` inside it: a caller decides what it holds.
    Param {
        depth: u8,
        index: usize,
        keys: Vec<String>,
    },
    /// A name or a member read here, by its span (discovery numbering): the
    /// compiler's row there says what it holds.
    Read { lo: u32, hi: u32, member: bool },
    /// `this.<field>` (spanning `lo..hi`) in a member of the class whose
    /// span starts at `class`, a field its constructor sets from a parameter
    /// (carrick#2452): what each construction of the class passes there.
    /// Read as the member it is where the class is not read through its
    /// constructions.
    Field {
        lo: u32,
        hi: u32,
        class: u32,
        field: String,
    },
    /// What the call spanning `lo..hi` returns. A call of a member
    /// (`f.bind(x)`) carries the member read's span and what its object may
    /// hold, for a member the language's own library declares to return its
    /// object (carrick#2432 slice A2).
    Returned {
        lo: u32,
        hi: u32,
        member: Option<Box<MemberCall>>,
    },
    /// A function written here, by where its node starts.
    Function(u32),
    /// An object literal: each key's values, and whether a spread or a
    /// computed key may put any other key in it.
    Object {
        fields: BTreeMap<String, Vec<ValueAt>>,
        open: bool,
    },
    /// Anything else.
    Unread,
}

/// A call of a member, `object.method(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemberCall {
    /// The member read `object.method`, by its span (discovery numbering).
    pub(super) lo: u32,
    pub(super) hi: u32,
    /// How many arguments the call passes.
    pub(super) args: usize,
    /// What the object may hold.
    pub(super) object: Vec<ValueAt>,
}

impl ValueAt {
    /// The same value, as a function written inside the one that reads it
    /// reads it: each parameter is one function further out.
    pub(super) fn enclosed(&self) -> Self {
        match self {
            Self::Param { depth, index, keys } => Self::Param {
                depth: depth.saturating_add(1),
                index: *index,
                keys: keys.clone(),
            },
            Self::Object { fields, open } => Self::Object {
                fields: fields
                    .iter()
                    .map(|(key, values)| (key.clone(), values.iter().map(Self::enclosed).collect()))
                    .collect(),
                open: *open,
            },
            Self::Returned { lo, hi, member } => Self::Returned {
                lo: *lo,
                hi: *hi,
                member: member.as_ref().map(|call| {
                    Box::new(MemberCall {
                        object: call.object.iter().map(Self::enclosed).collect(),
                        ..(**call).clone()
                    })
                }),
            },
            other => other.clone(),
        }
    }

    /// The values at `keys` inside this one, read where it is written. `None`
    /// when a closed object literal holds nothing under a key: the caller
    /// wrote nothing there.
    pub(super) fn at(&self, path: &[String]) -> Option<Vec<ValueAt>> {
        let Some((key, rest)) = path.split_first() else {
            return Some(vec![self.clone()]);
        };
        match self {
            Self::Param { depth, index, keys } => Some(vec![Self::Param {
                depth: *depth,
                index: *index,
                keys: keys.iter().chain(path).cloned().collect(),
            }]),
            Self::Object { fields, open } => match fields.get(key) {
                Some(values) => {
                    let mut out = Vec::new();
                    for value in values {
                        out.extend(value.at(rest).unwrap_or_else(|| vec![Self::Unread]));
                    }
                    Some(out)
                }
                None if *open => Some(vec![Self::Unread]),
                None => None,
            },
            // A key of a value read elsewhere has no span of its own to read
            // a row at.
            _ => Some(vec![Self::Unread]),
        }
    }
}

/// Each key of a destructured parameter, with the parameter's position and
/// the key (`{ send = fetch }`, `{ fetch: fetchImpl }`), and each default a
/// parameter or one of its keys is given, read in the function's own scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParamValues {
    pub(super) held: HashMap<BindingKey, Vec<ValueAt>>,
    /// `(position, keys, the default's values)`.
    pub(super) defaults: Vec<(usize, Vec<String>, Vec<ValueAt>)>,
}

impl Reader<'_> {
    /// What `expr` may hold, each side of a fallback (`a ?? b`, `a || b`)
    /// one value.
    pub(super) fn value_of(
        &self,
        expr: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<ValueAt> {
        self.values_in(expr, scope, bound, 0)
    }

    fn values_in(
        &self,
        expr: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
        depth: usize,
    ) -> Vec<ValueAt> {
        if depth > MAX_VALUE_DEPTH {
            return vec![ValueAt::Unread];
        }
        let expr = crate::graphql_document_sites::unwrap_expression(expr);
        match expr {
            Expr::Ident(ident) => {
                if let Some(held) = scope.held.get(&ident_key(ident)) {
                    return held.clone();
                }
                match self.subject(expr, scope, bound).parameter() {
                    Some((depth, index, keys)) => vec![ValueAt::Param { depth, index, keys }],
                    None => vec![ValueAt::Read {
                        lo: ident.span.lo.0,
                        hi: ident.span.hi.0,
                        member: false,
                    }],
                }
            }
            Expr::Member(member) => {
                // A field the class's constructor sets from a parameter.
                if matches!(
                    crate::graphql_document_sites::unwrap_expression(&member.obj),
                    Expr::This(_)
                ) && let Some(fields) = scope.fields
                    && let Some(field) = this_field(member)
                    && fields.origins.contains_key(&field)
                {
                    return vec![ValueAt::Field {
                        lo: member.span.lo.0,
                        hi: member.span.hi.0,
                        class: fields.class,
                        field,
                    }];
                }
                let key = match &member.prop {
                    MemberProp::Ident(prop) => Some(prop.sym.to_string()),
                    MemberProp::Computed(computed) => match &*computed.expr {
                        Expr::Lit(Lit::Str(text)) => {
                            Some(text.value.to_string_lossy().into_owned())
                        }
                        _ => None,
                    },
                    MemberProp::PrivateName(_) => None,
                };
                if let Some(key) = key
                    && !matches!(
                        crate::graphql_document_sites::unwrap_expression(&member.obj),
                        Expr::This(_)
                    )
                {
                    let base = self.values_in(&member.obj, scope, bound, depth + 1);
                    if !base.is_empty()
                        && base
                            .iter()
                            .all(|value| matches!(value, ValueAt::Param { .. }))
                    {
                        return base
                            .iter()
                            .filter_map(|value| value.at(std::slice::from_ref(&key)))
                            .flatten()
                            .collect();
                    }
                }
                vec![ValueAt::Read {
                    lo: member.span.lo.0,
                    hi: member.span.hi.0,
                    member: true,
                }]
            }
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                let mut out = self.values_in(&bin.left, scope, bound, depth + 1);
                out.extend(self.values_in(&bin.right, scope, bound, depth + 1));
                out
            }
            Expr::Arrow(arrow) => vec![ValueAt::Function(arrow.span.lo.0)],
            Expr::Fn(function) => vec![ValueAt::Function(function.function.span.lo.0)],
            Expr::Call(call) if matches!(call.callee, Callee::Expr(_)) => {
                let member = match &call.callee {
                    Callee::Expr(callee) => {
                        match crate::graphql_document_sites::unwrap_expression(callee) {
                            Expr::Member(member) if matches!(member.prop, MemberProp::Ident(_)) => {
                                Some(Box::new(MemberCall {
                                    lo: member.span.lo.0,
                                    hi: member.span.hi.0,
                                    args: call.args.len(),
                                    object: self.values_in(&member.obj, scope, bound, depth + 1),
                                }))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                vec![ValueAt::Returned {
                    lo: call.span.lo.0,
                    hi: call.span.hi.0,
                    member,
                }]
            }
            Expr::Object(object) => {
                let mut fields: BTreeMap<String, Vec<ValueAt>> = BTreeMap::new();
                let mut open = false;
                for prop in &object.props {
                    let PropOrSpread::Prop(prop) = prop else {
                        open = true;
                        continue;
                    };
                    match &**prop {
                        Prop::KeyValue(kv) => match prop_name(&kv.key) {
                            Some(key) => {
                                fields.insert(
                                    key,
                                    self.values_in(&kv.value, scope, bound, depth + 1),
                                );
                            }
                            None => open = true,
                        },
                        Prop::Shorthand(ident) => {
                            let value = Expr::Ident(ident.clone());
                            fields.insert(
                                ident.sym.to_string(),
                                self.values_in(&value, scope, bound, depth + 1),
                            );
                        }
                        Prop::Method(method) => match prop_name(&method.key) {
                            Some(key) => {
                                fields.insert(
                                    key,
                                    vec![ValueAt::Function(method.function.span.lo.0)],
                                );
                            }
                            None => open = true,
                        },
                        _ => open = true,
                    }
                }
                vec![ValueAt::Object { fields, open }]
            }
            _ => vec![ValueAt::Unread],
        }
    }

    /// What each argument may hold, by position. A spread is an empty entry
    /// and ends the list: it moves every position from it on.
    pub(super) fn arg_values(
        &self,
        args: &[ExprOrSpread],
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<Vec<ValueAt>> {
        let mut out = Vec::new();
        for arg in args {
            if arg.spread.is_some() {
                out.push(Vec::new());
                break;
            }
            out.push(self.value_of(&arg.expr, scope, bound));
        }
        out
    }

    /// What a call's callee may hold, when it is a value: a parameter, a key
    /// of one, a local holding another value, or each side of a fallback.
    /// Empty for a callee read as itself (a function's name, a member of an
    /// object the module declares, `this.<member>`): the rows' call target
    /// says what that reaches.
    pub(super) fn callee_values(
        &self,
        callee: &Expr,
        scope: &Scope<'_>,
        bound: &guards::Bound,
    ) -> Vec<ValueAt> {
        let span = crate::graphql_document_sites::unwrap_expression(callee).span();
        let values = self.value_of(callee, scope, bound);
        let itself = |value: &ValueAt| matches!(value, ValueAt::Read { lo, hi, .. } if *lo == span.lo.0 && *hi == span.hi.0);
        if values.iter().all(itself) {
            return Vec::new();
        }
        values
    }

    /// The functions written as a parameter's default (`doFetch = (url, init)
    /// => fetch(url, init)`), positional or at a destructured key, read the
    /// way [`Self::param_values`] records them as [`ValueAt::Function`]. No
    /// body states them (carrick#2813), so the value walk finds them here.
    pub(super) fn param_functions<'p>(
        &self,
        params: impl IntoIterator<Item = &'p Pat>,
        fields: Option<&ClassFields>,
        module: &ModuleScope,
        captured: &Captured,
    ) -> Vec<FnIr> {
        let mut out = Vec::new();
        let mut read = |expr: &Expr| match expr {
            Expr::Arrow(arrow) => out.push(self.arrow(arrow, fields, module, captured)),
            Expr::Fn(function) => {
                out.push(self.function(&function.function, fields, module, captured));
            }
            _ => {}
        };
        for pat in params {
            let pattern = match pat {
                Pat::Assign(assign) => {
                    if matches!(&*assign.left, Pat::Ident(_)) {
                        read(&assign.right);
                    }
                    &*assign.left
                }
                other => other,
            };
            let Pat::Object(object) = pattern else {
                continue;
            };
            for prop in &object.props {
                match prop {
                    ObjectPatProp::Assign(assign) => {
                        if let Some(default) = assign.value.as_deref() {
                            read(default);
                        }
                    }
                    ObjectPatProp::KeyValue(kv) => {
                        if let Pat::Assign(assign) = &*kv.value
                            && matches!(&*assign.left, Pat::Ident(_))
                            && prop_name(&kv.key).is_some()
                        {
                            read(&assign.right);
                        }
                    }
                    ObjectPatProp::Rest(_) => {}
                }
            }
        }
        out
    }

    /// The keys each destructured parameter holds and the defaults the
    /// parameters are given, read in `scope` (the function's own, its
    /// parameters read). A binding the body assigns again holds nothing it
    /// was handed.
    pub(super) fn param_values<'p>(
        &self,
        params: impl IntoIterator<Item = &'p Pat>,
        scope: &Scope<'_>,
    ) -> ParamValues {
        let mut out = ParamValues::default();
        let bound = guards::Bound::default();
        for (index, pat) in params.into_iter().enumerate() {
            let pattern = match pat {
                Pat::Assign(assign) => {
                    if matches!(&*assign.left, Pat::Ident(_)) {
                        out.defaults.push((
                            index,
                            Vec::new(),
                            self.value_of(&assign.right, scope, &bound),
                        ));
                    }
                    &*assign.left
                }
                other => other,
            };
            let Pat::Object(object) = pattern else {
                continue;
            };
            for prop in &object.props {
                let (binding, key, default) = match prop {
                    ObjectPatProp::Assign(assign) => (
                        &assign.key.id,
                        assign.key.id.sym.to_string(),
                        assign.value.as_deref(),
                    ),
                    ObjectPatProp::KeyValue(kv) => {
                        let Some(key) = prop_name(&kv.key) else {
                            continue;
                        };
                        match &*kv.value {
                            Pat::Ident(binding) => (&binding.id, key, None),
                            Pat::Assign(assign) => match &*assign.left {
                                Pat::Ident(binding) => (&binding.id, key, Some(&*assign.right)),
                                _ => continue,
                            },
                            _ => continue,
                        }
                    }
                    ObjectPatProp::Rest(_) => continue,
                };
                let held = if scope.reassigned.contains(binding.sym.as_ref()) {
                    vec![ValueAt::Unread]
                } else {
                    vec![ValueAt::Param {
                        depth: 0,
                        index,
                        keys: vec![key.clone()],
                    }]
                };
                out.held.insert(ident_key(binding), held);
                if let Some(default) = default {
                    out.defaults
                        .push((index, vec![key], self.value_of(default, scope, &bound)));
                }
            }
        }
        out
    }
}
