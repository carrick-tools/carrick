//! Procedure routes: the key path a procedure is served under (carrick#2474
//! R6c). With the receiver trace on.
//!
//! A procedure router names each operation by the key its value sits under
//! in an object a router member collects (label `collects`, contract
//! amendment 3 on carrick#2434), not by a path the call writes: the op's
//! label names it `bound: key`, so its call is a route site with no name
//! slot ([`super::Walk::site`]). An adapter (a maker whose `runs` slot takes
//! the collection) serves the collection over HTTP. This pass reads both
//! from the code, top down, one adapter call at a time:
//!
//! - **The collection** the adapter is handed at its `runs` slot is read
//!   through the syntax: a call the compiler resolves to a verified
//!   `collects` member (its object at the label's slot, each key joined
//!   with the label's `join`), a name the compiler resolves to a binding of
//!   this service (its initialiser, in its own module), a call of one of
//!   this service's functions (each value it returns), or an object literal
//!   under a key. A procedure op's labelled call reached this way is named
//!   by the keys above it.
//! - **A key is unread** when it is computed, or written before a spread or
//!   a computed key that may set it again; a spread's own keys are unread.
//!   A procedure under an unread key is counted, never named.
//! - **The walk stops** at a binding that may be set again (not `const`):
//!   a procedure below it is counted (`walk_stopped`), never named. A name
//!   whose binding it cannot read (a destructured one), a function whose
//!   returns it cannot read, and the depth cap are counted on the pass
//!   ([`super::Decided::procedure_stops`]), apart from a procedure no
//!   adapter serves.
//! - **The adapter's mount**: every value its `prefix` slot states; with
//!   none, the host call the adapter's result is handed to: a verified
//!   `mounts` member at its mounted slot, under its prefix and the scopes
//!   above it ([`super::composed::compose`]), or a `receive` op's handler
//!   slot whose path ends in a catch-all segment, at that route's composed
//!   paths. A catch-all segment at the end of either is dropped
//!   ([`carrick_match::is_catch_all_path`]). Anything else is unread.
//!
//! A procedure's full path is the mount, `/`, and its key path, one per
//! mount and per key path that reach it (#2474 Q2). The served paths ride
//! on the site ([`super::DecidedSite::procedure`]) and never on its record's
//! state; [`crate::site_rows::traced`] writes them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use swc_common::{SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::super::RequestSummaryInputs;
use super::composed::{Mounts, compose};
use super::instances::{InstanceWalk, MakerLabel, Prefix, ScopeBinds, ScopeLabel, WalkIndex};
use super::{Calls, Decided, LabelledEnd, SiteLabel, relative};
use crate::position_rows::Kind;

/// What the store labels for procedure routers, as this pass reads it.
#[derive(Debug, Clone, Default)]
pub struct ProcedureLabels {
    /// The ops named by their key (`bound: key`): package, export, member.
    keyed: Vec<(String, String, String)>,
    /// The verified router members.
    collects: Vec<CollectsLabel>,
    /// The makers that take a collection at their `runs` slot.
    adapters: Vec<AdapterLabel>,
}

/// A router member the trace's own request verified: where it takes its
/// object, how its keys join, and the member declarations its verdicts
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CollectsLabel {
    arg: usize,
    key: Option<String>,
    join: String,
    declarations: Vec<(String, u32)>,
}

/// A maker that may serve a collection: the export, the member and form,
/// the `runs` slot and the `prefix` slot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AdapterLabel {
    package: String,
    export: String,
    member: Option<String>,
    new: bool,
    runs: (usize, Option<String>),
    prefix: Option<(usize, Option<String>)>,
}

