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
}

impl CallThroughSites {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.hands_back_body.is_empty() && self.side_calls.is_empty()
    }
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
    }
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
    /// Rows not corrected (confirmed, or left alone) whose response entry
    /// was dropped: the row's own callee is a function the repository
    /// declares that does not hand back a request's parsed body, so the
    /// call's value is that function's own result.
    pub responses_not_carried: usize,
}

/// Settle every model call row whose call reaches exactly one request
/// operation, over every service of the scan.
///
/// | Reached | The row |
/// |---|---|
/// | One operation, the row's own | stays the model's and keeps its type anchors, except a response entry its own callee does not hand back (below); gains `role: wrapper_call` and `reaches_request` |
/// | One operation, another | takes the reached row's method, target, base and dispatch value; its source becomes `request_summary`; its response type entry is keyed to the reached operation where the call hands back the request's parsed body, and goes otherwise; its request type entry goes |
/// | None, several, or one beside a request no fact states | left as it is, except a response entry its own callee does not hand back (below), and counted |
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
///
/// A row that is not corrected loses its RESPONSE entry on the same reading
/// (carrick#1801): where the row's own call resolves to a function the
/// repository declares and that function does not hand back a request's
/// parsed body unchanged, the call's value is the function's own result (a
/// copy it maps, an envelope), whatever operation the row names. A row whose
/// own callee resolves to nothing the repository declares (a library call)
/// keeps its entries: nothing here reads what that call returns.
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
    let mut decider = Decider::new(&graph, &sites);
    let mut settled = Settled::default();
    for service in services.iter_mut().filter(|service| service.writable) {
        let mut corrected = 0;
        for row in &service.sites.rows {
            // What the row's own call returns: nothing here reads it where
            // the callee resolves to no definition of the repository; the
            // request's parsed body where every definition it goes through
            // hands that body back unchanged (carrick#1601); the function's
            // own result anywhere else.
            let callee = row.steps.first().filter(|callee| !callee.is_empty());
            let callee_hands_back = callee.is_some_and(|callee| {
                callee
                    .iter()
                    .all(|definition| hands_back_body.contains(definition))
            });
            let returns_own_result = callee.is_some() && !callee_hands_back;
            let left_alone = match decider.decide(row) {
                Reach::None => {
                    settled.reaches_none += 1;
                    true
                }
                Reach::Several => {
                    settled.reaches_several += 1;
                    true
                }
                Reach::Unstated => {
                    settled.reaches_unstated += 1;
                    true
                }
                Reach::One {
                    request,
                    by_own_callee,
                } => match apply(
                    service.data,
                    row,
                    request,
                    // The row's call is the one that reaches the request,
                    // and it hands back the request's parsed body.
                    by_own_callee && callee_hands_back,
                    returns_own_result,
                ) {
                    Some(Change::Relabelled { response_dropped }) => {
                        settled.relabelled += 1;
                        settled.responses_not_carried += usize::from(response_dropped);
                        false
                    }
                    Some(Change::Corrected {
                        types_dropped,
                        request_dropped,
                    }) => {
                        corrected += 1;
                        settled.types_not_carried += usize::from(types_dropped);
                        settled.requests_not_carried += usize::from(request_dropped);
                        false
                    }
                    None => false,
                },
            };
            if left_alone
                && returns_own_result
                && let Some((method, path)) = model_row_key(service.data, row)
                && drop_response(service.data, row, &method, &path)
            {
                settled.responses_not_carried += 1;
            }
        }
        settled.corrected += corrected;
        if corrected > 0
            && let Some(mut boundary) = service.data.boundary.take()
        {
            boundary.fold_settled_calls(service.data, corrected);
            service.data.boundary = Some(boundary);
        }
    }
    debug!(
        "Call-through rows settled: {} confirmed, {} corrected ({} lost the types anchored at a call that is not the request's, {} kept the response type and lost the request type); left alone: {} reach no request row, {} reach several operations, {} reach a request only the model states (carrick#1146); {} confirmed or left alone lost a response type their callee does not hand back (carrick#1801)",
        settled.relabelled,
        settled.corrected,
        settled.types_not_carried,
        settled.requests_not_carried,
        settled.reaches_none,
        settled.reaches_several,
        settled.reaches_unstated,
        settled.responses_not_carried
    );
    settled
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
    (
        request.method.to_uppercase(),
        request.canonical_path.clone(),
        crate::dispatch::Dispatch::key_suffix(request.dispatch.as_ref()),
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
            for (first, last, _) in definitions {
                let callees = graph
                    .calls
                    .entry(DefinitionSite {
                        file: file.clone(),
                        line: *first,
                    })
                    .or_default();
                for (inner_first, inner_last, inner_calls) in definitions {
                    if first <= inner_first && inner_last <= last {
                        callees.extend(inner_calls.iter().cloned());
                    }
                }
                callees.sort();
                callees.dedup();
            }
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
    Relabelled {
        /// The row's response entry was dropped: its own callee does not
        /// hand back the request's parsed body.
        response_dropped: bool,
    },
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
/// unchanged, so the response type anchored there is the request's.
/// `returns_own_result`: the row's own callee is a function the repository
/// declares that does not hand that body back, so a confirmed row's response
/// entry goes too. `None`
/// when the blob holds no such row: the graph build keeps fewer rows than the
/// analysis stated.
fn apply(
    data: &mut CloudRepoData,
    row: &SiteRow,
    reached: &Reached,
    carries_response: bool,
    returns_own_result: bool,
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
        let (method, path) = (call.method.clone(), call.canonical_path.clone());
        let response_dropped = returns_own_result && drop_response(data, row, &method, &path);
        return Some(Change::Relabelled { response_dropped });
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
    let own_route = graph
        .find_matching_endpoints(&reached.canonical_path, &reached.method)
        .iter()
        .any(|endpoint| {
            endpoint.evidence == carrick_match::MatchEvidence::RouteDefinition
                && carrick_match::match_agreement(&endpoint.full_path, &reached.canonical_path)
                    .unwrap_or(0)
                    > 0
        });
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

/// The method and canonical path of the model row `row` names in `data`'s
/// graph, as its type entries are keyed.
fn model_row_key(data: &CloudRepoData, row: &SiteRow) -> Option<(String, String)> {
    let location = format!("{}:{}", row.file, row.line);
    data.mount_graph
        .as_ref()?
        .data_calls
        .iter()
        .find(|call| {
            call.file_location == location
                && call.method == row.method
                && call.target_url == row.target
                && call.resolution_source == Some(ResolutionSource::Model)
                && call.reaches_request.is_none()
        })
        .map(|call| (call.method.clone(), call.canonical_path.clone()))
}

/// Drop the consumer response entry anchored at `row`'s site under the
/// operation `method` `path`, and the response type its projection carries:
/// the call's value is a function's own result, not a response body
/// (carrick#1801). The request entry stays. Whether an entry went.
fn drop_response(data: &mut CloudRepoData, row: &SiteRow, method: &str, path: &str) -> bool {
    let location = format!("{}:{}", row.file, row.line);
    let key = OperationKey::http(method, path.to_string());
    if let Some(projected) = data.calls.iter_mut().find(|operation| {
        operation.key == key
            && operation.resolution_source == Some(ResolutionSource::Model)
            && operation.file_path == Path::new(&location)
    }) {
        projected.response_type = None;
    }
    let Some(manifest) = data.type_manifest.as_mut() else {
        return false;
    };
    let method = normalize_manifest_method(method);
    let before = manifest.len();
    manifest.retain(|entry| {
        !(entry.role == ManifestRole::Consumer
            && entry.type_kind == ManifestTypeKind::Response
            && entry.file_path == row.file
            && entry.line_number == row.line
            && entry
                .key
                .as_http()
                .is_some_and(|(entry_method, entry_path)| {
                    entry_method == method && entry_path == path
                }))
    });
    manifest.len() < before
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
}
