//! The compiler engine's pass over the pairs a model placed after the scan.
//!
//! A model finds which call reaches which server code for the calls the scan
//! did not pair, and the cloud runner stores them as one `carrick.matched/1`
//! record (`tests/fixtures/matched-record/README.md` pins its shape). This
//! pass reads that record and the repository at the record's commit, and
//! writes the record back with what the compiler engine says of each pair:
//!
//! - `sides.<side>.compiler` on the four sides of every pair. The response
//!   sides (`caller_expects`, `server_returns`) are read by the scan's own
//!   type stage ([`crate::engine::matched_types`]); the request sides are not
//!   read in this build and say `request_side_not_read`, so a model's
//!   reading can fill them.
//! - `verdicts.compiler`: the one judge's verdict, or `cannot_tell` with the
//!   reason it was not judged in `detail`. A verdict never gates.
//! - `wrapper`: where the caller's call sits inside a function of the
//!   repository's own code, that function and the calls of it the compiler's
//!   position rows name, one level up.
//! - the top-level `engine` block.
//!
//! Which kinds are read ([`reading_of`]): a `route` and a `function` (a call
//! to a deployed function) at both ends, judged; a `table` and a
//! `database_function` at the caller only, never judged; a `graphql_field`,
//! `procedure`, `event`, `topic`, `queue` or `other` pair at neither end in
//! this build, each end saying `protocol_not_read`.
//!
//! Nothing else in the record is changed. The pass writes no blob, rows file
//! or store entry: those are read, never rewritten. The compiler processes
//! it starts may leave their own caches under the repository root, as they
//! do in a scan (a Deno project's dependency cache). It runs in the cloud
//! runner after the model step and in the offline harness, never in a local
//! check.
//!
//! What decides each end:
//!
//! - **Which service reads a file** ([`owner_of`]): the deepest service whose
//!   directory holds it, else the first in `carrick.json` order whose
//!   `include` holds it, else the first sharing the longest leading path.
//! - **Which call the caller's line names** ([`locate_call`]): the outermost
//!   call, `new` or tagged template that starts on the line; a `.then`,
//!   `.catch` or `.finally` on a call is read as that call. A line two pairs
//!   name with different operations names no one call: both say
//!   `several_sites_on_line`.
//! - **Whether the caller's call goes through the repository's own code**:
//!   the compiler's `call` row at the call names the callee's declaration;
//!   a callee in the repository's own source is judged only when it hands
//!   back the parsed body ([`crate::call_through::OwnCode`]).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use swc_common::{
    BytePos, SourceMap, Span, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    CallExpr, Callee, ClassMethod, ClassProp, Expr, FnDecl, KeyValueProp, MemberProp, MethodProp,
    NewExpr, OptCall, Pat, PrivateMethod, PropName, TaggedTpl, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::warn;

use crate::call_through::OwnCode;
use crate::cloud_storage::CloudRepoData;
use crate::config::Config;
use crate::engine::matched_types::{
    MatchedEnd, MatchedEndTypes, MatchedPair, MatchedPairTypes, MatchedService, MatchedVerdict,
    type_matched_pairs,
};
use crate::position_rows::{FileRows, Kind, POSITIONS_DIR, Row};
use crate::services::type_sidecar::VerdictBucket;
use crate::swc_scanner::SWC_SPAN_BASE;

/// The command the cloud runner and the offline harness run:
/// `carrick __matched-pass <record> --repo <dir> --out <file> [--store <dir>]
/// [--ceiling-secs <n>]`. Internal, so it is listed in no help text.
pub const COMMAND: &str = "__matched-pass";

/// How long the pass may run before a service not started yet is skipped
/// (each of its ends says `budget_exhausted`): its share of the runner's
/// window after the model step.
pub const CEILING: Duration = Duration::from_secs(540);

/// The record's word for a request side, which this build does not read.
pub const REQUEST_SIDE_NOT_READ: &str = "request_side_not_read";

/// Wrapper reasons: the caller's call sits in no function of its file; no
/// rows file holds the repository's calls; the caller's call was not found.
const NOT_IN_WRAPPER: &str = "not_in_wrapper";
const ROWS_MISSING: &str = "rows_missing";
const NOT_LOCATED: &str = "not_located";
const SEVERAL_SITES_ON_LINE: &str = "several_sites_on_line";

/// A record's type text is at most this many UTF-8 bytes, and a verdict's
/// `detail` at most this many UTF-16 units: the cloud refuses the whole record
/// past either (`lambdas/_shared/matched_record_core.ts` in carrick-cloud).
const MAX_TYPE_TEXT_BYTES: usize = 16_384;
const MAX_DETAIL_UNITS: usize = 500;
const MAX_WRAPPER_CALLERS: usize = 200;

/// Why a caller's verdict was skipped when the compiler's rows do not say
/// where its call goes: the own-code sentence would claim more than is known.
pub(crate) const CALLEE_NOT_READ: &str = "the compiler's rows do not say which function the call reaches, so whether it hands back the response unchanged is not known and the pair was not judged";

/// Why a pair whose server is a table or a database function is not judged.
pub(crate) const SERVER_NOT_READ: &str = "the server end is a table or a database function, which the compiler engine does not read as a response body, so the pair was not judged";

/// How the engine reads a pair of one `operation.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Both response sides are read and the pair is judged: a route, and a
    /// call to a deployed function (HTTP underneath, a TypeScript handler).
    BothEnds,
    /// Only the caller's response side is read; the server end and the
    /// verdict are not: a table or a database function.
    CallerOnly,
    /// Neither end is read in this build: a GraphQL field, a procedure, an
    /// event, a topic, a queue, or any other kind. Each says
    /// `protocol_not_read`.
    Neither,
}

pub(crate) fn reading_of(kind: &str) -> Reading {
    match kind {
        "route" | "function" => Reading::BothEnds,
        "table" | "database_function" => Reading::CallerOnly,
        _ => Reading::Neither,
    }
}

/// How a file's service was decided ([`owner_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Directory,
    Include,
    Nearest,
}

fn trim_dir(dir: &str) -> &str {
    let dir = dir.trim_matches('/');
    let dir = dir.strip_prefix("./").unwrap_or(dir);
    if dir == "." { "" } else { dir }
}

fn holds(dir: &str, file: &str) -> bool {
    dir.is_empty() || file == dir || file.starts_with(&format!("{dir}/"))
}