impl ProcedureLabels {
    /// The procedure labels of the settled `exports`, with the router
    /// members the trace's own request verified (`verified`, by claim id
    /// and receiver, as [`ScopeLabel::from_exports`] reads them).
    pub fn from_exports(
        exports: &[crate::library_claims::ExportClaims],
        verified: &HashMap<super::super::discovery::AskedScope, Vec<(String, u32)>>,
    ) -> Self {
        use crate::services::type_sidecar::{BoundName, LibraryClaim, MakeForm, OpName};
        let mut labels = ProcedureLabels::default();
        for export in exports {
            for (claim_id, claim) in export.claims_with_scopes_by_id() {
                match claim {
                    LibraryClaim::Scope {
                        collects: Some(collects),
                        path,
                        ..
                    } if path.is_empty() => {
                        let mut declarations: Vec<(String, u32)> = verified
                            .iter()
                            .filter(|(asked, _)| {
                                asked.package == export.package
                                    && asked.export == export.export
                                    && asked.claim_id == claim_id
                            })
                            .flat_map(|(_, declarations)| declarations.iter().cloned())
                            .collect();
                        declarations.sort();
                        declarations.dedup();
                        if declarations.is_empty() {
                            continue;
                        }
                        let label = CollectsLabel {
                            arg: collects.arg as usize,
                            key: collects.key.clone(),
                            join: collects.join.clone(),
                            declarations,
                        };
                        if !labels.collects.contains(&label) {
                            labels.collects.push(label);
                        }
                    }
                    LibraryClaim::Op {
                        member: Some(member),
                        name: Some(name),
                        ..
                    } if matches!(
                        **name,
                        OpName::Bound {
                            bound: BoundName::Key,
                            ..
                        }
                    ) =>
                    {
                        let keyed = (
                            export.package.clone(),
                            export.export.clone(),
                            member.clone(),
                        );
                        if !labels.keyed.contains(&keyed) {
                            labels.keyed.push(keyed);
                        }
                    }
                    _ => {}
                }
            }
            for claim in &export.claims {
                if let LibraryClaim::Make {
                    form,
                    member,
                    runs: Some(runs),
                    prefix,
                    ..
                } = claim
                {
                    let label = AdapterLabel {
                        package: export.package.clone(),
                        export: export.export.clone(),
                        member: member.clone(),
                        new: *form == MakeForm::New,
                        runs: (runs.arg as usize, runs.key.clone()),
                        prefix: prefix
                            .as_ref()
                            .map(|prefix| (prefix.arg as usize, prefix.key.clone())),
                    };
                    if !labels.adapters.contains(&label) {
                        labels.adapters.push(label);
                    }
                }
            }
        }
        labels
    }

    /// Whether the labelled call `end` is a procedure op's.
    fn keyed(&self, end: &LabelledEnd) -> bool {
        self.keyed.iter().any(|(package, export, member)| {
            *package == end.package
                && *export == end.export
                && end.member.as_deref() == Some(member.as_str())
        })
    }

    fn adapter(&self, end: &LabelledEnd) -> Option<&AdapterLabel> {
        self.adapters.iter().find(|adapter| {
            adapter.package == end.package
                && adapter.export == end.export
                && adapter.member == end.member
                && adapter.new == end.new
        })
    }
}

/// Where a procedure route site is served (carrick#2474 R6c): every full
/// path, the router binding that collects it, and the ways that reached it
/// and named no path, by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Procedure {
    /// Sorted, one per mount and key path.
    pub full_paths: Vec<String>,
    /// Sorted: each key path a served way names it by.
    pub key_paths: Vec<String>,
    /// The binding of the innermost router that collects it, where one
    /// holds it.
    pub owner: Option<String>,
    /// `unread_key`, `walk_stopped`, `unread_mount`, `mount_hole`: one per adapter and key
    /// path that reached the site and named no path.
    pub unread: BTreeMap<String, usize>,
}

/// One parsed module, with the source map its spans are in.
struct Parsed {
    cm: Lrc<SourceMap>,
    module: Module,
}

impl Parsed {
    fn line(&self, span: swc_common::Span) -> u32 {
        self.cm.lookup_char_pos(span.lo).line as u32
    }
}

/// The key path above a value: `Some` while every key is read.
#[derive(Clone)]
struct Above {
    path: Option<String>,
    /// The innermost router's join, once one is reached.
    join: Option<String>,
    owner: Option<String>,
    /// The binding whose initialiser the value is.
    bound: Option<String>,
    /// The value is read through a binding that may be set again: a
    /// procedure below it is counted, never named.
    stopped: bool,
}

/// The deepest a value is followed before the walk gives up on it.
const MAX_DEPTH: usize = 32;

pub(super) struct Pass<'w, 'a> {
    pub(super) calls: &'w Calls<'a>,
    pub(super) inputs: &'w RequestSummaryInputs,
    pub(super) root: &'w Path,
    pub(super) makers: &'w [MakerLabel],
    pub(super) scopes: &'w [ScopeLabel],
    pub(super) index: &'w WalkIndex<'a>,
    pub(super) mounts: &'w Mounts,
    pub(super) labels: &'w ProcedureLabels,
    pub(super) site_labels: &'w [SiteLabel],
}

