//! Where a service's route chains leave it (carrick#2474 R5a). Record-only,
//! with the receiver trace on.
//!
//! A route whose instance walk ends at a function nothing in its own
//! service calls or runs is served under whatever another service hands
//! that function. Each service records, during its own analysis, three
//! kinds of fact at its boundary, so that one pass at the engine's join
//! point can match them across services and compose the full path
//! (carrick#2474 R5b) without any service reading another's code:
//!
//! - **route segments** ([`RouteSegment`]): a route site whose chain ends at
//!   a boundary, with the prefixes from that boundary down to the route,
//!   and the site's chains that stay in the service ([`InService`]);
//! - **call-ins** ([`CallIn`]): a call in this service that hands a value to
//!   a function declared in another service, with the chains above the value
//!   and, for a library member that runs the function, the prefix that
//!   member's own name slot states here;
//! - **hand-outs** ([`HandOut`]): a call of a key of an entry point's
//!   parameter (`opts.extra(api)`), handing a value whose chains are kept.
//!
//! The prefix a receiving service's scope states is read in that service
//! and never filled from another's arguments (2026-10-14 ruling 1): a
//! plugin's `opts.prefix ?? '/api/v1'` reads its default because nothing in
//! its own service calls it.
//!
//! Every list is sorted and serialisable. None of it is part of a site
//! record's state, so no site's sha moves.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::RequestSummaryInputs;
use super::super::values::ValueAt;
use super::composed::Mounts;
use super::instances::{
    InstanceEnd, InstanceWalk, MakerLabel, Prefix, ScopeBinds, ScopeLabel, WalkIndex,
};
use super::{Calls, Decided, Placed, SiteLabel, relative};
use crate::position_rows::{FLAG_DECLARATION_UNNAMED, Kind};

/// A function's declaration as the compiler's rows spell it: the file,
/// repo-relative, and the line its declaration starts on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FnAt {
    pub decl_file: String,
    pub decl_line: u32,
}

/// Where a route chain leaves its service.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "boundary", rename_all = "snake_case")]
pub enum Boundary {
    /// The parameter `param` of `function`, which nothing in this service
    /// calls or runs: what fills it is another service's call-in.
    EntryParam { function: FnAt, param: u32 },
    /// A function written under `key` of the object literal at argument
    /// `arg` of the call at `host_at` (`file:line`), whose parameter `param`
    /// holds the instance: what calls it is `runs`, the function the host
    /// runs (a library member's `runs` argument; `runs_arg` is that
    /// argument) or calls (a plain call; `runs_arg` is `None`), through its
    /// hand-out of that key.
    CallbackOption {
        host_at: String,
        arg: u32,
        key: String,
        param: u32,
        runs: FnAt,
        runs_arg: Option<u32>,
    },
}

/// One piece of a chain from the top down, as [`super::composed`]'s chains
/// hold them: a scope or mount edge (`file:line member prefix`), then the
/// prefix it states.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum Part {
    Prefix { prefix: Prefix },
    Edge { edge: String },
}

/// Where a chain read upwards ends.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum End {
    /// An instance served at no mount.
    Top,
    /// A boundary another service's facts continue.
    Boundary { boundary: Boundary },
    /// Any other hole, by its reason (`cycle` included).
    Hole { reason: String },
    /// An instance handed to a call no verified mount reads (carrick#2474
    /// R3 S0).
    MountUnverified,
}

/// One chain above a value: its parts from where it ends down.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Above {
    pub parts: Vec<Part>,
    pub end: End,
}

/// A route site whose chain ends at a boundary: one per such chain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteSegment {
    /// Repo-relative.
    pub file: String,
    pub line: u32,
    /// The method the label forces, upper case.
    pub method: String,
    /// The path the route's own call writes.
    pub path: String,
    /// The handler's name where the call hands one by name.
    pub handler: Option<String>,
    /// The symbol the receiver is read through.
    pub owner: Option<String>,
    /// The parts from the boundary down to the route.
    pub below: Vec<Part>,
    pub at: Boundary,
    /// The site's chains that do not leave the service, as one more host
    /// beside the boundary's: `None` on every segment of the site but the
    /// first, so each is read once.
    pub in_service: Option<InService>,
}

/// A route site's chains that end in its own service: the full paths its
/// chains served at no mount compose, stated as a host, and why each other
/// chain gives none (a hole, an unverified mount, a prefix not read), each
/// an unread host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InService {
    pub paths: Vec<String>,
    pub unread: Vec<String>,
}

/// A call in this service that hands a value to `function`, declared in
/// another service, at its parameter `param`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CallIn {
    pub function: FnAt,
    pub param: u32,
    /// The call, `file:line`, repo-relative.
    pub at: String,
    /// For a library member that runs `function` (`register(plugin, …)`),
    /// the prefix its own name slot states here; `None` for a plain call.
    pub host_prefix: Option<Prefix>,
    /// The chains above the value handed in: the member's receiver for a
    /// library member, the argument for a plain call.
    pub above: Vec<Above>,
}

/// A call inside `function`, an entry point of this service, whose callee
/// is key `key` of the function's parameter `param`, handing a value at its
/// argument `arg` (`opts.extra(api)`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HandOut {
    pub function: FnAt,
    pub param: u32,
    pub key: String,
    pub arg: u32,
    pub instance: Vec<Above>,
}