/// The service whose program reads `file` (repo-relative): the deepest whose
/// `directory` holds it; else the first, in `carrick.json` order, whose
/// `include` holds it; else the first sharing the longest leading path with
/// it. `None` only with no service at all.
pub(crate) fn owner_of(file: &str, services: &[Config]) -> Option<(usize, Owner)> {
    let by_directory = services
        .iter()
        .enumerate()
        .filter_map(|(index, service)| {
            let dir = trim_dir(service.directory.as_deref().unwrap_or(""));
            holds(dir, file).then_some((index, dir.len()))
        })
        // The deepest; on a tie, the first declared.
        .min_by_key(|(index, depth)| (std::cmp::Reverse(*depth), *index));
    if let Some((index, _)) = by_directory {
        return Some((index, Owner::Directory));
    }
    if let Some(index) = services.iter().position(|service| {
        service
            .include
            .iter()
            .any(|include| holds(trim_dir(include), file))
    }) {
        return Some((index, Owner::Include));
    }
    let segments: Vec<&str> = file.split('/').collect();
    services
        .iter()
        .enumerate()
        .map(|(index, service)| {
            let dir = trim_dir(service.directory.as_deref().unwrap_or(""));
            let shared = dir
                .split('/')
                .zip(&segments)
                .take_while(|(a, b)| a == *b)
                .count();
            (index, shared)
        })
        .min_by_key(|(index, shared)| (std::cmp::Reverse(*shared), *index))
        .map(|(index, _)| (index, Owner::Nearest))
}

/// One call, `new` or tagged template of a file, in SWC positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Site {
    lo: u32,
    hi: u32,
    line: u32,
    /// For `X.then(..)`, `X.catch(..)` or `X.finally(..)` where `X` is a
    /// call: `X`'s span.
    peel: Option<(u32, u32)>,
}

impl Site {
    fn contains(&self, other: &Site) -> bool {
        self.lo <= other.lo && other.hi <= self.hi && (self.lo, self.hi) != (other.lo, other.hi)
    }
}

/// A function of a file a call can resolve to: where it runs, and the lines
/// its declaration is written on (the node's first line and its name's).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Definition {
    lo: u32,
    hi: u32,
    name: String,
    start_line: u32,
    name_line: u32,
}

struct FileSites<'a> {
    cm: &'a Lrc<SourceMap>,
    sites: Vec<Site>,
    definitions: Vec<Definition>,
}

impl FileSites<'_> {
    fn offset(&self, pos: BytePos) -> u32 {
        self.cm.lookup_byte_offset(pos).pos.0 + SWC_SPAN_BASE
    }

    fn line(&self, pos: BytePos) -> u32 {
        self.cm.lookup_char_pos(pos).line as u32
    }

    fn site(&mut self, span: Span, peel: Option<Span>) {
        let site = Site {
            lo: self.offset(span.lo),
            hi: self.offset(span.hi),
            line: self.line(span.lo),
            peel: peel.map(|span| (self.offset(span.lo), self.offset(span.hi))),
        };
        self.sites.push(site);
    }

    fn definition(&mut self, body: Span, start: BytePos, name: String, name_at: BytePos) {
        let definition = Definition {
            lo: self.offset(body.lo),
            hi: self.offset(body.hi),
            name,
            start_line: self.line(start),
            name_line: self.line(name_at),
        };
        self.definitions.push(definition);
    }
}

fn unwrap(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap(&inner.expr),
        Expr::TsAs(inner) => unwrap(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap(&inner.expr),
        Expr::TsNonNull(inner) => unwrap(&inner.expr),
        Expr::TsTypeAssertion(inner) => unwrap(&inner.expr),
        Expr::TsConstAssertion(inner) => unwrap(&inner.expr),
        other => other,
    }
}

fn is_function(expr: &Expr) -> bool {
    matches!(unwrap(expr), Expr::Arrow(_) | Expr::Fn(_))
}

fn is_call(expr: &Expr) -> bool {
    matches!(
        unwrap(expr),
        Expr::Call(_) | Expr::New(_) | Expr::TaggedTpl(_) | Expr::OptChain(_)
    )
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(text) => Some(text.value.to_string_lossy().into_owned()),
        _ => None,
    }
}

impl Visit for FileSites<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        let peel = match &node.callee {
            Callee::Expr(callee) => match unwrap(callee) {
                Expr::Member(member)
                    if matches!(&member.prop, MemberProp::Ident(prop)
                        if matches!(prop.sym.as_ref(), "then" | "catch" | "finally"))
                        && is_call(&member.obj) =>
                {
                    Some(unwrap(&member.obj).span())
                }
                _ => None,
            },
            _ => None,
        };
        self.site(node.span, peel);
        node.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, node: &OptCall) {
        self.site(node.span, None);
        node.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, node: &NewExpr) {
        self.site(node.span, None);
        node.visit_children_with(self);
    }

    fn visit_tagged_tpl(&mut self, node: &TaggedTpl) {
        self.site(node.span, None);
        node.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let start = node.ident.span.lo.min(node.function.span.lo);
        self.definition(
            node.function.span,
            start,
            node.ident.sym.to_string(),
            node.ident.span.lo,
        );
        node.visit_children_with(self);
    }

    fn visit_class_method(&mut self, node: &ClassMethod) {
        if let Some(name) = prop_name(&node.key) {
            self.definition(node.span, node.span.lo, name, node.key.span().lo);
        }
        node.visit_children_with(self);
    }

    fn visit_private_method(&mut self, node: &PrivateMethod) {
        let name = format!("#{}", node.key.name);
        self.definition(node.span, node.span.lo, name, node.key.span.lo);
        node.visit_children_with(self);
    }

    fn visit_method_prop(&mut self, node: &MethodProp) {
        if let Some(name) = prop_name(&node.key) {
            let start = node.key.span().lo;
            self.definition(node.function.span, start, name, start);
        }
        node.visit_children_with(self);
    }

    fn visit_key_value_prop(&mut self, node: &KeyValueProp) {
        if is_function(&node.value)
            && let Some(name) = prop_name(&node.key)
        {
            let start = node.key.span().lo;
            self.definition(unwrap(&node.value).span(), start, name, start);
        }
        node.visit_children_with(self);
    }

    fn visit_class_prop(&mut self, node: &ClassProp) {
        if let Some(value) = node.value.as_deref()
            && is_function(value)
            && let Some(name) = prop_name(&node.key)
        {
            self.definition(unwrap(value).span(), node.span.lo, name, node.key.span().lo);
        }
        node.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let (Pat::Ident(binding), Some(init)) = (&node.name, node.init.as_deref()) {
            let name = binding.id.sym.to_string();
            let body = match unwrap(init) {
                function @ (Expr::Arrow(_) | Expr::Fn(_)) => Some(function.span()),
                // `const load = useCallback(async () => .., [])`: the
                // function handed first to the call that makes the value.
                Expr::Call(call) => call
                    .args
                    .first()
                    .filter(|arg| arg.spread.is_none() && is_function(&arg.expr))
                    .map(|arg| unwrap(&arg.expr).span()),
                _ => None,
            };
            if let Some(body) = body {
                self.definition(body, node.span.lo, name, binding.id.span.lo);
            }
        }
        node.visit_children_with(self);
    }
}