/// A procedure site the walk reached (by its labelled call's file and
/// span) with the key path that names it, or why none does (`unread_key`,
/// `walk_stopped`), and its owner.
type Leaf = (
    (String, u32, u32),
    Result<String, &'static str>,
    Option<String>,
);

/// What the walk down one adapter's collection reached.
#[derive(Default)]
struct Reached {
    leaves: Vec<Leaf>,
    /// Each value the walk stopped at with no procedure to count it on: a
    /// name of this service it could not read the binding of, a function
    /// of this service whose returns it could not read, or the depth cap.
    stops: usize,
}

impl Pass<'_, '_> {
    /// Name every procedure route site of `out` (carrick#2474 R6c), read
    /// from the adapter calls among `discovered`.
    pub(super) fn run(&self, out: &mut Decided, discovered: &[super::DiscoverySite]) {
        let leaves: HashSet<(String, u32, u32)> = out
            .sites
            .iter()
            .filter(|site| self.labels.keyed(&site.end))
            .map(|site| {
                (
                    site.end.file.clone(),
                    site.end.span_start,
                    site.end.span_end,
                )
            })
            .collect();
        if leaves.is_empty() {
            return;
        }
        let mut modules: HashMap<PathBuf, Option<Rc<Parsed>>> = HashMap::new();
        let mut served: HashMap<(String, u32, u32), Procedure> = HashMap::new();
        let mut adapters: BTreeSet<(String, u32, u32)> = BTreeSet::new();
        for site in discovered {
            let (Some(end), None) = (&site.end, &site.stop) else {
                continue;
            };
            let Some(adapter) = self.labels.adapter(end) else {
                continue;
            };
            if !adapters.insert((end.file.clone(), end.span_start, end.span_end)) {
                continue;
            }
            let file = self.root.join(&end.file);
            let Some(parsed) = self.parsed(&mut modules, &file) else {
                continue;
            };
            let mut at = super::CallAt {
                target: (end.span_start, end.span_end),
                args: None,
            };
            parsed.module.visit_with(&mut at);
            let Some(args) = at.args else {
                continue;
            };
            let Some(collection) = slot_expr(&args, &adapter.runs) else {
                continue;
            };
            let mut reached = Reached::default();
            let above = Above {
                path: Some(String::new()),
                join: None,
                owner: None,
                bound: None,
                stopped: false,
            };
            self.walk(
                &mut modules,
                &file,
                &collection,
                above,
                &leaves,
                &mut Vec::new(),
                &mut reached,
            );
            out.procedure_stops += reached.stops;
            if reached.leaves.is_empty() {
                continue;
            }
            let mounts = self.mounts_of(&mut modules, &file, end, adapter, &out.sites);
            for (leaf, path, owner) in reached.leaves {
                let procedure = served.entry(leaf).or_default();
                if procedure.owner.is_none() {
                    procedure.owner = owner;
                }
                for mount in &mounts {
                    match (mount, &path) {
                        (Ok(mount), Ok(path)) => {
                            procedure
                                .full_paths
                                .push(crate::utils::join_prefix_and_path(mount, path));
                            procedure.key_paths.push(path.clone());
                        }
                        (Err(reason), _) | (Ok(_), Err(reason)) => {
                            *procedure.unread.entry(reason.to_string()).or_default() += 1
                        }
                    }
                }
            }
        }
        for site in &mut out.sites {
            if !self.labels.keyed(&site.end) {
                continue;
            }
            let leaf = (
                site.end.file.clone(),
                site.end.span_start,
                site.end.span_end,
            );
            let mut procedure = served.get(&leaf).cloned().unwrap_or_default();
            procedure.full_paths.sort();
            procedure.full_paths.dedup();
            procedure.key_paths.sort();
            procedure.key_paths.dedup();
            site.procedure = Some(procedure);
        }
    }

