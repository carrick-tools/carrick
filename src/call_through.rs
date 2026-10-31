//! A model row at a call through the repository's own code takes the request
//! that call reaches (carrick#1146).
//!
//! A request is one row at the line that writes it. A call that reaches the
//! request through functions the repository declares writes neither a method
//! nor a path, and the model, asked about such a line, makes them up from the
//! names it can see:
//!
//! ```ignore
//! // package `client`, one service
//! export const getRun = (options) =>
//!   transport.get({ url: "/api/v1/runs/{id}", ...options });   // the request row
//!
//! // package `cli`, another service
//! const client = createClient();
//! const run = await client.getRun(id);   // the model's row: `GET /getRun`
//! ```
//!
//! The call graph says which definition each call reaches, and every service's
//! deterministic rows say which requests each definition writes. Where the two
//! together prove ONE operation behind a model row's call, the row takes that
//! operation. Where they prove none, or several, the row is left as the model
//! stated it and counted: it can be the only record of the call.
//!
//! The reached request is usually in another service, and one service's
//! analysis never holds another's rows (each runs on a thread of its own). So
//! the work is in two halves:
//!
//! - [`read_sites`], per service, while its files and its resolved call sites
//!   are in hand: for each model call row, the definitions its call expression
//!   goes through. Stored with the service's blob ([`StoredCallThrough`]),
//!   so a service `--changed-only` carries states them as an analysed one
//!   does (carrick#2378).
//! - [`settle`], once per scan, when every service's rows and definitions are
//!   in hand: the walk and the change.
//!
//! Nothing here names a library, a framework, a file or a symbol.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use swc_common::{
    BytePos, FileName, SourceMap, Span, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{CallExpr, Callee, Expr, ExprOrSpread, OptCall, Prop, PropOrSpread, Str, Tpl};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult, ResolutionSource};
use crate::agents::file_orchestrator::FileOrchestrator;
use crate::cloud_storage::{CloudRepoData, ManifestRole, ManifestTypeKind};
use crate::graphql_document_sites::unwrap_expression;
use crate::mount_graph::{ConsumerRole, DataFetchingCall};
use crate::operation::OperationKey;
use crate::parser::{parse_file, parse_source};
use crate::request_summary::{CallReach, RequestSummaryInputs};
use crate::return_flow::ShapeKind;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::normalize_manifest_method;
use crate::wrapper_request_shape::{RequestShapeSignal, call_request_verb};

/// How many call edges the walk follows from the definition a site reaches.
const MAX_EDGE_DEPTH: usize = 4;

/// The step a callback among a site's arguments decides at ([`Reach::One`]).
const CALLBACK_STEP: usize = 2;

/// The step the call a site's callee is read off decides at ([`Reach::One`]):
/// not the own callee's, so the row keeps no type the call hands back.
const RECEIVER_STEP: usize = 3;

/// A definition as every service names it: its repository-relative file and
/// the line it opens on. A definition key is one service's own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DefinitionSite {
    pub file: String,
    pub line: u32,
}

/// One model call row and the definitions its call expression goes through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteRow {
    /// Repository-relative file and line, as the row's `file_location` packs
    /// them.
    pub file: String,
    pub line: u32,
    /// The method the row is indexed under.
    pub method: String,
    /// The target the model stated.
    pub target: String,
    /// The definitions reached, in the order they are read. The first step
    /// that reaches a request decides:
    ///
    /// 1. the call's own callee;
    /// 2. the calls whose result is spread into an object-literal argument;
    /// 3. the calls in the body of the one function-valued argument.
    pub steps: Vec<Vec<DefinitionSite>>,
    /// The definitions the call this one is made on reaches, when the row's
    /// own callee is a member the compiler places outside the repository
    /// (`load(id).catch(…)`, `load(id).then(f).catch(g)`: `load(id)`), read
    /// in the own callee's place (carrick#2832). Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiver: Vec<DefinitionSite>,
    /// The calls of each function-valued argument, kept apart, when the call
    /// holds two or more (carrick#2832): step 3 then states nothing, and one
    /// of them decides only where every other is known to request nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callbacks: Vec<CallbackCalls>,
}

/// The calls written in one of a site's function-valued arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackCalls {
    /// The definitions its calls reach.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reached: Vec<DefinitionSite>,
    /// Every call written in it reaches a definition of the scan: nothing
    /// unknown can make a request from it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub every_call_resolved: bool,
}

/// One service's model call rows at sites that write no request of their own.
#[derive(Debug, Default, Clone)]
pub struct CallThroughSites {
    pub rows: Vec<SiteRow>,
    /// What each of the service's functions returns when that is one call's
    /// value read one way ([`BodyShape`], carrick#2377), by the definition.
    /// A definition not listed returns something else, or nothing.
    pub return_shapes: HashMap<DefinitionSite, BodyShape>,
    /// The service's resolved calls whose value never reaches the return of
    /// the function they are written in, or reaches it only as another
    /// call's argument ([`crate::return_flow`]), by file, line and the
    /// definition called. A call not listed hands its value back.
    pub side_calls: HashMap<SideKey, SideCall>,
    /// The service's calls into a function another of its files declares,
    /// whether or not a row sits at them ([`LeafCall`]). Read only in the run
    /// that analyses the service, so never stored: a carried service's blob
    /// already holds the rows they stated.
    pub leaves: Vec<LeafCall>,
}

impl CallThroughSites {
    /// Whether there is nothing to store. The leaf calls are never stored.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.return_shapes.is_empty() && self.side_calls.is_empty()
    }
}

/// What a function returns when that is the value of one call, read one way
/// ([`crate::return_flow::return_shapes`]), and the lines that call is written
/// on. A call edge counts as the one whose value is returned only when it is
/// written on those lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyShape {
    pub kind: ShapeKind,
    pub first_line: u32,
    pub last_line: u32,
}

/// A call in one file of a service whose callee, as the compiler resolves it
/// ([`RequestSummaryInputs::reach`]), is a function another file of the same
/// service declares: `client.save(id)` in a command module, `save` a method
/// of the service's own client class. Where that function itself calls into
/// another service and the walk from it reaches exactly one request there,
/// the call states that request at its own line ([`settle`]).
///
/// A call straight into a function another service declares (the boundary
/// call, [`LeafCallee::Sibling`]) states the request that function reaches, in
/// the service that writes it, unless it is written inside a function of its
/// own service that other calls of the service already reach the request
/// through: those callers state the row, not the call inside.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeafCall {
    /// Repository-relative file, and the lines the call opens and closes on.
    pub file: String,
    pub line: u32,
    pub last_line: u32,
    /// The lines the calls written around this one open on (`useQuery(` in
    /// `useQuery({ queryFn: () => client.getRun(id) })`): where a row of that
    /// outer call sits.
    pub enclosing_lines: Vec<u32>,
    /// The name the call is made through, as a row names its client.
    pub client: String,
    /// The function the call reaches.
    pub callee: LeafCallee,
}

/// The function a [`LeafCall`] reaches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeafCallee {
    /// A function another file of the same service declares, as the line it
    /// opens on.
    Own(DefinitionSite),
    /// A function another service declares, by repository-relative file and
    /// name (the last segment of a member's name). Only [`settle`], which
    /// holds every service's definitions, can say which one it is.
    Sibling { file: String, name: String },
}

/// A service's [`CallThroughSites`] as its blob stores them (carrick#2378):
/// every fact [`settle`] reads of a service, so a service `--changed-only`
/// carries is read as the scan that stored it read it. Sorted, so one scan
/// writes the same bytes every time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCallThrough {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<SiteRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub return_shapes: Vec<StoredReturnShape>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side_calls: Vec<StoredSideCall>,
}

/// One entry of [`CallThroughSites::return_shapes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredReturnShape {
    pub definition: DefinitionSite,
    pub shape: BodyShape,
}

/// One entry of [`CallThroughSites::side_calls`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSideCall {
    pub file: String,
    pub line: u32,
    pub definition: DefinitionSite,
    pub side: SideCall,
}

impl StoredCallThrough {
    /// `None` for a service with no facts, which writes no field.
    pub fn of(sites: &CallThroughSites) -> Option<Self> {
        if sites.is_empty() {
            return None;
        }
        let mut return_shapes: Vec<StoredReturnShape> = sites
            .return_shapes
            .iter()
            .map(|(definition, shape)| StoredReturnShape {
                definition: definition.clone(),
                shape: *shape,
            })
            .collect();
        return_shapes.sort_by(|a, b| a.definition.cmp(&b.definition));
        let mut side_calls: Vec<StoredSideCall> = sites
            .side_calls
            .iter()
            .map(|((file, line, definition), side)| StoredSideCall {
                file: file.clone(),
                line: *line,
                definition: definition.clone(),
                side: side.clone(),
            })
            .collect();
        side_calls.sort_by(|a, b| {
            (&a.file, a.line, &a.definition).cmp(&(&b.file, b.line, &b.definition))
        });
        Some(Self {
            rows: sites.rows.clone(),
            return_shapes,
            side_calls,
        })
    }

    pub fn sites(&self) -> CallThroughSites {
        CallThroughSites {
            leaves: Vec::new(),
            rows: self.rows.clone(),
            return_shapes: self
                .return_shapes
                .iter()
                .map(|entry| (entry.definition.clone(), entry.shape))
                .collect(),
            side_calls: self
                .side_calls
                .iter()
                .map(|entry| {
                    (
                        (entry.file.clone(), entry.line, entry.definition.clone()),
                        entry.side.clone(),
                    )
                })
                .collect(),
        }
    }
}

/// A resolved call: the repository-relative file and line it is written on,
/// and the definition it reaches.
pub type SideKey = (String, u32, DefinitionSite);

/// How a call that is not returned is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideCall {
    /// Its value is dropped.
    Discarded,
    /// Its value goes only into the arguments of these calls, each of which
    /// reaches the return. It is a side request when every one of them makes
    /// a request: the value is that request's input, not its answer.
    InputTo(Vec<Outer>),
}

/// A call whose arguments another call's value goes into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outer {
    /// A row of the service is written on its line: a request.
    Request,
    /// It reaches this definition, which may or may not make a request.
    Calls(DefinitionSite),
    /// Nothing says what it is.
    Unknown,
}

/// Read, for each model call row of one service, the definitions its call
/// expression goes through.
///
/// A row is read only when its call expression writes no request of its own:
/// no string or template inside it is shaped like a route or a URL, no call
/// inside it states a verb ([`call_request_verb`]), and no deterministic row
/// sits inside it. A row whose call cannot be placed is not read either.
///
/// `file_results` is the service's joined rows, keyed absolute or relative to
/// `repo_path`. `definitions` are the service's own: what each returns is read
/// off its file ([`BodyShape`]).
pub fn read_sites(
    file_results: &HashMap<String, FileAnalysisResult>,
    inputs: &RequestSummaryInputs,
    repo_path: &str,
    definitions: &HashMap<String, crate::visitor::FunctionDefinition>,
) -> CallThroughSites {
    let root = repo_path.trim_end_matches('/');
    let (side_calls, return_shapes) = read_function_facts(file_results, inputs, root, definitions);
    let lines = definition_lines(definitions, root);
    let leaves = read_leaves(inputs, root, &lines);
    let mut keys: Vec<&String> = file_results
        .iter()
        .filter(|(_, result)| result.data_calls.iter().any(is_model_row))
        .map(|(key, _)| key)
        .collect();
    keys.sort();
    if keys.is_empty() {
        return CallThroughSites {
            rows: Vec::new(),
            return_shapes,
            side_calls,
            leaves,
        };
    }

    // The discovery tables are keyed by the file as it was walked, which is
    // not always the spelling a row's key joins to.
    let mut walked: Option<HashMap<PathBuf, &PathBuf>> = None;
    let mut rows = Vec::new();
    for key in keys {
        let absolute = if Path::new(key).is_absolute() {
            PathBuf::from(key)
        } else {
            Path::new(root).join(key)
        };
        let discovered = match inputs.files.get_key_value(&absolute) {
            Some((path, _)) => Some(path),
            None => {
                let table = walked.get_or_insert_with(|| {
                    inputs
                        .files
                        .keys()
                        .filter_map(|path| Some((path.canonicalize().ok()?, path)))
                        .collect()
                });
                absolute
                    .canonicalize()
                    .ok()
                    .and_then(|canonical| table.get(&canonical).copied())
            }
        };
        let Some(discovered) = discovered else {
            continue;
        };
        let Some(ir) = inputs.files.get(discovered) else {
            continue;
        };
        // Span start and end, in the units a row's span carries -> the
        // definition the call reaches, as the compiler's `call` row names it
        // (carrick#2822): a member called on a call's value
        // (`createClient().getRun(id)`) included, which the call graph's
        // name join cannot follow. Where the rows do not say, the call
        // graph's answer stands, as before.
        let graph: HashMap<(u32, u32), DefinitionSite> = inputs
            .sites
            .sites_in(discovered)
            .map(|(lo, hi, file, line)| {
                (
                    (ir.swc_start(lo), ir.swc_start(hi)),
                    DefinitionSite {
                        file: crate::engine::repo_relative(&file.to_string_lossy(), root),
                        line,
                    },
                )
            })
            .collect();
        let mut resolved: HashMap<(u32, u32), DefinitionSite> = HashMap::new();
        // The calls whose callee is no function of the repository: the
        // default library or a package (`.catch` on a promise).
        let mut outside: HashSet<(u32, u32)> = HashSet::new();
        for (span, reach) in inputs.reaches_in(discovered) {
            if reach == CallReach::NoFunction {
                outside.insert(span);
            }
            let definition = match reach {
                CallReach::Function(file, key) | CallReach::OutsideService(file, key) => {
                    let file = crate::engine::repo_relative(&file.to_string_lossy(), root);
                    let name = key.split('@').next().unwrap_or(&key);
                    match lines.get(&(file.clone(), name)).map(Vec::as_slice) {
                        Some([line]) => Some(DefinitionSite { file, line: *line }),
                        // A definition this service's table does not name
                        // once: the call graph's line, when it names the
                        // same file.
                        _ => graph.get(&span).filter(|site| site.file == file).cloned(),
                    }
                }
                CallReach::NoFunction => None,
                CallReach::Unknown(_) => graph.get(&span).cloned(),
            };
            if let Some(definition) = definition {
                resolved.insert(span, definition);
            }
        }
        let relative = crate::engine::repo_relative(key, root);
        rows.extend(read_file_sites(
            &absolute,
            &relative,
            &file_results[key],
            &Resolution {
                resolved: &resolved,
                outside: &outside,
            },
        ));
    }
    CallThroughSites {
        rows,
        return_shapes,
        side_calls,
        leaves,
    }
}

/// The lines each definition of the service opens on, by repository-relative
/// file and definition name. A file and name more than one definition share
/// lists each line.
type DefinitionLines<'d> = HashMap<(String, &'d str), Vec<u32>>;

fn definition_lines<'d>(
    definitions: &'d HashMap<String, crate::visitor::FunctionDefinition>,
    root: &str,
) -> DefinitionLines<'d> {
    let mut lines: DefinitionLines<'d> = HashMap::new();
    for definition in definitions
        .values()
        .filter(|definition| definition.name != crate::visitor::MODULE_SCOPE_KEY)
    {
        let file = crate::engine::repo_relative(&definition.file_path.to_string_lossy(), root);
        lines
            .entry((file, definition.name.as_str()))
            .or_default()
            .push(definition.line_number);
    }
    for at in lines.values_mut() {
        at.sort_unstable();
        at.dedup();
    }
    lines
}

/// Every call of the service into a function another of its files declares
/// ([`LeafCall`]), with the line that function opens on as every service
/// names a definition, and every call into a function another service
/// declares, by file and name. A callee of the service whose file and name
/// more than one definition share names no one definition, and its calls are
/// left out.
fn read_leaves(
    inputs: &RequestSummaryInputs,
    root: &str,
    lines: &DefinitionLines<'_>,
) -> Vec<LeafCall> {
    let mut by_file: BTreeMap<PathBuf, Vec<crate::request_summary::CallIntoFile>> = BTreeMap::new();
    for call in inputs.calls_into_other_files() {
        by_file.entry(call.file.clone()).or_default().push(call);
    }
    let mut leaves = Vec::new();
    for (path, calls) in by_file {
        // Where each call closes, read off the file as a row's span is.
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let extents: HashMap<(u32, u32), CallExtent> = match parse_file(&path, &cm, &handler) {
            Some(module) => {
                let positions = Positions { cm: &cm };
                let mut every = EveryCall {
                    positions: &positions,
                    calls: Vec::new(),
                    callees: HashMap::new(),
                };
                module.visit_with(&mut every);
                every
                    .calls
                    .into_iter()
                    .map(|extent| (extent.span(), extent))
                    .collect()
            }
            None => HashMap::new(),
        };
        let file = crate::engine::repo_relative(&path.to_string_lossy(), root);
        for call in calls {
            let name = call.key.split('@').next().unwrap_or(&call.key);
            let target = crate::engine::repo_relative(&call.target.to_string_lossy(), root);
            let callee = if call.boundary {
                LeafCallee::Sibling {
                    file: target,
                    name: name.to_string(),
                }
            } else {
                let Some([line]) = lines.get(&(target.clone(), name)).map(Vec::as_slice) else {
                    continue;
                };
                LeafCallee::Own(DefinitionSite {
                    file: target,
                    line: *line,
                })
            };
            let own = extents.get(&(call.span_start, call.span_end));
            let last_line = own.map_or(call.line, |extent| extent.last_line);
            let mut enclosing_lines: Vec<u32> = own
                .map(|own| {
                    extents
                        .values()
                        .filter(|outer| outer.contains(own) && outer.line != own.line)
                        .map(|outer| outer.line)
                        .collect()
                })
                .unwrap_or_default();
            enclosing_lines.sort_unstable();
            enclosing_lines.dedup();
            leaves.push(LeafCall {
                file: file.clone(),
                line: call.line,
                last_line,
                enclosing_lines,
                client: name.rsplit('.').next().unwrap_or(name).to_string(),
                callee,
            });
        }
    }
    leaves.sort();
    leaves.dedup();
    leaves
}