/// One service's boundary facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteBoundaries {
    pub routes: Vec<RouteSegment>,
    pub call_ins: Vec<CallIn>,
    pub hand_outs: Vec<HandOut>,
}

impl RouteBoundaries {
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.call_ins.is_empty() && self.hand_outs.is_empty()
    }

    /// Every list sorted and without repeats.
    pub(super) fn sorted(mut self) -> Self {
        self.routes.sort();
        self.routes.dedup();
        self.call_ins.sort();
        self.call_ins.dedup();
        self.hand_outs.sort();
        self.hand_outs.dedup();
        self
    }
}

/// What one service's facts are read from: its calls, the labels and the
/// walk index its sites were decided with, and its mount edges.
pub(super) struct Collect<'c, 'a> {
    pub(super) calls: &'c Calls<'a>,
    pub(super) inputs: &'c RequestSummaryInputs,
    pub(super) root: &'c Path,
    pub(super) makers: &'c [MakerLabel],
    pub(super) scopes: &'c [ScopeLabel],
    pub(super) index: &'c WalkIndex<'a>,
    pub(super) mounts: &'c Mounts,
}

/// Whether a declaration's file, as the rows name it, is a file of the
/// repository ([`super::super::discovery::in_repo`]'s file test).
fn repo_file(file: &str) -> bool {
    !file.starts_with("lib:")
        && !file.starts_with("pkg:")
        && !file.split('/').any(|part| part == "node_modules")
}