    fn parsed(
        &self,
        modules: &mut HashMap<PathBuf, Option<Rc<Parsed>>>,
        file: &Path,
    ) -> Option<Rc<Parsed>> {
        modules
            .entry(file.to_path_buf())
            .or_insert_with(|| {
                let cm: Lrc<SourceMap> = Default::default();
                let handler = swc_common::errors::Handler::with_tty_emitter(
                    swc_common::errors::ColorConfig::Never,
                    false,
                    false,
                    Some(cm.clone()),
                );
                let module = crate::parser::parse_file(file, &cm, &handler)?;
                Some(Rc::new(Parsed { cm, module }))
            })
            .clone()
    }

    /// The declaration the compiler's row of `kind` at `span` in `file`
    /// names, as this service's file and line, with the name it declares
    /// (an import's alias resolved: `import { a as b }` names `a`); `None`
    /// for one outside the service.
    fn own_declaration(
        &self,
        file: &Path,
        span: swc_common::Span,
        kind: Kind,
    ) -> Option<(PathBuf, u32, Option<String>)> {
        let positions = self.inputs.positions.as_ref()?;
        let row = positions.lookup(file, span.lo.0, span.hi.0, kind).ok()?;
        if !super::super::discovery::in_repo(row) {
            return None;
        }
        let declared = positions.path_of(row.decl_file()?);
        let line = row.decl_line()?;
        self.inputs
            .files
            .contains_key(&declared)
            .then(|| (declared, line, row.symbol().map(str::to_string)))
    }

    /// The verified router member the call at `span` in `file` resolves
    /// to, as the compiler's call row names its declaration.
    fn collects_of(&self, file: &Path, span: swc_common::Span) -> Option<&CollectsLabel> {
        let positions = self.inputs.positions.as_ref()?;
        let row = positions
            .lookup(file, span.lo.0, span.hi.0, Kind::Call)
            .ok()?;
        let declared = (row.decl_file()?.to_string(), row.decl_line()?);
        let mut matched = self
            .labels
            .collects
            .iter()
            .filter(|label| label.declarations.contains(&declared));
        let label = matched.next()?;
        // Two router members at one declaration that collect differently
        // say nothing about which this is.
        matched.all(|other| other == label).then_some(label)
    }