/// Every resolved call of the service whose value is not handed back by the
/// function it is written in ([`SideCall`]), and what each of the service's
/// definitions returns ([`BodyShape`]). Each file is parsed once; a file with
/// no resolved call (a generated client) is read for its definitions alone.
fn read_function_facts(
    file_results: &HashMap<String, FileAnalysisResult>,
    inputs: &RequestSummaryInputs,
    root: &str,
    definitions: &HashMap<String, crate::visitor::FunctionDefinition>,
) -> (
    HashMap<SideKey, SideCall>,
    HashMap<DefinitionSite, BodyShape>,
) {
    let defined: HashSet<DefinitionSite> = definitions
        .values()
        .filter(|definition| definition.name != crate::visitor::MODULE_SCOPE_KEY)
        .map(|definition| DefinitionSite {
            file: crate::engine::repo_relative(&definition.file_path.to_string_lossy(), root),
            line: definition.line_number,
        })
        .collect();
    let defining: HashSet<&str> = defined.iter().map(|site| site.file.as_str()).collect();
    let mut return_shapes: HashMap<DefinitionSite, BodyShape> = HashMap::new();
    // Lines a row of the service is written on, per file.
    let mut row_lines: HashMap<String, HashSet<u32>> = HashMap::new();
    for (key, result) in file_results {
        let lines = row_lines
            .entry(crate::engine::repo_relative(key, root))
            .or_default();
        lines.extend(
            result
                .data_calls
                .iter()
                .filter_map(|call| u32::try_from(call.line_number).ok()),
        );
    }
    let mut side_calls = HashMap::new();
    let mut paths: Vec<&PathBuf> = inputs.files.keys().collect();
    paths.sort();
    for path in paths {
        let ir = &inputs.files[path];
        let resolved: HashMap<(u32, u32), DefinitionSite> = inputs
            .sites
            .sites_in(path)
            .map(|(lo, hi, file, line)| {
                (
                    (ir.swc_start(lo), ir.swc_start(hi)),
                    DefinitionSite {
                        file: crate::engine::repo_relative(&file.to_string_lossy(), root),
                        line,
                    },
                )
            })
            .collect();
        let relative = crate::engine::repo_relative(&path.to_string_lossy(), root);
        if resolved.is_empty() && !defining.contains(relative.as_str()) {
            continue;
        }
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let Some(module) = parse_file(path, &cm, &handler) else {
            continue;
        };
        let positions = Positions { cm: &cm };
        // The function node that opens a definition's line is the definition
        // (every kind takes its line from its node); where two open on one
        // line, the outer one.
        let mut opening: HashMap<u32, (CallExtent, Option<crate::return_flow::ReturnShape>)> =
            HashMap::new();
        for (node, shape) in crate::return_flow::return_shapes(&module) {
            let extent = positions.extent(node);
            if opening
                .get(&extent.line)
                .is_none_or(|(known, _)| extent.contains(known))
            {
                opening.insert(extent.line, (extent, shape));
            }
        }
        for (line, (_, shape)) in opening {
            let Some(shape) = shape else {
                continue;
            };
            if !defined.contains(&DefinitionSite {
                file: relative.clone(),
                line,
            }) {
                continue;
            }
            let base = positions.extent(shape.base);
            return_shapes.insert(
                DefinitionSite {
                    file: relative.clone(),
                    line,
                },
                BodyShape {
                    kind: shape.kind,
                    first_line: base.line,
                    last_line: base.last_line,
                },
            );
        }
        if resolved.is_empty() {
            continue;
        }
        let rows_here = row_lines.get(&relative);
        let flows = crate::return_flow::call_flows(&module);
        for (span, flow) in &flows {
            let extent = positions.extent(*span);
            let Some(target) = resolved.get(&(extent.lo, extent.hi)) else {
                continue;
            };
            let side = match flow {
                crate::return_flow::Flow::Returned => continue,
                crate::return_flow::Flow::Discarded => SideCall::Discarded,
                crate::return_flow::Flow::Through(outers) => SideCall::InputTo(
                    outers
                        .iter()
                        .map(|outer| {
                            let outer = positions.extent(*outer);
                            if rows_here.is_some_and(|lines| lines.contains(&outer.line)) {
                                Outer::Request
                            } else if let Some(definition) = resolved.get(&(outer.lo, outer.hi)) {
                                Outer::Calls(definition.clone())
                            } else {
                                Outer::Unknown
                            }
                        })
                        .collect(),
                ),
            };
            side_calls.insert((relative.clone(), extent.line, target.clone()), side);
        }
    }
    (side_calls, return_shapes)
}

/// A row this pass may settle: the model's own reading, with nothing linking
/// it to a request already.
fn is_model_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model) && call.reaches_request.is_none()
}

/// What the compiler's rows say a file's calls reach, by the call's span in
/// the units a row's span carries.
struct Resolution<'a> {
    /// The definition each call reaches.
    resolved: &'a HashMap<(u32, u32), DefinitionSite>,
    /// The calls whose callee is no function of the repository.
    outside: &'a HashSet<(u32, u32)>,
}

impl Resolution<'_> {
    /// The definitions `spans` reach, sorted and deduplicated.
    fn reached(&self, spans: &[(u32, u32)]) -> Vec<DefinitionSite> {
        let mut definitions: Vec<DefinitionSite> = spans
            .iter()
            .filter_map(|span| self.resolved.get(span).cloned())
            .collect();
        definitions.sort();
        definitions.dedup();
        definitions
    }

    /// What the call a site's call is made on reaches (carrick#2832), read
    /// down `chain` (the calls each call is made on, outermost first) while
    /// each call's callee is a member the compiler places outside the
    /// repository: `load(id).then(f).catch(g)` reads `load(id)`. Empty where
    /// the site's own callee resolves, a callee on the way is unknown, or the
    /// chain ends on a call that reaches nothing.
    fn receiver(&self, call: (u32, u32), chain: &[(u32, u32)]) -> Vec<DefinitionSite> {
        let mut outer = call;
        for &receiver in chain {
            if self.resolved.contains_key(&outer) || !self.outside.contains(&outer) {
                return Vec::new();
            }
            if let Some(definition) = self.resolved.get(&receiver) {
                return vec![definition.clone()];
            }
            outer = receiver;
        }
        Vec::new()
    }
}

fn read_file_sites(
    absolute: &Path,
    relative: &str,
    result: &FileAnalysisResult,
    resolution: &Resolution<'_>,
) -> Vec<SiteRow> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let Some(module) = parse_file(absolute, &cm, &handler) else {
        return Vec::new();
    };
    let positions = Positions { cm: &cm };

    let mut every = EveryCall {
        positions: &positions,
        calls: Vec::new(),
        callees: HashMap::new(),
    };
    module.visit_with(&mut every);

    // The call each row was recorded at.
    let placed: Vec<(&DataCallResult, CallExtent)> = result
        .data_calls
        .iter()
        .filter(|call| is_model_row(call))
        .filter_map(|call| Some((call, place(&every.calls, call)?)))
        .collect();
    if placed.is_empty() {
        return Vec::new();
    }

    let mut reader = SiteReader {
        positions: &positions,
        wanted: placed.iter().map(|(_, extent)| extent.span()).collect(),
        read: HashMap::new(),
    };
    module.visit_with(&mut reader);

    let mut rows = Vec::new();
    for (call, extent) in placed {
        let Some(site) = reader.read.get(&extent.span()) else {
            continue;
        };
        if site.writes_a_request || holds_a_deterministic_row(result, &extent) {
            continue;
        }
        let Some(method) = FileOrchestrator::normalize_consumer_method(call.method.as_deref())
        else {
            continue;
        };
        let Ok(line) = u32::try_from(call.line_number) else {
            continue;
        };
        let callbacks = match site.callbacks.as_slice() {
            [_, _, ..] => site
                .callbacks
                .iter()
                .map(|calls| CallbackCalls {
                    reached: resolution.reached(calls),
                    every_call_resolved: calls
                        .iter()
                        .all(|span| resolution.resolved.contains_key(span)),
                })
                .collect(),
            _ => Vec::new(),
        };
        let callback_calls = match site.callbacks.as_slice() {
            [one] => resolution.reached(one),
            _ => Vec::new(),
        };
        rows.push(SiteRow {
            file: relative.to_string(),
            line,
            method,
            target: call.target.clone(),
            steps: vec![
                resolution.reached(&[extent.span()]),
                resolution.reached(&site.spread_calls),
                callback_calls,
            ],
            receiver: resolution.receiver(extent.span(), &site.receivers),
            callbacks,
        });
    }
    rows
}

/// Whether a deterministic row of the file sits inside the call expression:
/// the site then holds a request the source states, and the model's row beside
/// it is not this pass's to read.
fn holds_a_deterministic_row(result: &FileAnalysisResult, extent: &CallExtent) -> bool {
    result
        .data_calls
        .iter()
        .filter(|call| {
            call.resolution_source
                .is_some_and(|source| source != ResolutionSource::Model)
        })
        .any(|call| match call.call_expression_span_start {
            Some(start) => extent.lo <= start && start < extent.hi,
            None => u32::try_from(call.line_number)
                .is_ok_and(|line| extent.line <= line && line <= extent.last_line),
        })
}

/// Where one call expression sits: its span in the units a row's span
/// carries, and the lines it opens and closes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallExtent {
    lo: u32,
    hi: u32,
    line: u32,
    last_line: u32,
}

impl CallExtent {
    fn span(&self) -> (u32, u32) {
        (self.lo, self.hi)
    }

    fn contains(&self, other: &CallExtent) -> bool {
        self.lo <= other.lo && other.hi <= self.hi && self.span() != other.span()
    }
}

struct Positions<'a> {
    cm: &'a Lrc<SourceMap>,
}

impl Positions<'_> {
    fn offset(&self, pos: BytePos) -> u32 {
        self.cm.lookup_byte_offset(pos).pos.0 + SWC_SPAN_BASE
    }

    fn extent(&self, span: Span) -> CallExtent {
        CallExtent {
            lo: self.offset(span.lo),
            hi: self.offset(span.hi),
            line: self.cm.lookup_char_pos(span.lo).line as u32,
            last_line: self.cm.lookup_char_pos(span.hi).line as u32,
        }
    }
}

/// The call a row was recorded at: the one at its span, or, for a row with no
/// span, the one call that opens on its line and sits inside no other call
/// opening there. A line that opens two such calls states nothing about which
/// the row is, and answers `None`.
fn place(calls: &[CallExtent], row: &DataCallResult) -> Option<CallExtent> {
    if let Some(start) = row.call_expression_span_start {
        let mut at_start = calls.iter().filter(|call| call.lo == start);
        return match row.call_expression_span_end {
            Some(end) => at_start.find(|call| call.hi == end).copied(),
            None => {
                let first = at_start.next()?;
                at_start.next().is_none().then_some(*first)
            }
        };
    }
    let line = u32::try_from(row.line_number).ok()?;
    let on_line: Vec<&CallExtent> = calls.iter().filter(|call| call.line == line).collect();
    let mut outermost = on_line
        .iter()
        .filter(|call| !on_line.iter().any(|other| other.contains(call)));
    let first = outermost.next()?;
    outermost.next().is_none().then_some(**first)
}

/// Every call expression of a file, with where each one's callee is written.
struct EveryCall<'a> {
    positions: &'a Positions<'a>,
    calls: Vec<CallExtent>,
    /// The callee's span, by its call's span, for a callee that is an
    /// expression (not `super` or `import`).
    callees: HashMap<(u32, u32), (u32, u32)>,
}

impl EveryCall<'_> {
    fn record(&mut self, call: Span, callee: Option<Span>) {
        let extent = self.positions.extent(call);
        if let Some(callee) = callee {
            let callee = self.positions.extent(callee);
            self.callees.insert(extent.span(), callee.span());
        }
        self.calls.push(extent);
    }
}

impl Visit for EveryCall<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        let callee = match &node.callee {
            Callee::Expr(callee) => Some(callee.span()),
            Callee::Super(_) | Callee::Import(_) => None,
        };
        self.record(node.span, callee);
        node.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, node: &OptCall) {
        self.record(node.span, Some(node.callee.span()));
        node.visit_children_with(self);
    }
}

/// The calls of one file as [`place`] reads them, with the text each call's
/// callee is written as (carrick#2281): what places a model row with no span
/// at the call it was recorded at.
pub(crate) struct FileCalls {
    calls: Vec<CallExtent>,
    /// The callee's text with every whitespace character removed, by its
    /// call's span.
    callees: HashMap<(u32, u32), String>,
}

impl FileCalls {
    /// Every call of `content`, parsed as the file at `path`. `None` when it
    /// does not parse.
    pub(crate) fn parse(path: &Path, content: &str) -> Option<Self> {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let module = parse_source(
            path,
            FileName::Real(path.to_path_buf()),
            content.to_string(),
            &cm,
            &handler,
        )?;
        let positions = Positions { cm: &cm };
        let mut every = EveryCall {
            positions: &positions,
            calls: Vec::new(),
            callees: HashMap::new(),
        };
        module.visit_with(&mut every);
        let local = |at: u32| usize::try_from(at.checked_sub(SWC_SPAN_BASE)?).ok();
        let callees = every
            .callees
            .into_iter()
            .filter_map(|(call, (lo, hi))| {
                let text = content.get(local(lo)?..local(hi)?)?;
                Some((call, squeeze(text)))
            })
            .collect();
        Some(Self {
            calls: every.calls,
            callees,
        })
    }

    /// The span start of the call a model row with no span was recorded at
    /// ([`place`]), when the row's own call text writes that call: it holds
    /// the call's callee followed by `(`, whitespace aside. A row with no
    /// text, one whose text writes another call (an argument of the call,
    /// say), or one at a chain of calls, names none.
    pub(crate) fn written_at(&self, row: &DataCallResult) -> Option<u32> {
        if row.call_expression_span_start.is_some() {
            return None;
        }
        let call = place(&self.calls, row)?;
        let callee = self.callees.get(&call.span())?;
        let text = squeeze(row.call_expression_text.as_deref()?);
        // A chain (`register(…).then(…)`) opens where its first call does, and
        // a silent site is known by its start alone: the outer call would read
        // as the inner one.
        let chained = self
            .calls
            .iter()
            .any(|other| other.lo == call.lo && other.span() != call.span());
        (!chained && text.contains(&format!("{callee}("))).then_some(call.lo)
    }

    /// The span start of the call a model row with no span names by its id
    /// (carrick#2726): an id `span:<lo>-<hi>` minted from a candidate's span
    /// ([`crate::swc_scanner`]'s `candidate_id`, the units [`CallExtent`]
    /// counts in), when this file holds a call at exactly that span and the
    /// row's line is one the call covers.
    ///
    /// A cached answer keeps the ids of the candidates it was asked about. A
    /// file read again with none of those candidates offered joins none of
    /// them, so the row reaches the withdrawal with no span; its id still
    /// names the call it was recorded at, and the silent-site rule reads it
    /// as it reads a joined span: by its start, chain or not. An id naming no
    /// call of the file at exactly that span names none.
    pub(crate) fn named_by_id(&self, row: &DataCallResult) -> Option<u32> {
        if row.call_expression_span_start.is_some() {
            return None;
        }
        let span = span_named_by_id(&row.candidate_id)?;
        let line = u32::try_from(row.line_number).ok()?;
        self.calls
            .iter()
            .find(|call| call.span() == span && (call.line..=call.last_line).contains(&line))
            .map(|call| call.lo)
    }
}

/// The SWC span a `span:<lo>-<hi>` candidate id names, or `None` for any
/// other spelling.
pub(crate) fn span_named_by_id(candidate_id: &str) -> Option<(u32, u32)> {
    let (lo, hi) = candidate_id.strip_prefix("span:")?.split_once('-')?;
    Some((lo.parse().ok()?, hi.parse().ok()?))
}

/// `text` with every whitespace character removed.
fn squeeze(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What one site's call expression holds.
#[derive(Debug, Default)]
struct SiteReading {
    /// The expression writes a request of its own.
    writes_a_request: bool,
    /// The calls whose result is spread into an object-literal argument:
    /// `useQuery({ ...runOptions(input) })`.
    spread_calls: Vec<(u32, u32)>,
    /// The calls in the body of each function-valued argument, passed
    /// directly or as a property of an object-literal argument, one list per
    /// function.
    callbacks: Vec<Vec<(u32, u32)>>,
    /// The call the expression is made on, when its callee is a member of a
    /// call's value (through `await`, parentheses and type assertions), then
    /// the call that one is made on, and so on: outermost first.
    receivers: Vec<(u32, u32)>,
}

struct SiteReader<'a> {
    positions: &'a Positions<'a>,
    wanted: HashSet<(u32, u32)>,
    read: HashMap<(u32, u32), SiteReading>,
}

