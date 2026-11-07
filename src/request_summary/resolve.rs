//! The value resolver (carrick#3504): what a value may hold, read where it
//! is written and followed to where it is decided.
//!
//! Design: carrick-cloud
//! `docs/internal/decisions/2026-11-07-one-value-resolver.md` (sections 2.1
//! to 2.7 and 3). This is step S0: discovery's value walk (carrick#2432)
//! lifted here unchanged, so every later reader grows from one walk.
//!
//! Two halves:
//!
//! - **Lowering** ([`Reader::value_of`] and its siblings), at extract time:
//!   a callee that is a value rather than a definition (a parameter, a key
//!   of one, a parameter's default, a local set to `a ?? b`, a function an
//!   in-repo factory returns) becomes a [`ValueAt`] term. Nothing decides
//!   what a value is by its name: a name or a member is kept as its span,
//!   and the compiler's row there says what it holds.
//! - **Resolving** ([`Resolver`]), after the compiler rows are read: a term
//!   is followed through what holds it: what each caller passes at that
//!   position and key, the default when a caller passes nothing there, each
//!   side of a fallback, each construction of a class whose field it is,
//!   what an in-repo factory returns, `f.bind(x)` as `f`. The resolver never
//!   collapses the values it finds (the decision record's D4): the consumer
//!   applies its own agreement policy (discovery's is `agree`).
//!
//! The consumer turns each resolved end (a labelled export, an in-repo
//! definition, a function written in place) into its own result through
//! [`Follow`], at the point the walk reaches it, so the resolver's guards
//! (`fields_reading`, the returns cap) wrap that work exactly as they did
//! inside discovery.

use super::row_calls::Reach;
use super::*;
use crate::position_rows::Kind;
use std::cell::{Cell, RefCell};

/// How many object literals deep an argument's keys are read (the decision
/// record's object-depth cap, D7).
const MAX_VALUE_DEPTH: usize = 4;

/// How many values one query holds, and how many in-repo factories' returns
/// one value may be read through (D7 sets the second at 8 hops; S0 keeps
/// today's 16 so nothing moves).
pub(super) const MAX_VALUES: usize = 16;

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

/// The call that invokes a value, where it is written.
#[derive(Debug, Clone, Copy)]
pub(super) struct Invoke<'a> {
    pub(super) file: &'a Path,
    pub(super) call: &'a CallIr,
    pub(super) construction: bool,
}

/// One value a position may hold, read (carrick#2432). `R` is what the
/// consumer made of a resolved end ([`Follow`]).
#[derive(Debug, Clone)]
pub(super) enum Held<R> {
    /// A parameter of the body it is read in: how many functions out, its
    /// position and keys. The callers of that body decide it.
    Param(u8, usize, Vec<String>),
    /// A function written at a caller, or a parameter of a function written
    /// in the body: what runs it decides, which no caller here states.
    Callback,
    /// An end the consumer followed; `R::default()` when the value holds
    /// nothing the resolver can read.
    Reached(R),
}

