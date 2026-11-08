//! What each construction of a class passes, across the service, for the
//! fields its constructor sets from a parameter (carrick#2452 slice 2).
//!
//! The reader records, per module-scope class, which constructor parameter
//! each field it sets once in the constructor's own statements comes from
//! ([`ClassOrigin`]): as text, its position, the keys down to it and a
//! fallback (`opts.base ?? "/v1"`, a default); as a value a call may run, what
//! [`super::values`] reads it as in the constructor (`opts.send ?? fetch`).
//!
//! Once every module is read, the compiler's `call` row at each `new` in the
//! service names the class it builds ([`constructed`]). A class is read
//! through its constructions only when every `new` naming it is a
//! construction the reader read, none spreads its arguments, at least one
//! exists, no class anywhere in the service extends one of its name, and no
//! module reads its name as a value (`register(Api)`, `[Api]`): a class
//! handed on may be built where no `new` names it.
//!
//! A class no file of the service constructs, and that none of those rules
//! refused, is read through its constructor alone (carrick#2808): each
//! parameter holds its default.
//!
//! - Text ([`RequestSummaryInputs::read_fields`]): a field's piece
//!   (`this.base`) takes the value every construction passes there when they
//!   agree, and the value is text the source states: literals, or an
//!   environment read. A construction that leaves the key out passes the
//!   fallback. Anything else keeps today's reading.
//! - A value a call runs (`this.send(…)`): discovery reads each value the
//!   constructor sets, a parameter filled at each construction as a call
//!   fills one, through the construction's own callers where it passes its
//!   own parameter on ([`super::discovery`]).

use super::decided_sites::Placed;
use super::*;
use crate::position_rows::Kind;

/// A field of the class whose span starts at `class`, in the file a piece is
/// read in: where [`RequestSummaryInputs::read_fields`] reads its value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldLink {
    pub(super) class: u32,
    pub(super) field: String,
}

/// A module-scope class's fields its constructor sets from a parameter.
#[derive(Debug, Clone, Default)]
pub(super) struct ClassOrigin {
    pub(super) name: String,
    /// The lines a compiler row may name the declaration by: where it
    /// starts (a decorator, the `class` keyword) and where its name is.
    pub(super) lines: Vec<u32>,
    pub(super) fields: HashMap<String, FieldOrigin>,
    /// Each default a constructor parameter, or a key of a destructured one,
    /// is given, as [`FnIr::param_defaults`] holds a function's.
    pub(super) defaults: Vec<(usize, Vec<String>, Vec<ValueAt>)>,
}

/// Where one field's value comes from.
#[derive(Debug, Clone, Default)]
pub(super) struct FieldOrigin {
    /// What the field may hold, read in the constructor: a parameter, a key
    /// of one, each side of a fallback.
    pub(super) values: Vec<ValueAt>,
    /// The parameter the field's text is, when it is one.
    pub(super) text: Option<TextOrigin>,
    /// The constructor parameter, by position, the field is whole and
    /// plain: a parameter property with no default, or `this.field = p`
    /// with `p` a parameter with no default and no key taken
    /// (carrick#1670 T3).
    pub(super) parameter: Option<usize>,
}

/// A field whose text is a constructor parameter, or a key of one.
#[derive(Debug, Clone)]
pub(super) struct TextOrigin {
    position: usize,
    keys: Vec<String>,
    /// What the field holds when a construction leaves the position or the
    /// key out: a `??` / `||` right side or a default the source states.
    fallback: Option<Value>,
}

/// One `new` the file writes: its span (discovery numbering) and the name
/// its callee is read through (`Api` for `new Api()` and `new mod.Api()`).
#[derive(Debug, Clone)]
pub(super) struct NewAt {
    lo: u32,
    hi: u32,
    callee: Option<String>,
}

/// Every `new` a file writes, wherever it is, the name of every class any
/// class it declares extends, and every name it reads as a value.
#[derive(Debug, Clone, Default)]
pub(super) struct NewsAndBases {
    news: Vec<NewAt>,
    bases: HashSet<String>,
    /// Each identifier read as a value: anywhere but a `new`'s callee, a
    /// class's base and the object of a member (`Api.create()`).
    handed: HashSet<String>,
}

/// The name an expression reads a class through: an identifier, or a
/// member's property.
fn class_name(expr: &Expr) -> Option<String> {
    match crate::graphql_document_sites::unwrap_expression(expr) {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) => match &member.prop {
            MemberProp::Ident(prop) => Some(prop.sym.to_string()),
            _ => None,
        },
        _ => None,
    }
}