/// A file's calls and functions, as the scanner's parser reads it. `None` for
/// a file that does not parse.
fn read_file_sites(path: &Path) -> Option<(Vec<Site>, Vec<Definition>)> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let module = crate::parser::parse_file(path, &cm, &handler)?;
    let mut sites = FileSites {
        cm: &cm,
        sites: Vec::new(),
        definitions: Vec::new(),
    };
    module.visit_with(&mut sites);
    Some((sites.sites, sites.definitions))
}

/// Which call a caller's line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Located {
    /// The call, in SWC positions.
    Call { lo: u32, hi: u32 },
    /// No call starts on the line.
    NoCall,
    /// More than one outermost call starts on the line.
    Several,
}

/// The call `line` names among `sites`: the outermost starting on it, read
/// through `.then`, `.catch` and `.finally` to the call they are chained on.
fn locate_call(sites: &[Site], line: u32) -> Located {
    let on_line: Vec<&Site> = sites.iter().filter(|site| site.line == line).collect();
    let outermost: Vec<&Site> = on_line
        .iter()
        .copied()
        .filter(|site| !on_line.iter().any(|outer| outer.contains(site)))
        .collect();
    let mut chosen = match outermost.as_slice() {
        [] => return Located::NoCall,
        [one] => **one,
        _ => return Located::Several,
    };
    while let Some((lo, hi)) = chosen.peel {
        match sites.iter().find(|site| (site.lo, site.hi) == (lo, hi)) {
            Some(inner) => chosen = *inner,
            None => break,
        }
    }
    Located::Call {
        lo: chosen.lo,
        hi: chosen.hi,
    }
}

/// The innermost function of `definitions` the call at `lo..hi` runs in.
fn enclosing(definitions: &[Definition], lo: u32, hi: u32) -> Option<&Definition> {
    definitions
        .iter()
        .filter(|definition| definition.lo <= lo && hi <= definition.hi)
        .min_by_key(|definition| definition.hi - definition.lo)
}

/// The line a UTF-16 offset of `content` falls on, as the compiler counts
/// both (a leading BOM dropped).
fn line_of_utf16(content: &str, offset: u32) -> u32 {
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut units = 0u32;
    let mut line = 1u32;
    for character in body.chars() {
        if units >= offset {
            break;
        }
        units += character.len_utf16() as u32;
        if character == '\n' {
            line += 1;
        }
    }
    line
}

/// Every service's compiler rows the scan landed, by repo-relative file. A
/// file whose bytes on disk hash otherwise than its rows' holds stale rows
/// and is left out: it reads as a file with no rows, never as a guess.
pub(crate) struct Rows {
    files: HashMap<String, Vec<Row>>,
    /// Whether any rows file was read at all.
    any: bool,
}

impl Rows {
    fn read(repo: &Path, paths: &[PathBuf]) -> Self {
        let mut files: HashMap<String, Vec<Row>> = HashMap::new();
        let mut any = false;
        for path in paths {
            let rows = match crate::position_rows::read_rows_file(path) {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("matched pass: rows not read: {e}");
                    continue;
                }
            };
            any = true;
            for FileRows { f, h, r, .. } in rows.files {
                let fresh = std::fs::read(repo.join(&f)).is_ok_and(|bytes| {
                    h.as_deref() == Some(crate::position_rows::content_hash(&bytes).as_str())
                });
                if fresh {
                    files.entry(f).or_default().extend(r);
                }
            }
        }
        Self { files, any }
    }

    /// The `call` row at the UTF-16 span `start..end` of `file`.
    fn call_at(&self, file: &str, start: u32, end: u32) -> Option<&Row> {
        self.files
            .get(file)?
            .iter()
            .find(|row| row.kind() == Kind::Call && row.start() == start && row.end() == end)
    }

    /// The calls whose callee the compiler declares at `file` on one of
    /// `lines`: `(file, line)` of each, sorted, once each.
    fn callers_of(&self, repo: &Path, file: &str, lines: &[u32]) -> Vec<(String, u32)> {
        let mut found: BTreeSet<(String, u32)> = BTreeSet::new();
        for (caller, rows) in &self.files {
            let starts: Vec<u32> = rows
                .iter()
                .filter(|row| {
                    row.kind() == Kind::Call
                        && row.decl_file() == Some(file)
                        && row.decl_line().is_some_and(|line| lines.contains(&line))
                })
                .map(Row::start)
                .collect();
            if starts.is_empty() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(repo.join(caller)) else {
                continue;
            };
            for start in starts {
                found.insert((caller.clone(), line_of_utf16(&content, start)));
            }
        }
        found.into_iter().collect()
    }
}

fn is_declaration_file(file: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| file.ends_with(suffix))
}

/// Whether the compiler engine reads `file` as source: a source extension
/// ([`crate::file_finder::has_source_extension`]), not a declaration file.
fn is_compiler_source(file: &str) -> bool {
    crate::file_finder::has_source_extension(Path::new(file)) && !is_declaration_file(file)
}

/// Whether a declaration the compiler records sits in the repository's own
/// source: not the default library, not an installed package, not a
/// declaration file.
fn is_own_source(decl_file: &str) -> bool {
    !decl_file.starts_with("lib:")
        && !decl_file.starts_with("pkg:")
        && !decl_file
            .split('/')
            .any(|segment| segment == "node_modules")
        && !is_declaration_file(decl_file)
}

/// One pair of the record as the pass reads it.
#[derive(Debug, Clone)]
struct PairIn {
    caller_file: String,
    caller_line: u32,
    server_file: String,
    server_line: u32,
    kind: String,
    method: String,
    name: String,
}

fn pair_in(pair: &Value) -> Option<PairIn> {
    let end = |key: &str| -> Option<(String, u32)> {
        let end = pair.get(key)?;
        let file = end.get("file")?.as_str()?.to_string();
        let line = u32::try_from(end.get("line")?.as_u64()?).ok()?;
        (line >= 1).then_some((file, line))
    };
    let (caller_file, caller_line) = end("caller")?;
    let (server_file, server_line) = end("server")?;
    let operation = pair.get("operation")?;
    Some(PairIn {
        caller_file,
        caller_line,
        server_file,
        server_line,
        kind: operation.get("kind")?.as_str()?.to_string(),
        method: operation.get("method")?.as_str()?.to_string(),
        name: operation.get("name")?.as_str()?.to_string(),
    })
}