    /// Walk down `expr`, written in `file`, to every procedure site in
    /// `leaves` it holds, each with the key path above it.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        modules: &mut HashMap<PathBuf, Option<Rc<Parsed>>>,
        file: &Path,
        expr: &Expr,
        above: Above,
        leaves: &HashSet<(String, u32, u32)>,
        stack: &mut Vec<(PathBuf, u32, u32)>,
        reached: &mut Reached,
    ) {
        let expr = super::super::unwrap_value(expr);
        let span = expr.span();
        let here = (file.to_path_buf(), span.lo.0, span.hi.0);
        if stack.contains(&here) {
            return;
        }
        if stack.len() >= MAX_DEPTH {
            reached.stops += 1;
            return;
        }
        stack.push(here);
        match expr {
            Expr::Call(call) => {
                let leaf = (relative(self.root, file), span.lo.0, span.hi.0);
                if leaves.contains(&leaf) {
                    // A procedure is named by its keys only under a router,
                    // and never below a binding that may be set again.
                    let path = if above.stopped {
                        Err("walk_stopped")
                    } else {
                        above
                            .join
                            .as_ref()
                            .and(above.path.clone())
                            .ok_or("unread_key")
                    };
                    reached.leaves.push((leaf, path, above.owner.clone()));
                } else if let Some(label) = self.collects_of(file, span) {
                    let collected = slot_expr(&call.args, &(label.arg, label.key.clone()));
                    if let Some(collected) = collected {
                        let below = Above {
                            path: above.path.clone(),
                            join: Some(label.join.clone()),
                            owner: above.bound.clone().or(above.owner.clone()),
                            bound: None,
                            stopped: above.stopped,
                        };
                        self.walk(modules, file, &collected, below, leaves, stack, reached);
                    }
                } else if let Some((declared, line, _)) =
                    self.own_declaration(file, span, Kind::Call)
                {
                    let values = self
                        .parsed(modules, &declared)
                        .map(|parsed| returned_values(&parsed, line))
                        .unwrap_or_default();
                    if values.is_empty() {
                        reached.stops += 1;
                    }
                    for value in values {
                        let below = Above {
                            bound: None,
                            ..above.clone()
                        };
                        self.walk(modules, &declared, &value, below, leaves, stack, reached);
                    }
                }
            }
            Expr::Ident(_) => {
                if let Some((declared, line, symbol)) =
                    self.own_declaration(file, span, Kind::Ident)
                {
                    // The binding the compiler resolves the name to, by the
                    // name it declares: a use under an import alias reads
                    // the declaration's own name.
                    let binding = symbol.and_then(|symbol| {
                        let parsed = self.parsed(modules, &declared)?;
                        binding_init(&parsed, &symbol, line).map(|binding| (symbol, binding))
                    });
                    match binding {
                        Some((symbol, (init, constant))) => {
                            let below = Above {
                                bound: Some(symbol),
                                stopped: above.stopped || !constant,
                                ..above
                            };
                            self.walk(modules, &declared, &init, below, leaves, stack, reached);
                        }
                        None => reached.stops += 1,
                    }
                }
            }
            Expr::Object(object) => {
                // A key some later spread, computed key or accessor may set
                // again is not read as written.
                let wide_after: Vec<bool> = {
                    let mut wide = false;
                    let mut after = vec![false; object.props.len()];
                    for (index, prop) in object.props.iter().enumerate().rev() {
                        after[index] = wide;
                        wide |= widens(prop);
                    }
                    after
                };
                for (prop, wide) in object.props.iter().zip(wide_after) {
                    let (key, value): (Option<String>, Expr) = match prop {
                        PropOrSpread::Spread(spread) => (None, (*spread.expr).clone()),
                        PropOrSpread::Prop(prop) => match &**prop {
                            Prop::KeyValue(entry) => {
                                (super::super::prop_name(&entry.key), (*entry.value).clone())
                            }
                            Prop::Shorthand(name) => {
                                (Some(name.sym.to_string()), Expr::Ident(name.clone()))
                            }
                            _ => continue,
                        },
                    };
                    let path = match (&above.path, &above.join, key) {
                        (Some(path), Some(join), Some(key)) if !wide => Some(if path.is_empty() {
                            key
                        } else {
                            format!("{path}{join}{key}")
                        }),
                        _ => None,
                    };
                    let below = Above {
                        path,
                        bound: None,
                        ..above.clone()
                    };
                    self.walk(modules, file, &value, below, leaves, stack, reached);
                }
            }
            _ => {}
        }
        stack.pop();
    }

    /// Where the adapter call `end`, written in `file`, serves what it
    /// runs: each mount path, or why one is unread.
    fn mounts_of(
        &self,
        modules: &mut HashMap<PathBuf, Option<Rc<Parsed>>>,
        file: &Path,
        end: &LabelledEnd,
        adapter: &AdapterLabel,
        sites: &[super::DecidedSite],
    ) -> Vec<Result<String, &'static str>> {
        let walk = || {
            InstanceWalk::new(
                self.calls,
                self.inputs,
                self.root,
                self.makers,
                self.scopes,
                self.index,
            )
        };
        let Some(placed) = self.calls.at_span(file, end.span_start, end.span_end) else {
            return vec![Err("unread_mount")];
        };
        if let Some(prefix) = &adapter.prefix {
            match walk().prefix_of(&placed, prefix, None) {
                Prefix::Stated { values } => return values.into_iter().map(Ok).collect(),
                Prefix::Unread { .. } => return vec![Err("unread_mount")],
                Prefix::Unstated => {}
            }
        }
        let Some(parsed) = self.parsed(modules, file) else {
            return vec![Err("unread_mount")];
        };
        let Some((host_lo, host_hi, arg)) = host_of(&parsed.module, end.span_start, end.span_end)
        else {
            return vec![Err("unread_mount")];
        };
        let Some(host) = self.calls.at_span(file, host_lo, host_hi) else {
            return vec![Err("unread_mount")];
        };
        // A verified mount member, at the slot it mounts.
        let mount_labels: Vec<&ScopeLabel> = self
            .scopes
            .iter()
            .filter(|label| {
                label.member == host.call.callee
                    && matches!(&label.binds, ScopeBinds::Mounts { arg: at, key: None } if *at == arg)
            })
            .collect();
        if !mount_labels.is_empty() && walk().library_call(file, host.call) {
            let receivers = walk().of(&[host]);
            let mut out: Vec<Result<String, &'static str>> = Vec::new();
            for label in mount_labels {
                let applying: Vec<_> = receivers
                    .iter()
                    .filter(|end| label.applies_to(end))
                    .cloned()
                    .collect();
                if applying.is_empty() {
                    continue;
                }
                let values = match walk().prefix_of(&host, &label.name, None) {
                    Prefix::Stated { values } => values,
                    Prefix::Unstated => vec!["/".to_string()],
                    Prefix::Unread { .. } => {
                        out.push(Err("unread_mount"));
                        continue;
                    }
                };
                // A mount that ends in a catch-all serves the paths below
                // it, as one written without the segment does.
                for value in values {
                    out.extend(
                        served_at(&compose(&applying, Some(value.as_str()), self.mounts).0)
                            .into_iter()
                            .map(|path| path.map(|path| below_catch_all(&path).unwrap_or(path))),
                    );
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        // A route that hands every path below a catch-all to its handler.
        let host_end = relative(self.root, file);
        let routes: Vec<&super::DecidedSite> = sites
            .iter()
            .filter(|site| {
                site.end.file == host_end
                    && site.end.span_start == host_lo
                    && site.end.span_end == host_hi
                    && SiteLabel::find(self.site_labels, &site.end)
                        .and_then(|label| label.handler.as_ref())
                        .is_some_and(|slot| slot.arg == arg && slot.key.is_none())
            })
            .collect();
        if routes.is_empty() {
            return vec![Err("unread_mount")];
        }
        let mut out = Vec::new();
        for route in routes {
            if route.composed_trimmed || route.composed.is_empty() {
                out.push(Err("mount_hole"));
                continue;
            }
            for path in served_at(&route.composed) {
                out.push(path.and_then(|path| below_catch_all(&path).ok_or("unread_mount")));
            }
        }
        out
    }
}

/// Each composed path served from the top, or why it is not.
fn served_at(composed: &[super::Composed]) -> Vec<Result<String, &'static str>> {
    composed
        .iter()
        .map(|composed| match (composed.end.as_str(), &composed.path) {
            ("top", Some(path)) => Ok(path.clone()),
            ("top", None) => Err("unread_mount"),
            _ => Err("mount_hole"),
        })
        .collect()
}