impl NewsAndBases {
    pub(super) fn of(module: &Module) -> Self {
        let mut found = NewsAndBases::default();
        module.visit_with(&mut found);
        found
    }
}

impl NewsAndBases {
    /// `expr` read as a class's name (a `new`'s callee, a base): its
    /// identifier is not handed; any other part of it is read.
    fn visit_named(&mut self, expr: &Expr) {
        if !matches!(
            crate::graphql_document_sites::unwrap_expression(expr),
            Expr::Ident(_)
        ) {
            expr.visit_with(self);
        }
    }
}

impl Visit for NewsAndBases {
    fn visit_new_expr(&mut self, new: &NewExpr) {
        self.news.push(NewAt {
            lo: new.span.lo.0,
            hi: new.span.hi.0,
            callee: class_name(&new.callee),
        });
        self.visit_named(&new.callee);
        new.args.visit_with(self);
    }

    fn visit_class(&mut self, class: &Class) {
        if let Some(base) = class.super_class.as_deref() {
            if let Some(name) = class_name(base) {
                self.bases.insert(name);
            }
            self.visit_named(base);
        }
        class.decorators.visit_with(self);
        class.body.visit_with(self);
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        // `Api.create()`: the class's own member, not the class handed on.
        if !matches!(&*member.obj, Expr::Ident(_)) {
            member.obj.visit_with(self);
        }
        member.prop.visit_with(self);
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Ident(ident) = expr {
            self.handed.insert(ident.sym.to_string());
        }
        expr.visit_children_with(self);
    }

    fn visit_prop(&mut self, prop: &Prop) {
        // `{ Api }`.
        if let Prop::Shorthand(ident) = prop {
            self.handed.insert(ident.sym.to_string());
        }
        prop.visit_children_with(self);
    }
}

/// Text a construction states for a field: literals, and environment reads.
fn states_text(value: &Value) -> bool {
    let Value::Str(pieces) = value else {
        return false;
    };
    !pieces.is_empty()
        && pieces.iter().all(|piece| {
            matches!(
                piece,
                Piece::Lit(_)
                    | Piece::Opaque {
                        end: Root::Env { .. },
                        link: None,
                        ..
                    }
            )
        })
}

/// Each default a parameter, or a key of a destructured one, is given: its
/// position, the keys down to it and what it may hold.
pub(super) type ParamDefaults = Vec<(usize, Vec<String>, Vec<ValueAt>)>;

impl TextOrigin {
    /// What the construction `call` passes for the field: `None` when it
    /// states no text the source writes there.
    fn at(&self, call: &CallIr) -> Option<Value> {
        // A spread moves every position.
        call.site_args.as_ref()?;
        let mut value = call.args.get(self.position);
        for key in &self.keys {
            value = match value {
                None => None,
                Some(Value::Obj(object)) if !object.open && object.under_param.is_none() => {
                    object.fields.get(key)
                }
                Some(_) => return None,
            };
        }
        let value = match value {
            Some(value) => value.clone(),
            None => self.fallback.clone()?,
        };
        states_text(&value).then_some(value)
    }