/// The method a route site's label forces, upper case: the label states an
/// HTTP route it handles, and one producer method, as the member always
/// sends it, as the default where nothing writes the slot, or as literal
/// text the code writes there. The site record forces the same, and reads
/// written values through the source as well; a value only that read
/// states gives no segment here.
fn forced_route_method(site: &super::DecidedSite, labels: &[SiteLabel]) -> Option<String> {
    let label = SiteLabel::find(labels, &site.end)?;
    let (kind, role) = super::super::site_record::kind_and_role(&label.role, label.op.as_deref());
    if kind != Some("http") || role != Some("handles") {
        return None;
    }
    let method = label.method.clone().unwrap_or_default();
    let text = match (&method.literal, &method.from, &site.method) {
        (Some(literal), _, _) => Some(literal.clone()),
        (None, Some(_), Some(super::MethodValue::Unwritten)) | (None, None, _) => method.default,
        (None, Some(_), Some(super::MethodValue::Pieces(pieces))) => pieces
            .iter()
            .map(|piece| match piece {
                super::Piece::Lit(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Option<String>>(),
        (None, Some(_), None) => None,
    }?
    .to_ascii_uppercase();
    crate::type_manifest::is_http_method(&text).then_some(text)
}

/// Whether every chain ends at a value the walk does not read: nothing a
/// boundary continues.
fn unread(aboves: &[Above]) -> bool {
    aboves
        .iter()
        .all(|above| matches!(&above.end, End::Hole { reason } if reason == "unread"))
}

impl<'c, 'a> Collect<'c, 'a> {
    fn walk(&self) -> InstanceWalk<'c, 'a> {
        InstanceWalk::new(
            self.calls,
            self.inputs,
            self.root,
            self.makers,
            self.scopes,
            self.index,
        )
    }

    /// The service's boundary facts, its decided sites `decided` read with
    /// `labels`.
    pub(super) fn of(&self, decided: &Decided, labels: &[SiteLabel]) -> RouteBoundaries {
        let mut placed: Vec<Placed<'a>> = self.calls.by_span.values().copied().collect();
        placed.sort_by(|a, b| {
            (a.file, a.call.site.span_start, a.call.site.span_end).cmp(&(
                b.file,
                b.call.site.span_start,
                b.call.site.span_end,
            ))
        });
        let files: HashSet<String> = self
            .inputs
            .files
            .keys()
            .map(|file| relative(self.root, file))
            .collect();
        RouteBoundaries {
            routes: self.routes(decided, labels),
            call_ins: self.call_ins(&placed, &files),
            hand_outs: self.hand_outs(&placed),
        }
        .sorted()
    }

    /// 1. A route site whose label forces its method, one per chain that
    ///    ends at a boundary. The first carries the site's other chains
    ///    ([`InService`]): a site that also serves the route in its own
    ///    service keeps those paths, and its holes are counted.
    fn routes(&self, decided: &Decided, labels: &[SiteLabel]) -> Vec<RouteSegment> {
        let boundary_end = |end: &str| matches!(end, "entry_point" | "callback_option");
        let mut out = Vec::new();
        for site in &decided.sites {
            if !site
                .composed
                .iter()
                .any(|composed| boundary_end(&composed.end))
            {
                continue;
            }
            let (Some(method), Some(path)) = (forced_route_method(site, labels), site.name_text())
            else {
                continue;
            };
            let mut in_service = InService::default();
            for composed in site
                .composed
                .iter()
                .filter(|composed| !boundary_end(&composed.end))
            {
                match (composed.end.as_str(), &composed.path) {
                    ("top", Some(full_path)) => in_service.paths.push(full_path.clone()),
                    ("top", None) => in_service.unread.push("unread_prefix".to_string()),
                    (end @ ("cycle" | "mount_unverified"), _) => {
                        in_service.unread.push(end.to_string())
                    }
                    (reason, _) => in_service.unread.push(format!("hole_{reason}")),
                }
            }
            in_service.paths.sort();
            in_service.paths.dedup();
            let mut in_service = (!in_service.paths.is_empty() || !in_service.unread.is_empty())
                .then_some(in_service);
            for above in super::composed::aboves(&site.instances, self.mounts) {
                let End::Boundary { boundary } = above.end else {
                    continue;
                };
                out.push(RouteSegment {
                    file: site.file.clone(),
                    line: site.line,
                    method: method.to_uppercase(),
                    path: path.clone(),
                    handler: site
                        .handler
                        .as_ref()
                        .and_then(|handler| handler.name.clone()),
                    owner: site
                        .receiver
                        .as_ref()
                        .and_then(|receiver| receiver.symbol.clone()),
                    below: above.parts,
                    at: boundary,
                    in_service: in_service.take(),
                });
            }
        }
        out
    }

    /// 2. A call that hands a value to a function declared in the repository
    ///    and in no file of this service: through a library member that runs
    ///    it, or a plain call.
    fn call_ins(&self, placed: &[Placed<'a>], files: &HashSet<String>) -> Vec<CallIn> {
        let outside = |file: &str| repo_file(file) && !files.contains(file);
        let mut out = Vec::new();
        for ((decl_file, decl_line), hosts) in self.index.runs_hosts() {
            if !outside(decl_file) {
                continue;
            }
            for (host, arg) in hosts {
                for label in self.scopes.iter().filter(|label| {
                    label.member == host.call.callee
                        && matches!(label.binds, ScopeBinds::Runs { arg: runs, key: None, .. } if runs == *arg)
                }) {
                    let ScopeBinds::Runs { param, .. } = label.binds else {
                        continue;
                    };
                    let ends: Vec<InstanceEnd> = self
                        .walk()
                        .of(&[*host])
                        .into_iter()
                        .filter(|end| end.hole().is_some() || label.applies_to(end))
                        .collect();
                    let above = super::composed::aboves(&ends, self.mounts);
                    if unread(&above) {
                        continue;
                    }
                    out.push(CallIn {
                        function: FnAt {
                            decl_file: decl_file.clone(),
                            decl_line: *decl_line,
                        },
                        param: param as u32,
                        at: self.walk().at_call(host.file, host.call),
                        host_prefix: Some(self.walk().prefix_of(host, &label.name, None)),
                        above,
                    });
                }
            }
        }
        let Some(positions) = self.inputs.positions.as_ref() else {
            return out;
        };
        for call in placed {
            // A call the call graph resolves into this service is its own.
            let own = self
                .calls
                .target(call)
                .is_some_and(|(file, _)| self.inputs.files.contains_key(file));
            if own || self.walk().library_call(call.file, call.call) {
                continue;
            }
            let Some(ir) = self.inputs.files.get(call.file) else {
                continue;
            };
            let Ok(row) = positions.lookup(
                call.file,
                ir.swc_start(call.call.site.lo),
                ir.swc_start(call.call.site.hi),
                Kind::Call,
            ) else {
                continue;
            };
            let (Some(decl_file), Some(decl_line)) = (row.decl_file(), row.decl_line()) else {
                continue;
            };
            if row.flags() & FLAG_DECLARATION_UNNAMED != 0 || !outside(decl_file) {
                continue;
            }
            let args = call.call.args.len().max(call.call.instance.args.len());
            for arg in 0..args {
                let above =
                    super::composed::aboves(&self.walk().of_arg(*call, arg, None), self.mounts);
                if unread(&above) {
                    continue;
                }
                out.push(CallIn {
                    function: FnAt {
                        decl_file: decl_file.to_string(),
                        decl_line,
                    },
                    param: arg as u32,
                    at: self.walk().at_call(call.file, call.call),
                    host_prefix: None,
                    above,
                });
            }
        }
        out
    }

    /// 3. A call inside a function nothing in the service calls or runs,
    ///    whose callee is one key of that function's own parameter.
    fn hand_outs(&self, placed: &[Placed<'a>]) -> Vec<HandOut> {
        let mut out = Vec::new();
        for call in placed {
            let Some(key) = call.key else {
                continue;
            };
            let keys: Vec<(usize, &String)> = call
                .call
                .callee_values
                .iter()
                .filter_map(|value| match value {
                    ValueAt::Param { depth, index, keys } if *depth == call.depth => {
                        match keys.as_slice() {
                            [one] => Some((*index, one)),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect();
            if keys.is_empty()
                || !self.calls.callers_of(call.file, key).is_empty()
                || !self.walk().hosts_of(call.file, key).is_empty()
            {
                continue;
            }
            let Some(function) = self.walk().fn_at(call.file, key) else {
                continue;
            };
            let args = call.call.args.len().max(call.call.instance.args.len());
            for (param, option) in keys {
                for arg in 0..args {
                    let instance =
                        super::composed::aboves(&self.walk().of_arg(*call, arg, None), self.mounts);
                    if unread(&instance) {
                        continue;
                    }
                    out.push(HandOut {
                        function: function.clone(),
                        param: param as u32,
                        key: option.clone(),
                        arg: arg as u32,
                        instance,
                    });
                }
            }
        }
        out
    }
}

/// What a route served by several hosts is, where some host's prefix is not
/// read (carrick#2474 Q5). This is the one switch point for that ruling:
/// the census below and R5b's writer at the engine's join point read
/// [`FAN_OUT`].
///
/// A host is the call-in at the segment's own boundary (for a callback
/// option, its one host). A host is unread when any of its ways does not
/// give a full path (a chain above it, a prefix, a hole): its other ways'
/// paths are not written either, so a partly read host fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanOut {
    /// (i) The route keeps today's reading: nothing is resolved unless every
    /// host gives a full path.
    EveryHost,
    /// (ii) The route takes the paths the other hosts state; each host
    /// whose path is not read is counted.
    StatedHosts,
}

/// carrick#2474 Q5, ruled (ii) by David on 2026-10-23.
pub const FAN_OUT: FanOut = FanOut::StatedHosts;

/// How one route segment resolves across services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    /// The service that recorded the segment.
    pub service: String,
    pub file: String,
    pub line: u32,
    pub method: String,
    /// The path the route's own call writes.
    #[serde(skip)]
    pub path: String,
    /// The handler's name where the call hands one by name.
    #[serde(skip)]
    pub handler: Option<String>,
    /// The symbol the receiver is read through.
    #[serde(skip)]
    pub owner: Option<String>,
    /// Every full path, sorted; empty where the segment does not resolve.
    pub paths: Vec<String>,
    /// Hosts whose path is not read, written around under (ii).
    pub unread_hosts: usize,
    /// For each full path that crossed a service, the files (repo-relative)
    /// of the scope and register calls it was composed through: what the
    /// documentation fold across services reads (carrick#2474 R5d).
    #[serde(skip)]
    pub scope_files: BTreeMap<String, BTreeSet<String>>,
    /// Why nothing resolves: `unmatched_boundary`, `cycle`,
    /// `unread_prefix`, `trimmed`, `hole_<x>`, `mount_unverified` or
    /// `unread_host`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untraced: Option<String>,
}

/// The most full paths one segment composes ([`super::composed::MAX_COMPOSED`]).
const MAX_RESOLVED: usize = super::composed::MAX_COMPOSED;

/// The parts above a boundary, per way it resolves: the parts from the top
/// down, or why a way does not resolve.
type Ways = Vec<Result<Vec<Part>, String>>;

/// The ways `ways` gives, at most [`MAX_RESOLVED`]: past that, the one
/// way left says `trimmed`.
fn capped(ways: impl IntoIterator<Item = Result<Vec<Part>, String>>) -> Ways {
    let mut out = Ways::new();
    for way in ways {
        if out.len() == MAX_RESOLVED {
            return vec![Err("trimmed".to_string())];
        }
        out.push(way);
    }
    out
}

/// Every service's facts, read across services, by the function and
/// parameter each fills or reads.
struct Across<'b> {
    call_ins: BTreeMap<&'b FnAt, BTreeMap<u32, Vec<&'b CallIn>>>,
    hand_outs: BTreeMap<&'b FnAt, BTreeMap<u32, Vec<&'b HandOut>>>,
}

impl<'b> Across<'b> {
    fn of(services: &[(&str, &'b RouteBoundaries)]) -> Self {
        let mut across = Across {
            call_ins: BTreeMap::new(),
            hand_outs: BTreeMap::new(),
        };
        for (_, facts) in services {
            for call_in in &facts.call_ins {
                across
                    .call_ins
                    .entry(&call_in.function)
                    .or_default()
                    .entry(call_in.param)
                    .or_default()
                    .push(call_in);
            }
            for hand_out in &facts.hand_outs {
                across
                    .hand_outs
                    .entry(&hand_out.function)
                    .or_default()
                    .entry(hand_out.param)
                    .or_default()
                    .push(hand_out);
            }
        }
        across
    }

    /// The call-ins that fill parameter `param` of `function`.
    fn call_ins_of(&self, function: &FnAt, param: u32) -> &[&'b CallIn] {
        self.call_ins
            .get(function)
            .and_then(|by_param| by_param.get(&param))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The ways the boundary `at` resolves, each its parts from the top
    /// down to the boundary. `only_at` narrows an entry parameter's call-ins
    /// to the call at that `file:line` (a callback option belongs to its
    /// host's object literal). A boundary met again on the way up is a
    /// cycle.
    fn up(&self, at: &Boundary, only_at: Option<&str>, seen: &mut Vec<Boundary>) -> Ways {
        if seen.contains(at) {
            return vec![Err("cycle".to_string())];
        }
        seen.push(at.clone());
        let ways = match at {
            Boundary::EntryParam { function, param } => {
                let call_ins: Vec<&CallIn> = self
                    .call_ins_of(function, *param)
                    .iter()
                    .copied()
                    .filter(|call_in| only_at.is_none_or(|only| call_in.at == only))
                    .collect();
                if call_ins.is_empty() {
                    vec![Err("unmatched_boundary".to_string())]
                } else {
                    let mut ways = Ways::new();
                    for call_in in call_ins {
                        ways.extend(self.call_in(call_in, seen));
                        if ways.len() > MAX_RESOLVED {
                            break;
                        }
                    }
                    capped(ways)
                }
            }
            Boundary::CallbackOption {
                host_at,
                arg,
                key,
                param,
                runs,
                runs_arg,
            } => self.callback(host_at, *arg, key, *param, runs, *runs_arg, seen),
        };
        seen.pop();
        ways
    }

    /// The ways one call-in resolves: each chain above its value, then the
    /// host member's own prefix.
    fn call_in(&self, call_in: &CallIn, seen: &mut Vec<Boundary>) -> Ways {
        let mut out = Ways::new();
        for above in &call_in.above {
            for way in self.above(above, None, seen) {
                out.push(way.map(|mut parts| {
                    if let Some(prefix) = &call_in.host_prefix {
                        parts.push(Part::Edge {
                            edge: call_in.at.clone(),
                        });
                        parts.push(Part::Prefix {
                            prefix: prefix.clone(),
                        });
                    }
                    parts
                }));
            }
            if out.len() > MAX_RESOLVED {
                break;
            }
        }
        capped(out)
    }

    /// The ways one chain resolves, its own parts below whatever is above
    /// its end.
    fn above(&self, above: &Above, only_at: Option<&str>, seen: &mut Vec<Boundary>) -> Ways {
        let tops = match &above.end {
            End::Top => vec![Ok(Vec::new())],
            End::Boundary { boundary } => self.up(boundary, only_at, seen),
            End::Hole { reason } if reason == "cycle" => vec![Err("cycle".to_string())],
            End::Hole { reason } => vec![Err(format!("hole_{reason}"))],
            End::MountUnverified => vec![Err("mount_unverified".to_string())],
        };
        tops.into_iter()
            .map(|top| {
                top.map(|mut parts| {
                    parts.extend(above.parts.iter().cloned());
                    parts
                })
            })
            .collect()
    }

    /// A callback option: the hand-outs of the function the host runs for
    /// that key and parameter, each filled only by the call-in at the host.
    #[allow(clippy::too_many_arguments)]
    fn callback(
        &self,
        host_at: &str,
        arg: u32,
        key: &str,
        param: u32,
        runs: &FnAt,
        runs_arg: Option<u32>,
        seen: &mut Vec<Boundary>,
    ) -> Ways {
        // The parameter of `runs` the options land at: for a member that
        // runs it, the arguments after the function's are its parameters
        // after the one the member yields (`register(plugin, options)`
        // runs `plugin(instance, options)`).
        let mut q: Vec<u32> = match runs_arg {
            None => vec![arg],
            Some(runs_arg) => self
                .call_ins
                .get(runs)
                .into_iter()
                .flat_map(BTreeMap::values)
                .flatten()
                .filter(|call_in| call_in.at == host_at)
                .filter_map(|call_in| (arg + call_in.param).checked_sub(runs_arg))
                .collect(),
        };
        q.sort_unstable();
        q.dedup();
        let by_param = self.hand_outs.get(runs);
        let hand_outs: Vec<&HandOut> = q
            .iter()
            .filter_map(|q| by_param.and_then(|by_param| by_param.get(q)))
            .flatten()
            .copied()
            .filter(|hand_out| hand_out.key == key && hand_out.arg == param)
            .collect();
        if hand_outs.is_empty() {
            return vec![Err("unmatched_boundary".to_string())];
        }
        let mut out = Ways::new();
        for hand_out in hand_outs {
            for above in &hand_out.instance {
                out.extend(self.above(above, Some(host_at), seen));
                if out.len() > MAX_RESOLVED {
                    return capped(out);
                }
            }
        }
        capped(out)
    }
}

/// The text of each full path `parts` and the route's own `path` compose:
/// one per stated value (#2474 Q2), or why none.
fn full_paths(parts: &[Part], path: &str) -> Result<Vec<String>, String> {
    let mut paths: Vec<Vec<String>> = vec![Vec::new()];
    for part in parts {
        let Part::Prefix { prefix } = part else {
            continue;
        };
        match prefix {
            Prefix::Unstated => {}
            Prefix::Unread { .. } => return Err("unread_prefix".to_string()),
            Prefix::Stated { values } => {
                paths = paths
                    .into_iter()
                    .flat_map(|path| {
                        values.iter().map(move |value| {
                            let mut path = path.clone();
                            path.push(value.clone());
                            path
                        })
                    })
                    .take(MAX_RESOLVED + 1)
                    .collect();
                if paths.len() > MAX_RESOLVED {
                    return Err("trimmed".to_string());
                }
            }
        }
    }
    Ok(paths
        .into_iter()
        .map(|mut parts| {
            parts.push(path.to_string());
            parts.iter().fold(String::new(), |path, part| {
                crate::utils::join_prefix_and_path(&path, part)
            })
        })
        .collect())
}

/// Every route segment of `services` (each service's name and facts),
/// resolved across all of them under `fan_out`.
///
/// A segment's boundary takes each call-in that fills it from any service;
/// a callback option takes the hand-outs of the function its host runs,
/// filled only by the call-in at that host. Each call-in at the segment's
/// own boundary is one host: under [`FanOut::EveryHost`] a host whose path
/// is not read leaves the whole route unresolved, under
/// [`FanOut::StatedHosts`] the other hosts' paths stand and it is counted.
/// The site's chains that stay in its own service ([`InService`]) are one
/// more host: under (ii) a site whose boundary nothing fills still resolves
/// at the paths it serves in its own service, with that boundary counted
/// as an unread host rather than the route left `unmatched_boundary`.
pub fn resolve(services: &[(&str, &RouteBoundaries)], fan_out: FanOut) -> Vec<Resolved> {
    let across = Across::of(services);
    let mut out = Vec::new();
    for (service, facts) in services {
        for segment in &facts.routes {
            out.push(resolve_segment(&across, service, segment, fan_out));
        }
    }
    out
}

fn resolve_segment(
    across: &Across<'_>,
    service: &str,
    segment: &RouteSegment,
    fan_out: FanOut,
) -> Resolved {
    // One entry per host: the ways that host's call-in resolves.
    let hosts: Vec<Ways> = match &segment.at {
        Boundary::EntryParam { function, param } => {
            let mut hosts: Vec<Ways> = across
                .call_ins_of(function, *param)
                .iter()
                .map(|call_in| {
                    let mut seen = vec![segment.at.clone()];
                    across.call_in(call_in, &mut seen)
                })
                .collect();
            if hosts.is_empty() {
                hosts.push(vec![Err("unmatched_boundary".to_string())]);
            }
            hosts
        }
        Boundary::CallbackOption { .. } => vec![across.up(&segment.at, None, &mut Vec::new())],
    };
    let mut paths: Vec<String> = Vec::new();
    let mut scope_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut unread_hosts = 0;
    for ways in hosts {
        let mut host_paths: Vec<String> = Vec::new();
        let mut host_reason: Option<String> = None;
        for way in ways {
            let composed = way.and_then(|mut parts| {
                parts.extend(segment.below.iter().cloned());
                let files = edge_files(&parts);
                full_paths(&parts, &segment.path).map(|paths| (paths, files))
            });
            match composed {
                Ok((composed, files)) => {
                    for path in &composed {
                        scope_files
                            .entry(path.clone())
                            .or_default()
                            .extend(files.iter().cloned());
                    }
                    host_paths.extend(composed);
                }
                Err(reason) => {
                    host_reason.get_or_insert(reason);
                }
            }
        }
        match host_reason {
            Some(reason) => {
                unread_hosts += 1;
                *reasons.entry(reason).or_default() += 1;
            }
            None => paths.extend(host_paths),
        }
    }
    // The site's own chains are one more host: the paths it serves the
    // route at in its own service stand beside the boundary's, and each of
    // its holes is a host not read.
    if let Some(in_service) = &segment.in_service {
        paths.extend(in_service.paths.iter().cloned());
        for reason in &in_service.unread {
            unread_hosts += 1;
            *reasons.entry(reason.clone()).or_default() += 1;
        }
    }
    paths.sort();
    paths.dedup();
    let untraced = if paths.is_empty() {
        reasons.into_keys().next()
    } else if paths.len() > MAX_RESOLVED {
        Some("trimmed".to_string())
    } else if unread_hosts > 0 && fan_out == FanOut::EveryHost {
        Some("unread_host".to_string())
    } else {
        None
    };
    if untraced.is_some() {
        paths.clear();
        scope_files.clear();
    }
    scope_files.retain(|path, _| paths.contains(path));
    Resolved {
        service: service.to_string(),
        file: segment.file.clone(),
        line: segment.line,
        method: segment.method.clone(),
        path: segment.path.clone(),
        handler: segment.handler.clone(),
        owner: segment.owner.clone(),
        paths,
        unread_hosts,
        scope_files,
        untraced,
    }
}

/// The files of the scope and register calls `parts` pass through: each
/// edge's `file:line`, repo-relative.
fn edge_files(parts: &[Part]) -> BTreeSet<String> {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Edge { edge } => edge
                .split_whitespace()
                .next()
                .and_then(|at| at.rsplit_once(':'))
                .map(|(file, _)| file.to_string()),
            Part::Prefix { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file: &str, line: u32) -> FnAt {
        FnAt {
            decl_file: file.to_string(),
            decl_line: line,
        }
    }

    fn stated(value: &str) -> Part {
        Part::Prefix {
            prefix: Prefix::Stated {
                values: vec![value.to_string()],
            },
        }
    }

    fn entry(function: &FnAt) -> Boundary {
        Boundary::EntryParam {
            function: function.clone(),
            param: 0,
        }
    }

    fn top(parts: Vec<Part>) -> Above {
        Above {
            parts,
            end: End::Top,
        }
    }

    fn segment(boundary: Boundary, below: Vec<Part>) -> RouteSegment {
        RouteSegment {
            file: "packages/r/src/a.ts".to_string(),
            line: 3,
            method: "GET".to_string(),
            path: "/x".to_string(),
            handler: None,
            owner: None,
            below,
            at: boundary,
            in_service: None,
        }
    }

    fn call_in(function: &FnAt, at: &str, host_prefix: Option<Prefix>, above: Above) -> CallIn {
        CallIn {
            function: function.clone(),
            param: 0,
            at: at.to_string(),
            host_prefix,
            above: vec![above],
        }
    }

    /// The plugin `P(app, opts)` of `r` opens a scope under `/api` and hands
    /// it to `opts.extra`; the host in `s` registers `P` on its root with a
    /// callback under `extra` that calls `r`'s `admin(scope)`.
    fn two_crossings() -> (RouteBoundaries, RouteBoundaries, FnAt, FnAt) {
        let plugin = at("packages/r/src/plugin.ts", 5);
        let admin = at("packages/r/src/admin.ts", 2);
        let callback = Boundary::CallbackOption {
            host_at: "packages/s/src/main.ts:9".to_string(),
            arg: 1,
            key: "extra".to_string(),
            param: 0,
            runs: plugin.clone(),
            runs_arg: Some(0),
        };
        let r = RouteBoundaries {
            routes: vec![segment(entry(&admin), Vec::new())],
            call_ins: Vec::new(),
            hand_outs: vec![HandOut {
                function: plugin.clone(),
                param: 1,
                key: "extra".to_string(),
                arg: 0,
                instance: vec![Above {
                    parts: vec![stated("/api")],
                    end: End::Boundary {
                        boundary: entry(&plugin),
                    },
                }],
            }],
        };
        let s = RouteBoundaries {
            routes: Vec::new(),
            call_ins: vec![
                call_in(
                    &plugin,
                    "packages/s/src/main.ts:9",
                    Some(Prefix::Unstated),
                    top(vec![stated("/root")]),
                ),
                call_in(
                    &admin,
                    "packages/s/src/main.ts:11",
                    None,
                    Above {
                        parts: Vec::new(),
                        end: End::Boundary { boundary: callback },
                    },
                ),
            ],
            hand_outs: Vec::new(),
        };
        (r, s, plugin, admin)
    }

    /// carrick#2474 R5a: a route reached through two crossings (its
    /// function called from the host's callback, the callback run by the
    /// plugin's hand-out, the plugin registered by the host) takes every
    /// prefix from the host's root down.
    #[test]
    fn a_route_resolves_through_two_crossings() {
        let (r, s, _, _) = two_crossings();
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].paths, vec!["/root/api/x".to_string()]);
        assert_eq!(resolved[0].untraced, None);
    }

    /// carrick#2474 R5d: each full path names the files of the scope and
    /// register calls it was composed through, so the documentation fold
    /// across services can tell whose code gave the route its path.
    #[test]
    fn a_path_names_the_files_of_the_calls_it_was_composed_through() {
        let (r, mut s, _, _) = two_crossings();
        s.call_ins[0].above = vec![top(vec![
            Part::Edge {
                edge: "packages/s/src/main.ts:3 register /root".to_string(),
            },
            stated("/root"),
        ])];
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        let files: Vec<&str> = resolved[0].scope_files["/root/api/x"]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(files, vec!["packages/s/src/main.ts"]);
        // A path that does not resolve names none.
        let (r, mut s, _, _) = two_crossings();
        s.call_ins[0].at = "packages/s/src/other.ts:4".to_string();
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert!(resolved[0].scope_files.is_empty());
    }

    /// carrick#2474 R5a: a callback option belongs to its host's object
    /// literal: another host registering the same plugin does not fill it.
    #[test]
    fn a_callback_option_is_filled_only_by_its_own_host() {
        let (r, mut s, plugin, _) = two_crossings();
        // The callback's host is no call-in any more; another host is.
        s.call_ins[0].at = "packages/s/src/other.ts:4".to_string();
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved[0].paths, Vec::<String>::new());
        assert_eq!(resolved[0].untraced.as_deref(), Some("unmatched_boundary"));
        // Both hosts present: only the callback's own host's prefix is read.
        let (r, mut s, _, _) = two_crossings();
        s.call_ins.push(call_in(
            &plugin,
            "packages/s/src/other.ts:4",
            Some(Prefix::Unstated),
            top(vec![stated("/elsewhere")]),
        ));
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved[0].paths, vec!["/root/api/x".to_string()]);
    }

    /// carrick#2474 R5a: a boundary met again on the way up ends the way as
    /// a cycle.
    #[test]
    fn a_boundary_met_again_is_a_cycle() {
        let f = at("packages/r/src/f.ts", 1);
        let g = at("packages/s/src/g.ts", 1);
        let r = RouteBoundaries {
            routes: vec![segment(entry(&f), Vec::new())],
            call_ins: vec![call_in(
                &g,
                "packages/r/src/f.ts:3",
                None,
                Above {
                    parts: Vec::new(),
                    end: End::Boundary {
                        boundary: entry(&f),
                    },
                },
            )],
            hand_outs: Vec::new(),
        };
        let s = RouteBoundaries {
            routes: Vec::new(),
            call_ins: vec![call_in(
                &f,
                "packages/s/src/g.ts:3",
                None,
                Above {
                    parts: Vec::new(),
                    end: End::Boundary {
                        boundary: entry(&g),
                    },
                },
            )],
            hand_outs: Vec::new(),
        };
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved[0].untraced.as_deref(), Some("cycle"));
    }

    /// carrick#2474 R5b: the ways one host resolves are capped at
    /// [`MAX_RESOLVED`]: a host reached through more ways than that is
    /// counted `trimmed`, even where every way would give the same path.
    #[test]
    fn a_host_reached_through_too_many_ways_is_trimmed() {
        let f = at("packages/r/src/f.ts", 1);
        let g = at("packages/s/src/g.ts", 1);
        let r = RouteBoundaries {
            routes: vec![segment(entry(&f), Vec::new())],
            ..RouteBoundaries::default()
        };
        // `g` calls `f` with its own parameter; `g` is called from as many
        // places as the cap allows, then one more, each under `/a`.
        let mut call_ins = vec![call_in(
            &f,
            "packages/s/src/g.ts:3",
            None,
            Above {
                parts: Vec::new(),
                end: End::Boundary {
                    boundary: entry(&g),
                },
            },
        )];
        let callers = |count: usize| -> Vec<CallIn> {
            (0..count)
                .map(|index| {
                    call_in(
                        &g,
                        &format!("packages/t/src/main.ts:{index}"),
                        None,
                        top(vec![stated("/a")]),
                    )
                })
                .collect()
        };
        call_ins.extend(callers(MAX_RESOLVED));
        let s = RouteBoundaries {
            call_ins: call_ins.clone(),
            ..RouteBoundaries::default()
        };
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved[0].paths, vec!["/a/x".to_string()]);
        call_ins.extend(callers(MAX_RESOLVED + 1).into_iter().skip(MAX_RESOLVED));
        let s = RouteBoundaries {
            call_ins,
            ..RouteBoundaries::default()
        };
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(resolved[0].paths, Vec::<String>::new());
        assert_eq!(resolved[0].untraced.as_deref(), Some("trimmed"));
    }

    /// carrick#2474 Q5: two hosts that agree give one path; a third whose
    /// prefix is not read is counted, and under (i) leaves the route
    /// unresolved while under (ii) the stated hosts' path stands.
    #[test]
    fn hosts_that_agree_give_one_path_and_an_unread_host_follows_the_switch() {
        let plugin = at("packages/r/src/plugin.ts", 5);
        let r = RouteBoundaries {
            routes: vec![segment(entry(&plugin), vec![stated("/api/v1")])],
            ..RouteBoundaries::default()
        };
        let host = |at: &str, prefix: Prefix| call_in(&plugin, at, Some(prefix), top(Vec::new()));
        let s = RouteBoundaries {
            call_ins: vec![
                host("packages/s/src/main.ts:4", Prefix::Unstated),
                host("packages/t/src/main.ts:4", Prefix::Unstated),
            ],
            ..RouteBoundaries::default()
        };
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::EveryHost);
        assert_eq!(resolved[0].paths, vec!["/api/v1/x".to_string()]);
        assert_eq!(resolved[0].unread_hosts, 0);
        let mut s = s;
        s.call_ins.push(host(
            "packages/u/src/main.ts:4",
            Prefix::Unread { text: None },
        ));
        let every = resolve(&[("r", &r), ("s", &s)], FanOut::EveryHost);
        assert_eq!(every[0].paths, Vec::<String>::new());
        assert_eq!(every[0].untraced.as_deref(), Some("unread_host"));
        assert_eq!(every[0].unread_hosts, 1);
        let stated = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(stated[0].paths, vec!["/api/v1/x".to_string()]);
        assert_eq!(stated[0].unread_hosts, 1);
        assert_eq!(stated[0].untraced, None);
    }

    /// PR #2734 review: a site whose route is also served in its own
    /// service keeps that path beside the boundary's, and each chain of its
    /// own that gives no path is an unread host. With no call-in at the
    /// boundary, the site's own path still stands.
    #[test]
    fn a_site_served_in_its_own_service_keeps_that_path_beside_the_boundarys() {
        let plugin = at("packages/r/src/plugin.ts", 5);
        let r = RouteBoundaries {
            routes: vec![RouteSegment {
                in_service: Some(InService {
                    paths: vec!["/x".to_string()],
                    unread: vec!["hole_unread".to_string()],
                }),
                ..segment(entry(&plugin), vec![stated("/api/v1")])
            }],
            ..RouteBoundaries::default()
        };
        let s = RouteBoundaries {
            call_ins: vec![call_in(
                &plugin,
                "packages/s/src/main.ts:4",
                Some(Prefix::Unstated),
                top(Vec::new()),
            )],
            ..RouteBoundaries::default()
        };
        let resolved = resolve(&[("r", &r), ("s", &s)], FanOut::StatedHosts);
        assert_eq!(
            resolved[0].paths,
            vec!["/api/v1/x".to_string(), "/x".to_string()]
        );
        assert_eq!(resolved[0].unread_hosts, 1);
        assert_eq!(resolved[0].untraced, None);
        let alone = resolve(&[("r", &r)], FanOut::StatedHosts);
        assert_eq!(alone[0].paths, vec!["/x".to_string()]);
        assert_eq!(alone[0].unread_hosts, 2);
        let every = resolve(&[("r", &r), ("s", &s)], FanOut::EveryHost);
        assert_eq!(every[0].untraced.as_deref(), Some("unread_host"));
    }
}