/// What a consumer makes of each end the resolver reaches, at the point it
/// reaches it.
pub(super) trait Follow<'a> {
    type Reached: Default;

    /// The name or member read at `lo..hi` in `file`, when the consumer
    /// knows what it is from the compiler's row there (discovery: a
    /// labelled export). `None` reads the row as a definition.
    fn export(
        &mut self,
        file: &'a Path,
        lo: u32,
        hi: u32,
        kind: Kind,
        invoke: Invoke<'a>,
    ) -> Option<Self::Reached>;

    /// The value is the in-repo definition `key` of `target`.
    fn definition(&mut self, target: PathBuf, key: String, invoke: Invoke<'a>) -> Self::Reached;

    /// The value is `function`, written in place in `file`.
    fn written(&mut self, file: &'a Path, function: &'a FnIr, invoke: Invoke<'a>) -> Self::Reached;
}

/// A callee's parameters' defaults, with the file they are read in.
pub(super) type Defaults<'a> = (&'a Path, &'a [(usize, Vec<String>, Vec<ValueAt>)]);

pub(super) fn defaults_of<'a>(callee: Option<(&'a Path, &'a FnIr)>) -> Option<Defaults<'a>> {
    callee.map(|(file, callee)| (file, callee.param_defaults.as_slice()))
}

/// One per service run, built after the compiler rows are read: the one
/// caller index and the constructions, and the guards one query holds.
pub(super) struct Resolver<'a> {
    inputs: &'a RequestSummaryInputs,
    /// The run's other services, in declared order (carrick#2618): a file of
    /// one of them is read with that service's own inputs.
    siblings: &'a [&'a RequestSummaryInputs],
    /// Each function's callers, through what the rows say each call reaches:
    /// what fills a parameter a value is read from (carrick#2432).
    callers: super::decided_sites::Calls<'a>,
    /// Each class of the service read through its constructions, with them
    /// (carrick#2452): what fills a field its constructor sets from a
    /// parameter.
    constructed: super::constructions::Constructed<'a>,
    /// How many own factories' returns the value being read is inside.
    returns_depth: Cell<usize>,
    /// The fields being read through their constructions (carrick#2452): a
    /// construction that passes a field of a class whose field is being
    /// read (`new Fork({ send: this.send })` in a method of `Fork`) adds
    /// nothing.
    fields_reading: RefCell<HashSet<(&'a Path, u32, String)>>,
    /// The step census that sets the decision record's per-query step cap
    /// (D7). It counts and never stops a read.
    census: Cell<Census>,
    /// How many resolver calls are open, and the step count when the
    /// outermost one opened.
    open: Cell<(usize, usize)>,
}

/// What the resolver read (carrick#3504 S0): every value read, how many
/// queries (outermost resolver calls, the ends their consumer followed
/// inside them included), and the most values one query read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Census {
    pub(super) steps: usize,
    pub(super) queries: usize,
    pub(super) max_query_steps: usize,
}

/// One open resolver call; the outermost one closing ends a query.
struct Query<'r, 'a>(&'r Resolver<'a>);