    /// The value every construction in `built` passes, when they agree.
    fn agreed(&self, built: &[Placed<'_>]) -> Option<Value> {
        let first = self.at(built.first()?.call)?;
        built
            .iter()
            .all(|placed| self.at(placed.call).as_ref() == Some(&first))
            .then_some(first)
    }
}

/// A constructor parameter, or a key of a destructured one, a field may be
/// set from.
struct ParamAt {
    position: usize,
    keys: Vec<String>,
    /// Its default, as the module reads it.
    default: Option<Value>,
}

impl Reader<'_> {
    /// The fields `ctor` sets from a parameter in its own statements, and the
    /// defaults its parameters are given. A field set twice is the caller's
    /// to drop, as the field table drops it.
    pub(super) fn ctor_origins(
        &self,
        ctor: &Constructor,
        module: &ModuleScope,
    ) -> (HashMap<String, FieldOrigin>, ParamDefaults) {
        let module_scope = Scope::module(module);
        let stated = |expr: &Expr| Some(self.eval(expr, &module_scope)).filter(states_text);
        let mut fields: HashMap<String, FieldOrigin> = HashMap::new();
        let mut pats: Vec<Pat> = Vec::new();
        let mut params: HashMap<BindingKey, ParamAt> = HashMap::new();
        for (position, param) in ctor.params.iter().enumerate() {
            let pat = match param {
                ParamOrTsParamProp::Param(param) => param.pat.clone(),
                ParamOrTsParamProp::TsParamProp(prop) => {
                    let (pat, ident, default) = match &prop.param {
                        TsParamPropParam::Ident(ident) => (Pat::Ident(ident.clone()), ident, None),
                        TsParamPropParam::Assign(assign) => match &*assign.left {
                            Pat::Ident(ident) => {
                                (Pat::Assign(assign.clone()), ident, stated(&assign.right))
                            }
                            _ => continue,
                        },
                    };
                    // A parameter property is the field.
                    fields.insert(
                        ident.id.sym.to_string(),
                        FieldOrigin {
                            values: vec![ValueAt::Param {
                                depth: 0,
                                index: position,
                                keys: Vec::new(),
                            }],
                            text: Some(TextOrigin {
                                position,
                                keys: Vec::new(),
                                fallback: default,
                            }),
                            parameter: matches!(&prop.param, TsParamPropParam::Ident(_))
                                .then_some(position),
                        },
                    );
                    pat
                }
            };
            let (pattern, default) = match &pat {
                Pat::Assign(assign) => (&*assign.left, Some(&*assign.right)),
                other => (other, None),
            };
            match pattern {
                Pat::Ident(ident) => {
                    params.insert(
                        ident_key(&ident.id),
                        ParamAt {
                            position,
                            keys: Vec::new(),
                            default: default.map(|expr| self.eval(expr, &module_scope)),
                        },
                    );
                }
                Pat::Object(object) => {
                    for prop in &object.props {
                        let (binding, key, default) = match prop {
                            ObjectPatProp::Assign(assign) => (
                                &assign.key.id,
                                assign.key.id.sym.to_string(),
                                assign.value.as_deref(),
                            ),
                            ObjectPatProp::KeyValue(kv) => {
                                let (Some(key), Pat::Ident(binding)) =
                                    (prop_name(&kv.key), &*kv.value)
                                else {
                                    continue;
                                };
                                (&binding.id, key, None)
                            }
                            ObjectPatProp::Rest(_) => continue,
                        };
                        params.insert(
                            ident_key(binding),
                            ParamAt {
                                position,
                                keys: vec![key],
                                default: default.map(|expr| self.eval(expr, &module_scope)),
                            },
                        );
                    }
                }
                _ => {}
            }
            pats.push(pat);
        }

        let mut scope = Scope::module(module);
        scope.read_params(pats.iter(), ctor, module, &HashSet::new());
        let read = self.param_values(pats.iter(), &scope);
        scope.held.extend(read.held);
        let bound = guards::Bound::default();
        if let Some(body) = &ctor.body {
            for stmt in &body.stmts {
                let Some((name, value)) = this_assignment(stmt) else {
                    continue;
                };
                fields.insert(
                    name,
                    FieldOrigin {
                        values: self.value_of(value, &scope, &bound),
                        text: self.text_origin(value, &params, &stated),
                        parameter: match crate::graphql_document_sites::unwrap_expression(value) {
                            Expr::Ident(ident) => params
                                .get(&ident_key(ident))
                                .filter(|at| at.keys.is_empty() && at.default.is_none())
                                .map(|at| at.position),
                            _ => None,
                        },
                    },
                );
            }
        }
        (fields, read.defaults)
    }

    /// The parameter `expr` reads as text, with its fallback: the parameter
    /// or its key (`base`, `opts.base`), or a `??` / `||` whose left side is
    /// one and whose right side the source states.
    fn text_origin(
        &self,
        expr: &Expr,
        params: &HashMap<BindingKey, ParamAt>,
        stated: &dyn Fn(&Expr) -> Option<Value>,
    ) -> Option<TextOrigin> {
        match crate::graphql_document_sites::unwrap_expression(expr) {
            Expr::Ident(ident) => {
                let at = params.get(&ident_key(ident))?;
                Some(TextOrigin {
                    position: at.position,
                    keys: at.keys.clone(),
                    fallback: at.default.clone().filter(states_text),
                })
            }
            Expr::Member(member) => {
                let Expr::Ident(object) =
                    crate::graphql_document_sites::unwrap_expression(&member.obj)
                else {
                    return None;
                };
                let MemberProp::Ident(prop) = &member.prop else {
                    return None;
                };
                let at = params.get(&ident_key(object))?;
                let key = prop.sym.to_string();
                // `opts = { base: "/v1" }`: the default's key.
                let fallback = match &at.default {
                    Some(Value::Obj(object)) => object.fields.get(&key).cloned(),
                    _ => None,
                };
                let mut keys = at.keys.clone();
                keys.push(key);
                Some(TextOrigin {
                    position: at.position,
                    keys,
                    fallback: fallback.filter(states_text),
                })
            }
            Expr::Bin(bin)
                if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
            {
                let mut origin = self.text_origin(&bin.left, params, stated)?;
                origin.fallback = Some(stated(&bin.right)?);
                Some(origin)
            }
            _ => None,
        }
    }
}