impl SiteReader<'_> {
    fn read_site<N: VisitWith<RequestWriter>>(
        &mut self,
        span: Span,
        node: &N,
        callee: Option<&Expr>,
        args: &[ExprOrSpread],
    ) {
        let key = self.positions.extent(span).span();
        if !self.wanted.contains(&key) {
            return;
        }
        let mut writer = RequestWriter::default();
        node.visit_with(&mut writer);

        let mut spread_calls = Vec::new();
        // The calls in the body of each function-valued argument.
        let mut callbacks: Vec<Vec<(u32, u32)>> = Vec::new();
        for arg in args.iter().filter(|arg| arg.spread.is_none()) {
            match unwrap_expression(&arg.expr) {
                Expr::Object(object) => {
                    for prop in &object.props {
                        match prop {
                            PropOrSpread::Spread(spread) => {
                                if let Some(span) = call_span(&spread.expr) {
                                    spread_calls.push(self.positions.extent(span).span());
                                }
                            }
                            PropOrSpread::Prop(prop) => match &**prop {
                                Prop::KeyValue(pair) => {
                                    callbacks.extend(self.calls_in_function(&pair.value));
                                }
                                // `queryFn() { … }`
                                Prop::Method(method) => {
                                    callbacks.push(self.calls_in(&method.function));
                                }
                                _ => {}
                            },
                        }
                    }
                }
                value => callbacks.extend(self.calls_in_function(value)),
            }
        }
        let mut receivers = Vec::new();
        let mut callee = callee;
        while let Some(receiver) = callee.and_then(member_object_call) {
            receivers.push(self.positions.extent(receiver.span).span());
            callee = match &receiver.callee {
                Callee::Expr(expr) => Some(&**expr),
                Callee::Super(_) | Callee::Import(_) => None,
            };
        }
        self.read.insert(
            key,
            SiteReading {
                writes_a_request: writer.found,
                spread_calls,
                callbacks,
                receivers,
            },
        );
    }

    /// The calls in the body of `expr`, when it is a function.
    fn calls_in_function(&self, expr: &Expr) -> Option<Vec<(u32, u32)>> {
        match unwrap_expression(expr) {
            Expr::Arrow(arrow) => Some(self.calls_in(arrow)),
            Expr::Fn(function) => Some(self.calls_in(&function.function)),
            _ => None,
        }
    }

    fn calls_in<'n, N>(&'n self, node: &N) -> Vec<(u32, u32)>
    where
        N: VisitWith<EveryCall<'n>>,
    {
        let mut inner = EveryCall {
            positions: self.positions,
            calls: Vec::new(),
            callees: HashMap::new(),
        };
        node.visit_with(&mut inner);
        inner.calls.iter().map(CallExtent::span).collect()
    }
}

impl Visit for SiteReader<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        let callee = match &node.callee {
            Callee::Expr(callee) => Some(&**callee),
            Callee::Super(_) | Callee::Import(_) => None,
        };
        self.read_site(node.span, node, callee, &node.args);
        node.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, node: &OptCall) {
        self.read_site(node.span, node, Some(&node.callee), &node.args);
        node.visit_children_with(self);
    }
}

/// The span of the call an expression is, through `await` and the wrappers
/// that pass a value along.
fn call_span(expr: &Expr) -> Option<Span> {
    call_of(expr).map(|call| call.span)
}

/// The call an expression is, through `await` and the wrappers that pass a
/// value along.
fn call_of(expr: &Expr) -> Option<&CallExpr> {
    match unwrap_expression(expr) {
        Expr::Call(call) => Some(call),
        Expr::Await(awaited) => call_of(&awaited.arg),
        _ => None,
    }
}

/// The call whose value a member callee is read off: `load(id)` in
/// `load(id).catch(…)` or `(await load(id)).run()`.
fn member_object_call(callee: &Expr) -> Option<&CallExpr> {
    match unwrap_expression(callee) {
        Expr::Member(member) => call_of(&member.obj),
        _ => None,
    }
}

/// Whether an expression writes a request of its own: a string or a template
/// shaped like a route or a URL, or a call that states a verb.
#[derive(Default)]
struct RequestWriter {
    found: bool,
}

impl Visit for RequestWriter {
    fn visit_str(&mut self, node: &Str) {
        self.found |= writes_a_route(&node.value.to_string_lossy());
    }

    fn visit_tpl(&mut self, node: &Tpl) {
        let text = node
            .quasis
            .iter()
            .map(|quasi| quasi.raw.to_string())
            .collect::<Vec<_>>()
            .join(INTERPOLATION);
        self.found |= writes_a_route(&text);
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee {
            let property = match unwrap_expression(callee) {
                Expr::Member(member) => member.prop.as_ident().map(|ident| ident.sym.to_string()),
                _ => None,
            };
            if matches!(
                call_request_verb(node, property.as_deref()),
                RequestShapeSignal::Known(_)
            ) {
                self.found = true;
            }
        }
        node.visit_children_with(self);
    }
}

/// What an interpolation reads as in a template's text here.
const INTERPOLATION: &str = "${}";

/// A path with something behind the slash, a URL, or an interpolated base
/// with a path behind it. A bare `/` is a separator, not a route.
fn writes_a_route(text: &str) -> bool {
    let text = text.trim();
    let path = |text: &str| text.starts_with('/') && text.len() > 1 && !text.starts_with("//");
    path(text)
        || text.starts_with("http://")
        || text.starts_with("https://")
        || text.strip_prefix(INTERPOLATION).is_some_and(path)
}

/// One service's blob and site facts, as [`settle`] reads and changes them.
pub struct ServiceRows<'a> {
    pub data: &'a mut CloudRepoData,
    pub sites: &'a CallThroughSites,
    /// Analysed in this run, and so written. A service whose stored
    /// generation stands in for it is read for its requests and definitions
    /// and never changed: that generation is what the index serves.
    pub writable: bool,
}

/// What [`settle`] decided, so a scan can state it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    /// Rows whose operation the reached request confirms: kept as the model
    /// stated them, and marked as the call through.
    pub relabelled: usize,
    /// Rows that took the reached request's operation.
    pub corrected: usize,
    /// Rows left alone because their call reaches no request row.
    pub reaches_none: usize,
    /// Rows left alone because their call reaches more than one operation.
    pub reaches_several: usize,
    /// Rows left alone because their call also reaches a request no fact
    /// states (the model's row at a `fetch`, a row of unknown source): what
    /// that request is, nothing here can say, so one operation is unproven.
    pub reaches_unstated: usize,
    /// Corrected rows whose consumer type entries were dropped, because the
    /// row's call is not the call that reaches the request: what it returns
    /// is another function's result.
    pub types_not_carried: usize,
    /// Corrected rows whose chain hands back the request's body, and whose
    /// captured response type is a bare top type (`unknown`, `any`): the
    /// entry is dropped, since an entry left at `unknown` makes its pair
    /// unverifiable.
    pub types_unknown: usize,
    /// Corrected rows that kept their response entry and lost their request
    /// entry: the call's arguments are the called function's parameters.
    pub requests_not_carried: usize,
    /// Rows stated at a leaf call no row sat at ([`LeafCall`]), at a call into
    /// a function of the service's own and at a boundary call alike.
    pub leaf_rows: usize,
    /// Of those, the rows stated at a call straight into a function another
    /// service declares ([`LeafCallee::Sibling`]).
    pub boundary_rows: usize,
    /// Boundary calls left alone because the function they are written in is
    /// reached by calls of the service that state the row themselves.
    pub boundary_in_wrapper: usize,
    /// Boundary calls left alone because the function called reaches a
    /// request that a third service writes, not the one it belongs to.
    pub boundary_third_service: usize,
    /// Boundary calls left alone because no one definition of any service
    /// is the function called.
    pub boundary_unresolved: usize,
    /// Rows withdrawn at a client wrapper's call once a leaf row states the
    /// request at its caller ([`withdraw_wrapped`]).
    pub wrapper_rows_withdrawn: usize,
    /// Rows skipped because a fact site row replaced the model's row at the
    /// call, and the graph no longer holds it (carrick#2795).
    pub replaced_by_site_row: usize,
}

/// Settle every model call row whose call reaches exactly one request
/// operation, over every service of the scan.
///
/// | Reached | The row |
/// |---|---|
/// | One operation, the row's own | stays the model's and keeps its type anchors; gains `role: wrapper_call` and `reaches_request` |
/// | One operation, another | takes the reached row's method, target, base and dispatch value; its source becomes `request_summary`; its response type entry is keyed to the reached operation where the call hands back the request's parsed body, and goes otherwise; its request type entry goes |
/// | None, several, or one beside a request no fact states | left as it is, and counted |
///
/// A corrected row keeps the RESPONSE type anchored at it only where the
/// row's call is itself the call that reaches the request (its own callee
/// decided) and the chain of functions from that callee to the request hands
/// back the request's body, read exactly one step off the transport's value
/// ([`ReachGraph::hands_back_body`], carrick#2377): only then is what the
/// call returns what the caller reads of the request, so the pair is judged
/// against the reached operation's producer. Its request entry always goes:
/// the call's arguments are the called function's parameters, not a request
/// body. Anywhere else (a spread call or a callback decided, or the chain
/// builds a value around the body, reads it twice, or passes it through
/// another function) the anchor names a function's own result, and the
/// entries are dropped and counted.
pub fn settle(services: &mut [ServiceRows<'_>]) -> Settled {
    let mut graph = ReachGraph::of(services.iter().map(|service| &*service.data));
    let sites: Vec<&CallThroughSites> = services.iter().map(|service| service.sites).collect();
    graph.side_calls = sites
        .iter()
        .flat_map(|sites| sites.side_calls.iter())
        .map(|(key, side)| (key.clone(), side.clone()))
        .collect();
    graph.shapes = sites
        .iter()
        .flat_map(|sites| sites.return_shapes.iter())
        .map(|(definition, shape)| (definition.clone(), *shape))
        .collect();
    let owners = file_owners(services.iter().map(|service| &*service.data));
    let siblings = SiblingLines::of(services.iter().map(|service| &*service.data));
    let mut decider = Decider::new(&graph, &sites);
    let mut settled = Settled::default();
    for (index, service) in services
        .iter_mut()
        .enumerate()
        .filter(|(_, service)| service.writable)
    {
        let mut corrected = 0;
        for row in &service.sites.rows {
            let left = |why: &str| {
                debug!(
                    "  - {}:{} {} {}: left as the model stated it, {why}",
                    row.file, row.line, row.method, row.target
                );
            };
            if replaced_by_site_row(service.data, row) {
                debug!(
                    "  - {}:{} {} {}: the model's row was replaced by a fact site row",
                    row.file, row.line, row.method, row.target
                );
                settled.replaced_by_site_row += 1;
                continue;
            }
            match decider.decide(row) {
                Reach::None => {
                    left("reaches no request row");
                    settled.reaches_none += 1;
                }
                Reach::Several => {
                    left("reaches several operations");
                    settled.reaches_several += 1;
                }
                Reach::Unstated => {
                    left("reaches a request only the model states");
                    settled.reaches_unstated += 1;
                }
                Reach::One { request, step } => match apply(
                    service.data,
                    row,
                    request,
                    // What the row's call returns is the request's body only
                    // where its own callee decided and every definition that
                    // callee goes through hands back that body, read one way
                    // (carrick#2377).
                    step == 0
                        && row.steps.first().is_some_and(|callee| {
                            let operation = operation_of(request);
                            !callee.is_empty()
                                && callee
                                    .iter()
                                    .all(|definition| graph.hands_back_body(definition, &operation))
                        }),
                ) {
                    Some(Change::Relabelled) => settled.relabelled += 1,
                    Some(Change::Corrected {
                        types_dropped,
                        types_unknown,
                        request_dropped,
                    }) => {
                        corrected += 1;
                        settled.types_not_carried += usize::from(types_dropped);
                        settled.types_unknown += usize::from(types_unknown);
                        settled.requests_not_carried += usize::from(request_dropped);
                    }
                    None => {}
                },
            }
        }
        settled.corrected += corrected;
        let stated = state_leaves(service, index, &owners, &siblings, &graph, &mut decider);
        settled.leaf_rows += stated.own + stated.boundary;
        settled.boundary_rows += stated.boundary;
        settled.boundary_in_wrapper += stated.in_wrapper;
        settled.boundary_third_service += stated.third_service;
        settled.boundary_unresolved += stated.unresolved;
        settled.wrapper_rows_withdrawn += stated.withdrawn;
        if (corrected > 0 || stated.own + stated.boundary > 0)
            && let Some(mut boundary) = service.data.boundary.take()
        {
            boundary.fold_settled_calls(service.data, corrected);
            service.data.boundary = Some(boundary);
        }
    }
    debug!(
        "Call-through rows settled: {} confirmed, {} corrected ({} lost the types anchored at a call that is not the request's, {} lost an unknown type the chain hands back, {} kept the response type and lost the request type); left alone: {} reach no request row, {} reach several operations, {} reach a request only the model states (carrick#1146); {} skipped because a fact site row replaced the model's row (carrick#2795)",
        settled.relabelled,
        settled.corrected,
        settled.types_not_carried,
        settled.types_unknown,
        settled.requests_not_carried,
        settled.reaches_none,
        settled.reaches_several,
        settled.reaches_unstated,
        settled.replaced_by_site_row
    );
    debug!(
        "Call-through leaf rows stated: {} at calls into a function of the caller's own service that calls into another service and reaches exactly one request there (carrick#1146), and {} at calls straight into a function another service declares (carrick#2773); boundary calls left alone: {} inside a function whose callers state the row, {} whose function reaches a request a third service writes, {} whose function no one definition names",
        settled.leaf_rows - settled.boundary_rows,
        settled.boundary_rows,
        settled.boundary_in_wrapper,
        settled.boundary_third_service,
        settled.boundary_unresolved
    );
    debug!(
        "Call-through wrapper rows withdrawn: {} at a client wrapper's call into another service, whose caller's leaf row now states the request (carrick#2618)",
        settled.wrapper_rows_withdrawn
    );
    settled
}

/// Whether the model's row `row` names is gone from the graph because a fact
/// site row now stands at its call (carrick#2795): the site fold replaced it,
/// so there is no row to settle. A row absent for any other reason is not
/// this, and [`apply`] leaves it as before.
fn replaced_by_site_row(data: &CloudRepoData, row: &SiteRow) -> bool {
    let Some(graph) = data.mount_graph.as_ref() else {
        return false;
    };
    let location = format!("{}:{}", row.file, row.line);
    let model_row = graph.data_calls.iter().any(|call| {
        call.file_location == location
            && call.method == row.method
            && call.target_url == row.target
            && call.resolution_source == Some(ResolutionSource::Model)
            && call.reaches_request.is_none()
    });
    !model_row
        && graph.data_calls.iter().any(|call| {
            call.file_location == location
                && call.reaches_request.is_none()
                && call
                    .resolution_source
                    .is_some_and(|source| source != ResolutionSource::Model)
        })
}

/// Each file a service's definitions or rows name, with the index of the
/// first service that names it.
fn file_owners<'d>(services: impl Iterator<Item = &'d CloudRepoData>) -> HashMap<String, usize> {
    let mut owners: HashMap<String, usize> = HashMap::new();
    for (index, data) in services.enumerate() {
        let definitions = data
            .function_definitions
            .values()
            .map(|definition| definition.file_path.to_string_lossy().into_owned());
        let rows = data
            .mount_graph
            .iter()
            .flat_map(|graph| &graph.data_calls)
            .filter_map(|call| site_of(call).map(|(file, _)| file.to_string()));
        for file in definitions.chain(rows) {
            owners.entry(file).or_insert(index);
        }
    }
    owners
}

/// Every service's definitions by file and the last segment of their name,
/// each with the lines it opens on: what a boundary call's callee is looked
/// up in ([`LeafCallee::Sibling`]).
struct SiblingLines(HashMap<(String, String), Vec<u32>>);

impl SiblingLines {
    fn of<'d>(services: impl Iterator<Item = &'d CloudRepoData>) -> Self {
        let mut lines: HashMap<(String, String), Vec<u32>> = HashMap::new();
        for data in services {
            for definition in data
                .function_definitions
                .values()
                .filter(|definition| definition.name != crate::visitor::MODULE_SCOPE_KEY)
            {
                lines
                    .entry((
                        definition.file_path.to_string_lossy().into_owned(),
                        last_segment(&definition.name).to_string(),
                    ))
                    .or_default()
                    .push(definition.line_number);
            }
        }
        for at in lines.values_mut() {
            at.sort_unstable();
            at.dedup();
        }
        Self(lines)
    }

    /// The one definition `name` is in `file`; none when no definition, or
    /// more than one, goes by it there.
    fn resolve(&self, file: &str, name: &str) -> Option<DefinitionSite> {
        match self
            .0
            .get(&(file.to_string(), last_segment(name).to_string()))
            .map(Vec::as_slice)
        {
            Some([line]) => Some(DefinitionSite {
                file: file.to_string(),
                line: *line,
            }),
            _ => None,
        }
    }
}