/// `path` without its last segment, when that segment is a catch-all
/// ([`carrick_match::is_catch_all_path`]: `*`, `**`, `(.*)`): what serves
/// every path below it.
fn below_catch_all(path: &str) -> Option<String> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if !carrick_match::is_catch_all_path(&path) {
        return None;
    }
    let (above, _) = path.rsplit_once('/')?;
    Some(if above.is_empty() {
        "/".to_string()
    } else {
        above.to_string()
    })
}

/// Whether `prop` may set a key the object writes before it: a spread, a
/// computed key, or an accessor or method.
fn widens(prop: &PropOrSpread) -> bool {
    match prop {
        PropOrSpread::Spread(_) => true,
        PropOrSpread::Prop(prop) => match &**prop {
            Prop::KeyValue(entry) => super::super::prop_name(&entry.key).is_none(),
            Prop::Shorthand(_) => false,
            _ => true,
        },
    }
}

/// The expression at `slot` (an argument, or one key of the object written
/// there) of a call's arguments; `None` past a spread argument, for a key
/// of anything but an object literal, or for a key a later spread or
/// computed key may set.
fn slot_expr(args: &[ExprOrSpread], slot: &(usize, Option<String>)) -> Option<Expr> {
    let (arg, key) = slot;
    if args.iter().take(arg + 1).any(|arg| arg.spread.is_some()) {
        return None;
    }
    let written = super::super::unwrap_value(&args.get(*arg)?.expr);
    let Some(key) = key else {
        return Some(written.clone());
    };
    let Expr::Object(object) = written else {
        return None;
    };
    let mut found = None;
    for prop in &object.props {
        if found.is_some() && widens(prop) {
            return None;
        }
        if let PropOrSpread::Prop(prop) = prop {
            match &**prop {
                Prop::KeyValue(entry)
                    if super::super::prop_name(&entry.key).as_ref() == Some(key) =>
                {
                    found = Some((*entry.value).clone());
                }
                Prop::Shorthand(name) if name.sym == *key.as_str() => {
                    found = Some(Expr::Ident(name.clone()));
                }
                _ => {}
            }
        }
    }
    found
}