/// What the scanner read at a pair's caller before any type is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallerRead {
    /// The located call in compiler positions, or why there is none.
    span: Result<(u32, u32), &'static str>,
    /// See [`MatchedEnd::hands_back_body`].
    hands_back_body: Option<bool>,
    /// Whether the compiler's rows named no callee at the located call.
    callee_unread: bool,
    wrapper: Value,
}

/// The service each pair's ends belong to, and the services' own settings.
struct Repo<'a> {
    services: &'a [Config],
    names: Vec<String>,
}

impl Repo<'_> {
    fn service_of(&self, file: &str) -> String {
        owner_of(file, self.services)
            .map(|(index, _)| self.names[index].clone())
            .unwrap_or_default()
    }
}

fn read_caller(
    repo: &Path,
    pair: &PairIn,
    several: bool,
    rows: &Rows,
    own: &OwnCode,
    parsed: &mut HashMap<String, Option<ParsedFile>>,
) -> CallerRead {
    let unread = |reason: &'static str| CallerRead {
        span: Err(reason),
        hands_back_body: None,
        callee_unread: false,
        wrapper: json!({ "reason": reason }),
    };
    let file = parsed.entry(pair.caller_file.clone()).or_insert_with(|| {
        let path = repo.join(&pair.caller_file);
        let content = std::fs::read_to_string(&path).ok()?;
        let (sites, definitions) = read_file_sites(&path)?;
        Some((content, sites, definitions))
    });
    let Some((content, sites, definitions)) = file.as_ref() else {
        return unread(NOT_LOCATED);
    };
    if several {
        return unread(SEVERAL_SITES_ON_LINE);
    }
    let (lo, hi) = match locate_call(sites, pair.caller_line) {
        Located::Call { lo, hi } => (lo, hi),
        Located::NoCall => return unread(NOT_LOCATED),
        Located::Several => return unread(SEVERAL_SITES_ON_LINE),
    };
    let start = crate::utils::sidecar_position(content, lo);
    let end = crate::utils::sidecar_position(content, hi);

    let wrapper = match enclosing(definitions, lo, hi) {
        _ if !rows.any => json!({ "reason": ROWS_MISSING }),
        None => json!({ "reason": NOT_IN_WRAPPER }),
        Some(definition) => {
            let mut lines = vec![definition.name_line, definition.start_line];
            lines.dedup();
            let callers = rows.callers_of(repo, &pair.caller_file, &lines);
            json!({
                "name": definition.name,
                "file": pair.caller_file,
                "line": definition.name_line,
                "callers": callers
                    .iter()
                    .take(MAX_WRAPPER_CALLERS)
                    .map(|(file, line)| json!({ "file": file, "line": line }))
                    .collect::<Vec<_>>(),
                "callers_total": callers.len(),
            })
        }
    };

    // Where the call goes: a callee of the repository's own source is
    // judged only when it hands back the parsed body; a library's or the
    // platform's call is the client's own and is judged; a call the rows do
    // not name is not judged.
    let (hands_back_body, callee_unread) = match rows.call_at(&pair.caller_file, start, end) {
        Some(row) => match (row.decl_file(), row.decl_line()) {
            (Some(file), Some(line)) if is_own_source(file) => {
                (Some(own.hands_back_parse(file, line)), false)
            }
            (Some(_), _) => (None, false),
            _ => (Some(false), true),
        },
        None => (Some(false), true),
    };
    CallerRead {
        span: Ok((start, end)),
        hands_back_body,
        callee_unread,
        wrapper,
    }
}

/// A caller file as read once: its text, its calls and its functions.
type ParsedFile = (String, Vec<Site>, Vec<Definition>);

/// A pair's operation: kind, method, name.
type Operation<'a> = (&'a str, &'a str, &'a str);

/// A stored blob of one of the repository's services, with its rows file.
struct Stored {
    blob: CloudRepoData,
    rows: PathBuf,
}

/// The blobs of `services` in `store`, matched by the repository's name
/// (`owner/name` or `name`) and the service's.
fn read_store(store: &Path, repo_name: &str, names: &[String]) -> HashMap<String, Stored> {
    let short = repo_name.rsplit('/').next().unwrap_or(repo_name);
    let mut found = HashMap::new();
    let Ok(entries) = std::fs::read_dir(store) else {
        return found;
    };
    for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(blob) = serde_json::from_slice::<CloudRepoData>(&bytes) else {
            continue;
        };
        if blob.repo_name != repo_name && blob.repo_name != short {
            continue;
        }
        let service = blob
            .service_name
            .clone()
            .unwrap_or_else(|| short.to_string());
        if !names.contains(&service) {
            continue;
        }
        let stem = crate::cloud_storage::blob_stem(&blob.repo_name, blob.service_name.as_deref());
        let rows = store.join(POSITIONS_DIR).join(format!("{stem}.jsonl.gz"));
        found.insert(service, Stored { blob, rows });
    }
    found
}

/// `text` cut to at most `units` UTF-16 units.
fn cut_utf16(text: &str, units: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            used += character.len_utf16();
            used <= units
        })
        .collect()
}

/// The compiler's reading of a response side.
fn reading(end: &MatchedEndTypes, file: &str, line: u32) -> Value {
    if let (Some(read), None) = (&end.type_read, &end.reason) {
        let text = read.expanded.trim();
        let concrete = !text.is_empty() && !matches!(text, "any" | "unknown");
        if concrete && text.len() <= MAX_TYPE_TEXT_BYTES {
            return json!({ "text": text, "evidence": { "file": file, "line": line } });
        }
        let reason = if concrete {
            "type_too_large"
        } else {
            "not_recorded"
        };
        return json!({ "reason": reason });
    }
    json!({ "reason": end.reason.clone().unwrap_or_else(|| "not_recorded".to_string()) })
}

/// The compiler's verdict on a pair, graded on the record's scale.
fn skipped_verdict(detail: &str) -> Value {
    json!({
        "grade": "cannot_tell",
        "compiler_verdict": "unverifiable",
        "detail": cut_utf16(detail, MAX_DETAIL_UNITS),
    })
}