fn last_segment(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// What [`state_leaves`] did with one service's leaf calls.
#[derive(Default)]
struct LeafOutcome {
    /// Rows stated at calls into a function of the service's own.
    own: usize,
    /// Rows stated at calls straight into another service's function.
    boundary: usize,
    in_wrapper: usize,
    third_service: usize,
    unresolved: usize,
    /// Fact rows withdrawn at a client wrapper's call ([`withdraw_wrapped`]).
    withdrawn: usize,
}

/// State a row at each of `service`'s leaf calls ([`LeafCall`]) that holds no
/// row on its lines, and whose operation no row states at the line a call
/// written around it opens on ([`Occupied::holds`]). Two passes over one
/// snapshot of the rows the service holds.
///
/// Pass 1, the calls into a function of the service's own
/// ([`LeafCallee::Own`]), when all of these hold:
///
/// - the walk from the callee reaches exactly ONE request operation
///   ([`Decider::decide`], the callee's own step only);
/// - that request is written in another service's file: a request the
///   service writes itself is stated by its own request summaries, at its
///   line and at the calls other modules make into its module;
/// - the callee itself calls into that service: it is the function that
///   crosses the boundary, so a call one hop further up (a command calling a
///   helper that calls the client) states nothing, as the summaries state a
///   request once per module that calls into it and not once per hop.
///
/// Every callee that passes those three checks is a wrapper, whether or not
/// a row sits at the call.
///
/// Pass 2, the calls straight into a function another service declares
/// (the boundary call, [`LeafCallee::Sibling`], carrick#2773), when all of
/// these hold:
///
/// - exactly one definition of any service is the function called, and it is
///   not the service's own;
/// - the walk from it reaches exactly ONE request operation, as in pass 1,
///   with no crossing check: the call is the crossing, and the function may
///   hand its request back unrun (a factory's `queryFn`);
/// - that request is written in the service that declares the function
///   called, not in a third;
/// - the call is not written inside a wrapper of pass 1: the calls that
///   reach the wrapper state the row, one per module that calls into it.
///   A function nothing calls is no wrapper, so its calls state theirs.
///
/// The row is the reached request's operation (method, target, base,
/// dispatch) as a call through, `wrapper_call` with `reaches_request`, from
/// `request_summary`. It carries no type entry: no capture anchored one at
/// the call. A fact row the service's site questions stated inside a pass 1
/// callee for the operation a leaf row was stated through, the wrapper's own
/// call into the other service, is then withdrawn ([`withdraw_wrapped`]).
fn state_leaves<'g>(
    service: &mut ServiceRows<'_>,
    index: usize,
    owners: &HashMap<String, usize>,
    siblings: &SiblingLines,
    graph: &'g ReachGraph,
    decider: &mut Decider<'g>,
) -> LeafOutcome {
    let mut reached: HashMap<DefinitionSite, Option<&'g Reached>> = HashMap::new();
    let mut reach_from = |decider: &mut Decider<'g>, leaf: &LeafCall, callee: &DefinitionSite| {
        *reached.entry(callee.clone()).or_insert_with(|| {
            let row = SiteRow {
                file: leaf.file.clone(),
                line: leaf.line,
                method: String::new(),
                target: String::new(),
                steps: vec![vec![callee.clone()]],
                receiver: Vec::new(),
                callbacks: Vec::new(),
            };
            match decider.decide(&row) {
                Reach::One { request, .. } => Some(request),
                _ => None,
            }
        })
    };
    // Read before any leaf row is written: two calls on one line state a row
    // each, and a line another pass holds a row on states none.
    let occupied = Occupied::of(service.data);
    let mut written: HashSet<(String, Operation)> = HashSet::new();
    let mut outcome = LeafOutcome::default();
    let mut wrappers: HashSet<DefinitionSite> = HashSet::new();
    // The callees a leaf row was stated through, with the operation stated.
    let mut through: Vec<(DefinitionSite, OperationKey)> = Vec::new();
    // The rows held before any leaf row is written: only those can be a
    // wrapper's own.
    let held = service.data.calls.len();
    for leaf in &service.sites.leaves {
        let LeafCallee::Own(callee) = &leaf.callee else {
            continue;
        };
        let Some(request) = reach_from(decider, leaf, callee) else {
            continue;
        };
        let Some(&request_owner) = request
            .site
            .rsplit_once(':')
            .and_then(|(file, _)| owners.get(file))
        else {
            continue;
        };
        if request_owner == index {
            continue;
        }
        let crosses = graph.runs.get(callee).is_some_and(|edges| {
            edges
                .iter()
                .any(|edge| owners.get(&edge.to.file) == Some(&request_owner))
        });
        if !crosses {
            continue;
        }
        wrappers.insert(callee.clone());
        let operation = operation_of(request);
        if occupied.holds(leaf, &operation) {
            continue;
        }
        // One row per operation at a line.
        if !written.insert((format!("{}:{}", leaf.file, leaf.line), operation)) {
            continue;
        }
        if state_leaf(service.data, leaf, request) {
            outcome.own += 1;
            through.push((
                callee.clone(),
                OperationKey::http(&request.method, request.canonical_path.clone()),
            ));
        }
    }
    // The lines each wrapper opens and closes on, per file.
    let mut wrapper_extents: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for definition in service.data.function_definitions.values() {
        let file = definition.file_path.to_string_lossy().into_owned();
        let site = DefinitionSite {
            file: file.clone(),
            line: definition.line_number,
        };
        if wrappers.contains(&site) {
            wrapper_extents.entry(file).or_default().push((
                definition.line_number,
                definition.end_line.max(definition.line_number),
            ));
        }
    }
    for leaf in &service.sites.leaves {
        let LeafCallee::Sibling { file, name } = &leaf.callee else {
            continue;
        };
        let at = format!("{}:{}", leaf.file, leaf.line);
        let Some(callee) = siblings.resolve(file, name) else {
            outcome.unresolved += 1;
            debug!(
                "  - {at}: a call into another service states nothing: no one definition is {file}:{name}"
            );
            continue;
        };
        let callee_owner = match owners.get(&callee.file) {
            Some(&owner) if owner != index => owner,
            _ => {
                outcome.unresolved += 1;
                debug!(
                    "  - {at}: a call into another service states nothing: {file}:{name} is not another service's"
                );
                continue;
            }
        };
        let Some(request) = reach_from(decider, leaf, &callee) else {
            debug!(
                "  - {at}: a call into another service states nothing: {file}:{name} reaches no one request a fact states"
            );
            continue;
        };
        let request_owner = request
            .site
            .rsplit_once(':')
            .and_then(|(file, _)| owners.get(file));
        if request_owner != Some(&callee_owner) {
            outcome.third_service += 1;
            debug!(
                "  - {at}: a call into another service states nothing: the request {file}:{name} reaches is written in a third service"
            );
            continue;
        }
        if wrapper_extents.get(&leaf.file).is_some_and(|extents| {
            extents
                .iter()
                .any(|(first, last)| *first <= leaf.line && leaf.line <= *last)
        }) {
            outcome.in_wrapper += 1;
            debug!(
                "  - {at}: a call into another service states nothing: the function around it is reached by calls that state the row"
            );
            continue;
        }
        let operation = operation_of(request);
        if occupied.holds(leaf, &operation) {
            continue;
        }
        // A helper only its own file calls has no leaf call to make it a
        // wrapper: its callers' rows, corrected above, are the sites.
        if callers_hold(service.data, &occupied, leaf, &operation) {
            outcome.in_wrapper += 1;
            debug!(
                "  - {at}: a call into another service states nothing: a call of the function around it holds the row"
            );
            continue;
        }
        if !written.insert((at, operation)) {
            continue;
        }
        if state_leaf(service.data, leaf, request) {
            outcome.boundary += 1;
        }
    }
    outcome.withdrawn = withdraw_wrapped(service.data, held, &through);
    outcome
}

/// Withdraw each fact row a service's site questions stated inside a callee
/// a leaf row was just stated through, for the operation that leaf row
/// states (carrick#2618, ruled on carrick#2448): the client wrapper's call
/// into the sibling, whose request its caller in another file now states.
/// A wrapper no leaf row was stated through keeps its row, so each request
/// keeps one fact row whichever call states it. Only the first `held` rows,
/// those held before any leaf row was written, are read. Returns how many
/// rows went.
fn withdraw_wrapped(
    data: &mut CloudRepoData,
    held: usize,
    through: &[(DefinitionSite, OperationKey)],
) -> usize {
    if through.is_empty() {
        return 0;
    }
    // Each callee's lines, from the service's own definitions.
    let spans: Vec<(&str, u32, u32, &OperationKey)> = through
        .iter()
        .filter_map(|(callee, key)| {
            let definition = data.function_definitions.values().find(|definition| {
                definition.line_number == callee.line
                    && definition.file_path.to_string_lossy() == callee.file
            })?;
            Some((
                callee.file.as_str(),
                callee.line,
                definition.end_line.max(callee.line),
                key,
            ))
        })
        .collect();
    let wrapped = |call: &crate::analyzer::ApiEndpointDetails| {
        call.resolution_source == Some(ResolutionSource::RequestSummary)
            && call
                .file_path
                .to_str()
                .and_then(|text| text.rsplit_once(':'))
                .and_then(|(file, line)| Some((file, line.parse::<u32>().ok()?)))
                .is_some_and(|(file, line)| {
                    spans.iter().any(|(at, first, last, key)| {
                        *at == file
                            && (*first..=*last).contains(&line)
                            && crate::site_rows::agrees(&call.key, key)
                    })
                })
    };
    let mut withdrawn = 0;
    let mut index = 0;
    let mut gone: Vec<(String, OperationKey)> = Vec::new();
    data.calls.retain(|call| {
        let keep = index >= held || !wrapped(call);
        index += 1;
        if !keep {
            debug!(
                "  - {}: withdrawn, {:?} is stated at the call into the wrapper",
                call.file_path.display(),
                call.key
            );
            withdrawn += 1;
            gone.push((
                call.file_path.to_string_lossy().into_owned(),
                call.key.clone(),
            ));
        }
        keep
    });
    // The graph holds a twin of each fact row the site fold stated
    // (carrick#2795): it goes with the row, or the graph keeps a request the
    // blob's calls no longer state.
    if let Some(graph) = data.mount_graph.as_mut() {
        graph.data_calls.retain(|call| {
            !(call.resolution_source == Some(ResolutionSource::RequestSummary)
                && call.reaches_request.is_none()
                && gone.iter().any(|(place, key)| {
                    *place == call.file_location
                        && key.as_http().is_some_and(|(method, path)| {
                            call.method.eq_ignore_ascii_case(method) && call.canonical_path == path
                        })
                }))
        });
    }
    withdrawn
}

/// Whether a call site of the function `leaf` is written in holds a row of
/// `operation`: the function's own callers already state the request.
fn callers_hold(
    data: &CloudRepoData,
    occupied: &Occupied,
    leaf: &LeafCall,
    operation: &Operation,
) -> bool {
    let Some(enclosing) = data
        .function_definitions
        .values()
        .filter(|definition| {
            definition.file_path.to_string_lossy() == leaf.file.as_str()
                && definition.line_number <= leaf.line
                && leaf.line <= definition.end_line.max(definition.line_number)
        })
        .max_by_key(|definition| definition.line_number)
    else {
        return false;
    };
    data.function_definitions.values().any(|caller| {
        caller.calls.iter().any(|call| {
            call.file_path == leaf.file
                && call.line_number == enclosing.line_number
                && call.call_site_line > 0
                && occupied.holds_operation(
                    &caller.file_path.to_string_lossy(),
                    call.call_site_line,
                    operation,
                )
        })
    })
}

/// The rows a service already holds, before any leaf row is written.
struct Occupied {
    /// Every file and line a row of any source sits on, in the graph or in
    /// the calls.
    lines: HashSet<(String, u32)>,
    /// The operations the graph's rows state at each file and line.
    operations: HashMap<(String, u32), HashSet<Operation>>,
}

impl Occupied {
    fn of(data: &CloudRepoData) -> Self {
        let mut operations: HashMap<(String, u32), HashSet<Operation>> = HashMap::new();
        for call in data.mount_graph.iter().flat_map(|graph| &graph.data_calls) {
            if let Some((file, line)) = site_of(call) {
                operations
                    .entry((file.to_string(), line))
                    .or_default()
                    .insert(operation(
                        &call.method,
                        &call.canonical_path,
                        call.dispatch.as_ref(),
                    ));
            }
        }
        let in_calls = data.calls.iter().filter_map(|call| {
            let text = call.file_path.to_str()?;
            let (file, line) = text.rsplit_once(':')?;
            Some((file.to_string(), line.parse().ok()?))
        });
        Self {
            lines: operations.keys().cloned().chain(in_calls).collect(),
            operations,
        }
    }

    /// Whether a row of `operation` sits at `file:line`.
    fn holds_operation(&self, file: &str, line: u32, operation: &Operation) -> bool {
        self.operations
            .get(&(file.to_string(), line))
            .is_some_and(|stated| stated.contains(operation))
    }

    /// Whether a row already stands for `leaf`: one on its own lines, or one
    /// of `operation` on the line a call written around it opens on, the
    /// outer call's row for the same request (one call, one row).
    fn holds(&self, leaf: &LeafCall, operation: &Operation) -> bool {
        let file = &leaf.file;
        (leaf.line..=leaf.last_line).any(|line| self.lines.contains(&(file.clone(), line)))
            || leaf.enclosing_lines.iter().any(|line| {
                self.operations
                    .get(&(file.clone(), *line))
                    .is_some_and(|stated| stated.contains(operation))
            })
    }
}

/// Whether the service serves the route a call to `canonical_path` with
/// `method` requests itself: the one rule for a row's `own_route`, whichever
/// pass writes the row (carrick#1926).
pub(crate) fn serves_own_route(
    graph: &crate::mount_graph::MountGraph,
    method: &str,
    canonical_path: &str,
) -> bool {
    graph
        .find_matching_endpoints(canonical_path, method)
        .iter()
        .any(|endpoint| {
            endpoint.evidence == carrick_match::MatchEvidence::RouteDefinition
                && carrick_match::match_agreement(&endpoint.full_path, canonical_path).unwrap_or(0)
                    > 0
        })
}

/// Write the row `leaf` states for `reached` into the graph and the calls.
fn state_leaf(data: &mut CloudRepoData, leaf: &LeafCall, reached: &Reached) -> bool {
    let Some(graph) = data.mount_graph.as_mut() else {
        return false;
    };
    let own_route = serves_own_route(graph, &reached.method, &reached.canonical_path);
    let call = DataFetchingCall {
        method: reached.method.clone(),
        target_url: reached.target_url.clone(),
        canonical_path: reached.canonical_path.clone(),
        client: leaf.client.clone(),
        file_location: format!("{}:{}", leaf.file, leaf.line),
        call_kind: None,
        repo_name: None,
        service_name: None,
        host: reached.host.clone(),
        line: Some(leaf.line),
        base: reached.base.clone(),
        consumers_not_resolved: None,
        resolution_source: Some(ResolutionSource::RequestSummary),
        dispatch: reached.dispatch.clone(),
        role: Some(ConsumerRole::WrapperCall),
        reaches_request: Some(reached.site.clone()),
        library_semantics: Vec::new(),
        own_route,
    };
    debug!(
        "  - {}: states {} {}, the request written at {}",
        call.file_location, call.method, call.target_url, reached.site
    );
    data.calls.push(crate::cloud_storage::call_details(&call));
    graph.data_calls.push(call);
    true
}

/// A deterministic request row, as much of it as a corrected row takes.
#[derive(Debug, Clone)]
struct Reached {
    method: String,
    target_url: String,
    canonical_path: String,
    host: Option<String>,
    base: Option<crate::call_base::CallBaseResolution>,
    dispatch: Option<crate::dispatch::Dispatch>,
    /// `"<file>:<line>"`, the row's own `file_location`.
    site: String,
    /// The line the row is written on.
    line: u32,
}

enum Reach<'g> {
    None,
    Several,
    /// The call also reaches a request no fact states.
    Unstated,
    One {
        request: &'g Reached,
        /// The step of the row that reached it: 0 is the row's own callee, 1
        /// a call spread into an argument, [`CALLBACK_STEP`] a callback among
        /// the arguments, [`RECEIVER_STEP`] the call the callee is read off.
        step: usize,
    },
}

/// An operation as [`ReachGraph::decide`] tells two requests apart.
type Operation = (String, String, String);

fn operation_of(request: &Reached) -> Operation {
    operation(
        &request.method,
        &request.canonical_path,
        request.dispatch.as_ref(),
    )
}

fn operation(
    method: &str,
    canonical_path: &str,
    dispatch: Option<&crate::dispatch::Dispatch>,
) -> Operation {
    (
        method.to_uppercase(),
        canonical_path.to_string(),
        crate::dispatch::Dispatch::key_suffix(dispatch),
    )
}

/// One call edge: the definition called, the line the call is written on
/// (zero when unknown) and how many times the caller makes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    to: DefinitionSite,
    line: u32,
    count: u32,
}

/// A row no fact states: the model's own, or one of unknown source.
#[derive(Debug, Clone)]
struct Unstated {
    method: String,
    target_url: String,
    site: String,
}

/// Every service's definitions and the requests each one writes.
#[derive(Default)]
struct ReachGraph {
    /// Definition -> the definitions it calls, and the ones the definitions
    /// written inside it call.
    calls: HashMap<DefinitionSite, Vec<Edge>>,
    /// Definition -> the calls it makes when it runs: its own, and those of
    /// the definitions written inside it that it calls or hands to a call of
    /// its own (`this.send(() => getRun(options))`). A function written
    /// inside it and only handed back (a factory's `const read = () =>
    /// getRun(..); return read`) is not run by it, and its calls are not
    /// here.
    runs: HashMap<DefinitionSite, Vec<Edge>>,
    /// Definition -> the request rows written inside its lines.
    requests: HashMap<DefinitionSite, Vec<Reached>>,
    /// Definition -> the lines inside it that a request row is written on.
    request_lines: HashMap<DefinitionSite, BTreeSet<u32>>,
    /// Definition -> the rows inside its lines that no fact states.
    unstated: HashMap<DefinitionSite, Vec<Unstated>>,
    /// Every service's side calls ([`CallThroughSites::side_calls`]).
    side_calls: HashMap<SideKey, SideCall>,
    /// Whether a definition makes a request, as far as it has been asked.
    makes_request: std::cell::RefCell<HashMap<DefinitionSite, bool>>,
    /// Every service's return shapes ([`CallThroughSites::return_shapes`]).
    shapes: HashMap<DefinitionSite, BodyShape>,
}