/// The `call` row at the `new` spanning `at` in `file`: the declaration it
/// names, as the declaring file, the symbol and the line. `None` when the
/// rows say nothing there.
fn named_by<'r>(
    positions: &'r crate::position_rows::PositionIndex,
    file: &Path,
    ir: &FileIr,
    at: &NewAt,
) -> Option<(PathBuf, &'r str, u32)> {
    let row = positions
        .lookup(file, ir.swc_start(at.lo), ir.swc_start(at.hi), Kind::Call)
        .ok()?;
    let symbol = row.symbol()?;
    let declaring = positions.path_of(row.decl_file()?);
    Some((declaring, symbol, row.decl_line()?))
}

/// The classes of the service read through their constructions, and the
/// ones that were refused (carrick#2808).
pub(super) struct Constructed<'a> {
    /// Each class read through its constructions, by its file and span
    /// start, with every construction of it.
    pub(super) built: HashMap<(&'a Path, u32), Vec<Placed<'a>>>,
    /// Each class the rules above turned away, or that cannot be told from
    /// one (no compiler rows). A class in neither map is one no file of the
    /// service constructs: nothing refused it, and its constructor's own
    /// values are all there is to read.
    pub(super) refused: HashSet<(&'a Path, u32)>,
}

/// Each class of the service read through its constructions, by its file
/// and span start, with every construction of it; and the classes refused.
pub(super) fn constructed(inputs: &RequestSummaryInputs) -> Constructed<'_> {
    let Some(positions) = &inputs.positions else {
        return Constructed {
            built: HashMap::new(),
            refused: inputs
                .files
                .iter()
                .flat_map(|(file, ir)| {
                    ir.class_origins
                        .keys()
                        .map(move |class| (file.as_path(), *class))
                })
                .collect(),
        };
    };
    let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
    paths.sort();

    // Every construction the reader read, by where it is.
    fn walk<'a>(
        file: &'a Path,
        key: Option<&'a str>,
        depth: u8,
        ir: &'a FnIr,
        out: &mut HashMap<(&'a Path, u32, u32), Placed<'a>>,
    ) {
        for call in &ir.constructions {
            out.entry((file, call.site.lo, call.site.hi))
                .or_insert(Placed {
                    file,
                    key,
                    depth,
                    call,
                    host: None,
                    written_in: None,
                });
        }
        for inner in ir.nested.iter().chain(&ir.detached) {
            walk(file, key, depth.saturating_add(1), inner, out);
        }
    }
    let mut placed = HashMap::new();
    for path in &paths {
        let file = &inputs.files[*path];
        let mut keys: Vec<&String> = file.functions.keys().collect();
        keys.sort();
        for key in keys {
            walk(
                path,
                Some(key.as_str()),
                0,
                &file.functions[key],
                &mut placed,
            );
        }
        walk(path, None, 0, &file.module_level, &mut placed);
    }

    let bases: HashSet<&str> = inputs
        .files
        .values()
        .flat_map(|file| file.news.bases.iter().map(String::as_str))
        .collect();
    let handed: HashSet<&str> = inputs
        .files
        .values()
        .flat_map(|file| file.news.handed.iter().map(String::as_str))
        .collect();
    let mut found: HashMap<(&Path, u32), Vec<Placed<'_>>> = HashMap::new();
    let mut refused: HashSet<(&Path, u32)> = HashSet::new();
    let mut refused_names: HashSet<&str> = HashSet::new();
    for path in &paths {
        let ir = &inputs.files[*path];
        for at in &ir.news.news {
            let Some((declaring, symbol, line)) = named_by(positions, path, ir, at) else {
                // A `new` the rows say nothing of may build any class of its
                // name.
                if let Some(name) = &at.callee {
                    refused_names.insert(name.as_str());
                }
                continue;
            };
            // A class outside the service: the language's, a package's.
            let Some((declaring, declared)) = inputs.files.get_key_value(&declaring) else {
                continue;
            };
            let mut classes: Vec<(&u32, &ClassOrigin)> = declared
                .class_origins
                .iter()
                .filter(|(_, origin)| origin.name == symbol)
                .collect();
            classes.sort_by_key(|(class, _)| **class);
            let matching: Vec<u32> = classes
                .iter()
                .filter(|(_, origin)| origin.lines.contains(&line))
                .map(|(class, _)| **class)
                .collect();
            match (
                matching.as_slice(),
                placed.get(&(path.as_path(), at.lo, at.hi)),
            ) {
                ([class], Some(construction)) => {
                    found
                        .entry((declaring.as_path(), *class))
                        .or_default()
                        .push(*construction);
                }
                // A construction the reader did not read, or a row naming a
                // class of that name by no line it is declared on.
                _ => {
                    for (class, _) in classes {
                        refused.insert((declaring.as_path(), *class));
                    }
                }
            }
        }
    }
    let turned_away = |file: &Path, class: u32, name: &str| {
        refused.contains(&(file, class))
            || refused_names.contains(name)
            || bases.contains(name)
            || handed.contains(name)
    };
    // A class a `new` named but a rule turned away, or one a name rule
    // turns away whether or not a `new` named it.
    let mut refusals: HashSet<(&Path, u32)> = HashSet::new();
    for (file, ir) in &inputs.files {
        for (class, origin) in &ir.class_origins {
            if turned_away(file, *class, origin.name.as_str()) {
                refusals.insert((file.as_path(), *class));
            }
        }
    }
    found.retain(|(file, class), built| {
        let keep = !turned_away(
            file,
            *class,
            inputs.files[*file].class_origins[class].name.as_str(),
        ) && built.iter().all(|placed| placed.call.site_args.is_some());
        if !keep {
            refusals.insert((*file, *class));
        }
        keep
    });
    Constructed {
        built: found,
        refused: refusals,
    }
}