fn verdict(row: &MatchedPairTypes, callee_unread: bool) -> Value {
    let cannot_tell = skipped_verdict;
    let Some(judged) = &row.verdict else {
        return cannot_tell(
            row.verdict_reason
                .unwrap_or(crate::engine::matched_types::ENDS_NOT_READ),
        );
    };
    if let Some(skipped) = judged.skipped {
        return cannot_tell(if callee_unread {
            CALLEE_NOT_READ
        } else {
            skipped
        });
    }
    graded(judged)
}

fn graded(judged: &MatchedVerdict) -> Value {
    let detail = judged
        .diagnostic
        .as_deref()
        .or(judged.unresolved_reason.as_deref());
    let mut verdict = match judged.bucket {
        // A producer wider than its handler's return is never a break
        // (`VerdictBucket::ProducerWider`): on the wire it is compatible.
        VerdictBucket::Compatible | VerdictBucket::ProducerWider => {
            json!({ "grade": "agree", "compiler_verdict": "compatible" })
        }
        VerdictBucket::Incompatible => {
            json!({ "grade": Value::Null, "compiler_verdict": "incompatible" })
        }
        VerdictBucket::Unverifiable | VerdictBucket::GateCaughtBakedAny => {
            json!({ "grade": "cannot_tell", "compiler_verdict": "unverifiable" })
        }
    };
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        verdict["detail"] = Value::String(cut_utf16(detail, MAX_DETAIL_UNITS));
    }
    verdict
}

/// Put `value` at `pair.sides.<side>.compiler`, keeping whatever else the
/// side holds (a model's reading).
fn put_side(pair: &mut Map<String, Value>, side: &str, value: Value) {
    let sides = pair
        .entry("sides")
        .or_insert_with(|| Value::Object(Map::new()));
    if !sides.is_object() {
        *sides = Value::Object(Map::new());
    }
    let slot = sides
        .as_object_mut()
        .expect("sides is an object")
        .entry(side)
        .or_insert_with(|| Value::Object(Map::new()));
    if !slot.is_object() {
        *slot = Value::Object(Map::new());
    }
    slot.as_object_mut()
        .expect("the side is an object")
        .insert("compiler".to_string(), value);
}

fn put_verdict(pair: &mut Map<String, Value>, value: Value) {
    let verdicts = pair
        .entry("verdicts")
        .or_insert_with(|| Value::Object(Map::new()));
    if !verdicts.is_object() {
        *verdicts = Value::Object(Map::new());
    }
    verdicts
        .as_object_mut()
        .expect("verdicts is an object")
        .insert("compiler".to_string(), value);
}

/// Where the pass finds what it reads.
pub struct Inputs<'a> {
    /// The repository checked out at the record's commit.
    pub repo: &'a Path,
    /// The local store the scan wrote its blobs and rows to; `None` when
    /// there is none (every wrapper is `rows_missing`, every caller that
    /// goes through a call the rows do not name is not judged).
    pub store: Option<&'a Path>,
    /// The engine's entry point; `None` when it was not found.
    pub sidecar: Option<&'a Path>,
    pub ceiling: Duration,
}