impl ReachGraph {
    fn of<'d>(services: impl Iterator<Item = &'d CloudRepoData> + Clone) -> Self {
        // A definition's body holds the definitions written inside it: a
        // callback handed to a call is indexed as a definition of its own,
        // and what it calls and requests is what the function around it does
        // (`return this.send(() => getRun(options))`). So a definition takes
        // the calls of every definition inside its lines, and the request
        // rows inside its lines whichever definition is innermost.
        let mut graph = Self::default();
        // File -> each definition's first and last line, and what it calls.
        let mut by_file: HashMap<String, Vec<(u32, u32, Vec<Edge>)>> = HashMap::new();
        for data in services.clone() {
            // The file-level owner of module-scope calls is no function: no
            // call reaches it, and its lines are the whole file.
            let functions = data
                .function_definitions
                .values()
                .filter(|definition| definition.name != crate::visitor::MODULE_SCOPE_KEY);
            for definition in functions {
                by_file
                    .entry(definition.file_path.to_string_lossy().into_owned())
                    .or_default()
                    .push((
                        definition.line_number,
                        definition.end_line.max(definition.line_number),
                        definition
                            .calls
                            .iter()
                            .map(|call| Edge {
                                to: DefinitionSite {
                                    file: call.file_path.clone(),
                                    line: call.line_number,
                                },
                                line: call.call_site_line,
                                count: call.call_count,
                            })
                            .collect(),
                    ));
            }
        }
        for (file, definitions) in &by_file {
            let within = |outer: usize, inner: usize| {
                let (first, last, _) = &definitions[outer];
                let (inner_first, inner_last, _) = &definitions[inner];
                first <= inner_first && inner_last <= last
            };
            // Run by a definition around it: that definition calls it
            // (`const read = () => ..; await read()`), or makes a call on the
            // line it opens on, the call it is handed to.
            let run: Vec<bool> = (0..definitions.len())
                .map(|inner| {
                    let (inner_first, inner_last, _) = &definitions[inner];
                    definitions
                        .iter()
                        .enumerate()
                        .any(|(around, (first, last, calls))| {
                            (first, last) != (inner_first, inner_last)
                                && within(around, inner)
                                && calls.iter().any(|edge| {
                                    edge.line == *inner_first
                                        || (edge.to.file == *file && edge.to.line == *inner_first)
                                })
                        })
                })
                .collect();
            for (outer, (first, _, own)) in definitions.iter().enumerate() {
                let site = DefinitionSite {
                    file: file.clone(),
                    line: *first,
                };
                let mut callees: Vec<Edge> = Vec::new();
                let mut runs: Vec<Edge> = own.clone();
                for (inner, (_, _, inner_calls)) in definitions.iter().enumerate() {
                    if !within(outer, inner) {
                        continue;
                    }
                    callees.extend(inner_calls.iter().cloned());
                    if run[inner] {
                        runs.extend(inner_calls.iter().cloned());
                    }
                }
                callees.sort();
                callees.dedup();
                runs.sort();
                runs.dedup();
                graph.calls.entry(site.clone()).or_default().extend(callees);
                graph.runs.entry(site).or_default().extend(runs);
            }
        }
        for edges in graph.calls.values_mut().chain(graph.runs.values_mut()) {
            edges.sort();
            edges.dedup();
        }
        for data in services {
            let Some(mount_graph) = data.mount_graph.as_ref() else {
                continue;
            };
            for call in &mount_graph.data_calls {
                let request = is_request_row(call);
                if !request && !is_unstated_row(call) {
                    continue;
                }
                let Some((file, line)) = site_of(call) else {
                    continue;
                };
                let holding = by_file
                    .get(file)
                    .into_iter()
                    .flatten()
                    .filter(|(first, last, _)| *first <= line && line <= *last);
                for (first, _, _) in holding {
                    let definition = DefinitionSite {
                        file: file.to_string(),
                        line: *first,
                    };
                    if request {
                        graph
                            .request_lines
                            .entry(definition.clone())
                            .or_default()
                            .insert(line);
                        graph.requests.entry(definition).or_default().push(Reached {
                            method: call.method.clone(),
                            target_url: call.target_url.clone(),
                            canonical_path: call.canonical_path.clone(),
                            host: call.host.clone(),
                            base: call.base.clone(),
                            dispatch: call.dispatch.clone(),
                            site: call.file_location.clone(),
                            line,
                        });
                    } else {
                        graph
                            .unstated
                            .entry(definition)
                            .or_default()
                            .push(Unstated {
                                method: call.method.clone(),
                                target_url: call.target_url.clone(),
                                site: call.file_location.clone(),
                            });
                    }
                }
            }
        }
        graph
    }

    /// Collect the requests written in `start` and in the definitions it
    /// reaches within [`MAX_EDGE_DEPTH`] call edges, and the rows no fact
    /// states there, each with whether it was reached only through a side
    /// call ([`SideCall`]).
    ///
    /// A request's own call (the one edge written on a request row's line)
    /// leads into how the request is made, a transport and the primitive it
    /// calls: a row past it is that request's, not another. A fact there is
    /// the transport's side call (a token refresh made before the request is
    /// sent), and a row no fact states there is the primitive the transport
    /// calls (carrick#2795). Every other path counts the rows it reaches,
    /// so a request reached past one request's own call and on another path
    /// counts. A definition two paths
    /// reach is walked once on each, and a request reached both through a
    /// side call and not is not a side request.
    fn walk<'g>(
        &'g self,
        start: &DefinitionSite,
        reached: &mut BTreeMap<&'g str, (&'g Reached, bool)>,
        unstated: &mut BTreeMap<&'g str, (Vec<&'g Unstated>, bool)>,
    ) {
        let mut seen: HashSet<(&DefinitionSite, bool, bool)> = HashSet::new();
        let mut queue: VecDeque<(&DefinitionSite, usize, bool, bool)> = VecDeque::new();
        let Some((start, _)) = self.calls.get_key_value(start) else {
            return;
        };
        seen.insert((start, false, false));
        queue.push_back((start, 0, false, false));
        while let Some((definition, depth, within_request, side)) = queue.pop_front() {
            if !within_request {
                for request in self.requests.get(definition).into_iter().flatten() {
                    let entry = reached
                        .entry(request.site.as_str())
                        .or_insert((request, side));
                    entry.1 &= side;
                }
                for row in self.unstated.get(definition).into_iter().flatten() {
                    let entry = unstated
                        .entry(row.site.as_str())
                        .or_insert_with(|| (Vec::new(), side));
                    if !entry.0.iter().any(|known| std::ptr::eq(*known, row)) {
                        entry.0.push(row);
                    }
                    entry.1 &= side;
                }
            }
            if depth == MAX_EDGE_DEPTH {
                continue;
            }
            let request_lines = self.request_lines.get(definition);
            for edge in self.calls.get(definition).into_iter().flatten() {
                // A callee no service indexes is a definition outside the
                // scan: nothing is known of what it writes.
                let Some((callee, _)) = self.calls.get_key_value(&edge.to) else {
                    continue;
                };
                let one_call = edge.line != 0 && edge.count == 1;
                let request_call =
                    one_call && request_lines.is_some_and(|lines| lines.contains(&edge.line));
                let within = within_request || request_call;
                let side = side || (one_call && self.side_edge(definition, edge));
                if seen.insert((callee, within, side)) {
                    queue.push_back((callee, depth + 1, within, side));
                }
            }
        }
    }

    /// Whether the one call `edge` names, written in `from`, is a side call:
    /// its value is dropped, or goes only into the arguments of calls that
    /// make a request.
    fn side_edge(&self, from: &DefinitionSite, edge: &Edge) -> bool {
        match self
            .side_calls
            .get(&(from.file.clone(), edge.line, edge.to.clone()))
        {
            None => false,
            Some(SideCall::Discarded) => true,
            Some(SideCall::InputTo(outers)) => {
                !outers.is_empty()
                    && outers.iter().all(|outer| match outer {
                        Outer::Request => true,
                        Outer::Calls(definition) => self.makes_a_request(definition),
                        Outer::Unknown => false,
                    })
            }
        }
    }

    /// Whether a call of `start` is worth the body of a request of
    /// `operation` (carrick#2377): the value each function on the way returns
    /// is the value of the call that leads on, and exactly one of them parses
    /// it.
    ///
    /// From `start`, each definition is read through its return shape
    /// ([`BodyShape`]); a definition with none, or one that returns anything
    /// else, leaves the chain unproven. A parse (`.json()`) or one member
    /// read counts a step, a value passed whole counts none. Where the
    /// definition writes a request of `operation` inside the lines of the call
    /// it returns, the request is reached and the walk goes no further from
    /// it. Elsewhere it follows only the calls written inside those lines
    /// (the helper called, and the calls in its arguments, such as a callback
    /// handed to it), never a call written outside them (a client's factory):
    /// that call's value is not what is returned. A call whose value is
    /// dropped, or goes only into a request's arguments, is skipped; a call
    /// whose value goes into another function that makes no request is
    /// unproven, since the value passes through that function.
    ///
    /// Proven means the request is reached, nothing is unproven, the steps
    /// total one, and that step is the Fetch standard's `Response.json()`. A
    /// member read (`.data`, `.status`) is never taken as the body: which
    /// member of a library client's result holds it is the library store's to
    /// state (carrick-cloud#2639), so such a chain abstains and the row keeps
    /// no type.
    fn hands_back_body(&self, start: &DefinitionSite, operation: &Operation) -> bool {
        self.returned_steps(start, operation)
            .is_some_and(|steps| steps == [ShapeKind::Parsed])
    }

    /// Whether `start` returns the value of a request of `operation`, whole,
    /// parsed or read through members, as [`Self::hands_back_body`] follows
    /// the returned call (carrick#2832): a function that requests something
    /// and then returns a value of its own does not.
    fn returns_request(&self, start: &DefinitionSite, operation: &Operation) -> bool {
        self.returned_steps(start, operation).is_some()
    }

    /// The steps read off a request of `operation` on the way from it to what
    /// `start` returns, where the returned call reaches that request and
    /// nothing on the way is unproven ([`Self::hands_back_body`]).
    fn returned_steps(
        &self,
        start: &DefinitionSite,
        operation: &Operation,
    ) -> Option<Vec<ShapeKind>> {
        let (start, _) = self.calls.get_key_value(start)?;
        let mut seen: HashSet<&DefinitionSite> = HashSet::from([start]);
        let mut queue: VecDeque<(&DefinitionSite, usize)> = VecDeque::from([(start, 0)]);
        let mut steps: Vec<ShapeKind> = Vec::new();
        let mut reached = false;
        while let Some((definition, depth)) = queue.pop_front() {
            let shape = self.shapes.get(definition)?;
            if shape.kind != ShapeKind::Value {
                steps.push(shape.kind);
            }
            let written_in_base = |line: u32| shape.first_line <= line && line <= shape.last_line;
            let holds = self
                .requests
                .get(definition)
                .into_iter()
                .flatten()
                .any(|request| {
                    written_in_base(request.line) && operation_of(request) == *operation
                });
            if holds {
                reached = true;
                continue;
            }
            if depth == MAX_EDGE_DEPTH {
                return None;
            }
            for edge in self.calls.get(definition).into_iter().flatten() {
                if edge.line == 0 {
                    return None;
                }
                if !written_in_base(edge.line) {
                    continue;
                }
                if edge.count == 1 {
                    let side =
                        self.side_calls
                            .get(&(definition.file.clone(), edge.line, edge.to.clone()));
                    if side.is_some() {
                        if self.side_edge(definition, edge) {
                            continue;
                        }
                        return None;
                    }
                }
                let (callee, _) = self.calls.get_key_value(&edge.to)?;
                if seen.insert(callee) {
                    queue.push_back((callee, depth + 1));
                }
            }
        }
        reached.then_some(steps)
    }

    /// Whether `start`, or a definition it reaches within [`MAX_EDGE_DEPTH`]
    /// call edges, writes a request row of any source.
    fn makes_a_request(&self, start: &DefinitionSite) -> bool {
        if let Some(known) = self.makes_request.borrow().get(start) {
            return *known;
        }
        let mut seen: HashSet<&DefinitionSite> = HashSet::new();
        let mut queue: VecDeque<(&DefinitionSite, usize)> = VecDeque::new();
        let mut found = false;
        if let Some((start, _)) = self.calls.get_key_value(start) {
            seen.insert(start);
            queue.push_back((start, 0));
        }
        while let Some((definition, depth)) = queue.pop_front() {
            if self.requests.contains_key(definition) || self.unstated.contains_key(definition) {
                found = true;
                break;
            }
            if depth == MAX_EDGE_DEPTH {
                continue;
            }
            for edge in self.calls.get(definition).into_iter().flatten() {
                if let Some((callee, _)) = self.calls.get_key_value(&edge.to)
                    && seen.insert(callee)
                {
                    queue.push_back((callee, depth + 1));
                }
            }
        }
        self.makes_request.borrow_mut().insert(start.clone(), found);
        found
    }
}

/// The walk over every row, deciding each once: a row another row's walk
/// reaches is decided where it is reached.
struct Decider<'g> {
    graph: &'g ReachGraph,
    /// Every service's call-through rows, by `file_location`.
    through: HashMap<String, Vec<&'g SiteRow>>,
    /// A call-through row's decided operation, by its site, method and target.
    decided: HashMap<(String, String, String), Option<Operation>>,
    in_progress: HashSet<(String, String, String)>,
}

impl<'g> Decider<'g> {
    fn new(graph: &'g ReachGraph, sites: &[&'g CallThroughSites]) -> Self {
        let mut through: HashMap<String, Vec<&'g SiteRow>> = HashMap::new();
        for row in sites.iter().flat_map(|sites| &sites.rows) {
            through
                .entry(format!("{}:{}", row.file, row.line))
                .or_default()
                .push(row);
        }
        Self {
            graph,
            through,
            decided: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    /// The request the first step of `row` that reaches any decides.
    ///
    /// A step decides on ONE operation only when every request it reaches is
    /// stated by a fact and is that operation. A row no fact states that the
    /// step also reaches is another request unless it is itself a call
    /// through that settles on the same operation (a command calling a
    /// command): the model's row at a `fetch` beside a token request, or a
    /// row of unknown source, leaves the step unproven.
    ///
    /// Where the step reaches more than that, the request is the one whose
    /// value the functions hand back: the step is read again without the
    /// requests reached only through a side call (a probe awaited and
    /// dropped, a token handed to the request returned). Where that leaves
    /// none, or still more than one, the row is left alone.
    ///
    /// The steps are read in order: the row's own callee; the call that
    /// callee is read off, where the compiler places the callee outside the
    /// repository ([`SiteRow::receiver`]); a call spread into an argument;
    /// the callbacks among the arguments. A receiver's request decides only
    /// where the receiver returns that request's value: a function that
    /// requests something and returns a value of its own (a session object)
    /// leaves the row alone. With two or more callbacks, the ones that reach
    /// a request decide only where every call written in each other callback
    /// reaches a definition of the scan: an unresolved call there could be
    /// the request (carrick#2832).
    fn decide(&mut self, row: &SiteRow) -> Reach<'g> {
        let own = row.steps.first().map(Vec::as_slice).unwrap_or_default();
        if let Some(reach) = self.decide_step(row, own, 0) {
            return reach;
        }
        if let Some(reach) = self.decide_step(row, &row.receiver, RECEIVER_STEP) {
            return match reach {
                Reach::One { request, .. }
                    if !row.receiver.iter().all(|definition| {
                        self.graph
                            .returns_request(definition, &operation_of(request))
                    }) =>
                {
                    Reach::None
                }
                other => other,
            };
        }
        for (position, step) in row.steps.iter().enumerate().skip(1) {
            if let Some(reach) = self.decide_step(row, step, position) {
                return reach;
            }
        }
        if row.callbacks.is_empty() {
            return Reach::None;
        }
        let (reaching, silent): (Vec<&CallbackCalls>, Vec<&CallbackCalls>) = row
            .callbacks
            .iter()
            .partition(|callback| self.reaches_any(row, &callback.reached));
        if reaching.is_empty() || !silent.iter().all(|callback| callback.every_call_resolved) {
            return Reach::None;
        }
        let mut step: Vec<DefinitionSite> = reaching
            .iter()
            .flat_map(|callback| callback.reached.iter().cloned())
            .collect();
        step.sort();
        step.dedup();
        self.decide_step(row, &step, CALLBACK_STEP)
            .unwrap_or(Reach::None)
    }

    /// The requests and the rows no fact states that `step` reaches, each
    /// once by its site, the row's own site aside.
    #[allow(clippy::type_complexity)]
    fn reached_by(
        &self,
        row: &SiteRow,
        step: &[DefinitionSite],
    ) -> (
        BTreeMap<&'g str, (&'g Reached, bool)>,
        BTreeMap<&'g str, (Vec<&'g Unstated>, bool)>,
    ) {
        // By site, so a request two paths reach is one request.
        let mut reached: BTreeMap<&'g str, (&'g Reached, bool)> = BTreeMap::new();
        let mut unstated: BTreeMap<&'g str, (Vec<&'g Unstated>, bool)> = BTreeMap::new();
        for definition in step {
            self.graph.walk(definition, &mut reached, &mut unstated);
        }
        // A site a fact states is that fact's request, and the row's own
        // site is the call being decided.
        let own_site = format!("{}:{}", row.file, row.line);
        unstated.retain(|site, _| !reached.contains_key(site) && *site != own_site);
        (reached, unstated)
    }

    /// Whether `step` reaches any request, stated or not.
    fn reaches_any(&self, row: &SiteRow, step: &[DefinitionSite]) -> bool {
        let (reached, unstated) = self.reached_by(row, step);
        !(reached.is_empty() && unstated.is_empty())
    }

    /// What one step of `row` decides, `None` when it reaches nothing and the
    /// next step is read.
    fn decide_step(
        &mut self,
        row: &SiteRow,
        step: &[DefinitionSite],
        position: usize,
    ) -> Option<Reach<'g>> {
        let (reached, unstated) = self.reached_by(row, step);
        if reached.is_empty() && unstated.is_empty() {
            return None;
        }
        let every = self.judge(&reached, &unstated, false);
        if matches!(every, Reach::One { .. }) {
            return Some(self.placed(every, position));
        }
        let side =
            reached.values().any(|(_, side)| *side) || unstated.values().any(|(_, side)| *side);
        if !side {
            return Some(every);
        }
        let returned = self.judge(&reached, &unstated, true);
        Some(match returned {
            Reach::One { .. } => self.placed(returned, position),
            Reach::None => every,
            other => other,
        })
    }

    /// `reach` with the step of the row that decided.
    fn placed(&self, reach: Reach<'g>, position: usize) -> Reach<'g> {
        match reach {
            Reach::One { request, .. } => Reach::One {
                request,
                step: position,
            },
            other => other,
        }
    }

    /// The one operation the requests of a step name, every one of them or
    /// (`returned_only`) the ones not reached only through a side call.
    /// `Reach::None` when that leaves no request.
    fn judge(
        &mut self,
        reached: &BTreeMap<&'g str, (&'g Reached, bool)>,
        unstated: &BTreeMap<&'g str, (Vec<&'g Unstated>, bool)>,
        returned_only: bool,
    ) -> Reach<'g> {
        let kept = |side: bool| !(returned_only && side);
        let requests: Vec<&'g Reached> = reached
            .values()
            .filter(|(_, side)| kept(*side))
            .map(|(request, _)| *request)
            .collect();
        let rows: Vec<&'g Unstated> = unstated
            .values()
            .filter(|(_, side)| kept(*side))
            .flat_map(|(rows, _)| rows.iter().copied())
            .collect();
        if requests.is_empty() && rows.is_empty() {
            return Reach::None;
        }
        let operations: BTreeSet<Operation> = requests
            .iter()
            .map(|request| operation_of(request))
            .collect();
        if operations.len() > 1 {
            return Reach::Several;
        }
        let (Some(first), Some(operation)) =
            (requests.first().copied(), operations.into_iter().next())
        else {
            return Reach::Unstated;
        };
        for unstated in rows {
            if !self.settles_on(unstated, &operation) {
                return Reach::Unstated;
            }
        }
        Reach::One {
            request: first,
            step: 0,
        }
    }

    /// Whether `row` is a call-through row that settles on `operation`. A
    /// row met again while it is being decided proves nothing.
    fn settles_on(&mut self, row: &Unstated, operation: &Operation) -> bool {
        let Some(through) = self.through.get(&row.site).and_then(|rows| {
            rows.iter()
                .copied()
                .find(|through| through.method == row.method && through.target == row.target_url)
        }) else {
            return false;
        };
        let key = (row.site.clone(), row.method.clone(), row.target_url.clone());
        if let Some(decided) = self.decided.get(&key) {
            return decided.as_ref() == Some(operation);
        }
        if !self.in_progress.insert(key.clone()) {
            return false;
        }
        let decided = match self.decide(through) {
            Reach::One { request, .. } => Some(operation_of(request)),
            _ => None,
        };
        self.in_progress.remove(&key);
        let settles = decided.as_ref() == Some(operation);
        self.decided.insert(key, decided);
        settles
    }
}

