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
//!   through what the scan's IR holds of each value ([`ValueAt`], with an
//!   object literal's entries in order, [`super::super::resolve::ObjectEntries`],
//!   and the binding table, carrick#3538): a call the compiler resolves to a verified
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
//!   adapter serves. A name declared in a nested block, and a function no
//!   definition key joins, are read from a fallback the binding table does
//!   not yet replace (carrick#3794), and each such read is counted there
//!   too.
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

use super::super::resolve::{BindingKind, MAX_PROCEDURE_DEPTH, ValueAt};
use super::super::row_calls::Reach;
use super::super::{CallIr, FileIr, RequestSummaryInputs};
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
    /// The verified members that merge the routers they are given.
    merges: Vec<MergesLabel>,
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

/// A member the trace's own request verified as taking routers from `arg`
/// on (a rest slot, contract amendment 9), and the declarations its
/// verdicts name. Its return serves every argument's procedures at the
/// level it is called at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MergesLabel {
    arg: usize,
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

/// The member declarations the request's verdicts name for the claim
/// `claim_id` of `export`, sorted and without repeats.
fn verified_declarations(
    export: &crate::library_claims::ExportClaims,
    claim_id: &str,
    verified: &HashMap<super::super::discovery::AskedScope, Vec<(String, u32)>>,
) -> Vec<(String, u32)> {
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
    declarations
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
                        let declarations = verified_declarations(export, &claim_id, verified);
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
                    LibraryClaim::Scope {
                        merges: Some(merges),
                        collects: None,
                        path,
                        ..
                    } if path.is_empty() => {
                        let declarations = verified_declarations(export, &claim_id, verified);
                        if declarations.is_empty() {
                            continue;
                        }
                        let label = MergesLabel {
                            arg: merges.arg as usize,
                            declarations,
                        };
                        if !labels.merges.contains(&label) {
                            labels.merges.push(label);
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
    /// Sorted: each full path with the key path it is served under and the
    /// router that declares it, where the router is named.
    pub at: Vec<ProcedureAt>,
    /// `unread_key`, `walk_stopped`, `unread_mount`, `mount_hole`: one per adapter and key
    /// path that reached the site and named no path.
    pub unread: BTreeMap<String, usize>,
}

/// A procedure at one full path: the key path it is served under and the
/// router that declares it, as [`super::router_holder`] names it. The repo
/// is the engine's to add.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcedureAt {
    pub full_path: String,
    pub key_path: String,
    /// Repo-relative, `/`-separated.
    pub router_file: String,
    pub router_binding: String,
}

/// The key path above a value: `Some` while every key is read.
#[derive(Clone)]
struct Above {
    path: Option<String>,
    /// The innermost router's join, once one is reached.
    join: Option<String>,
    owner: Option<String>,
    /// The file and line of the property of the first key under the router,
    /// once one is read.
    first_key: Option<(String, u32)>,
    /// The binding whose initialiser the value is.
    bound: Option<String>,
    /// The value is read through a binding that may be set again: a
    /// procedure below it is counted, never named.
    stopped: bool,
    /// The value is part of what a function returns, read with the
    /// function's own locals in place: a local it is held in binds no
    /// router name, as the returns were always read (carrick#3538 M1).
    inlined: bool,
}

/// A span in SWC's own numbering, as the compiler's rows and the sites key
/// one.
type SwcSpan = (u32, u32);

/// A binding a name reads, as [`Pass::binding`] finds it.
struct Binding {
    init: Vec<ValueAt>,
    /// Declared `const`: never set again.
    constant: bool,
    /// Read from the fallback (carrick#3794).
    fallback: bool,
    /// A function's own local, not a module binding.
    local: bool,
}

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
    Option<(String, u32)>,
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
        // The router each first key's property is held by, by its file and
        // line ([`super::router_holder::router_holder`]).
        let mut holders: HashMap<(String, u32), Option<String>> = HashMap::new();
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
            let Some(placed) = self.calls.at_span(&file, end.span_start, end.span_end) else {
                continue;
            };
            let Some(collection) = slot_values(placed.call, &adapter.runs) else {
                continue;
            };
            let mut reached = Reached::default();
            let above = Above {
                path: Some(String::new()),
                join: None,
                owner: None,
                first_key: None,
                bound: None,
                stopped: false,
                inlined: false,
            };
            let mut stack = Vec::new();
            for value in &collection {
                self.walk(
                    &file,
                    value,
                    above.clone(),
                    &leaves,
                    &mut stack,
                    &mut reached,
                );
            }
            out.procedure_stops += reached.stops;
            if reached.leaves.is_empty() {
                continue;
            }
            let mounts = self.mounts_of(&file, end, adapter, &out.sites);
            for (leaf, path, owner, first_key) in reached.leaves {
                let procedure = served.entry(leaf).or_default();
                if procedure.owner.is_none() {
                    procedure.owner = owner;
                }
                let router = first_key.and_then(|(router_file, line)| {
                    let binding = holders
                        .entry((router_file.clone(), line))
                        .or_insert_with(|| {
                            super::router_holder::router_holder(self.root, &router_file, line)
                        })
                        .clone()?;
                    Some((router_file, binding))
                });
                for mount in &mounts {
                    match (mount, &path) {
                        (Ok(mount), Ok(path)) => {
                            procedure
                                .full_paths
                                .push(crate::utils::join_prefix_and_path(mount, path));
                            procedure.key_paths.push(path.clone());
                            if let Some((router_file, router_binding)) = &router {
                                procedure.at.push(ProcedureAt {
                                    full_path: crate::utils::join_prefix_and_path(mount, path),
                                    key_path: path.clone(),
                                    router_file: router_file.clone(),
                                    router_binding: router_binding.clone(),
                                });
                            }
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
            procedure.at.sort();
            procedure.at.dedup();
            site.procedure = Some(procedure);
        }
    }

    /// The declaration the compiler's row of `kind` at `span` in `file`
    /// names, as this service's file and line, with the name it declares
    /// (an import's alias resolved: `import { a as b }` names `a`); `None`
    /// for one outside the service.
    fn own_declaration(
        &self,
        file: &Path,
        span: SwcSpan,
        kind: Kind,
    ) -> Option<(PathBuf, u32, Option<String>)> {
        let positions = self.inputs.positions.as_ref()?;
        let row = positions.lookup(file, span.0, span.1, kind).ok()?;
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
    fn collects_of(&self, file: &Path, span: SwcSpan) -> Option<&CollectsLabel> {
        let positions = self.inputs.positions.as_ref()?;
        let row = positions.lookup(file, span.0, span.1, Kind::Call).ok()?;
        self.collects_at(&(row.decl_file()?.to_string(), row.decl_line()?))
    }

    /// The one `collects` label that holds `declared` as a declaration.
    fn collects_at(&self, declared: &(String, u32)) -> Option<&CollectsLabel> {
        let mut matched = self
            .labels
            .collects
            .iter()
            .filter(|label| label.declarations.contains(declared));
        let label = matched.next()?;
        // Two router members at one declaration that collect differently
        // say nothing about which this is.
        matched.all(|other| other == label).then_some(label)
    }

    /// The one `merges` label that holds `declared` as a declaration.
    fn merges_at(&self, declared: &(String, u32)) -> Option<&MergesLabel> {
        let mut matched = self
            .labels
            .merges
            .iter()
            .filter(|label| label.declarations.contains(declared));
        let label = matched.next()?;
        matched.all(|other| other == label).then_some(label)
    }

    /// The verified merging member the call at `span` in `file` resolves
    /// to, directly or through one `const` alias. A declaration both a
    /// `collects` and a `merges` label hold says nothing.
    fn merges_of(&self, file: &Path, span: SwcSpan) -> Option<&MergesLabel> {
        let direct = self.inputs.positions.as_ref().and_then(|positions| {
            let row = positions.lookup(file, span.0, span.1, Kind::Call).ok()?;
            Some((row.decl_file()?.to_string(), row.decl_line()?))
        });
        let declared = match direct.filter(|declared| self.merges_at(declared).is_some()) {
            Some(declared) => declared,
            None => self.aliased_declaration(file, span)?,
        };
        let label = self.merges_at(&declared)?;
        (self.collects_at(&declared).is_none()).then_some(label)
    }

    /// The declaration (file and line) a call's callee names through one
    /// hop: the callee is an in-repo `const` whose initialiser is a member
    /// or a name, and the compiler's row at that initialiser names the
    /// declaration. A `let`, or any other initialiser, names nothing.
    fn aliased_declaration(&self, file: &Path, span: SwcSpan) -> Option<(String, u32)> {
        let (declared, line, symbol) = self.own_declaration(file, span, Kind::Call)?;
        let binding = self.binding(&declared, line, &symbol?)?;
        if !binding.constant {
            return None;
        }
        let ir = self.calls.file_ir(&declared)?;
        let (lo, hi, kinds): (u32, u32, &[Kind]) = match binding.init.as_slice() {
            [
                ValueAt::KeyOf { lo, hi, .. }
                | ValueAt::Field { lo, hi, .. }
                | ValueAt::Read {
                    lo,
                    hi,
                    member: true,
                    ..
                },
            ] => (*lo, *hi, &[Kind::Member, Kind::Ident]),
            [
                ValueAt::Read {
                    lo,
                    hi,
                    member: false,
                    ..
                },
            ] => (*lo, *hi, &[Kind::Ident, Kind::Member]),
            _ => return None,
        };
        let positions = self.inputs.positions.as_ref()?;
        let (lo, hi) = (ir.swc_start(lo), ir.swc_start(hi));
        kinds.iter().find_map(|kind| {
            let row = positions.lookup(&declared, lo, hi, *kind).ok()?;
            Some((row.decl_file()?.to_string(), row.decl_line()?))
        })
    }

    /// The verified router member a call goes through an alias to.
    fn collects_through_alias(&self, file: &Path, span: SwcSpan) -> Option<&CollectsLabel> {
        let declared = self.aliased_declaration(file, span)?;
        self.collects_at(&declared)
    }

    /// The initialiser of the plain binding `name` declared on `line` of
    /// `declared`, whether it is a `const` (one that is not may be set
    /// again), and whether it was read from the fallback: the binding
    /// table's row, or, for a declarator a nested block writes, the fallback
    /// (carrick#3794). `None` for anything else the name declares (a
    /// destructured key, a parameter, an enum, a class member, a function)
    /// and for a declaration with no initialiser.
    fn binding(&self, declared: &Path, line: u32, name: &str) -> Option<Binding> {
        let ir = self.calls.file_ir(declared)?;
        let key = (line, name.to_string());
        let binding = match ir.module_bindings.get(&key) {
            Some(binding) => {
                let plain = binding.plain
                    && matches!(
                        binding.kind,
                        BindingKind::Const | BindingKind::Let | BindingKind::Var
                    );
                if !plain {
                    return None;
                }
                Binding {
                    init: binding.init.clone(),
                    constant: binding.kind == BindingKind::Const,
                    fallback: false,
                    local: binding.declared_in.is_some(),
                }
            }
            // FALLBACK (carrick#3794): delete once block-scoped and
            // nested-function locals are in the one binding table.
            None => {
                let block = ir.block_bindings.get(&key).filter(|_| false)?;
                Binding {
                    init: block.init.clone(),
                    constant: block.constant,
                    fallback: true,
                    local: true,
                }
            }
        };
        (!binding.init.is_empty()).then_some(binding)
    }

    /// The values the function the declaration of `name` on `line` of
    /// `declared` is returns: its definition's, or, for a function no
    /// definition key joins, the fallback (counted on `reached`).
    fn returns_of(
        &self,
        declared: &Path,
        line: u32,
        name: Option<&str>,
        reached: &mut Reached,
    ) -> Vec<ValueAt> {
        let Some(name) = name else {
            return Vec::new();
        };
        let Some(ir) = self.calls.file_ir(declared) else {
            return Vec::new();
        };
        if let Reach::Function(_, key) = self.inputs.definition_at(declared, Some(line), name)
            && let Some(function) = ir.functions.get(&key)
        {
            return function.returned_values.clone();
        }
        // FALLBACK (carrick#3794): delete once block-scoped and
        // nested-function locals are in the one binding table.
        match ir.block_returns.get(&(line, name.to_string())) {
            Some(values) => {
                reached.stops += 1;
                values.clone()
            }
            None => Vec::new(),
        }
    }

    /// The call of `file` handed the expression at `span` (SWC numbering) as
    /// one of its arguments, before any spread: its span and that argument.
    fn host_of(&self, file: &Path, lo: u32, hi: u32) -> Option<(u32, u32, usize)> {
        let ir = self.calls.file_ir(file)?;
        let target = (ir.discovery_start(lo), ir.discovery_start(hi));
        self.calls
            .by_span
            .iter()
            .filter(|(key, placed)| key.0 == file && !placed.call.constructs)
            .find_map(|(_, placed)| {
                let index = placed.call.arg_spans.iter().position(|at| *at == target)?;
                Some((
                    placed.call.site.span_start,
                    placed.call.site.span_end,
                    index,
                ))
            })
    }

    /// Walk down `value`, read in `file`, to every procedure site in
    /// `leaves` it holds, each with the key path above it.
    fn walk(
        &self,
        file: &Path,
        value: &ValueAt,
        above: Above,
        leaves: &HashSet<(String, u32, u32)>,
        stack: &mut Vec<(PathBuf, u32, u32)>,
        reached: &mut Reached,
    ) {
        let (lo, hi) = match value {
            // A parameter's binding holds no initialiser to read.
            ValueAt::Param { .. } => {
                reached.stops += 1;
                return;
            }
            ValueAt::Returned { lo, hi, .. }
            | ValueAt::Read {
                lo,
                hi,
                member: false,
                ..
            } => (*lo, *hi),
            ValueAt::Object { entries, .. } => (entries.lo, entries.hi),
            _ => return,
        };
        let here = (file.to_path_buf(), lo, hi);
        if stack.contains(&here) {
            return;
        }
        if stack.len() >= MAX_PROCEDURE_DEPTH {
            reached.stops += 1;
            return;
        }
        let Some(ir) = self.calls.file_ir(file) else {
            return;
        };
        // A value read through a function-local name is the binding's
        // (carrick#3538 M1), as one read through a module binding is.
        let held = ir.held_inits.get(&(lo, hi)).filter(|_| false);
        let above = match held {
            Some(held) => Above {
                bound: Some(held.name.clone()),
                stopped: above.stopped || !held.constant,
                ..above
            },
            None => above,
        };
        stack.push(here);
        match value {
            ValueAt::Returned { .. } => {
                self.call(file, ir, (lo, hi), above, leaves, stack, reached);
            }
            ValueAt::Read { .. } => self.name(file, ir, (lo, hi), above, leaves, stack, reached),
            ValueAt::Object {
                fields, entries, ..
            } => {
                for entry in &entries.props {
                    let path = match (&above.path, &above.join, &entry.key) {
                        (Some(path), Some(join), Some(key)) if !entry.widened => {
                            Some(if path.is_empty() {
                                key.clone()
                            } else {
                                format!("{path}{join}{key}")
                            })
                        }
                        _ => None,
                    };
                    // The first key read under a router is the property the
                    // router is named from (`router_holder`).
                    let first_key =
                        match (&above.path, &above.join, &path, above.first_key.is_none()) {
                            (Some(before), Some(_), Some(_), true) if before.is_empty() => {
                                Some((relative(self.root, file), entry.line))
                            }
                            _ => above.first_key.clone(),
                        };
                    let below = Above {
                        path,
                        first_key,
                        bound: None,
                        ..above.clone()
                    };
                    for value in entry.values(fields) {
                        self.walk(file, value, below.clone(), leaves, stack, reached);
                    }
                }
            }
            _ => {}
        }
        stack.pop();
    }

    /// The name read at `at` (discovery numbering) in `file`: the
    /// initialiser of the binding the compiler's row names.
    #[allow(clippy::too_many_arguments)]
    fn name(
        &self,
        file: &Path,
        ir: &FileIr,
        at: (u32, u32),
        above: Above,
        leaves: &HashSet<(String, u32, u32)>,
        stack: &mut Vec<(PathBuf, u32, u32)>,
        reached: &mut Reached,
    ) {
        let span = (ir.swc_start(at.0), ir.swc_start(at.1));
        let Some((declared, line, symbol)) = self.own_declaration(file, span, Kind::Ident) else {
            return;
        };
        // The binding the compiler resolves the name to, by the name it
        // declares: a use under an import alias reads the declaration's own
        // name.
        let binding = symbol.and_then(|symbol| {
            let binding = self.binding(&declared, line, &symbol)?;
            Some((symbol, binding))
        });
        match binding {
            Some((symbol, binding)) => {
                // FALLBACK (carrick#3794): each read is counted.
                if binding.fallback {
                    reached.stops += 1;
                }
                let stopped = above.stopped || !binding.constant;
                // A function's own local read inside what it returns is in
                // place there, as the returns were always read: it names
                // no router.
                let below = if above.inlined && binding.local {
                    Above { stopped, ..above }
                } else {
                    Above {
                        bound: Some(symbol),
                        stopped,
                        inlined: false,
                        ..above
                    }
                };
                for value in &binding.init {
                    self.walk(&declared, value, below.clone(), leaves, stack, reached);
                }
            }
            None => reached.stops += 1,
        }
    }

    /// The call at `at` (discovery numbering) in `file`: a procedure, a
    /// router member, a merge, or a function of this service.
    #[allow(clippy::too_many_arguments)]
    fn call(
        &self,
        file: &Path,
        ir: &FileIr,
        at: (u32, u32),
        above: Above,
        leaves: &HashSet<(String, u32, u32)>,
        stack: &mut Vec<(PathBuf, u32, u32)>,
        reached: &mut Reached,
    ) {
        let span = (ir.swc_start(at.0), ir.swc_start(at.1));
        let leaf = (relative(self.root, file), span.0, span.1);
        if leaves.contains(&leaf) {
            // A procedure is named by its keys only under a router, and
            // never below a binding that may be set again.
            let path = if above.stopped {
                Err("walk_stopped")
            } else {
                above
                    .join
                    .as_ref()
                    .and(above.path.clone())
                    .ok_or("unread_key")
            };
            reached
                .leaves
                .push((leaf, path, above.owner.clone(), above.first_key.clone()));
            return;
        }
        if let Some(label) = self
            .collects_of(file, span)
            .or_else(|| self.collects_through_alias(file, span))
        {
            let collected = self
                .calls
                .at_span(file, span.0, span.1)
                .and_then(|placed| slot_values(placed.call, &(label.arg, label.key.clone())));
            if let Some(collected) = collected {
                let below = Above {
                    path: above.path.clone(),
                    join: Some(label.join.clone()),
                    owner: above.bound.clone().or(above.owner.clone()),
                    first_key: above.first_key.clone(),
                    bound: None,
                    stopped: above.stopped,
                    inlined: above.inlined,
                };
                for value in &collected {
                    self.walk(file, value, below.clone(), leaves, stack, reached);
                }
            }
        } else if let Some(label) = self.merges_of(file, span) {
            // Every argument from the slot on is a router served at the
            // level the merge is called at, under the same keys.
            let Some(placed) = self.calls.at_span(file, span.0, span.1) else {
                return;
            };
            let below = Above {
                owner: above.bound.clone().or(above.owner.clone()),
                bound: None,
                ..above.clone()
            };
            let arguments = placed
                .call
                .arg_values
                .iter()
                .chain(&placed.call.rest_values)
                .skip(label.arg);
            for argument in arguments {
                // A spread argument: an empty entry.
                if argument.is_empty() {
                    reached.stops += 1;
                }
                for value in argument {
                    self.walk(file, value, below.clone(), leaves, stack, reached);
                }
            }
        } else if let Some((declared, line, symbol)) = self.own_declaration(file, span, Kind::Call)
        {
            let values = self.returns_of(&declared, line, symbol.as_deref(), reached);
            if values.is_empty() {
                reached.stops += 1;
            }
            let below = Above {
                bound: None,
                inlined: true,
                ..above.clone()
            };
            for value in &values {
                self.walk(&declared, value, below.clone(), leaves, stack, reached);
            }
        }
    }

    /// Where the adapter call `end`, written in `file`, serves what it
    /// runs: each mount path, or why one is unread.
    fn mounts_of(
        &self,
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
        let Some((host_lo, host_hi, arg)) = self.host_of(file, end.span_start, end.span_end) else {
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
            let applying_to = walk().applying(&host, &mount_labels, &receivers);
            for (label, applying) in mount_labels.into_iter().zip(applying_to) {
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

/// What the call's argument at `slot` (an argument, or one key of the
/// object literal written there) may hold; `None` past a spread argument,
/// for a key of anything but an object literal written in place, or for a
/// key a later spread or computed key may set.
fn slot_values(call: &CallIr, slot: &(usize, Option<String>)) -> Option<Vec<ValueAt>> {
    let (arg, key) = slot;
    // A spread at or before the position leaves it empty or absent.
    let values = call
        .arg_values
        .get(*arg)
        .filter(|values| !values.is_empty())?;
    let Some(key) = key else {
        return Some(values.clone());
    };
    let written = call.arg_spans.get(*arg)?;
    let (fields, entries) = values.iter().find_map(|value| match value {
        ValueAt::Object {
            fields, entries, ..
        } if (entries.lo, entries.hi) == *written => Some((fields, entries)),
        _ => None,
    })?;
    let mut matching = entries
        .props
        .iter()
        .filter(|entry| entry.key.as_deref() == Some(key.as_str()));
    let first = matching.next()?;
    if first.widened {
        return None;
    }
    // The last write of the key is the value.
    let last = matching.next_back().unwrap_or(first);
    Some(last.values(fields).to_vec())
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
