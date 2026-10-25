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
use crate::request_summary::RequestSummaryInputs;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::type_manifest::normalize_manifest_method;
use crate::wrapper_request_shape::{RequestShapeSignal, call_request_verb};

/// How many call edges the walk follows from the definition a site reaches.
const MAX_EDGE_DEPTH: usize = 4;

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
}

/// One service's model call rows at sites that write no request of their own.
#[derive(Debug, Default, Clone)]
pub struct CallThroughSites {
    pub rows: Vec<SiteRow>,
    /// The service's definitions whose call is worth their request's parsed
    /// body unchanged ([`crate::request_summary::RequestSummaryIndex::passes_body`],
    /// carrick#1601): every `return` hands back the body one request answers
    /// with, or a call of a function that does.
    pub hands_back_body: HashSet<DefinitionSite>,
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
        self.rows.is_empty() && self.hands_back_body.is_empty() && self.side_calls.is_empty()
    }
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
    pub hands_back_body: Vec<DefinitionSite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub side_calls: Vec<StoredSideCall>,
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
        let mut hands_back_body: Vec<DefinitionSite> =
            sites.hands_back_body.iter().cloned().collect();
        hands_back_body.sort();
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
            hands_back_body,
            side_calls,
        })
    }

    pub fn sites(&self) -> CallThroughSites {
        CallThroughSites {
            leaves: Vec::new(),
            rows: self.rows.clone(),
            hands_back_body: self.hands_back_body.iter().cloned().collect(),
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
/// `repo_path`. `passes_body` answers, for a declaring module's canonical
/// path and a function's name, whether a call of it is worth its request's
/// parsed body; it is read for the service's own `definitions`.
pub fn read_sites(
    file_results: &HashMap<String, FileAnalysisResult>,
    inputs: &RequestSummaryInputs,
    repo_path: &str,
    definitions: &HashMap<String, crate::visitor::FunctionDefinition>,
    passes_body: &dyn Fn(&Path, &str) -> bool,
) -> CallThroughSites {
    let root = repo_path.trim_end_matches('/');
    let hands_back_body = hands_back_body(definitions, root, passes_body);
    let side_calls = read_side_calls(file_results, inputs, root);
    let leaves = read_leaves(inputs, root, definitions);
    let mut keys: Vec<&String> = file_results
        .iter()
        .filter(|(_, result)| result.data_calls.iter().any(is_model_row))
        .map(|(key, _)| key)
        .collect();
    keys.sort();
    if keys.is_empty() {
        return CallThroughSites {
            rows: Vec::new(),
            hands_back_body,
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
        // definition the call reaches.
        let resolved: HashMap<(u32, u32), DefinitionSite> = inputs
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
        let relative = crate::engine::repo_relative(key, root);
        rows.extend(read_file_sites(
            &absolute,
            &relative,
            &file_results[key],
            &resolved,
        ));
    }
    CallThroughSites {
        rows,
        hands_back_body,
        side_calls,
        leaves,
    }
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
    definitions: &HashMap<String, crate::visitor::FunctionDefinition>,
) -> Vec<LeafCall> {
    let mut lines: HashMap<(String, &str), Vec<u32>> = HashMap::new();
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
/// function it is written in ([`SideCall`]).
fn read_side_calls(
    file_results: &HashMap<String, FileAnalysisResult>,
    inputs: &RequestSummaryInputs,
    root: &str,
) -> HashMap<SideKey, SideCall> {
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
        if resolved.is_empty() {
            continue;
        }
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let Some(module) = parse_file(path, &cm, &handler) else {
            continue;
        };
        let positions = Positions { cm: &cm };
        let relative = crate::engine::repo_relative(&path.to_string_lossy(), root);
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
    side_calls
}

/// The service's definitions a call of which is worth the request's parsed
/// body unchanged, named as every service names a definition.
fn hands_back_body(
    definitions: &HashMap<String, crate::visitor::FunctionDefinition>,
    root: &str,
    passes_body: &dyn Fn(&Path, &str) -> bool,
) -> HashSet<DefinitionSite> {
    definitions
        .values()
        .filter(|definition| definition.name != crate::visitor::MODULE_SCOPE_KEY)
        .filter_map(|definition| {
            let file = definition.file_path.to_string_lossy();
            let absolute = if definition.file_path.is_absolute() {
                definition.file_path.clone()
            } else {
                Path::new(root).join(&definition.file_path)
            };
            let canonical = absolute.canonicalize().unwrap_or(absolute);
            passes_body(&canonical, &definition.name).then(|| DefinitionSite {
                file: crate::engine::repo_relative(&file, root),
                line: definition.line_number,
            })
        })
        .collect()
}

/// A row this pass may settle: the model's own reading, with nothing linking
/// it to a request already.
fn is_model_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model) && call.reaches_request.is_none()
}

fn read_file_sites(
    absolute: &Path,
    relative: &str,
    result: &FileAnalysisResult,
    resolved: &HashMap<(u32, u32), DefinitionSite>,
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
        let reached = |spans: &[(u32, u32)]| -> Vec<DefinitionSite> {
            let mut definitions: Vec<DefinitionSite> = spans
                .iter()
                .filter_map(|span| resolved.get(span).cloned())
                .collect();
            definitions.sort();
            definitions.dedup();
            definitions
        };
        rows.push(SiteRow {
            file: relative.to_string(),
            line,
            method,
            target: call.target.clone(),
            steps: vec![
                reached(&[extent.span()]),
                reached(&site.spread_calls),
                reached(&site.callback_calls),
            ],
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
        let (lo, hi) = row.candidate_id.strip_prefix("span:")?.split_once('-')?;
        let span = (lo.parse::<u32>().ok()?, hi.parse::<u32>().ok()?);
        let line = u32::try_from(row.line_number).ok()?;
        self.calls
            .iter()
            .find(|call| call.span() == span && (call.line..=call.last_line).contains(&line))
            .map(|call| call.lo)
    }
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
    /// The calls in the body of the function-valued argument, when the
    /// expression holds exactly one, passed directly or as a property of an
    /// object-literal argument. Empty when it holds two or more: nothing says
    /// which of them makes the request.
    callback_calls: Vec<(u32, u32)>,
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
        let callback_calls = match callbacks.len() {
            1 => callbacks.remove(0),
            _ => Vec::new(),
        };
        self.read.insert(
            key,
            SiteReading {
                writes_a_request: writer.found,
                spread_calls,
                callback_calls,
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
        self.read_site(node.span, node, &node.args);
        node.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, node: &OptCall) {
        self.read_site(node.span, node, &node.args);
        node.visit_children_with(self);
    }
}

/// The span of the call an expression is, through `await` and the wrappers
/// that pass a value along.
fn call_span(expr: &Expr) -> Option<Span> {
    match unwrap_expression(expr) {
        Expr::Call(call) => Some(call.span),
        Expr::Await(awaited) => call_span(&awaited.arg),
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
/// decided) and that callee hands back the request's parsed body unchanged
/// (the service's request summaries say so, carrick#1601): only then is what
/// the call returns what the caller reads of the request, so the pair is
/// judged against the reached operation's producer. Its request entry always
/// goes: the call's arguments are the called function's parameters, not a
/// request body. Anywhere else (a spread call or a callback decided, or the
/// callee returns an envelope, a mapped object, or a value nothing here
/// reads) the anchor names a function's own result, and the entries are
/// dropped and counted.
pub fn settle(services: &mut [ServiceRows<'_>]) -> Settled {
    let mut graph = ReachGraph::of(services.iter().map(|service| &*service.data));
    let sites: Vec<&CallThroughSites> = services.iter().map(|service| service.sites).collect();
    graph.side_calls = sites
        .iter()
        .flat_map(|sites| sites.side_calls.iter())
        .map(|(key, side)| (key.clone(), side.clone()))
        .collect();
    let hands_back_body: HashSet<&DefinitionSite> = sites
        .iter()
        .flat_map(|sites| &sites.hands_back_body)
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
            match decider.decide(row) {
                Reach::None => settled.reaches_none += 1,
                Reach::Several => settled.reaches_several += 1,
                Reach::Unstated => settled.reaches_unstated += 1,
                Reach::One {
                    request,
                    by_own_callee,
                } => match apply(
                    service.data,
                    row,
                    request,
                    // What the row's call returns is the request's parsed
                    // body only where its own callee decided and every
                    // definition that callee goes through hands that body
                    // back unchanged (carrick#1601).
                    by_own_callee
                        && row.steps.first().is_some_and(|callee| {
                            !callee.is_empty()
                                && callee
                                    .iter()
                                    .all(|definition| hands_back_body.contains(definition))
                        }),
                ) {
                    Some(Change::Relabelled) => settled.relabelled += 1,
                    Some(Change::Corrected {
                        types_dropped,
                        request_dropped,
                    }) => {
                        corrected += 1;
                        settled.types_not_carried += usize::from(types_dropped);
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
        if (corrected > 0 || stated.own + stated.boundary > 0)
            && let Some(mut boundary) = service.data.boundary.take()
        {
            boundary.fold_settled_calls(service.data, corrected);
            service.data.boundary = Some(boundary);
        }
    }
    debug!(
        "Call-through rows settled: {} confirmed, {} corrected ({} lost the types anchored at a call that is not the request's, {} kept the response type and lost the request type); left alone: {} reach no request row, {} reach several operations, {} reach a request only the model states (carrick#1146)",
        settled.relabelled,
        settled.corrected,
        settled.types_not_carried,
        settled.requests_not_carried,
        settled.reaches_none,
        settled.reaches_several,
        settled.reaches_unstated
    );
    debug!(
        "Call-through leaf rows stated: {} at calls into a function of the caller's own service that calls into another service and reaches exactly one request there (carrick#1146), and {} at calls straight into a function another service declares (carrick#2773); boundary calls left alone: {} inside a function whose callers state the row, {} whose function reaches a request a third service writes, {} whose function no one definition names",
        settled.leaf_rows - settled.boundary_rows,
        settled.boundary_rows,
        settled.boundary_in_wrapper,
        settled.boundary_third_service,
        settled.boundary_unresolved
    );
    settled
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
/// the call.
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
    outcome
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

/// Whether the service serves the route `reached` requests itself.
fn serves_own_route(graph: &crate::mount_graph::MountGraph, reached: &Reached) -> bool {
    graph
        .find_matching_endpoints(&reached.canonical_path, &reached.method)
        .iter()
        .any(|endpoint| {
            endpoint.evidence == carrick_match::MatchEvidence::RouteDefinition
                && carrick_match::match_agreement(&endpoint.full_path, &reached.canonical_path)
                    .unwrap_or(0)
                    > 0
        })
}

/// Write the row `leaf` states for `reached` into the graph and the calls.
fn state_leaf(data: &mut CloudRepoData, leaf: &LeafCall, reached: &Reached) -> bool {
    let Some(graph) = data.mount_graph.as_mut() else {
        return false;
    };
    let own_route = serves_own_route(graph, reached);
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
}

enum Reach<'g> {
    None,
    Several,
    /// The call also reaches a request no fact states.
    Unstated,
    One {
        request: &'g Reached,
        /// The row's own callee reached it, not a spread call or a callback.
        by_own_callee: bool,
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
    /// calls: a row no fact states past it is that request's, not another.
    /// Every other path counts the rows it reaches. A definition two paths
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
            for request in self.requests.get(definition).into_iter().flatten() {
                let entry = reached
                    .entry(request.site.as_str())
                    .or_insert((request, side));
                entry.1 &= side;
            }
            if !within_request {
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
    fn decide(&mut self, row: &SiteRow) -> Reach<'g> {
        let graph = self.graph;
        for (position, step) in row.steps.iter().enumerate() {
            // By site, so a request two paths reach is one request.
            let mut reached: BTreeMap<&'g str, (&'g Reached, bool)> = BTreeMap::new();
            let mut unstated: BTreeMap<&'g str, (Vec<&'g Unstated>, bool)> = BTreeMap::new();
            for definition in step {
                graph.walk(definition, &mut reached, &mut unstated);
            }
            // A site a fact states is that fact's request, and the row's own
            // site is the call being decided.
            let own_site = format!("{}:{}", row.file, row.line);
            unstated.retain(|site, _| !reached.contains_key(site) && *site != own_site);
            if reached.is_empty() && unstated.is_empty() {
                continue;
            }
            let every = self.judge(&reached, &unstated, false);
            if matches!(every, Reach::One { .. }) {
                return self.placed(every, position);
            }
            let side =
                reached.values().any(|(_, side)| *side) || unstated.values().any(|(_, side)| *side);
            if !side {
                return every;
            }
            let returned = self.judge(&reached, &unstated, true);
            return match returned {
                Reach::One { .. } => self.placed(returned, position),
                Reach::None => every,
                other => other,
            };
        }
        Reach::None
    }

    /// `reach` with whether the row's own callee decided.
    fn placed(&self, reach: Reach<'g>, position: usize) -> Reach<'g> {
        match reach {
            Reach::One { request, .. } => Reach::One {
                request,
                by_own_callee: position == 0,
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
            by_own_callee: false,
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
        /// The row kept its response entry and its request entry was
        /// dropped: a call's arguments are not a request body.
        request_dropped: bool,
    },
}

/// Change the one row of `data` that `row` names to what `reached` states.
/// `carries_response`: the row's call is worth the request's parsed body
/// unchanged, so the response type anchored there is the request's. `None`
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
    let own_route = serves_own_route(graph, reached);
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

    // The types anchored at the site were captured under the operation the
    // model stated. Where the row's call hands back the request's parsed body
    // unchanged, what it returns is what the caller reads of that request:
    // the entries keep their captured types (the alias names the site, not
    // the operation) and take the reached operation's key, so the
    // cross-service check judges them against its producer. Anywhere else
    // the call's value is a function's own result (an envelope, a mapped
    // object, a hook's state), as at any call #1601 restates at a caller, or
    // the site already holds a row for the reached operation: they go.
    let mut types_dropped = false;
    let mut request_dropped = false;
    if let (Some(manifest), Some((method, path))) = (data.type_manifest.as_mut(), before.as_http())
    {
        let method = normalize_manifest_method(method);
        let at_the_row = |entry: &crate::cloud_storage::TypeManifestEntry| {
            entry.role == ManifestRole::Consumer
                && entry.file_path == row.file
                && entry.line_number == row.line
                && entry
                    .key
                    .as_http()
                    .is_some_and(|(entry_method, entry_path)| {
                        entry_method == method && entry_path == path
                    })
        };
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
            types_dropped = !twin && manifest.len() < before;
        }
    }
    Some(Change::Corrected {
        types_dropped,
        request_dropped,
    })
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
            let bodies = [definition("b.ts", 9), definition("b.ts", 1)];
            CallThroughSites {
                rows: vec![SiteRow {
                    file: "a.ts".to_string(),
                    line: 7,
                    method: "GET".to_string(),
                    target: "/getRun".to_string(),
                    steps: vec![vec![definition("b.ts", 9)], vec![]],
                }],
                hands_back_body: order.iter().map(|&i| bodies[i].clone()).collect(),
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
        assert_eq!(back.hands_back_body, sites.hands_back_body);
        assert_eq!(back.side_calls, sites.side_calls);
        assert_eq!(
            serde_json::to_string(&StoredCallThrough::of(&facts(&[1, 0])).unwrap()).unwrap(),
            written
        );
        assert_eq!(StoredCallThrough::of(&CallThroughSites::default()), None);
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
        .collect()
    }

    /// The rows [`read_file_sites`] reads for `rows` over [`SOURCE`], as
    /// (line, steps).
    fn read(rows: Vec<DataCallResult>) -> Vec<(u32, Vec<Vec<DefinitionSite>>)> {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("load.ts");
        std::fs::write(&file, SOURCE).expect("the source is written");
        let result = FileAnalysisResult {
            data_calls: rows,
            ..Default::default()
        };
        read_file_sites(&file, "src/load.ts", &result, &resolved())
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

    /// Nothing says which of two callbacks makes the request.
    #[test]
    fn two_callbacks_state_no_step() {
        assert_eq!(
            read(vec![model_row(5)]),
            [(5, vec![vec![], vec![], vec![]])]
        );
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
}