/// A request the source states: a fact, and not itself a call through.
fn is_request_row(call: &DataFetchingCall) -> bool {
    call.resolution_source
        .is_some_and(|source| source != ResolutionSource::Model)
        && call.role != Some(ConsumerRole::WrapperCall)
}

/// A row no fact states and nothing marks as a call through: the model's
/// reading of a request, or a row whose source the scan did not record.
fn is_unstated_row(call: &DataFetchingCall) -> bool {
    call.resolution_source
        .is_none_or(|source| source == ResolutionSource::Model)
        && call.role != Some(ConsumerRole::WrapperCall)
        && call.reaches_request.is_none()
}

/// The file and line a row's `file_location` packs.
fn site_of(call: &DataFetchingCall) -> Option<(&str, u32)> {
    let (file, line) = call.file_location.rsplit_once(':')?;
    Some((file, call.line.or_else(|| line.parse().ok())?))
}

enum Change {
    Relabelled,
    Corrected {
        /// The row held consumer type entries and they were dropped, because
        /// the row's call is not the call that reaches the request.
        types_dropped: bool,
        /// The chain hands back the request's body but the type captured at
        /// the row is a bare top type: the entries were dropped.
        types_unknown: bool,
        /// The row kept its response entry and its request entry was
        /// dropped: a call's arguments are not a request body.
        request_dropped: bool,
    },
}

/// Change the one row of `data` that `row` names to what `reached` states.
/// `carries_response`: the chain from the row's call hands back the request's
/// body ([`ReachGraph::hands_back_body`]), so the response type anchored there
/// is the request's. `None`
/// when the blob holds no such row: the graph build keeps fewer rows than the
/// analysis stated.
fn apply(
    data: &mut CloudRepoData,
    row: &SiteRow,
    reached: &Reached,
    carries_response: bool,
) -> Option<Change> {
    let location = format!("{}:{}", row.file, row.line);
    let graph = data.mount_graph.as_mut()?;
    let index = graph.data_calls.iter().position(|call| {
        call.file_location == location
            && call.method == row.method
            && call.target_url == row.target
            && call.resolution_source == Some(ResolutionSource::Model)
            && call.reaches_request.is_none()
    })?;

    let call = &graph.data_calls[index];
    let own = call.method.eq_ignore_ascii_case(&reached.method)
        && call.dispatch == reached.dispatch
        && carrick_match::paths_match(&reached.canonical_path, &call.canonical_path);
    if own {
        let call = &mut graph.data_calls[index];
        call.role = Some(ConsumerRole::WrapperCall);
        call.reaches_request = Some(reached.site.clone());
        debug!(
            "  - {location}: the model's {} {} is the call through to {}",
            call.method, call.canonical_path, reached.site
        );
        return Some(Change::Relabelled);
    }

    let before = OperationKey::http(&call.method, call.canonical_path.clone());
    let before_dispatch = call.dispatch.clone();
    debug!(
        "  - {location}: the model's {} {} becomes {} {}, the request written at {}",
        call.method, call.target_url, reached.method, reached.target_url, reached.site
    );
    // The site may already hold a row for the reached operation, stated
    // twice by the model. One row is kept.
    let twin = graph.data_calls.iter().enumerate().any(|(other, call)| {
        other != index
            && call.file_location == location
            && call.method.eq_ignore_ascii_case(&reached.method)
            && call.canonical_path == reached.canonical_path
            && call.dispatch == reached.dispatch
    });
    let own_route = serves_own_route(graph, &reached.method, &reached.canonical_path);
    // The entries anchored at the site were captured under the operation the
    // model stated.
    let at_the_row = |entry: &crate::cloud_storage::TypeManifestEntry| {
        entry.role == ManifestRole::Consumer
            && entry.file_path == row.file
            && entry.line_number == row.line
            && before.as_http().is_some_and(|(method, path)| {
                entry
                    .key
                    .as_http()
                    .is_some_and(|(entry_method, entry_path)| {
                        entry_method == normalize_manifest_method(method) && entry_path == path
                    })
            })
    };
    // A chain that hands back the body but whose type the compiler could not
    // say (the helper's `T` is never given) is a bare top type: judged, it
    // makes its pair unverifiable, so the entry goes (carrick#2377).
    let types_unknown = carries_response && !twin && {
        let responses: Vec<&crate::cloud_storage::TypeManifestEntry> = data
            .type_manifest
            .iter()
            .flatten()
            .filter(|entry| at_the_row(entry) && entry.type_kind == ManifestTypeKind::Response)
            .collect();
        !responses.is_empty() && responses.iter().all(|entry| is_bare_top_type(entry))
    };
    let carries_response = carries_response && !types_unknown;
    if twin {
        graph.data_calls.remove(index);
    } else {
        let call = &mut graph.data_calls[index];
        call.method = reached.method.clone();
        call.target_url = reached.target_url.clone();
        call.canonical_path = reached.canonical_path.clone();
        call.host = reached.host.clone();
        call.base = reached.base.clone();
        call.dispatch = reached.dispatch.clone();
        call.resolution_source = Some(ResolutionSource::RequestSummary);
        call.role = Some(ConsumerRole::WrapperCall);
        call.reaches_request = Some(reached.site.clone());
        call.own_route = own_route;
    }

    // The projection of the same row.
    let projected = data.calls.iter().position(|operation| {
        operation.key == before
            && operation.dispatch == before_dispatch
            && operation.resolution_source == Some(ResolutionSource::Model)
            && operation.file_path == Path::new(&location)
    });
    if let Some(projected) = projected {
        if twin {
            data.calls.remove(projected);
        } else {
            let operation = &mut data.calls[projected];
            operation.key = OperationKey::http(&reached.method, reached.canonical_path.clone());
            operation.dispatch = reached.dispatch.clone();
            operation.resolution_source = Some(ResolutionSource::RequestSummary);
            if !carries_response {
                operation.request_type = None;
                operation.response_type = None;
            }
        }
    }

    // Where the row's call hands back the request's body (the chain from its
    // callee reads one step off the transport's value), what it returns is
    // what the caller reads of that request: the entries keep their captured
    // types (the alias names the site, not the operation) and take the
    // reached operation's key, so the cross-service check judges them against
    // its producer. Anywhere else the call's value is a function's own result
    // (an envelope, a mapped object, a hook's state), or the site already
    // holds a row for the reached operation, or the type is a bare top type:
    // they go.
    let mut types_dropped = false;
    let mut request_dropped = false;
    if let Some(manifest) = data.type_manifest.as_mut() {
        if carries_response && !twin {
            let carried = OperationKey::http(
                &normalize_manifest_method(&reached.method),
                reached.canonical_path.clone(),
            );
            // The response entry only. The call's arguments are the called
            // function's parameters, not a request body, so a request-side
            // check at a call through is wrong by construction: the request
            // entry goes, as it does at a call to a function that builds its
            // own body (`drop_call_through_entries`).
            let before = manifest.len();
            manifest.retain(|entry| {
                !(at_the_row(entry) && entry.type_kind == ManifestTypeKind::Request)
            });
            request_dropped = manifest.len() < before;
            for entry in manifest.iter_mut().filter(|entry| at_the_row(entry)) {
                entry.key = carried.clone();
            }
        } else {
            let before = manifest.len();
            manifest.retain(|entry| !at_the_row(entry));
            types_dropped = !twin && !types_unknown && manifest.len() < before;
        }
    }
    debug!(
        "  - {location}: consumer types {}",
        if types_unknown {
            "dropped, the chain hands back the body but the type is unknown"
        } else if carries_response && !twin {
            "kept, the chain hands back the request's body"
        } else if types_dropped {
            "dropped, the chain does not hand back the request's body"
        } else {
            "none to carry"
        }
    );
    Some(Change::Corrected {
        types_dropped,
        types_unknown,
        request_dropped,
    })
}