/// The call of `module` handed the expression at `lo..hi` as one of its
/// arguments (before any spread): its span and that argument.
fn host_of(module: &Module, lo: u32, hi: u32) -> Option<(u32, u32, usize)> {
    struct Find {
        target: (u32, u32),
        found: Option<(u32, u32, usize)>,
    }
    impl Visit for Find {
        fn visit_call_expr(&mut self, call: &CallExpr) {
            for (index, arg) in call.args.iter().enumerate() {
                if arg.spread.is_some() {
                    break;
                }
                let span = super::super::unwrap_value(&arg.expr).span();
                if (span.lo.0, span.hi.0) == self.target {
                    self.found = Some((call.span.lo.0, call.span.hi.0, index));
                }
            }
            call.visit_children_with(self);
        }
    }
    let mut find = Find {
        target: (lo, hi),
        found: None,
    };
    module.visit_with(&mut find);
    find.found
}

/// The initialiser of the binding `name` declared on `line`, and whether
/// the binding is `const` (one that is not may be set again).
fn binding_init(parsed: &Parsed, name: &str, line: u32) -> Option<(Expr, bool)> {
    struct Find<'p> {
        parsed: &'p Parsed,
        name: &'p str,
        line: u32,
        found: Option<(Expr, bool)>,
    }
    impl Visit for Find<'_> {
        fn visit_var_decl(&mut self, var: &VarDecl) {
            for declarator in &var.decls {
                if let Pat::Ident(binding) = &declarator.name
                    && binding.id.sym == *self.name
                    && self.parsed.line(binding.id.span) == self.line
                    && let Some(init) = &declarator.init
                {
                    self.found = Some(((**init).clone(), var.kind == VarDeclKind::Const));
                }
            }
            var.visit_children_with(self);
        }
    }
    let mut find = Find {
        parsed,
        name,
        line,
        found: None,
    };
    parsed.module.visit_with(&mut find);
    find.found
}

/// The values the function declared on `line` returns: an arrow's body
/// expression, or each value its body returns as a builder's is read
/// ([`super::super::returned_values`]: a body of `const`s and `return`s,
/// none for any other).
fn returned_values(parsed: &Parsed, line: u32) -> Vec<Expr> {
    fn of_body(body: &FunctionBody) -> Vec<Expr> {
        super::super::returned_values(&body.stmts).unwrap_or_default()
    }
    fn of_arrow(arrow: &ArrowExpr) -> Vec<Expr> {
        match &*arrow.body {
            ArrowFunctionBody::Expr(expr) => vec![(**expr).clone()],
            ArrowFunctionBody::FunctionBody(body) => of_body(body),
        }
    }
    struct Find<'p> {
        parsed: &'p Parsed,
        line: u32,
        found: Option<Vec<Expr>>,
    }
    impl Visit for Find<'_> {
        fn visit_fn_decl(&mut self, decl: &FnDecl) {
            if self.parsed.line(decl.ident.span) == self.line
                && let Some(body) = &decl.function.body
            {
                self.found = Some(of_body(body));
            }
            decl.visit_children_with(self);
        }
        fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
            if let Pat::Ident(binding) = &declarator.name
                && self.parsed.line(binding.id.span) == self.line
            {
                match declarator.init.as_deref().map(super::super::unwrap_value) {
                    Some(Expr::Arrow(arrow)) => self.found = Some(of_arrow(arrow)),
                    Some(Expr::Fn(function)) => {
                        self.found = function.function.body.as_ref().map(of_body);
                    }
                    _ => {}
                }
            }
            declarator.visit_children_with(self);
        }
    }
    let mut find = Find {
        parsed,
        line,
        found: None,
    };
    parsed.module.visit_with(&mut find);
    find.found.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catch_all_segment_serves_the_paths_below_it() {
        assert_eq!(below_catch_all("/api/rpc/*"), Some("/api/rpc".to_string()));
        assert_eq!(below_catch_all("/*"), Some("/".to_string()));
        assert_eq!(below_catch_all("*"), Some("/".to_string()));
        assert_eq!(below_catch_all("/api/**"), Some("/api".to_string()));
        assert_eq!(
            below_catch_all("/api/rpc/(.*)"),
            Some("/api/rpc".to_string())
        );
        assert_eq!(below_catch_all("/(.*)"), Some("/".to_string()));
        assert_eq!(below_catch_all("/api/rpc"), None);
        assert_eq!(below_catch_all("/api/:rest"), None);
    }
}