impl RequestSummaryInputs {
    /// Each field piece (`this.base`) with the text every construction of
    /// its class passes there, where they agree ([`constructed`]).
    pub(super) fn read_fields(&mut self) {
        let table: HashMap<(PathBuf, u32, String), Value> = {
            let mut table = HashMap::new();
            for ((file, class), built) in constructed(self).built {
                let origin = &self.files[file].class_origins[&class];
                for (field, from) in &origin.fields {
                    if let Some(value) = from.text.as_ref().and_then(|text| text.agreed(&built)) {
                        table.insert((file.to_path_buf(), class, field.clone()), value);
                    }
                }
            }
            table
        };
        if table.is_empty() {
            return;
        }
        for (path, ir) in &mut self.files {
            ir.visit_pieces(path, &mut |scope, pieces| {
                let read = |piece: &Piece| match piece {
                    Piece::Opaque {
                        link: Some(link), ..
                    } => match &**link {
                        Link::Field(field) => table
                            .get(&(scope.to_path_buf(), field.class, field.field.clone()))
                            .map(Value::pieces),
                        Link::Named(_) => None,
                    },
                    _ => None,
                };
                if pieces.iter().any(|piece| read(piece).is_some()) {
                    let parts: Vec<Vec<Piece>> = pieces
                        .drain(..)
                        .map(|piece| read(&piece).unwrap_or_else(|| vec![piece]))
                        .collect();
                    *pieces = concat(parts);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture_walk::{Walked, walked};
    use super::*;

    /// `tests/fixtures/class-client-modules/repo`, one root service, read
    /// with the rows the real sidecar walks.
    fn modules() -> Option<Walked> {
        walked("class-client-modules", ".", "root", &[], true)
    }

    /// What the first argument of the first call named `callee` in the
    /// method `key` of `file` reads as once every module is read.
    fn first_argument(on: &Walked, file: &str, key: &str, callee: &str) -> Vec<Piece> {
        let path = on.path(file);
        let ir = on.inputs.files[&path]
            .functions
            .get(key)
            .unwrap_or_else(|| panic!("no function {key} in {file}"));
        ir.calls
            .iter()
            .find(|call| call.callee == callee)
            .unwrap_or_else(|| panic!("no call of {callee} in {key}"))
            .args[0]
            .pieces()
    }

    fn opaque(pieces: &[Piece]) -> Vec<&str> {
        pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Opaque { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// carrick#2452 slice 2: a field set from a constructor parameter reads
    /// as the text every construction in the service passes, from another
    /// module: a parameter property (case 1), a key of an options object
    /// (case 2) and a construction in the class's own static factory, called
    /// through the class (case 10). Constructions that disagree (3), a class
    /// extended anywhere (4), a construction that hands on its own parameter
    /// (5) and a class handed on as a value (9) keep the field's own name.
    #[test]
    fn a_field_reads_as_every_construction_in_the_service_passes_it() {
        let Some(on) = modules() else {
            return;
        };
        let mut wrong = Vec::new();
        for (case, file, key, callee, text) in [
            ("1", "src/api.ts", "Api.users", "fetch", "/api/v1/users"),
            ("2", "src/items.ts", "Items.list", "send", "/v2/items"),
            ("10", "src/hosts.ts", "Hosts.list", "fetch", "/local/hosts"),
        ] {
            let pieces = first_argument(&on, file, key, callee);
            if pieces != vec![Piece::Lit(text.to_string())] {
                wrong.push(format!("case {case}: {pieces:?}"));
            }
        }
        for (case, file, key) in [
            ("3", "src/region.ts", "Region.ping"),
            ("4", "src/feed.ts", "Feed.recent"),
            ("5", "src/reports.ts", "Reports.all"),
            ("9", "src/hooks.ts", "Hooks.hook"),
        ] {
            let pieces = first_argument(&on, file, key, "fetch");
            if opaque(&pieces) != vec!["this.base"] {
                wrong.push(format!("case {case}: {pieces:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The same request function through discovery: `this.send(…)` set from
    /// an option with `fetch` as the fallback is a site on `fetch`, where
    /// the construction leaves the option out (case 2) and where it hands on
    /// a parameter its own factory defaults to `{}` and nothing calls the
    /// factory (case 6).
    #[test]
    fn a_request_function_a_constructor_sets_is_a_site() {
        let Some(on) = modules() else {
            return;
        };
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "tests/fixtures/class-client-modules/repo/__llm__/library-claims/default.json",
            ))
            .unwrap(),
        );
        let labels = super::super::discovery::Labels::of_answer(&answer);
        let found = super::super::discovery::discover(&on.inputs, &labels, &on.repo);
        let at = |file: &str, line: u32| {
            found
                .sites
                .iter()
                .find(|site| site.file == file && site.line == line)
                .map(|site| {
                    site.end
                        .as_ref()
                        .map(|end| format!("{} {}", end.package, end.export))
                        .unwrap_or_else(|| format!("stop {:?}", site.stop))
                })
        };
        assert_eq!(
            at("src/items.ts", 18).as_deref(),
            Some("lib:lib.dom.d.ts fetch"),
            "case 2: {found:#?}"
        );
        assert_eq!(
            at("src/jobs.ts", 17).as_deref(),
            Some("lib:lib.dom.d.ts fetch"),
            "case 6: {found:#?}"
        );
    }

    /// A construction that passes a field of a class whose field is being
    /// read adds nothing, so the read ends: a method that constructs its own
    /// class with its own field (case 7) and two classes that construct each
    /// other with their own fields (case 8) are sites on the `??` right
    /// side, `fetch`.
    #[test]
    fn a_field_passed_back_into_its_own_construction_ends() {
        let Some(on) = modules() else {
            return;
        };
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(
                "tests/fixtures/class-client-modules/repo/__llm__/library-claims/default.json",
            ))
            .unwrap(),
        );
        let labels = super::super::discovery::Labels::of_answer(&answer);
        let found = super::super::discovery::discover(&on.inputs, &labels, &on.repo);
        let at = |file: &str, line: u32| {
            found
                .sites
                .iter()
                .find(|site| site.file == file && site.line == line)
                .map(|site| {
                    site.end
                        .as_ref()
                        .map(|end| format!("{} {}", end.package, end.export))
                        .unwrap_or_else(|| format!("stop {:?}", site.stop))
                })
        };
        for (case, file, line) in [
            ("7", "src/fork.ts", 20),
            ("8 Ping", "src/ping-pong.ts", 20),
            ("8 Pong", "src/ping-pong.ts", 36),
        ] {
            assert_eq!(
                at(file, line).as_deref(),
                Some("lib:lib.dom.d.ts fetch"),
                "case {case}: {found:#?}"
            );
        }
    }
}