/// Whether a captured type says nothing: the compiler could not state it, or
/// it is `unknown` or `any` as written.
fn is_bare_top_type(entry: &crate::cloud_storage::TypeManifestEntry) -> bool {
    let bare = |text: &Option<String>| {
        text.as_deref()
            .is_some_and(|text| matches!(text.trim(), "unknown" | "any"))
    };
    entry.type_state == crate::cloud_storage::ManifestTypeState::Unknown
        || bare(&entry.resolved_definition)
        || bare(&entry.expanded_definition)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"export async function load(id: string, target: string) {
  const run = await client.getRun(id);
  const page = useQuery({ ...runOptions({ id }) });
  const list = useQuery({ queryKey: ["runs"], queryFn: () => listRuns() });
  await retry(() => vendor.send(id), { onRetry: () => audit(id) });
  const orders = await request("/orders");
  const made = await timed(http.post(target, id));
  const saved = await save(id, await loadToken());
  const one = first(id); const two = second(id);
  const built = await request(`${target}/orders/${id}`);
  const split = await tokens(id.split("/"));
  const caught = await listRuns().then(keep).catch(() => []);
  const unknown = await listRuns().refresh();
}
"#;

    /// The span of the call written as `needle`, in the units a row carries.
    fn span(needle: &str) -> (u32, u32) {
        let offset = SOURCE.find(needle).expect("needle is in the source") as u32;
        (
            offset + SWC_SPAN_BASE,
            offset + SWC_SPAN_BASE + needle.len() as u32,
        )
    }

    fn definition(file: &str, line: u32) -> DefinitionSite {
        DefinitionSite {
            file: file.to_string(),
            line,
        }
    }

    /// carrick#2378: what a blob stores reads back as the facts the scan
    /// read, and two scans that read the same facts store the same bytes.
    #[test]
    fn stored_facts_read_back_as_the_scan_read_them() {
        let facts = |order: &[usize]| {
            let side_calls = [
                (
                    ("a.ts".to_string(), 3, definition("b.ts", 1)),
                    SideCall::Discarded,
                ),
                (
                    ("a.ts".to_string(), 7, definition("b.ts", 9)),
                    SideCall::InputTo(vec![
                        Outer::Request,
                        Outer::Calls(definition("c.ts", 2)),
                        Outer::Unknown,
                    ]),
                ),
            ];
            let shapes = [
                (
                    definition("b.ts", 9),
                    BodyShape {
                        kind: ShapeKind::Member,
                        first_line: 10,
                        last_line: 12,
                    },
                ),
                (
                    definition("b.ts", 1),
                    BodyShape {
                        kind: ShapeKind::Value,
                        first_line: 2,
                        last_line: 2,
                    },
                ),
            ];
            CallThroughSites {
                rows: vec![SiteRow {
                    file: "a.ts".to_string(),
                    line: 7,
                    method: "GET".to_string(),
                    target: "/getRun".to_string(),
                    steps: vec![vec![definition("b.ts", 9)], vec![]],
                    receiver: vec![definition("c.ts", 3)],
                    callbacks: vec![
                        CallbackCalls {
                            reached: vec![definition("b.ts", 9)],
                            every_call_resolved: false,
                        },
                        CallbackCalls {
                            reached: Vec::new(),
                            every_call_resolved: true,
                        },
                    ],
                }],
                return_shapes: order.iter().map(|&i| shapes[i].clone()).collect(),
                side_calls: order.iter().map(|&i| side_calls[i].clone()).collect(),
                leaves: Vec::new(),
            }
        };
        let sites = facts(&[0, 1]);
        let stored = StoredCallThrough::of(&sites).expect("facts are stored");
        let written = serde_json::to_string(&stored).unwrap();
        let read: StoredCallThrough = serde_json::from_str(&written).unwrap();
        let back = read.sites();
        assert_eq!(back.rows, sites.rows);
        assert_eq!(back.return_shapes, sites.return_shapes);
        assert_eq!(back.side_calls, sites.side_calls);
        assert_eq!(
            serde_json::to_string(&StoredCallThrough::of(&facts(&[1, 0])).unwrap()).unwrap(),
            written
        );
        assert_eq!(StoredCallThrough::of(&CallThroughSites::default()), None);
        // A service with return shapes and nothing else is still stored.
        let only_shapes = CallThroughSites {
            return_shapes: sites.return_shapes.clone(),
            ..CallThroughSites::default()
        };
        assert!(StoredCallThrough::of(&only_shapes).is_some());
    }

    /// A blob written before the return shapes carries the list of definitions
    /// whose call was worth the request's parsed body; it loads, reading as a
    /// service whose chains are unproven until it is rescanned.
    #[test]
    fn a_blob_written_with_the_old_body_list_still_loads() {
        let written = r#"{
            "rows": [{"file": "a.ts", "line": 7, "method": "GET", "target": "/getRun", "steps": [[{"file": "b.ts", "line": 9}]]}],
            "hands_back_body": [{"file": "b.ts", "line": 9}],
            "side_calls": []
        }"#;
        let read: StoredCallThrough = serde_json::from_str(written).unwrap();
        assert_eq!(read.rows.len(), 1);
        assert!(read.sites().return_shapes.is_empty());
    }

    /// A model row on `line` that no candidate joined: it has no span.
    fn model_row(line: i32) -> DataCallResult {
        DataCallResult {
            candidate_id: format!("model:{line}"),
            line_number: line,
            target: format!("/stated/{line}"),
            method: Some("GET".to_string()),
            call_kind: None,
            pattern_matched: String::new(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            call_expression_text: None,
            call_expression_line: Some(line),
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            dispatch: None,
            resolution_source: Some(ResolutionSource::Model),
            reaches_request: None,
            body_literals: Default::default(),
            library_semantics: Vec::new(),
            at_caller: false,
            call_body: None,
        }
    }

    /// Every call the call graph resolved in [`SOURCE`], by span.
    fn resolved() -> HashMap<(u32, u32), DefinitionSite> {
        [
            ("client.getRun(id)", definition("client.ts", 10)),
            ("runOptions({ id })", definition("options.ts", 3)),
            ("listRuns()", definition("client.ts", 20)),
            ("audit(id)", definition("audit.ts", 1)),
            (r#"request("/orders")"#, definition("request.ts", 1)),
            ("timed(http.post(target, id))", definition("timed.ts", 1)),
            ("save(id, await loadToken())", definition("save.ts", 1)),
            ("loadToken()", definition("token.ts", 1)),
            (
                "request(`${target}/orders/${id}`)",
                definition("request.ts", 1),
            ),
            (r#"tokens(id.split("/"))"#, definition("tokens.ts", 1)),
        ]
        .into_iter()
        .map(|(needle, reached)| (span(needle), reached))
        .chain([
            // The first `listRuns()` of line 12, which the needle above
            // cannot tell from the one inside the chain.
            (
                {
                    let (start, _) = span("listRuns().then(keep)");
                    (start, start + "listRuns()".len() as u32)
                },
                definition("client.ts", 20),
            ),
        ])
        .collect()
    }

    /// The calls whose callee the compiler places outside the repository:
    /// the promise methods of line 12. `refresh` on line 13 is not one.
    fn outside() -> HashSet<(u32, u32)> {
        [
            "listRuns().then(keep).catch(() => [])",
            "listRuns().then(keep)",
        ]
        .into_iter()
        .map(span)
        .collect()
    }

    /// The rows [`read_file_sites`] reads for `rows` over [`SOURCE`].
    fn read_rows(rows: Vec<DataCallResult>) -> Vec<SiteRow> {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("load.ts");
        std::fs::write(&file, SOURCE).expect("the source is written");
        let result = FileAnalysisResult {
            data_calls: rows,
            ..Default::default()
        };
        read_file_sites(
            &file,
            "src/load.ts",
            &result,
            &Resolution {
                resolved: &resolved(),
                outside: &outside(),
            },
        )
    }

    /// [`read_rows`] as (line, steps).
    fn read(rows: Vec<DataCallResult>) -> Vec<(u32, Vec<Vec<DefinitionSite>>)> {
        read_rows(rows)
            .into_iter()
            .map(|row| (row.line, row.steps))
            .collect()
    }

    #[test]
    fn the_calls_own_callee_is_the_first_step() {
        assert_eq!(
            read(vec![model_row(2)]),
            [(2, vec![vec![definition("client.ts", 10)], vec![], vec![]])]
        );
    }

    #[test]
    fn a_row_with_a_span_is_placed_by_it() {
        let (start, end) = span("client.getRun(id)");
        let row = DataCallResult {
            call_expression_span_start: Some(start),
            call_expression_span_end: Some(end),
            ..model_row(2)
        };
        assert_eq!(
            read(vec![row]),
            [(2, vec![vec![definition("client.ts", 10)], vec![], vec![]])]
        );
    }

    #[test]
    fn a_call_spread_into_an_object_argument_is_the_second_step() {
        assert_eq!(
            read(vec![model_row(3)]),
            [(3, vec![vec![], vec![definition("options.ts", 3)], vec![]])]
        );
    }

    #[test]
    fn the_calls_in_the_one_callback_are_the_third_step() {
        assert_eq!(
            read(vec![model_row(4)]),
            [(4, vec![vec![], vec![], vec![definition("client.ts", 20)]])]
        );
    }

    /// Two callbacks state no third step; each is kept apart, with whether
    /// every call written in it resolves (carrick#2832).
    #[test]
    fn two_callbacks_are_kept_apart() {
        let rows = read_rows(vec![model_row(5)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].steps, [vec![], vec![], vec![]]);
        assert_eq!(
            rows[0].callbacks,
            [
                // `() => vendor.send(id)`: nothing resolves `vendor`.
                CallbackCalls {
                    reached: Vec::new(),
                    every_call_resolved: false,
                },
                CallbackCalls {
                    reached: vec![definition("audit.ts", 1)],
                    every_call_resolved: true,
                },
            ]
        );
        // One callback is the third step, and is not kept apart.
        assert!(read_rows(vec![model_row(4)])[0].callbacks.is_empty());
    }

    /// `listRuns().then(keep).catch(() => [])`: both promise methods are the
    /// library's, so the call they are read off, `listRuns()`, is read in the
    /// own callee's place. `listRuns().refresh()`, whose callee the compiler
    /// does not place outside the repository, reads nothing (carrick#2832).
    #[test]
    fn the_call_a_library_member_is_read_off_is_the_receiver() {
        let rows = read_rows(vec![model_row(12), model_row(13)]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.line, row.steps.clone(), row.receiver.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    12,
                    vec![vec![], vec![], vec![]],
                    vec![definition("client.ts", 20)]
                ),
                (13, vec![vec![], vec![], vec![]], vec![]),
            ]
        );
        // A row whose own callee resolves reads no receiver.
        assert!(read_rows(vec![model_row(2)])[0].receiver.is_empty());
    }

    /// `save(id, await loadToken())`: `loadToken` reaches a definition too,
    /// and a request made while an argument is computed has a site of its own.
    #[test]
    fn a_plain_argument_is_never_read() {
        assert_eq!(
            read(vec![model_row(8)]),
            [(8, vec![vec![definition("save.ts", 1)], vec![], vec![]])]
        );
    }

    #[test]
    fn a_site_that_writes_a_route_or_states_a_verb_is_not_read() {
        // A literal path, a call that states its verb, and a template with a
        // path behind its base.
        assert_eq!(read(vec![model_row(6), model_row(7), model_row(10)]), []);
        // A bare separator is no route.
        assert_eq!(
            read(vec![model_row(11)]),
            [(11, vec![vec![definition("tokens.ts", 1)], vec![], vec![]])]
        );
    }

    /// A line that opens two calls, neither inside the other, says nothing
    /// about which the row is.
    #[test]
    fn a_row_on_a_line_with_two_calls_is_not_placed() {
        assert_eq!(read(vec![model_row(9)]), []);
    }

    #[test]
    fn a_site_that_holds_a_deterministic_row_is_not_read() {
        let (start, end) = span("listRuns()");
        let stated = DataCallResult {
            resolution_source: Some(ResolutionSource::RequestSummary),
            call_expression_span_start: Some(start),
            call_expression_span_end: Some(end),
            ..model_row(4)
        };
        assert_eq!(read(vec![model_row(4), stated]), []);
    }

    #[test]
    fn a_route_is_a_path_a_url_or_a_base_with_a_path_behind_it() {
        for route in [
            "/orders",
            "http://api.test/x",
            "https://api.test",
            "${}/orders",
        ] {
            assert!(writes_a_route(route), "{route}");
        }
        for other in ["/", "orders", "//", "${}", "${}orders", "a/b", ""] {
            assert!(!writes_a_route(other), "{other}");
        }
    }

    fn blob(name: &str) -> CloudRepoData {
        CloudRepoData {
            repo_name: name.to_string(),
            service_name: Some(name.to_string()),
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            dirty: None,
            mount_graph: Some(crate::mount_graph::MountGraph::new()),
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            types_degraded: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
            scanner_version: None,
            scanner_build: None,
            boundary: None,
            dispatch_tables: None,
            possible_calls: None,
            possible_calls_omitted: None,
            call_through: None,
            route_boundaries: None,
            memory_peaks: None,
        }
    }

    /// A function of `file` between `first` and `last`, calling the
    /// definition at `calls` (file, line) from `at`.
    fn function(
        name: &str,
        file: &str,
        first: u32,
        last: u32,
        calls: Option<(&str, u32, u32)>,
    ) -> crate::visitor::FunctionDefinition {
        let mut definition: crate::visitor::FunctionDefinition =
            serde_json::from_value(serde_json::json!({
                "name": name,
                "file_path": file,
                "node_type": "FunctionDeclaration",
                "arguments": [],
                "line_number": first,
                "end_line": last,
            }))
            .expect("a definition");
        if let Some((to, line, at)) = calls {
            definition.calls = vec![crate::visitor::FunctionCallRef {
                name: String::new(),
                file_path: to.to_string(),
                line_number: line,
                call_site_line: at,
                call_count: 1,
            }];
        }
        definition
    }

    /// A request written by a fact (`request_summary`) at `file:line`.
    fn request_at(file: &str, line: u32) -> DataFetchingCall {
        DataFetchingCall {
            method: "GET".to_string(),
            target_url: "/things".to_string(),
            canonical_path: "/things".to_string(),
            client: "transport".to_string(),
            file_location: format!("{file}:{line}"),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(line),
            base: None,
            consumers_not_resolved: None,
            resolution_source: Some(ResolutionSource::RequestSummary),
            dispatch: None,
            role: None,
            reaches_request: None,
            library_semantics: Vec::new(),
            own_route: false,
        }
    }

    fn put(data: &mut CloudRepoData, definition: crate::visitor::FunctionDefinition) {
        let key = format!(
            "{}:{}",
            definition.file_path.display(),
            definition.line_number
        );
        data.function_definitions.insert(key, definition);
    }

    fn boundary_leaf(file: &str, name: &str) -> LeafCall {
        LeafCall {
            file: "web/a.ts".to_string(),
            line: 3,
            last_line: 3,
            enclosing_lines: Vec::new(),
            client: name.to_string(),
            callee: LeafCallee::Sibling {
                file: file.to_string(),
                name: name.to_string(),
            },
        }
    }

    /// Settle three services: `web` calls `load` of `lib/q.ts` at its line 3.
    /// `load` calls `send`, which `lib` declares itself or a third service
    /// does, and the request is written inside `send`.
    fn settle_boundary(send_in_third_service: bool, second_load: bool) -> (Settled, usize) {
        let mut web = blob("web");
        put(&mut web, function("run", "web/a.ts", 1, 5, None));
        let mut lib = blob("lib");
        let (send_file, send_owner) = if send_in_third_service {
            ("third/r.ts", "third")
        } else {
            ("lib/r.ts", "lib")
        };
        put(
            &mut lib,
            function("load", "lib/q.ts", 1, 3, Some((send_file, 1, 2))),
        );
        if second_load {
            put(&mut lib, function("Other.load", "lib/q.ts", 5, 7, None));
        }
        let mut third = blob("third");
        let owner = if send_owner == "third" {
            &mut third
        } else {
            &mut lib
        };
        put(owner, function("send", send_file, 1, 3, None));
        owner
            .mount_graph
            .as_mut()
            .unwrap()
            .data_calls
            .push(request_at(send_file, 2));
        let leaves = CallThroughSites {
            leaves: vec![boundary_leaf("lib/q.ts", "load")],
            ..Default::default()
        };
        let none = CallThroughSites::default();
        let mut services = [
            ServiceRows {
                data: &mut web,
                sites: &leaves,
                writable: true,
            },
            ServiceRows {
                data: &mut lib,
                sites: &none,
                writable: true,
            },
            ServiceRows {
                data: &mut third,
                sites: &none,
                writable: true,
            },
        ];
        let settled = settle(&mut services);
        let rows = web.mount_graph.as_ref().unwrap().data_calls.len();
        (settled, rows)
    }

    #[test]
    fn a_call_into_another_services_function_states_the_request_it_reaches() {
        let (settled, rows) = settle_boundary(false, false);
        assert_eq!((settled.boundary_rows, rows), (1, 1), "{settled:?}");
    }

    #[test]
    fn a_call_into_a_service_whose_function_reaches_a_third_services_request_states_nothing() {
        let (settled, rows) = settle_boundary(true, false);
        assert_eq!(
            (settled.boundary_rows, settled.boundary_third_service, rows),
            (0, 1, 0),
            "{settled:?}"
        );
    }

    #[test]
    fn a_sibling_callee_whose_file_holds_two_definitions_of_its_name_is_left_alone() {
        let (settled, rows) = settle_boundary(false, true);
        assert_eq!(
            (settled.boundary_rows, settled.boundary_unresolved, rows),
            (0, 1, 0),
            "{settled:?}"
        );
    }

    /// A definition of `file` between `first` and `last` that calls these
    /// definitions, each as (file, line it opens on, line the call is on).
    fn calling(
        name: &str,
        file: &str,
        first: u32,
        last: u32,
        calls: &[(&str, u32, u32)],
    ) -> crate::visitor::FunctionDefinition {
        let mut definition = function(name, file, first, last, None);
        definition.calls = calls
            .iter()
            .map(|(to, line, at)| crate::visitor::FunctionCallRef {
                name: String::new(),
                file_path: to.to_string(),
                line_number: *line,
                call_site_line: *at,
                call_count: 1,
            })
            .collect();
        definition
    }

    fn shaped(
        definition: (&str, u32),
        kind: ShapeKind,
        first_line: u32,
        last_line: u32,
    ) -> (DefinitionSite, BodyShape) {
        (
            self::definition(definition.0, definition.1),
            BodyShape {
                kind,
                first_line,
                last_line,
            },
        )
    }

    /// The graph of one service with these definitions, return shapes and
    /// request rows (file, line, written through a library).
    fn chain(
        definitions: Vec<crate::visitor::FunctionDefinition>,
        shapes: Vec<(DefinitionSite, BodyShape)>,
        requests: &[(&str, u32, bool)],
    ) -> ReachGraph {
        let mut data = blob("app");
        for definition in definitions {
            put(&mut data, definition);
        }
        for (file, line, library) in requests {
            let mut call = request_at(file, *line);
            if *library {
                call.resolution_source = Some(ResolutionSource::RequestSpec);
            }
            data.mount_graph.as_mut().unwrap().data_calls.push(call);
        }
        let mut graph = ReachGraph::of(std::iter::once(&data));
        graph.shapes = shapes.into_iter().collect();
        graph
    }

    fn things() -> Operation {
        ("GET".to_string(), "/things".to_string(), String::new())
    }

    /// Value, then a helper that reads one member, then a spec: one step, off
    /// a library request.
    fn value_member_spec(helper: ShapeKind) -> bool {
        let graph = chain(
            vec![
                calling("method", "a.ts", 1, 5, &[("b.ts", 10, 2), ("c.ts", 1, 3)]),
                function("helper", "b.ts", 10, 14, None),
                function("spec", "c.ts", 1, 4, None),
            ],
            vec![
                shaped(("a.ts", 1), ShapeKind::Value, 2, 4),
                shaped(("b.ts", 10), helper, 12, 12),
                shaped(("c.ts", 1), ShapeKind::Value, 2, 3),
            ],
            &[("c.ts", 3, true)],
        );
        graph.hands_back_body(&definition("a.ts", 1), &things())
    }

    /// A manifest entry with this state and definition.
    fn entry_typed(
        state: &str,
        definition: Option<&str>,
    ) -> crate::cloud_storage::TypeManifestEntry {
        serde_json::from_value(serde_json::json!({
            "protocol": "http", "method": "GET", "path": "/things",
            "role": "consumer", "type_kind": "response", "type_alias": "A",
            "file_path": "a.ts", "line_number": 3, "is_explicit": false,
            "type_state": state, "resolved_definition": definition,
            "evidence": {
                "file_path": "a.ts", "span_start": null, "span_end": null,
                "line_number": 3, "infer_kind": "request_body",
                "is_explicit": false, "type_state": state
            }
        }))
        .expect("an entry")
    }

    #[test]
    fn a_bare_top_type_is_no_type_to_carry() {
        assert!(is_bare_top_type(&entry_typed("unknown", None)));
        assert!(is_bare_top_type(&entry_typed("implicit", Some("unknown"))));
        assert!(is_bare_top_type(&entry_typed("implicit", Some(" any "))));
        assert!(!is_bare_top_type(&entry_typed("explicit", Some("Run"))));
        assert!(!is_bare_top_type(&entry_typed(
            "implicit",
            Some("unknown[]")
        )));
    }

    #[test]
    fn a_parse_by_a_helper_hands_back_the_body() {
        assert!(value_member_spec(ShapeKind::Parsed));
    }

    /// carrick#2377: a made-up client's wrapper returns `res.status`, or `.data`
    /// of a library's result. Which member holds the body is the library
    /// store's to state, so a member read is never taken as the body.
    #[test]
    fn a_member_read_by_a_helper_over_a_library_request_abstains() {
        assert!(!value_member_spec(ShapeKind::Member));
    }

    #[test]
    fn a_chain_with_no_step_or_two_steps_hands_back_nothing() {
        assert!(!value_member_spec(ShapeKind::Value));
        // Two parses are two steps.
        let graph = chain(
            vec![
                calling("method", "a.ts", 1, 5, &[("c.ts", 1, 2)]),
                function("spec", "c.ts", 1, 4, None),
            ],
            vec![
                shaped(("a.ts", 1), ShapeKind::Parsed, 2, 2),
                shaped(("c.ts", 1), ShapeKind::Parsed, 2, 3),
            ],
            &[("c.ts", 3, true)],
        );
        assert!(!graph.hands_back_body(&definition("a.ts", 1), &things()));
    }

    #[test]
    fn a_parse_hands_back_the_body_and_a_member_read_never_does() {
        for (kind, library, proven) in [
            (ShapeKind::Parsed, false, true),
            (ShapeKind::Parsed, true, true),
            (ShapeKind::Member, true, false),
            (ShapeKind::Member, false, false),
        ] {
            let graph = chain(
                vec![
                    calling("method", "a.ts", 1, 5, &[("c.ts", 1, 2)]),
                    function("reader", "c.ts", 1, 4, None),
                ],
                vec![
                    shaped(("a.ts", 1), ShapeKind::Value, 2, 2),
                    shaped(("c.ts", 1), kind, 2, 3),
                ],
                &[("c.ts", 3, library)],
            );
            assert_eq!(
                graph.hands_back_body(&definition("a.ts", 1), &things()),
                proven,
                "{kind:?} over a library request: {library}"
            );
        }
    }

    #[test]
    fn a_definition_with_no_return_shape_leaves_the_chain_unproven() {
        let graph = chain(
            vec![
                calling("method", "a.ts", 1, 5, &[("c.ts", 1, 2)]),
                function("reader", "c.ts", 1, 4, None),
            ],
            vec![shaped(("c.ts", 1), ShapeKind::Parsed, 2, 3)],
            &[("c.ts", 3, false)],
        );
        assert!(!graph.hands_back_body(&definition("a.ts", 1), &things()));
    }

    #[test]
    fn a_value_that_goes_into_a_function_making_no_request_leaves_the_chain_unproven() {
        let mut graph = chain(
            vec![
                calling(
                    "method",
                    "a.ts",
                    1,
                    5,
                    &[("b.ts", 10, 2), ("d.ts", 1, 2), ("c.ts", 1, 3)],
                ),
                function("helper", "b.ts", 10, 14, None),
                function("mapper", "d.ts", 1, 3, None),
                function("reader", "c.ts", 1, 4, None),
            ],
            vec![
                shaped(("a.ts", 1), ShapeKind::Value, 2, 4),
                shaped(("b.ts", 10), ShapeKind::Parsed, 12, 12),
                shaped(("d.ts", 1), ShapeKind::Value, 2, 2),
                shaped(("c.ts", 1), ShapeKind::Value, 2, 3),
            ],
            &[("c.ts", 3, true)],
        );
        let call = ("a.ts".to_string(), 2, definition("d.ts", 1));
        // Dropped: a side call, skipped.
        graph.side_calls.insert(call.clone(), SideCall::Discarded);
        assert!(graph.hands_back_body(&definition("a.ts", 1), &things()));
        // Handed to a mapper that makes no request: the value passes through it.
        graph.side_calls.insert(
            call,
            SideCall::InputTo(vec![Outer::Calls(definition("d.ts", 1))]),
        );
        assert!(!graph.hands_back_body(&definition("a.ts", 1), &things()));
    }

    #[test]
    fn a_call_written_outside_the_returned_calls_lines_is_not_followed() {
        // The factory on line 2 has no shape; it is not the call returned.
        let graph = chain(
            vec![
                calling("method", "a.ts", 1, 6, &[("b.ts", 1, 2), ("c.ts", 1, 4)]),
                function("factory", "b.ts", 1, 3, None),
                function("reader", "c.ts", 1, 4, None),
            ],
            vec![
                shaped(("a.ts", 1), ShapeKind::Value, 4, 5),
                shaped(("c.ts", 1), ShapeKind::Parsed, 2, 3),
            ],
            &[("c.ts", 3, false)],
        );
        assert!(graph.hands_back_body(&definition("a.ts", 1), &things()));
    }

    #[test]
    fn a_chain_to_another_operation_is_not_proven() {
        let graph = chain(
            vec![
                calling("method", "a.ts", 1, 5, &[("c.ts", 1, 2)]),
                function("reader", "c.ts", 1, 4, None),
            ],
            vec![
                shaped(("a.ts", 1), ShapeKind::Value, 2, 2),
                shaped(("c.ts", 1), ShapeKind::Parsed, 2, 3),
            ],
            &[("c.ts", 3, false)],
        );
        let other = ("GET".to_string(), "/other".to_string(), String::new());
        assert!(!graph.hands_back_body(&definition("a.ts", 1), &other));
    }

    /// A blob of `service` with these definitions, calls and graph rows.
    fn wrapper_blob(
        service: &str,
        function_definitions: serde_json::Value,
        calls: serde_json::Value,
        data_calls: serde_json::Value,
    ) -> CloudRepoData {
        serde_json::from_value(serde_json::json!({
            "repo_name": "acme/repo",
            "service_name": service,
            "endpoints": [], "calls": calls, "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": function_definitions,
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc1234",
            "mount_graph": {"nodes": {}, "mounts": [], "endpoints": [], "data_calls": data_calls}
        }))
        .expect("the blob reads")
    }

    fn definition_json(
        name: &str,
        file: &str,
        first: u32,
        last: u32,
        calls: serde_json::Value,
    ) -> (String, serde_json::Value) {
        (
            name.to_string(),
            serde_json::json!({
                "name": name, "file_path": file, "node_type": "FunctionDeclaration",
                "arguments": [], "is_exported": true, "line_number": first, "end_line": last,
                "calls": calls
            }),
        )
    }

    fn summary_call(method: &str, path: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "owner": null, "key": {"protocol": "http", "method": method, "path": path},
            "params": [], "request_body": null, "response_body": null, "handler_name": null,
            "request_type": null, "response_type": null, "file_path": at,
            "resolution_source": "request_summary"
        })
    }

    /// The `sibling-wrapper` fixture's shape (carrick#2618): the app's
    /// `ItemsApi.get` and `ItemsApi.status` each call one function of the
    /// client, and the site questions stated a row at each wrapper's call;
    /// `commands.ts` calls each wrapper. The client's `getItem` request is a
    /// fact in its graph, `getStatus` is only the model's. Settled, with
    /// the client's request for `getItem` stated as `stated_items`.
    fn settle_wrappers(stated_items: &str) -> (Vec<(String, String)>, Settled) {
        let (facts, _, settled) = settle_wrappers_in_full(stated_items);
        (facts, settled)
    }

    /// [`settle_wrappers`], and the places of the app's graph rows after it:
    /// the site fold put a twin of each wrapper's row in the graph, as a call
    /// through (carrick#2795).
    fn settle_wrappers_in_full(
        stated_items: &str,
    ) -> (Vec<(String, String)>, Vec<String>, Settled) {
        let app_definitions: serde_json::Map<String, serde_json::Value> = [
            definition_json(
                "ItemsApi.get",
                "services/app/src/api.ts",
                5,
                7,
                serde_json::json!([{"name": "getItem", "file_path": "packages/client/src/http.ts", "line_number": 1, "call_site_line": 6}]),
            ),
            definition_json(
                "ItemsApi.status",
                "services/app/src/api.ts",
                9,
                11,
                serde_json::json!([{"name": "getStatus", "file_path": "packages/client/src/http.ts", "line_number": 11, "call_site_line": 10}]),
            ),
        ]
        .into_iter()
        .collect();
        let mut app = wrapper_blob(
            "app",
            serde_json::Value::Object(app_definitions),
            serde_json::json!([
                summary_call("GET", "/api/items/${id}", "services/app/src/api.ts:6"),
                summary_call("GET", "/api/status", "services/app/src/api.ts:10"),
            ]),
            serde_json::json!([
                wrapper_twin("/api/items/${id}", "services/app/src/api.ts:6"),
                wrapper_twin("/api/status", "services/app/src/api.ts:10"),
            ]),
        );
        let client_definitions: serde_json::Map<String, serde_json::Value> = [
            definition_json(
                "getItem",
                "packages/client/src/http.ts",
                1,
                4,
                serde_json::json!([]),
            ),
            definition_json(
                "getStatus",
                "packages/client/src/http.ts",
                11,
                14,
                serde_json::json!([]),
            ),
        ]
        .into_iter()
        .collect();
        let graph_row = |target: &str, canonical: &str, line: u32, source: &str| {
            serde_json::json!({
                "method": "GET", "target_url": target, "canonical_path": canonical,
                "client": "fetch", "file_location": format!("packages/client/src/http.ts:{line}"),
                "line": line, "resolution_source": source
            })
        };
        let mut client = wrapper_blob(
            "client",
            serde_json::Value::Object(client_definitions),
            serde_json::json!([]),
            serde_json::json!([
                graph_row("/api/items/${id}", "/api/items/:id", 2, stated_items),
                graph_row("/api/status", "/api/status", 12, "model"),
            ]),
        );
        let leaf = |line: u32, callee: u32, client: &str| LeafCall {
            file: "services/app/src/commands.ts".to_string(),
            line,
            last_line: line,
            enclosing_lines: Vec::new(),
            client: client.to_string(),
            callee: LeafCallee::Own(definition("services/app/src/api.ts", callee)),
        };
        let app_sites = CallThroughSites {
            leaves: vec![leaf(6, 5, "api.get"), leaf(11, 9, "api.status")],
            ..Default::default()
        };
        let client_sites = CallThroughSites::default();
        let settled = settle(&mut [
            ServiceRows {
                data: &mut app,
                sites: &app_sites,
                writable: true,
            },
            ServiceRows {
                data: &mut client,
                sites: &client_sites,
                writable: true,
            },
        ]);
        let mut facts: Vec<(String, String)> = app
            .calls
            .iter()
            .filter(|call| call.resolution_source == Some(ResolutionSource::RequestSummary))
            .map(|call| (call.file_path.display().to_string(), call.key.canonical()))
            .collect();
        facts.sort();
        let mut graph: Vec<String> = app
            .mount_graph
            .iter()
            .flat_map(|graph| graph.data_calls.iter())
            .map(|call| call.file_location.clone())
            .collect();
        graph.sort();
        (facts, graph, settled)
    }

    /// The graph twin of a wrapper's fact row: a call through, as the site
    /// fold writes it.
    fn wrapper_twin(path: &str, at: &str) -> serde_json::Value {
        serde_json::json!({
            "method": "GET", "target_url": path, "canonical_path": path,
            "client": "", "file_location": at,
            "line": at.rsplit_once(':').and_then(|(_, line)| line.parse::<u32>().ok()),
            "resolution_source": "request_summary", "role": "wrapper_call"
        })
    }

    /// carrick#2795: the graph twin of a wrapper's row goes with the row.
    #[test]
    fn a_wrapper_s_graph_twin_goes_with_its_row() {
        let (_, graph, settled) = settle_wrappers_in_full("request_summary");
        assert_eq!(settled.wrapper_rows_withdrawn, 1, "{graph:?}");
        assert_eq!(
            graph,
            [
                "services/app/src/api.ts:10".to_string(),
                "services/app/src/commands.ts:6".to_string(),
            ],
            "the withdrawn wrapper's twin is gone, the kept wrapper's stays"
        );
        let (_, graph, settled) = settle_wrappers_in_full("model");
        assert_eq!(settled.wrapper_rows_withdrawn, 0, "{graph:?}");
        assert_eq!(
            graph,
            [
                "services/app/src/api.ts:10".to_string(),
                "services/app/src/api.ts:6".to_string(),
            ],
            "no row withdrawn, no twin removed"
        );
    }

    /// carrick#2795: a fact that is a call through (`wrapper_call`) is no
    /// request of its own and no row the model left unstated, so the walk
    /// from a caller never takes the caller's own fact for the request.
    #[test]
    fn a_mirrored_call_through_fact_is_neither_a_request_nor_unstated() {
        let mut call = request_at("a.ts", 3);
        assert!(is_request_row(&call));
        call.role = Some(ConsumerRole::WrapperCall);
        assert!(!is_request_row(&call));
        assert!(!is_unstated_row(&call));
    }

    fn site_row_at(line: u32) -> SiteRow {
        SiteRow {
            file: "a.ts".to_string(),
            line,
            method: "GET".to_string(),
            target: "/stated".to_string(),
            steps: vec![Vec::new(), Vec::new(), Vec::new()],
            receiver: Vec::new(),
            callbacks: Vec::new(),
        }
    }

    fn settled_with_graph(data_calls: serde_json::Value) -> Settled {
        let mut data = wrapper_blob(
            "app",
            serde_json::json!({}),
            serde_json::json!([]),
            data_calls,
        );
        let sites = CallThroughSites {
            rows: vec![site_row_at(5)],
            ..Default::default()
        };
        settle(&mut [ServiceRows {
            data: &mut data,
            sites: &sites,
            writable: true,
        }])
    }

    /// carrick#2795: a row whose model row the site fold replaced with a fact
    /// is skipped and counted; the same row with nothing at its call, or with
    /// the model's row still in the graph, is read as before.
    #[test]
    fn a_row_a_fact_site_row_replaced_is_skipped_and_counted() {
        let stated = |source: &str| {
            serde_json::json!({
                "method": "GET", "target_url": "/stated", "canonical_path": "/stated",
                "client": "", "file_location": "a.ts:5", "line": 5,
                "resolution_source": source
            })
        };
        let replaced = settled_with_graph(serde_json::json!([stated("request_summary")]));
        assert_eq!(replaced.replaced_by_site_row, 1);
        assert_eq!(replaced.reaches_none, 0);

        let nothing_there = settled_with_graph(serde_json::json!([]));
        assert_eq!(nothing_there.replaced_by_site_row, 0);
        assert_eq!(nothing_there.reaches_none, 1);

        let model_row = settled_with_graph(serde_json::json!([stated("model")]));
        assert_eq!(model_row.replaced_by_site_row, 0);
        assert_eq!(model_row.reaches_none, 1);
    }

    /// An invented client whose transport refreshes a token before it sends:
    /// `refreshToken` (lines 1-5) writes `POST /auth/refresh` at line 3,
    /// stated as `refresh_source`; `send` (7-12) calls it at line 9;
    /// `getUser` (14) writes the request `GET /users/:id` at its call of
    /// `send`; `refreshAndLoad` (16-19) calls `refreshToken` itself, then
    /// `getUser`. The operation a caller whose own callee is the definition
    /// at `callee` reaches, or why it reaches none.
    fn reached_through_refreshing_transport(refresh_source: &str, callee: u32) -> String {
        const API: &str = "packages/client/src/api.ts";
        let call = |name: &str, line: u32, at: u32| serde_json::json!({"name": name, "file_path": API, "line_number": line, "call_site_line": at});
        let definitions: serde_json::Map<String, serde_json::Value> = [
            definition_json("refreshToken", API, 1, 5, serde_json::json!([])),
            definition_json(
                "send",
                API,
                7,
                12,
                serde_json::json!([call("refreshToken", 1, 9)]),
            ),
            definition_json(
                "getUser",
                API,
                14,
                14,
                serde_json::json!([call("send", 7, 14)]),
            ),
            definition_json(
                "refreshAndLoad",
                API,
                16,
                19,
                serde_json::json!([call("refreshToken", 1, 17), call("getUser", 14, 18)]),
            ),
        ]
        .into_iter()
        .collect();
        let row = |method: &str, path: &str, line: u32, source: &str| {
            serde_json::json!({
                "method": method, "target_url": path, "canonical_path": path,
                "client": "fetch", "file_location": format!("{API}:{line}"),
                "line": line, "resolution_source": source
            })
        };
        let client = wrapper_blob(
            "client",
            serde_json::Value::Object(definitions),
            serde_json::json!([]),
            serde_json::json!([
                row("POST", "/auth/refresh", 3, refresh_source),
                row("GET", "/users/:id", 14, "request_summary"),
            ]),
        );
        let graph = ReachGraph::of(std::iter::once(&client));
        let sites = CallThroughSites::default();
        let mut decider = Decider::new(&graph, &[&sites]);
        let mut caller = site_row_at(5);
        caller.file = "packages/app/src/page.ts".to_string();
        caller.steps[0] = vec![definition(API, callee)];
        match decider.decide(&caller) {
            Reach::One { request, .. } => format!("{} {}", request.method, request.canonical_path),
            Reach::None => "none".to_string(),
            Reach::Several => "several".to_string(),
            Reach::Unstated => "unstated".to_string(),
        }
    }

    /// carrick#2795: a fact request reached only past a request's own call
    /// (the transport's token refresh behind `getUser`'s `send`) is that
    /// transport's side call, not a second operation the caller reaches:
    /// the caller reaches `GET /users/:id` whether the refresh is a fact or
    /// the model's row. A caller that calls the refresh itself still reaches
    /// two operations, and a caller of the transport reaches the refresh.
    #[test]
    fn a_fact_past_a_requests_own_call_is_the_transports_side_call() {
        assert_eq!(
            reached_through_refreshing_transport("request_summary", 14),
            "GET /users/:id",
            "the refresh stated by a fact"
        );
        assert_eq!(
            reached_through_refreshing_transport("model", 14),
            "GET /users/:id",
            "the refresh stated by the model"
        );
        assert_eq!(
            reached_through_refreshing_transport("request_summary", 16),
            "several",
            "a caller that refreshes and then loads reaches both"
        );
        assert_eq!(
            reached_through_refreshing_transport("request_summary", 7),
            "POST /auth/refresh",
            "a caller of the transport itself reaches the refresh"
        );
    }

    /// carrick#2618, the stated side: once call-through states the leaf row
    /// at the wrapper's caller, the wrapper's own row for that operation is
    /// withdrawn, so the request keeps one fact row. A wrapper whose request
    /// no leaf row states keeps its row.
    #[test]
    fn a_wrapper_s_row_goes_once_its_caller_states_the_request() {
        let (facts, settled) = settle_wrappers("request_summary");
        assert_eq!(settled.leaf_rows, 1, "{facts:?}");
        assert_eq!(settled.wrapper_rows_withdrawn, 1, "{facts:?}");
        assert_eq!(
            facts,
            vec![
                (
                    "services/app/src/api.ts:10".to_string(),
                    OperationKey::http("GET", "/api/status").canonical()
                ),
                (
                    "services/app/src/commands.ts:6".to_string(),
                    OperationKey::http("GET", "/api/items/:id").canonical()
                ),
            ]
        );
    }

    /// carrick#2618: with no leaf row stated (the client's request is only
    /// the model's), every wrapper keeps its row.
    #[test]
    fn a_wrapper_keeps_its_row_when_no_caller_states_the_request() {
        let (facts, settled) = settle_wrappers("model");
        assert_eq!(settled.leaf_rows, 0, "{facts:?}");
        assert_eq!(settled.wrapper_rows_withdrawn, 0, "{facts:?}");
        assert_eq!(
            facts,
            vec![
                (
                    "services/app/src/api.ts:10".to_string(),
                    OperationKey::http("GET", "/api/status").canonical()
                ),
                (
                    "services/app/src/api.ts:6".to_string(),
                    OperationKey::http("GET", "/api/items/${id}").canonical()
                ),
            ]
        );
    }
}