/// Annotate `record` in place. An error is a record this pass cannot read at
/// all (no `pairs` array); everything else is stated in the record.
pub fn annotate(record: &mut Value, inputs: &Inputs<'_>) -> Result<(), String> {
    let started = Instant::now();
    let repo_name = record
        .get("repo")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let pairs_in: Vec<Option<PairIn>> = record
        .get("pairs")
        .and_then(Value::as_array)
        .ok_or("the record has no `pairs` array")?
        .iter()
        .map(pair_in)
        .collect();

    let root = inputs
        .repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", inputs.repo.display()))?;
    let services = crate::service_derivation::resolve(&root)?.services;
    let short = repo_name
        .rsplit('/')
        .next()
        .unwrap_or(&repo_name)
        .to_string();
    let names: Vec<String> = services
        .iter()
        .map(|service| {
            service
                .service_name
                .clone()
                .unwrap_or_else(|| short.clone())
        })
        .collect();
    let repo = Repo {
        services: &services,
        names: names.clone(),
    };

    let stored = inputs
        .store
        .map(|store| read_store(store, &repo_name, &names))
        .unwrap_or_default();
    let mut row_paths: Vec<PathBuf> = stored.values().map(|s| s.rows.clone()).collect();
    row_paths.sort();
    row_paths.retain(|path| path.exists());
    let rows = Rows::read(&root, &row_paths);
    let mut blobs: Vec<&Stored> = stored.values().collect();
    blobs.sort_by(|a, b| a.blob.service_name.cmp(&b.blob.service_name));
    let own = OwnCode::of(blobs.iter().map(|stored| &stored.blob));
    // What each service was read with, for the runner's log: the pass runs
    // before any log sink, so it says so on stderr.
    for name in &names {
        let held = stored.get(name);
        crate::errln!(
            "carrick {COMMAND}: {name}: extraction config {}, rows {}",
            if held.is_some_and(|s| s.blob.cached_extraction_config.is_some()) {
                "stored"
            } else {
                "absent"
            },
            if held.is_some_and(|s| s.rows.exists()) {
                "read"
            } else {
                "missing"
            },
        );
    }

    // A caller line two pairs name with different operations names no one
    // call.
    let mut operations_on_line: HashMap<(&str, u32), BTreeSet<Operation<'_>>> = HashMap::new();
    for pair in pairs_in.iter().flatten() {
        operations_on_line
            .entry((pair.caller_file.as_str(), pair.caller_line))
            .or_default()
            .insert((&pair.kind, &pair.method, &pair.name));
    }

    let mut parsed = HashMap::new();
    let callers: Vec<Option<CallerRead>> = pairs_in
        .iter()
        .map(|pair| {
            let pair = pair.as_ref()?;
            let several =
                operations_on_line[&(pair.caller_file.as_str(), pair.caller_line)].len() > 1;
            Some(read_caller(&root, pair, several, &rows, &own, &mut parsed))
        })
        .collect();

    // The engine's asks: one per readable pair, in record order.
    let mut indexes: Vec<usize> = Vec::new();
    let mut matched: Vec<MatchedPair> = Vec::new();
    for (index, (pair, caller)) in pairs_in.iter().zip(&callers).enumerate() {
        let (Some(pair), Some(caller)) = (pair, caller) else {
            continue;
        };
        let server_present = root.join(&pair.server_file).is_file();
        let kind = reading_of(&pair.kind);
        // A table's or database function's server end is never asked: a
        // schema written in TypeScript is not a response body.
        let server_reason = match kind {
            Reading::CallerOnly if !is_compiler_source(&pair.server_file) => {
                Some("not_typescript_source")
            }
            Reading::CallerOnly => Some("protocol_not_read"),
            _ if !server_present => Some(NOT_LOCATED),
            _ => None,
        };
        indexes.push(index);
        matched.push(MatchedPair {
            pair_id: index.to_string(),
            verb: pair.method.clone(),
            operation: pair.name.clone(),
            http: kind != Reading::Neither,
            caller: MatchedEnd {
                service: repo.service_of(&pair.caller_file),
                file: pair.caller_file.clone(),
                line: pair.caller_line,
                span: caller.span.ok(),
                reason: caller.span.err(),
                hands_back_body: caller.hands_back_body,
            },
            server: MatchedEnd {
                service: repo.service_of(&pair.server_file),
                file: pair.server_file.clone(),
                line: pair.server_line,
                span: None,
                reason: server_reason,
                hands_back_body: None,
            },
        });
    }
    let engine_services: Vec<MatchedService> = services
        .iter()
        .zip(&names)
        .map(|(service, name)| MatchedService {
            service_id: name.clone(),
            root: crate::engine::service_sidecar_root(&root.to_string_lossy(), service),
            scan_root: root.clone(),
            tsconfig: service.tsconfig.clone(),
            extraction_config: stored
                .get(name)
                .and_then(|stored| stored.blob.cached_extraction_config.clone()),
            server_claims: None,
        })
        .collect();

    let (types, status): (Option<Vec<MatchedPairTypes>>, (&str, Option<&str>)) =
        match inputs.sidecar {
            None => (None, ("failed", Some("engine_not_found"))),
            Some(sidecar) => {
                let rows = type_matched_pairs(
                    sidecar,
                    &engine_services,
                    &matched,
                    inputs.ceiling.saturating_sub(started.elapsed()),
                );
                let cut = rows.iter().any(|row| {
                    [&row.caller, &row.server]
                        .iter()
                        .any(|end| end.reason.as_deref() == Some("budget_exhausted"))
                });
                let status = if cut {
                    ("partial", Some("budget_exhausted"))
                } else {
                    ("done", None)
                };
                (Some(rows), status)
            }
        };

    let pairs = record
        .get_mut("pairs")
        .and_then(Value::as_array_mut)
        .expect("checked above");
    for (position, &index) in indexes.iter().enumerate() {
        let (Some(pair), Some(caller)) = (&pairs_in[index], &callers[index]) else {
            continue;
        };
        let Some(out) = pairs[index].as_object_mut() else {
            continue;
        };
        out.insert("wrapper".to_string(), caller.wrapper.clone());
        let Some(types) = types.as_ref() else {
            continue;
        };
        let row = &types[position];
        put_side(
            out,
            "caller_expects",
            reading(&row.caller, &pair.caller_file, pair.caller_line),
        );
        for side in ["caller_sends", "server_reads"] {
            put_side(out, side, json!({ "reason": REQUEST_SIDE_NOT_READ }));
        }
        if reading_of(&pair.kind) == Reading::CallerOnly {
            // Whatever the engine was given for the server end, it is not
            // this kind's to state, and neither is a verdict.
            let reason = matched[position]
                .server
                .reason
                .unwrap_or("protocol_not_read");
            put_side(out, "server_returns", json!({ "reason": reason }));
            put_verdict(out, skipped_verdict(SERVER_NOT_READ));
            continue;
        }
        put_side(
            out,
            "server_returns",
            reading(&row.server, &pair.server_file, pair.server_line),
        );
        put_verdict(out, verdict(row, caller.callee_unread));
    }

    let mut engine = json!({
        "status": status.0,
        "ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "scanner_version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(reason) = status.1 {
        engine["reason"] = Value::String(reason.to_string());
    }
    record
        .as_object_mut()
        .ok_or("the record is not an object")?
        .insert("engine".to_string(), engine);
    Ok(())
}

struct Args {
    record: PathBuf,
    repo: PathBuf,
    out: PathBuf,
    store: Option<PathBuf>,
    ceiling: Duration,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut record = None;
    let mut repo = None;
    let mut out = None;
    let mut store = std::env::var_os(crate::cloud_storage::CACHE_DIR_ENV)
        .map(PathBuf::from)
        .filter(|dir| !dir.as_os_str().is_empty());
    let mut ceiling = CEILING;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let mut value = || {
            words
                .next()
                .cloned()
                .ok_or_else(|| format!("{word} needs a value"))
        };
        match word.as_str() {
            "--repo" => repo = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--store" => store = Some(PathBuf::from(value()?)),
            "--ceiling-secs" => {
                let secs: u64 = value()?
                    .parse()
                    .map_err(|_| "--ceiling-secs takes a whole number of seconds".to_string())?;
                ceiling = Duration::from_secs(secs);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            path if record.is_none() => record = Some(PathBuf::from(path)),
            extra => return Err(format!("unexpected argument {extra}")),
        }
    }
    Ok(Args {
        record: record.ok_or("no record file given")?,
        repo: repo.ok_or("--repo is required")?,
        out: out.ok_or("--out is required")?,
        store,
        ceiling,
    })
}

/// Run [`COMMAND`] with the arguments after it. Exit 0 when the record was
/// written, 2 for a usage error, 1 when the record could not be read or
/// written.
pub fn run(args: &[String], sidecar: Option<&Path>) -> i32 {
    let usage = format!(
        "usage: carrick {COMMAND} <record.json> --repo <dir> --out <file> [--store <dir>] [--ceiling-secs <n>]"
    );
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(problem) => {
            crate::errln!("carrick {COMMAND}: {problem}\n{usage}");
            return 2;
        }
    };
    let mut record: Value = match std::fs::read(&args.record)
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
    {
        Ok(record) => record,
        Err(e) => {
            crate::errln!("carrick {COMMAND}: {}: {e}", args.record.display());
            return 1;
        }
    };
    let inputs = Inputs {
        repo: &args.repo,
        store: args.store.as_deref(),
        sidecar,
        ceiling: args.ceiling,
    };
    if let Err(e) = annotate(&mut record, &inputs) {
        crate::errln!("carrick {COMMAND}: {e}");
        return 1;
    }
    let text = serde_json::to_string_pretty(&record).expect("a JSON value serializes");
    if let Err(e) = std::fs::write(&args.out, text + "\n") {
        crate::errln!("carrick {COMMAND}: {}: {e}", args.out.display());
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn services(spec: &[(&str, Option<&str>, &[&str])]) -> Vec<Config> {
        spec.iter()
            .map(|(name, directory, include)| Config {
                service_name: Some(name.to_string()),
                directory: directory.map(str::to_string),
                include: include.iter().map(|s| s.to_string()).collect(),
                ..Config::default()
            })
            .collect()
    }

    #[test]
    fn service_of_file() {
        let services = services(&[
            ("web", Some("apps/web"), &["shared/ui"]),
            ("admin", Some("apps/web/admin"), &[]),
            ("api", Some("apps/api"), &["shared"]),
        ]);
        // The deepest directory first.
        assert_eq!(
            owner_of("apps/web/admin/src/a.ts", &services),
            Some((1, Owner::Directory))
        );
        assert_eq!(
            owner_of("apps/web/src/a.ts", &services),
            Some((0, Owner::Directory))
        );
        // A prefix that is not a whole segment is not a directory of it.
        assert_eq!(
            owner_of("apps/webhooks/a.ts", &services).map(|(_, owner)| owner),
            Some(Owner::Nearest)
        );
        // An include, the first service in order that names it.
        assert_eq!(
            owner_of("shared/ui/button.ts", &services),
            Some((0, Owner::Include))
        );
        assert_eq!(
            owner_of("shared/lib/x.ts", &services),
            Some((2, Owner::Include))
        );
        // Nearest: the longest shared leading path, the first on a tie.
        assert_eq!(
            owner_of("apps/clients/x.ts", &services),
            Some((0, Owner::Nearest))
        );
        assert_eq!(owner_of("tools/x.ts", &services), Some((0, Owner::Nearest)));
        // A root service holds every file.
        let root = super::tests::services(&[("app", None, &[])]);
        assert_eq!(owner_of("any/where.ts", &root), Some((0, Owner::Directory)));
        assert_eq!(owner_of("a.ts", &[]), None);
    }

    fn located(source: &str, line: u32) -> Option<String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.ts");
        std::fs::write(&path, source).unwrap();
        let (sites, _) = read_file_sites(&path).unwrap();
        match locate_call(&sites, line) {
            Located::Call { lo, hi } => {
                let lo = (lo - SWC_SPAN_BASE) as usize;
                let hi = (hi - SWC_SPAN_BASE) as usize;
                Some(source[lo..hi].to_string())
            }
            Located::NoCall => None,
            Located::Several => Some("<several>".to_string()),
        }
    }

    #[test]
    fn locate_caller_call_on_line() {
        let source = "\
const a = await (await fetch('/a')).json();
const b = await fetch('/b').then((r) => r.json()).catch(() => null);
const c = await Promise.all([fetch('/c1'), fetch('/c2')]);
const d = { tunnel: true, retries: 2 };
const e = new URL('/e', base);
const f = sql`select 1`;
load('/g'); load('/h');
const i = client.get(
  '/i',
);
";
        // The outermost call on the line.
        assert_eq!(
            located(source, 1).as_deref(),
            Some("(await fetch('/a')).json()")
        );
        // A `.then` / `.catch` chain is read as the call it is chained on.
        assert_eq!(located(source, 2).as_deref(), Some("fetch('/b')"));
        // The combinator is the one outermost call: the pair rule decides.
        assert_eq!(
            located(source, 3).as_deref(),
            Some("Promise.all([fetch('/c1'), fetch('/c2')])")
        );
        // An options object holds no call.
        assert_eq!(located(source, 4), None);
        // `new` and a tagged template are calls.
        assert_eq!(located(source, 5).as_deref(), Some("new URL('/e', base)"));
        assert_eq!(located(source, 6).as_deref(), Some("sql`select 1`"));
        // Two outermost calls on one line name no one call.
        assert_eq!(located(source, 7).as_deref(), Some("<several>"));
        // A call that starts on the line and ends below it.
        assert_eq!(
            located(source, 8).as_deref(),
            Some("client.get(\n  '/i',\n)")
        );
        // A line past the calls.
        assert_eq!(located(source, 9), None);
    }

    #[test]
    fn a_utf16_offset_is_placed_on_its_line() {
        let content = "\u{feff}a\u{1F600}\nbc\nd";
        // `a` 1, the emoji 2 units, the newline 1: `b` is at unit 4.
        assert_eq!(line_of_utf16(content, 0), 1);
        assert_eq!(line_of_utf16(content, 4), 2);
        assert_eq!(line_of_utf16(content, 7), 3);
    }

    #[test]
    fn a_declaration_outside_the_repository_is_not_own_code() {
        assert!(is_own_source("apps/web/src/api.ts"));
        for outside in [
            "lib:lib.dom.d.ts",
            "pkg:ky/distribution/index.d.ts",
            "apps/web/node_modules/ky/index.js",
            "apps/web/src/types.d.ts",
        ] {
            assert!(!is_own_source(outside), "{outside}");
        }
    }

    #[test]
    fn a_detail_is_cut_to_the_records_limit_in_utf16_units() {
        let long = "\u{1F600}".repeat(300);
        let cut = cut_utf16(&long, MAX_DETAIL_UNITS);
        assert_eq!(cut.encode_utf16().count(), 500);
        assert_eq!(cut_utf16("short", MAX_DETAIL_UNITS), "short");
    }

    fn end_types(type_text: Option<&str>, reason: Option<&str>) -> MatchedEndTypes {
        MatchedEndTypes {
            service: "web".to_string(),
            alias: Some("A".to_string()),
            type_read: type_text.map(|text| crate::engine::matched_types::MatchedType {
                alias: "A".to_string(),
                definition: text.to_string(),
                expanded: text.to_string(),
            }),
            reason: reason.map(str::to_string),
        }
    }

    #[test]
    fn a_compiler_reading_is_one_the_cloud_stores() {
        let typed = reading(&end_types(Some("{ id: string }"), None), "a.ts", 3);
        assert_eq!(
            typed,
            json!({ "text": "{ id: string }", "evidence": { "file": "a.ts", "line": 3 } })
        );
        // A text that is not concrete is a reason, so a model's reading can
        // fill the side.
        assert_eq!(
            reading(&end_types(Some("any"), None), "a.ts", 3),
            json!({ "reason": "not_recorded" })
        );
        assert_eq!(
            reading(&end_types(Some("any"), Some("site_unreadable")), "a.ts", 3),
            json!({ "reason": "site_unreadable" })
        );
        let huge = format!("{{ a: \"{}\" }}", "x".repeat(MAX_TYPE_TEXT_BYTES));
        assert_eq!(
            reading(&end_types(Some(&huge), None), "a.ts", 3),
            json!({ "reason": "type_too_large" })
        );
        assert_eq!(
            reading(&end_types(None, Some("not_typescript_source")), "a.ts", 3),
            json!({ "reason": "not_typescript_source" })
        );
    }

    fn pair_row(verdict: Option<MatchedVerdict>, reason: Option<&'static str>) -> MatchedPairTypes {
        MatchedPairTypes {
            pair_id: "0".to_string(),
            caller: end_types(None, Some("not_located")),
            server: end_types(None, Some("not_located")),
            verdict,
            verdict_reason: reason,
        }
    }

    fn judged(bucket: VerdictBucket, diagnostic: Option<&str>) -> MatchedVerdict {
        MatchedVerdict {
            bucket,
            gate: None,
            diagnostic: diagnostic.map(str::to_string),
            resolved: true,
            unresolved_reason: None,
            unresolved_side: None,
            judged: true,
            skipped: None,
        }
    }

    #[test]
    fn a_compiler_verdict_is_graded_as_the_record_reads_it() {
        assert_eq!(
            verdict(
                &pair_row(Some(judged(VerdictBucket::Compatible, None)), None),
                false
            ),
            json!({ "grade": "agree", "compiler_verdict": "compatible" })
        );
        assert_eq!(
            verdict(
                &pair_row(Some(judged(VerdictBucket::ProducerWider, None)), None),
                false
            ),
            json!({ "grade": "agree", "compiler_verdict": "compatible" })
        );
        let long = "x".repeat(900);
        let bad = verdict(
            &pair_row(Some(judged(VerdictBucket::Incompatible, Some(&long))), None),
            false,
        );
        assert_eq!(bad["grade"], Value::Null);
        assert_eq!(bad["compiler_verdict"], "incompatible");
        assert_eq!(bad["detail"].as_str().unwrap().len(), MAX_DETAIL_UNITS);
        // No verdict: cannot tell, with the reason.
        let none = verdict(
            &pair_row(None, Some(crate::engine::matched_types::ENDS_NOT_READ)),
            false,
        );
        assert_eq!(none["grade"], "cannot_tell");
        assert_eq!(none["compiler_verdict"], "unverifiable");
        assert_eq!(none["detail"], crate::engine::matched_types::ENDS_NOT_READ);
        // A skip says why: the own-code sentence only where the rows named
        // the own code.
        let mut skip = judged(VerdictBucket::Unverifiable, None);
        skip.skipped = Some(crate::engine::matched_types::OWN_CODE_SKIP);
        let row = pair_row(Some(skip), None);
        assert_eq!(
            verdict(&row, false)["detail"],
            crate::engine::matched_types::OWN_CODE_SKIP
        );
        assert_eq!(verdict(&row, true)["detail"], CALLEE_NOT_READ);
        assert_eq!(verdict(&row, true)["grade"], "cannot_tell");
    }

    #[test]
    fn the_compiler_slot_is_added_beside_a_models_reading() {
        let mut pair = json!({
            "sides": { "caller_expects": { "model": { "text": "{ id: string }" } } },
            "verdicts": { "model": { "grade": "agree" } }
        });
        let out = pair.as_object_mut().unwrap();
        put_side(out, "caller_expects", json!({ "reason": "not_located" }));
        put_side(
            out,
            "server_reads",
            json!({ "reason": REQUEST_SIDE_NOT_READ }),
        );
        put_verdict(
            out,
            json!({ "grade": "cannot_tell", "compiler_verdict": "unverifiable" }),
        );
        assert_eq!(
            pair,
            json!({
                "sides": {
                    "caller_expects": {
                        "model": { "text": "{ id: string }" },
                        "compiler": { "reason": "not_located" }
                    },
                    "server_reads": { "compiler": { "reason": REQUEST_SIDE_NOT_READ } }
                },
                "verdicts": {
                    "model": { "grade": "agree" },
                    "compiler": { "grade": "cannot_tell", "compiler_verdict": "unverifiable" }
                }
            })
        );
    }

    #[test]
    fn with_no_rows_the_wrapper_is_rows_missing_and_the_call_is_not_judged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.ts"),
            "export async function apiGet(p: string) {\n  return (await fetch(p)).json();\n}\n",
        )
        .unwrap();
        let pair = PairIn {
            caller_file: "a.ts".to_string(),
            caller_line: 2,
            server_file: "b.ts".to_string(),
            server_line: 1,
            kind: "route".to_string(),
            method: "GET".to_string(),
            name: "/x".to_string(),
        };
        let rows = Rows {
            files: HashMap::new(),
            any: false,
        };
        let read = read_caller(
            dir.path(),
            &pair,
            false,
            &rows,
            &OwnCode::of(std::iter::empty()),
            &mut HashMap::new(),
        );
        assert_eq!(read.wrapper, json!({ "reason": ROWS_MISSING }));
        assert!(read.span.is_ok());
        // No row names the callee: the own-code question is open, so the
        // pair is not judged, and says so in its own words.
        assert_eq!(read.hands_back_body, Some(false));
        assert!(read.callee_unread);
    }

    #[test]
    fn the_kinds_the_engine_reads() {
        assert_eq!(reading_of("route"), Reading::BothEnds);
        assert_eq!(reading_of("function"), Reading::BothEnds);
        assert_eq!(reading_of("table"), Reading::CallerOnly);
        assert_eq!(reading_of("database_function"), Reading::CallerOnly);
        for kind in [
            "graphql_field",
            "procedure",
            "event",
            "topic",
            "queue",
            "other",
            "new_kind",
        ] {
            assert_eq!(reading_of(kind), Reading::Neither, "{kind}");
        }
    }

    /// The sites and functions of a file, as a test reads them.
    fn definitions_of(source: &str) -> (Vec<Site>, Vec<Definition>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.ts");
        std::fs::write(&path, source).unwrap();
        read_file_sites(&path).unwrap()
    }

    #[test]
    fn the_function_a_call_runs_in_is_the_innermost_named_one() {
        let source = "\
export async function apiGet(url: string) {
  return (await fetch(url)).json();
}
export const api = {
  list: async () => (await fetch('/l')).json(),
};
const load = useCallback(async () => {
  await fetch('/u');
}, []);
await fetch('/top');
";
        let (sites, definitions) = definitions_of(source);
        let name_at = |line: u32| -> Option<(String, u32)> {
            let Located::Call { lo, hi } = locate_call(&sites, line) else {
                panic!("line {line} holds a call");
            };
            enclosing(&definitions, lo, hi).map(|d| (d.name.clone(), d.name_line))
        };
        assert_eq!(name_at(2), Some(("apiGet".to_string(), 1)));
        assert_eq!(name_at(5), Some(("list".to_string(), 5)));
        assert_eq!(name_at(8), Some(("load".to_string(), 7)));
        assert_eq!(name_at(10), None);
    }
}