impl Drop for Query<'_, '_> {
    fn drop(&mut self) {
        let (depth, start) = self.0.open.get();
        self.0.open.set((depth - 1, start));
        if depth == 1 {
            let mut census = self.0.census.get();
            census.queries += 1;
            census.max_query_steps = census.max_query_steps.max(census.steps - start);
            self.0.census.set(census);
        }
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        inputs: &'a RequestSummaryInputs,
        siblings: &'a [&'a RequestSummaryInputs],
    ) -> Self {
        Resolver {
            inputs,
            siblings,
            callers: super::decided_sites::Calls::through_reach(inputs),
            constructed: super::constructions::constructed(inputs),
            returns_depth: Cell::new(0),
            fields_reading: RefCell::new(HashSet::new()),
            census: Cell::new(Census::default()),
            open: Cell::new((0, 0)),
        }
    }

    /// What the resolver has read so far.
    pub(super) fn census(&self) -> Census {
        self.census.get()
    }

    /// Opens a resolver call for the census.
    fn enter(&self) -> Query<'_, 'a> {
        let (depth, start) = self.open.get();
        let start = if depth == 0 {
            self.census.get().steps
        } else {
            start
        };
        self.open.set((depth + 1, start));
        Query(self)
    }

    /// The inputs that hold `file`: this service's, else the first sibling's
    /// that does.
    pub(super) fn inputs_for(&self, file: &Path) -> &'a RequestSummaryInputs {
        super::discovery::inputs_holding(self.inputs, self.siblings, file)
    }

    /// The definition `key` of `file`, the file as `inputs` keys it.
    pub(super) fn function(&self, file: &Path, key: &str) -> Option<(&'a Path, &'a FnIr)> {
        let (path, ir) = self.inputs_for(file).files.get_key_value(file)?;
        Some((path.as_path(), ir.functions.get(key)?))
    }

    /// The function written in `file` whose node starts at `start`, at any
    /// depth of any body.
    pub(super) fn written_function(&self, file: &Path, start: u32) -> Option<&'a FnIr> {
        fn find(ir: &FnIr, start: u32) -> Option<&FnIr> {
            if ir.site.as_ref().is_some_and(|site| site.lo == start) {
                return Some(ir);
            }
            ir.nested
                .iter()
                .chain(&ir.detached)
                .chain(&ir.param_functions)
                .find_map(|inner| find(inner, start))
        }
        let ir = self.inputs_for(file).files.get(file)?;
        let mut keys: Vec<&String> = ir.functions.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| &ir.functions[key])
            .chain(std::iter::once(&ir.module_level))
            .chain(&ir.constructor_defaults)
            .find_map(|body| find(body, start))
    }

    /// What `value`, read in a body of `file`, holds when `invoke` runs it
    /// (carrick#2432): a parameter of that body is left for its callers; a
    /// name or member is what `follow` says its row is, else the in-repo
    /// definition the row names; a function written in place is that
    /// function. `at_caller` reads a function written in place as a
    /// callback, whatever runs it filling it.
    pub(super) fn read_value<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        value: &ValueAt,
        invoke: Invoke<'a>,
        at_caller: bool,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        let mut census = self.census.get();
        census.steps += 1;
        self.census.set(census);
        let nothing = || vec![Held::Reached(F::Reached::default())];
        match value {
            ValueAt::Param { depth, index, keys } => {
                vec![Held::Param(*depth, *index, keys.clone())]
            }
            ValueAt::Field {
                lo,
                hi,
                class,
                field,
            } => match self.field_values(follow, file, *class, field, invoke) {
                Some(held) => held,
                None => {
                    let read = ValueAt::Read {
                        lo: *lo,
                        hi: *hi,
                        member: true,
                    };
                    self.read_value(follow, file, &read, invoke, at_caller)
                }
            },
            ValueAt::Read { lo, hi, member } => {
                let kind = if *member { Kind::Member } else { Kind::Ident };
                if let Some(reached) = follow.export(file, *lo, *hi, kind, invoke) {
                    return vec![Held::Reached(reached)];
                }
                match self.inputs_for(file).row_names(file, *lo, *hi, kind) {
                    Reach::Function(target, key) => {
                        vec![Held::Reached(follow.definition(target, key, invoke))]
                    }
                    _ => nothing(),
                }
            }
            ValueAt::Returned { lo, hi, member } => {
                // `f.bind(this)` is `f`: the member the compiler's row names
                // as the language library's `bind` returns its object.
                if let Some(call) = member
                    && call.args == 1
                    && self.is_library_bind(file, call.lo, call.hi)
                {
                    let mut out = Vec::new();
                    for object in &call.object {
                        out.extend(self.read_value(follow, file, object, invoke, at_caller));
                    }
                    if !out.is_empty() {
                        return out;
                    }
                }
                let Reach::Function(target, key) = self.inputs_for(file).reach(file, *lo, *hi)
                else {
                    return nothing();
                };
                let Some((factory_file, factory)) = self.function(&target, &key) else {
                    return nothing();
                };
                if self.returns_depth.get() >= MAX_VALUES {
                    return nothing();
                }
                self.returns_depth.set(self.returns_depth.get() + 1);
                let mut out = Vec::new();
                for returned in &factory.returned_values {
                    match returned {
                        // A parameter of the factory: its own call fills it.
                        ValueAt::Param { .. } => out.extend(nothing()),
                        other => {
                            out.extend(self.read_value(follow, factory_file, other, invoke, false))
                        }
                    }
                }
                self.returns_depth.set(self.returns_depth.get() - 1);
                if out.is_empty() { nothing() } else { out }
            }
            ValueAt::Function(_) if at_caller => vec![Held::Callback],
            ValueAt::Function(start) => match self.written_function(file, *start) {
                Some(function) => vec![Held::Reached(follow.written(file, function, invoke))],
                None => nothing(),
            },
            ValueAt::Object { .. } | ValueAt::Unread => nothing(),
        }
    }

    /// Whether the member read at `lo..hi` in `file` is a `bind` the
    /// language's own library declares: the compiler's row there names a
    /// declaration in a `lib:` file.
    fn is_library_bind(&self, file: &Path, lo: u32, hi: u32) -> bool {
        let inputs = self.inputs_for(file);
        let (Some(positions), Some(ir)) = (inputs.positions.as_ref(), inputs.files.get(file))
        else {
            return false;
        };
        positions
            .lookup(file, ir.swc_start(lo), ir.swc_start(hi), Kind::Member)
            .ok()
            .is_some_and(|row| {
                row.symbol() == Some("bind")
                    && row
                        .decl_file()
                        .is_some_and(|declared| declared.starts_with("lib:"))
            })
    }

    /// The constructions of the class whose span starts at `class` in
    /// `file`, and where its constructor sets `field` from, when the class
    /// is read through its constructions (carrick#2452). A class no file of
    /// the service constructs, and that nothing refused, comes back with no
    /// constructions: its constructor alone is read (carrick#2808).
    fn field_origin(
        &self,
        file: &'a Path,
        class: u32,
        field: &str,
    ) -> Option<(
        &'a super::constructions::ClassOrigin,
        &'a super::constructions::FieldOrigin,
        Vec<super::decided_sites::Placed<'a>>,
    )> {
        let built = match self.constructed.built.get(&(file, class)) {
            Some(built) => built.clone(),
            None if self.constructed.refused.contains(&(file, class)) => return None,
            None => Vec::new(),
        };
        let origin = self.inputs.files.get(file)?.class_origins.get(&class)?;
        let from = origin.fields.get(field)?;
        Some((origin, from, built))
    }

    /// What `field` of the class whose span starts at `class` in `file` may
    /// hold when `invoke` runs it (carrick#2452): each value the constructor
    /// sets it from, a parameter read at every construction as a call's
    /// argument is, and from the construction's own callers where it hands
    /// its own parameter on. `None` when the class is not read through its
    /// constructions: the member is read as itself.
    pub(super) fn field_values<F: Follow<'a>>(
        &self,
        follow: &mut F,
        file: &'a Path,
        class: u32,
        field: &str,
        invoke: Invoke<'a>,
    ) -> Option<Vec<Held<F::Reached>>> {
        let _query = self.enter();
        let (origin, from, built) = self.field_origin(file, class, field)?;
        let reading = (file, class, field.to_string());
        if !self.fields_reading.borrow_mut().insert(reading.clone()) {
            return Some(Vec::new());
        }
        let defaults: Defaults<'a> = (file, origin.defaults.as_slice());
        let mut out = Vec::new();
        for value in &from.values {
            match value {
                ValueAt::Param {
                    depth: 0,
                    index,
                    keys,
                } => {
                    // No construction in the service (carrick#2808): the
                    // parameter holds its default, as a function nothing
                    // calls does.
                    if built.is_empty() {
                        out.extend(self.passed(follow, None, *index, keys, Some(defaults), invoke));
                    }
                    for placed in &built {
                        let at = Invoke {
                            file: placed.file,
                            call: placed.call,
                            construction: true,
                        };
                        for held in
                            self.passed(follow, Some(at), *index, keys, Some(defaults), invoke)
                        {
                            match held {
                                Held::Param(out_depth, index, keys) => {
                                    out.extend(self.callers_pass(
                                        follow,
                                        (placed.file, placed.key, placed.depth),
                                        (out_depth, index, &keys),
                                        invoke,
                                        &mut Vec::new(),
                                    ));
                                }
                                other => out.push(other),
                            }
                        }
                    }
                }
                // A parameter of a function the constructor is written in.
                ValueAt::Param { .. } => out.push(Held::Reached(F::Reached::default())),
                other => out.extend(self.read_value(follow, file, other, invoke, false)),
            }
        }
        self.fields_reading.borrow_mut().remove(&reading);
        Some(out)
    }

    /// Whether every value `call`'s callee may hold is a field whose class
    /// is not read through its constructions: the callee is read as itself,
    /// as a member is.
    pub(super) fn unbound_fields(&self, file: &'a Path, call: &CallIr) -> bool {
        call.callee_values.iter().all(|value| {
            matches!(value, ValueAt::Field { class, field, .. }
                if self.field_origin(file, *class, field).is_none())
        })
    }

    /// What the call `at` hands its callee's parameter `index` at `keys`:
    /// each value it writes there, read where `at` is written, or the
    /// callee's default where it writes nothing (or there is no call, `at`
    /// `None`). Empty when nothing is handed and there is no default.
    /// `defaults` are the callee's parameters' defaults, read in its file
    /// (a function's [`FnIr::param_defaults`], or a constructor's).
    pub(super) fn passed<F: Follow<'a>>(
        &self,
        follow: &mut F,
        at: Option<Invoke<'a>>,
        index: usize,
        keys: &[String],
        defaults: Option<Defaults<'a>>,
        invoke: Invoke<'a>,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        let mut written: Vec<ValueAt> = Vec::new();
        let mut defaulted = at.is_none();
        if let Some(at) = at {
            let args = &at.call.arg_values;
            // A spread (an empty entry) at or before the position moves it.
            let spread = args.iter().take(index + 1).any(Vec::is_empty);
            match args.get(index) {
                _ if spread => written.push(ValueAt::Unread),
                Some(values) => {
                    for value in values {
                        match value.at(keys) {
                            Some(values) => written.extend(values),
                            None => defaulted = true,
                        }
                    }
                }
                None => defaulted = true,
            }
        }
        written.dedup();
        let mut out = Vec::new();
        if let Some(at) = at {
            for value in &written {
                out.extend(self.read_value(follow, at.file, value, invoke, true));
            }
        }
        if defaulted && let Some((file, defaults)) = defaults {
            for (position, path, values) in defaults {
                if *position != index || !keys.starts_with(path) {
                    continue;
                }
                for value in values {
                    for value in value.at(&keys[path.len()..]).unwrap_or_default() {
                        match value {
                            // A default read from another parameter.
                            ValueAt::Param { .. } => out.push(Held::Reached(F::Reached::default())),
                            other => {
                                out.extend(self.read_value(follow, file, &other, invoke, false))
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// The values the parameter `param` (how many functions out of the body
    /// `at`, its position and keys) holds, read at every caller of the
    /// definition that body is in, and from theirs where a caller passes a
    /// parameter of its own. `passing` holds the definitions on the way: a
    /// cycle that passes the parameter on unchanged adds nothing (the
    /// decision record's soundness rule 7).
    pub(super) fn callers_pass<F: Follow<'a>>(
        &self,
        follow: &mut F,
        at: (&'a Path, Option<&'a str>, u8),
        param: (u8, usize, &[String]),
        invoke: Invoke<'a>,
        passing: &mut Vec<(PathBuf, String)>,
    ) -> Vec<Held<F::Reached>> {
        let _query = self.enter();
        let (file, key, depth) = at;
        let (out, index, keys) = param;
        // A parameter of a function written in the body: what runs it
        // fills it.
        if out < depth {
            return vec![Held::Callback];
        }
        let Some(key) = key.filter(|_| out == depth) else {
            return vec![Held::Reached(F::Reached::default())];
        };
        let id = (file.to_path_buf(), key.to_string());
        if passing.contains(&id) {
            return Vec::new();
        }
        passing.push(id);
        let callee = defaults_of(self.function(file, key));
        let callers: Vec<_> = self.callers.callers_of(file, key).to_vec();
        let mut values = Vec::new();
        if callers.is_empty() {
            // Nothing calls it: what it holds is its default, if any.
            values.extend(self.passed(follow, None, index, keys, callee, invoke));
        }
        for caller in callers {
            let at = Invoke {
                file: caller.file,
                call: caller.call,
                construction: false,
            };
            for value in self.passed(follow, Some(at), index, keys, callee, invoke) {
                match value {
                    Held::Param(out, index, keys) => values.extend(self.callers_pass(
                        follow,
                        (caller.file, caller.key, caller.depth),
                        (out, index, &keys),
                        invoke,
                        passing,
                    )),
                    other => values.push(other),
                }
            }
        }
        passing.pop();
        values
    }
}

#[cfg(test)]
mod tests {
    //! The resolver's API on the forms discovery's value walk followed
    //! before the lift (carrick#3504 S0), read with a follower that names
    //! each end, so later steps have a harness. Fixtures:
    //! `discovery-default-function`, `discovery-factory-function` and
    //! `resolve-values` (its README is the answer key).

    use super::super::fixture_walk::walked;
    use super::*;

    /// What the test follower makes of an end.
    #[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum End {
        /// Nothing the resolver can read.
        #[default]
        Nothing,
        /// An in-repo definition, by its key.
        Definition(String),
        /// A function written in place, by its line.
        Written(u32),
    }

    /// Names each end; knows no labels.
    struct Ends;

    impl<'a> Follow<'a> for Ends {
        type Reached = End;

        fn export(&mut self, _: &'a Path, _: u32, _: u32, _: Kind, _: Invoke<'a>) -> Option<End> {
            None
        }

        fn definition(&mut self, _: PathBuf, key: String, _: Invoke<'a>) -> End {
            End::Definition(key)
        }

        fn written(&mut self, _: &'a Path, function: &'a FnIr, _: Invoke<'a>) -> End {
            End::Written(function.site.as_ref().map_or(0, |site| site.line))
        }
    }

    /// One value, as a test compares it.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum Got {
        Param(u8, usize, Vec<String>),
        Callback,
        End(End),
    }

    fn got(held: Vec<Held<End>>) -> Vec<Got> {
        let mut out: Vec<Got> = held
            .into_iter()
            .map(|held| match held {
                Held::Param(depth, index, keys) => Got::Param(depth, index, keys),
                Held::Callback => Got::Callback,
                Held::Reached(end) => Got::End(end),
            })
            .collect();
        out.sort();
        out
    }

    /// The call at `line` of the file whose path ends with `file` whose
    /// callee is a value, with the definition it is written in and how many
    /// functions inside it.
    fn call_at<'a>(
        inputs: &'a RequestSummaryInputs,
        file: &str,
        line: u32,
    ) -> (&'a Path, Option<&'a str>, u8, &'a CallIr) {
        fn find(body: &FnIr, depth: u8, line: u32) -> Option<(u8, &CallIr)> {
            body.read_calls()
                .map(|(call, _)| call)
                .find(|call| call.site.line == line && !call.callee_values.is_empty())
                .map(|call| (depth, call))
                .or_else(|| {
                    body.nested
                        .iter()
                        .chain(&body.detached)
                        .find_map(|inner| find(inner, depth + 1, line))
                })
        }
        let (path, ir) = inputs
            .files
            .iter()
            .find(|(path, _)| path.ends_with(file))
            .unwrap_or_else(|| panic!("{file} is read"));
        ir.functions
            .iter()
            .find_map(|(key, body)| {
                find(body, 0, line).map(|(depth, call)| (Some(key.as_str()), depth, call))
            })
            .or_else(|| find(&ir.module_level, 0, line).map(|(depth, call)| (None, depth, call)))
            .map(|(key, depth, call)| (path.as_path(), key, depth, call))
            .unwrap_or_else(|| panic!("a call whose callee is a value at {file}:{line}"))
    }

    /// Every value the callee of the call at `file:line` holds: each callee
    /// value read, and each parameter among them read at every caller, as
    /// discovery's walk settles a call's open values.
    fn callee<'a>(
        resolver: &Resolver<'a>,
        inputs: &'a RequestSummaryInputs,
        file: &str,
        line: u32,
    ) -> Vec<Got> {
        let (path, key, depth, call) = call_at(inputs, file, line);
        let invoke = Invoke {
            file: path,
            call,
            construction: false,
        };
        let mut out = Vec::new();
        for value in &call.callee_values {
            for held in resolver.read_value(&mut Ends, path, value, invoke, false) {
                match held {
                    Held::Param(out_depth, index, keys) => out.extend(resolver.callers_pass(
                        &mut Ends,
                        (path, key, depth),
                        (out_depth, index, &keys),
                        invoke,
                        &mut Vec::new(),
                    )),
                    other => out.push(other),
                }
            }
        }
        got(out)
    }

    fn written(line: u32) -> Got {
        Got::End(End::Written(line))
    }

    fn definition(key: &str) -> Got {
        Got::End(End::Definition(key.to_string()))
    }

    /// A parameter holds what each caller passes there, or its default
    /// where a caller passes nothing (positional, destructured, a
    /// constructor parameter a field is set from). A function a caller
    /// passes from a local it wrote is a callback. A name whose row names
    /// no in-repo definition (the global `fetch`) is nothing this follower
    /// can read: discovery's follower reads it as a labelled export.
    #[test]
    fn a_parameter_holds_each_callers_argument_or_its_default() {
        let Some(on) = walked(
            "discovery-default-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(read("arrow.ts", 7), [written(5)], "positional default");
        assert_eq!(
            read("destructured.ts", 7),
            [written(5)],
            "destructured key's default"
        );
        assert_eq!(
            read("ctor.ts", 10),
            [written(6)],
            "field set from a constructor default"
        );
        assert_eq!(
            read("several.ts", 4),
            [Got::Callback, written(3)],
            "one caller passes nothing, the other its own function"
        );
        assert_eq!(
            read("control.ts", 4),
            [Got::End(End::Nothing)],
            "the global itself"
        );
        // Five queries above opened from outside the resolver; each reads at
        // least the callee value itself.
        let census = resolver.census();
        assert!(census.queries >= 5, "{census:?}");
        assert!(census.max_query_steps >= 1, "{census:?}");
        assert!(census.steps >= census.max_query_steps, "{census:?}");
    }

    /// What an in-repo factory returns is read through its call, and
    /// `f.bind(x)` is `f`: here the bound function is a factory parameter's
    /// key, so it is read at the factory's caller.
    #[test]
    fn a_factory_return_and_a_bound_function_are_read_through() {
        let Some(on) = walked(
            "discovery-factory-function",
            "services/web",
            "web",
            &[],
            true,
        ) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(
            read("items.ts", 5),
            [written(9)],
            "the factory's returned function"
        );
        assert_eq!(
            read("plain.ts", 10),
            [written(2)],
            "a factory that sends nothing"
        );
        assert_eq!(
            read("bound.ts", 10),
            [definition("stub")],
            "`options.fetch.bind(options)`"
        );
    }

    /// A function that passes its own parameter to itself unchanged adds no
    /// value (the decision record's soundness rule 7, pass-through half). A
    /// spread at or before the position makes the caller's value unread
    /// (rule 5). A function nothing calls holds its default.
    #[test]
    fn a_pass_through_cycle_adds_nothing_and_a_spread_is_unread() {
        let Some(on) = walked("resolve-values", "services/web", "web", &[], true) else {
            return;
        };
        let resolver = Resolver::new(&on.inputs, &[]);
        let read = |file: &str, line: u32| callee(&resolver, &on.inputs, file, line);
        assert_eq!(read("cycle.ts", 12), [definition("sendItems")]);
        assert_eq!(
            read("spread.ts", 8),
            [Got::End(End::Nothing), definition("sendGo")]
        );
        assert_eq!(read("spread.ts", 20), [definition("sendGo")]);
    }
}
