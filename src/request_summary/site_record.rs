//! The record each decided site carries (carrick#2435, part of
//! carrick#2441): the compiler's names, the syntax and the library labels at
//! the site, the values the code states, and which slots it already decides.
//!
//! The record is built from what the scan already holds: the compiler's
//! per-position rows ([`crate::position_rows::PositionIndex`]), the
//! scanner's own parse of each file ([`crate::parser::parse_file`]), the
//! decided sites ([`super::decided_sites`]) and the store's labels. No second
//! compiler is run.
//!
//! - **Compiler names**: every identifier the site's call writes, with the
//!   declaration its row names, the type, and the literal a constant stands
//!   for (a string literal type). An in-repo declaration's text is sliced
//!   from the scanner's own read of its line, at most
//!   [`MAX_DECLARATION_CHARS`]; a package's declaration is represented by its
//!   labels, never by its declaration file's text.
//! - **Reached bodies**: the functions between the site and the labelled
//!   call, two levels deep, with the strings they write.
//! - **Stated values**: every string and template literal the call and the
//!   reached bodies write, every object key, every literal a constant
//!   stands for, and the label's method fields. A request-method word is
//!   offered only as a method.
//! - **The slot rule** (D2, ruled 2026-10-16): a slot is forced when exactly
//!   one stated value fills it and the chain ends at a labelled export (every
//!   decided site's does). A forced slot is a fact and is not asked. Several
//!   stated values: asked. None: unstated, and a site whose name is unstated
//!   is a lead. A value is stated only when the code states all of it: one
//!   literal piece of a joined name is offered, never forced. The method is
//!   read at the label's method slot. Kind and role are forced by the
//!   label's role and op where they say one; a label that names no name slot
//!   forces neither the name nor its argument.
//! - **No absolute path** in any field: every file is repo-relative, so two
//!   checkouts of one tree give equal records.
//! - **The asked-slot list is part of the record**, so a change to which
//!   slots are asked moves the record's sha and the cloud's cache key
//!   (carrick#2437).
//!
//! The wire carries [`SiteRecord::state`] only. The type anchors
//! ([`TypeAnchors`], carrick#2472 C3) stay in the scanner: the model never
//! reads them, and a span in the state would move the cache key on every
//! edit above the site.
//!
//! Record-only: nothing in a scan reads it yet.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use swc_common::{BytePos, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::RequestSummaryInputs;
use super::decided_sites::{
    Decided, DecidedSite, HandlerAt, MethodValue, PayloadBinding, SiteLabel, SlotBinding,
};
use crate::position_rows::{Kind, PositionIndex};
use crate::site_questions::{
    Place, PlaceFrom, Point, SiteCall, SiteState, Slot, Stated, StatedValue,
};
use crate::type_manifest::is_http_method;

/// The most characters of an in-repo declaration's text a name carries.
pub const MAX_DECLARATION_CHARS: usize = 160;
/// The most stated values in each list.
pub const MAX_STATED: usize = 24;
/// The most characters a stated value may have; a longer one is not offered.
pub const MAX_STATED_CHARS: usize = 120;
/// The most places one stated value lists.
pub const MAX_PLACES: usize = 4;
/// The most syntax nodes a state carries.
const MAX_SYNTAX_NODES: usize = 60;
/// The most lines of a reached function's code.
const MAX_CODE_LINES: usize = 30;
/// The most characters of one line of code or one syntax text.
const MAX_LINE_CHARS: usize = 200;
/// Lines of context before and after the site's line in `point.code`.
const CONTEXT_BEFORE: u32 = 3;
const CONTEXT_AFTER: u32 = 2;

// ---------------------------------------------------------------------------
// The record's own parts (the wire carries them as JSON values: `compiler`,
// `syntax`, `libraries` and `package_network_roles` of
// [`crate::site_questions::SiteState`]).

/// What the compiler says at the site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compiler {
    pub names: Vec<CompilerName>,
    pub callees: Vec<CompilerCallee>,
    pub enclosing: Option<Enclosing>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reached: Vec<Reached>,
}

/// One identifier the site writes, as its row names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerName {
    pub text: String,
    pub at: String,
    pub declaration: Option<String>,
    pub declaration_text: Option<String>,
    pub package: Option<String>,
    #[serde(rename = "type")]
    pub type_text: Option<String>,
    pub literal: Option<String>,
}

/// The function the site's call runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilerCallee {
    pub text: String,
    pub declaration: Option<String>,
    pub package: Option<String>,
    pub code: Option<String>,
}

/// The call the site sits inside: a call it is handed to as a callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enclosing {
    pub text: String,
    pub at: String,
}

/// A function the site's calls run, `level` calls down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    pub level: u8,
    pub via: String,
    pub function: String,
    pub code: String,
    pub names: Vec<CompilerName>,
    pub strings: Vec<String>,
}

/// One syntax node at the site: the call (its callee and arguments), or a
/// property of an object it is handed (its key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxNode {
    #[serde(rename = "type")]
    pub node: String,
    pub at: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A package's labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub package: String,
    pub labels: Vec<LibraryLabel>,
    pub argument_labels: Vec<ArgumentLabel>,
    pub in_store: bool,
}

/// One op of a labelled export: what it does and the arguments its name
/// and payload are read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentLabel {
    pub export: String,
    pub member: Option<String>,
    pub does: String,
    pub name_argument: Option<usize>,
    pub name_key: Option<String>,
    pub payload_argument: Option<usize>,
}

/// The most argument labels one package lists.
const MAX_ARGUMENT_LABELS: usize = 20;

/// One labelled export of a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryLabel {
    pub export: String,
    pub role: String,
    pub side: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http: Vec<HttpLabel>,
}

/// An HTTP member's label: the method it sends and the argument its path
/// is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpLabel {
    pub member: Option<String>,
    pub method: Option<String>,
    pub path_from: super::decided_sites::ArgSlot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_from: Option<super::decided_sites::ArgSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_default: Option<String>,
}

/// How many other files of the service use labelled exports of one role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleFiles {
    pub role: String,
    pub files: usize,
}

// ---------------------------------------------------------------------------
// The scanner's side of the record.

/// A position the type capture can anchor at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnchorAt {
    pub file: String,
    pub line: u32,
    pub span_start: u32,
    pub span_end: u32,
}

/// What the type capture needs to type a row the fact layer adds with no
/// reader row (carrick#2472 C3): the deciding call (the send's expression,
/// and its result read for a consumer response), where its payload is bound,
/// and for a handler table's key the function that handles it (the producer
/// response, or a subscriber's parameter `param`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypeAnchors {
    pub call: AnchorAt,
    pub payload: Option<PayloadBinding>,
    /// The payload's own expression, when it is an argument of the deciding
    /// call or one option key of it (carrick#2439): the span the type
    /// capture reads a send's payload at. `None` for a payload a call below
    /// builds.
    pub payload_at: Option<AnchorAt>,
    pub handler: Option<HandlerAnchor>,
    /// The key of a handler table's entry, when the site is one: a table
    /// registered in a loop gives every key the loop's call span, so the key
    /// is what tells a record's site from its siblings'. Off the wire (only
    /// `state` is sent) and not part of the question id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A handler function and the parameter its payload arrives in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandlerAnchor {
    #[serde(flatten)]
    pub at: AnchorAt,
    pub param: usize,
    /// That parameter as the source writes it (a name, or a destructuring
    /// pattern), which the type capture finds it by; `None` when the
    /// function takes none or is not one this pass can read.
    pub param_name: Option<String>,
}

/// Where a decided site's payloads are written in place (phase 3, step 1):
/// the fact layer says which expression holds each payload, and the type
/// capture prints its type there. A call's request is its body argument and
/// its response is its own result; a handler's request is the parameter it
/// is run with. A slot is filled only where the site's own call or handler
/// states it, in the same function: a payload a call below builds, a value
/// a caller hands in, and a route handler's body read and response value
/// (which need the label fields carrick#2474 lists) are left empty.
///
/// Scanner-side only, like the anchors, and outside [`SiteState`], so no
/// site's sha and no cache key moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PayloadSlots {
    /// What the site sends, or what its handler is run with.
    pub request: Option<PayloadAt>,
    /// What the site's call gives back.
    pub response: Option<PayloadAt>,
}

/// One payload's place, and how its type is read there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadAt {
    /// The value an expression holds: a call's body argument.
    Value(AnchorAt),
    /// What a call gives back, read at the call.
    Result(AnchorAt),
    /// What a handler is run with: its parameter, as the source writes it,
    /// in the function at `at`.
    Param { at: AnchorAt, name: String },
    /// A call's options literal, which states `key` absent (carrick#2674
    /// P3-1a): the call sends no body. Read at the literal as the sidecar's
    /// `request_config` question, which confirms there is no spread, computed
    /// key or method in it; its null answer never anchors a body.
    Unwritten { at: AnchorAt, key: String },
}

impl PayloadAt {
    pub fn at(&self) -> &AnchorAt {
        match self {
            PayloadAt::Value(at)
            | PayloadAt::Result(at)
            | PayloadAt::Param { at, .. }
            | PayloadAt::Unwritten { at, .. } => at,
        }
    }
}

/// The payload slots a site's own call or handler fills ([`PayloadSlots`]):
/// a send's body at the argument its label names (`payload_at`), a call's
/// result at the call, a receive's parameter at its handler.
fn payload_slots(
    call: &AnchorAt,
    payload_at: Option<&AnchorAt>,
    carried: Option<PayloadAt>,
    handler: Option<&HandlerAnchor>,
) -> PayloadSlots {
    match handler {
        // A receive: the payload arrives in the handler's parameter. What
        // it responds with is not stated by any label yet.
        Some(handler) => PayloadSlots {
            request: handler.param_name.as_ref().map(|name| PayloadAt::Param {
                at: handler.at.clone(),
                name: name.clone(),
            }),
            response: None,
        },
        None => PayloadSlots {
            // The carried binding (the option key read through a spread or a
            // rest) takes over from the locator where it names a place; the
            // locator's slot stands wherever it filled one.
            request: carried.or_else(|| payload_at.cloned().map(PayloadAt::Value)),
            response: Some(PayloadAt::Result(call.clone())),
        },
    }
}

/// Which bucket a site falls in under the slot rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    /// Every slot is forced: a fact, nothing asked.
    Forced,
    /// Some slot is asked.
    Asked,
    /// The name is unstated: a lead for the agent, never a row.
    Lead,
}

/// What a forced slot rests on (carrick#2520): a value the code states, or
/// the store's label alone. A slot the model answers is asked, not forced,
/// and has no basis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// A literal, a summary-composed path, an object key, or a method the
    /// code writes at the label's method slot.
    Code,
    /// The label's role or op (kind, role), the method the member always
    /// sends or its default, and the argument positions the label names.
    Label,
}

/// One decided site's record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SiteRecord {
    /// What crosses the wire (carrick#2437).
    pub state: SiteState,
    pub anchors: TypeAnchors,
    /// Where the site's payloads are written in place. Off the wire.
    #[serde(skip)]
    pub payloads: PayloadSlots,
    /// What each forced slot rests on (carrick#2520). Scanner-side only,
    /// like the anchors: off the wire, so no cache key moves.
    pub basis: BTreeMap<Slot, Basis>,
    pub bucket: Bucket,
    /// Why a lead is one.
    pub lead: Option<&'static str>,
    /// The binding the labelled call is made on (carrick#2474 R1), with the
    /// receiver trace on. Scanner-side only, off the wire like the anchors:
    /// no site's sha moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<super::discovery::ReceiverAt>,
}

impl SiteRecord {
    /// The state's bytes as the wire sends them.
    pub fn state_bytes(&self) -> usize {
        serde_json::to_vec(&self.state).map_or(0, |bytes| bytes.len())
    }

    /// The state's `compiler` part, read back from the wire value. `None`
    /// when the value is not a [`Compiler`].
    pub fn compiler(&self) -> Option<Compiler> {
        serde_json::from_value(self.state.compiler.clone()).ok()
    }
}

// ---------------------------------------------------------------------------
// Building.

/// One file as the scanner reads and parses it.
struct Parsed {
    text: String,
    cm: Lrc<SourceMap>,
    module: Module,
}

impl Parsed {
    fn read(path: &Path) -> Option<Self> {
        let cm: Lrc<SourceMap> = Default::default();
        let handler = swc_common::errors::Handler::with_tty_emitter(
            swc_common::errors::ColorConfig::Never,
            false,
            false,
            Some(cm.clone()),
        );
        let text = crate::parser::read_source(path)?;
        let module = crate::parser::parse_file(path, &cm, &handler)?;
        Some(Self { text, cm, module })
    }

    fn line(&self, pos: u32) -> u32 {
        self.cm.lookup_char_pos(BytePos(pos)).line as u32
    }

    /// The source text of `span`, as one line of at most
    /// [`MAX_LINE_CHARS`].
    fn text_of(&self, span: Span) -> String {
        cap(&one_line(self.raw(span)), MAX_LINE_CHARS)
    }

    /// The source text of `span` as written.
    fn raw(&self, span: Span) -> &str {
        let start = (span.lo.0 as usize).saturating_sub(crate::swc_scanner::SWC_SPAN_BASE as usize);
        let end = (span.hi.0 as usize).saturating_sub(crate::swc_scanner::SWC_SPAN_BASE as usize);
        self.text.get(start..end).unwrap_or_default()
    }

    /// Lines `from..=to` (1-based), each numbered.
    fn numbered(&self, from: u32, to: u32) -> String {
        numbered(&self.text, from, to)
    }

    fn line_text(&self, line: u32) -> Option<&str> {
        self.text.lines().nth(line.checked_sub(1)? as usize)
    }
}

/// Lines `from..=to` (1-based) of `text`, each numbered `NN: ` and cut at
/// [`MAX_LINE_CHARS`]: the form a site's `point.code` and a handler
/// record's `code` share (carrick#2474 R4b).
pub(super) fn numbered(text: &str, from: u32, to: u32) -> String {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index as u32 + 1, line))
        .filter(|(number, _)| *number >= from && *number <= to)
        .map(|(number, line)| format!("{number}: {}", cap(line, MAX_LINE_CHARS)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cap(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// `text` with every run of whitespace one space.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every whole value `expr` may take, as the code states it: a string or a
/// plain template literal, a name whose type is a literal or a union of
/// literals (`literals`), or a choice between such values. `None` when any
/// value it may take is not stated whole (a concatenation with a value the
/// code does not state, a call, a member read).
fn whole_values(expr: &Expr, literals: &dyn Fn(&str) -> Vec<String>) -> Option<BTreeSet<String>> {
    match super::unwrap_value(expr) {
        Expr::Lit(Lit::Str(lit)) => lit.value.as_str().map(|text| BTreeSet::from([text.into()])),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() => match tpl.quasis.as_slice() {
            [quasi] => quasi
                .cooked
                .as_ref()
                .and_then(|cooked| cooked.as_str())
                .map(|text| BTreeSet::from([text.to_string()])),
            _ => None,
        },
        Expr::Cond(cond) => {
            let mut values = whole_values(&cond.cons, literals)?;
            values.extend(whole_values(&cond.alt, literals)?);
            Some(values)
        }
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::LogicalOr | BinaryOp::NullishCoalescing) => {
            let mut values = whole_values(&bin.left, literals)?;
            values.extend(whole_values(&bin.right, literals)?);
            Some(values)
        }
        Expr::Ident(ident) => {
            let values: BTreeSet<String> = literals(ident.sym.as_ref()).into_iter().collect();
            (!values.is_empty()).then_some(values)
        }
        _ => None,
    }
}

/// The first expression inside `exprs` whose source text is `text`.
fn expr_written_as(parsed: &Parsed, exprs: &[&Expr], text: &str) -> Option<Expr> {
    struct Find<'p> {
        parsed: &'p Parsed,
        text: String,
        found: Option<Expr>,
    }
    impl Visit for Find<'_> {
        fn visit_expr(&mut self, expr: &Expr) {
            if self.found.is_some() {
                return;
            }
            if one_line(self.parsed.raw(expr.span())) == self.text {
                self.found = Some(expr.clone());
                return;
            }
            expr.visit_children_with(self);
        }
    }
    let mut find = Find {
        parsed,
        text: one_line(text),
        found: None,
    };
    for expr in exprs {
        expr.visit_with(&mut find);
    }
    find.found
}

/// The whole values a slot's composed value (`pieces`) states: its text
/// when every piece is literal, or when it is one value this pass does not
/// read, the values of the expression the call writes for it among `exprs`.
fn stated_whole(
    parsed: Option<&Parsed>,
    pieces: &[super::Piece],
    exprs: &[&Expr],
    literals: &dyn Fn(&str) -> Vec<String>,
) -> Option<BTreeSet<String>> {
    if let Some(text) = literal_text(pieces).or_else(|| env_based_text(pieces)) {
        return Some(BTreeSet::from([text]));
    }
    match pieces {
        [super::Piece::Opaque { text, .. }] => {
            whole_values(&expr_written_as(parsed?, exprs, text)?, literals)
        }
        _ => None,
    }
}

/// The text of `pieces` when every one is literal and the text is not empty.
fn literal_text(pieces: &[super::Piece]) -> Option<String> {
    let text: String = pieces
        .iter()
        .map(|piece| match piece {
            super::Piece::Lit(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Option<String>>()?;
    (!text.is_empty()).then_some(text)
}

/// The text of `pieces` when they are text the code writes and environment
/// reads, with at least one read: an environment base is stated, as the
/// request rows write it, a named piece (`${process.env.API}/items`). Any
/// other piece (a parameter, a value nothing reads) leaves it unstated.
fn env_based_text(pieces: &[super::Piece]) -> Option<String> {
    let mut reads = 0;
    let text: String = pieces
        .iter()
        .map(|piece| match piece {
            super::Piece::Lit(text) => Some(text.clone()),
            super::Piece::Opaque {
                end: super::Root::Env { written, .. },
                ..
            } => {
                reads += 1;
                Some(format!("${{{written}}}"))
            }
            _ => None,
        })
        .collect::<Option<String>>()?;
    (reads > 0).then_some(text)
}

/// A declaration file as the record names it: an installed package outside
/// the repo (`pkg:<name>/<path>` in the rows) under `node_modules/`, as an
/// installed package inside it is.
fn declaration_path(decl_file: &str) -> String {
    match decl_file.strip_prefix("pkg:") {
        Some(rest) => format!("node_modules/{rest}"),
        None => decl_file.to_string(),
    }
}

/// The parsed files a run's records read, by repo-relative path.
struct Sources<'r> {
    root: &'r Path,
    files: HashMap<String, Option<Parsed>>,
}

impl Sources<'_> {
    fn get(&mut self, file: &str) -> Option<&Parsed> {
        let root = self.root;
        self.files
            .entry(file.to_string())
            .or_insert_with(|| Parsed::read(&root.join(file)))
            .as_ref()
    }
}

/// A part of the record as the wire carries it: a JSON value.
fn wire_value<T: Serialize + ?Sized>(part: &T) -> serde_json::Value {
    serde_json::to_value(part).unwrap_or(serde_json::Value::Null)
}

/// What one call expression holds, as the record reads it.
#[derive(Default)]
struct CallParts {
    text: String,
    callee: String,
    constructed: bool,
    args: Vec<String>,
    /// Per argument: the literals, keys and identifiers written in it.
    in_args: Vec<Written>,
    /// The identifiers the callee writes.
    in_callee: Written,
    enclosing: Option<(String, u32)>,
    /// The arguments as written.
    arg_exprs: Vec<Expr>,
    /// The call's span.
    span: Span,
}

/// The syntax nodes that start on the site's lines (the call's line and the
/// three after it): the kinds the concept test kept (carrick#2441), each with
/// its type, line and text; calls and constructions carry their callee and
/// argument texts, an object entry its key. The call itself is not repeated
/// here: it leads the list.
struct SyntaxNodes<'p> {
    parsed: &'p Parsed,
    from: u32,
    to: u32,
    call: Span,
    nodes: Vec<SyntaxNode>,
}

/// Lines after the call's own that a node may start on.
const SYNTAX_LINES_AFTER: u32 = 3;

impl<'p> SyntaxNodes<'p> {
    fn of(parsed: &'p Parsed, file: &str, call: Span) -> Vec<SyntaxNode> {
        let from = parsed.line(call.lo.0);
        let mut found = Self {
            parsed,
            from,
            to: from + SYNTAX_LINES_AFTER,
            call,
            nodes: Vec::new(),
        };
        parsed.module.visit_with(&mut found);
        found
            .nodes
            .into_iter()
            .map(|mut node| {
                node.at = format!("{file}:{}", node.at);
                node
            })
            .collect()
    }

    /// Whether the span lies wholly outside the window, so nothing inside
    /// it can start there.
    fn outside(&self, span: Span) -> bool {
        self.parsed.line(span.hi.0) < self.from || self.parsed.line(span.lo.0) > self.to
    }

    fn keep(&self, span: Span) -> bool {
        let line = self.parsed.line(span.lo.0);
        line >= self.from && line <= self.to && span != self.call
    }

    fn push(&mut self, node: &str, span: Span) {
        if self.keep(span) {
            self.push_with(node, span, None, None, None);
        }
    }

    fn push_with(
        &mut self,
        node: &str,
        span: Span,
        callee: Option<String>,
        args: Option<Vec<String>>,
        key: Option<String>,
    ) {
        self.nodes.push(SyntaxNode {
            node: node.to_string(),
            at: self.parsed.line(span.lo.0).to_string(),
            text: self.parsed.text_of(span),
            callee,
            args,
            key,
        });
    }

    fn call(&mut self, node: &str, span: Span, callee: &Expr, args: &[ExprOrSpread]) {
        if self.keep(span) {
            let callee = self.parsed.text_of(callee.span());
            let args = args
                .iter()
                .map(|arg| self.parsed.text_of(arg.span()))
                .collect();
            self.push_with(node, span, Some(callee), Some(args), None);
        }
    }
}

impl Visit for SyntaxNodes<'_> {
    fn visit_module_item(&mut self, item: &ModuleItem) {
        if !self.outside(item.span()) {
            item.visit_children_with(self);
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        if !self.outside(stmt.span()) {
            stmt.visit_children_with(self);
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if !self.outside(expr.span()) {
            expr.visit_children_with(self);
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee {
            self.call("CallExpression", call.span, callee, &call.args);
        } else {
            self.push("CallExpression", call.span);
        }
        call.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        self.call(
            "NewExpression",
            new.span,
            &new.callee,
            new.args.as_deref().unwrap_or_default(),
        );
        new.visit_children_with(self);
    }

    fn visit_key_value_prop(&mut self, entry: &KeyValueProp) {
        if self.keep(entry.span()) {
            let key = super::prop_name(&entry.key)
                .unwrap_or_else(|| self.parsed.text_of(entry.key.span()));
            self.push_with("KeyValueProperty", entry.span(), None, None, Some(key));
        }
        entry.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, node: &MemberExpr) {
        self.push("MemberExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_str(&mut self, node: &Str) {
        self.push("StringLiteral", node.span);
    }

    fn visit_tpl(&mut self, node: &Tpl) {
        self.push("TemplateLiteral", node.span);
        node.visit_children_with(self);
    }

    fn visit_bin_expr(&mut self, node: &BinExpr) {
        self.push("BinaryExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_object_lit(&mut self, node: &ObjectLit) {
        self.push("ObjectExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        self.push("ArrowFunctionExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_fn_expr(&mut self, node: &FnExpr) {
        self.push("FunctionExpression", node.function.span);
        node.visit_children_with(self);
    }

    fn visit_if_stmt(&mut self, node: &IfStmt) {
        self.push("IfStatement", node.span);
        node.visit_children_with(self);
    }

    fn visit_switch_case(&mut self, node: &SwitchCase) {
        self.push("SwitchCase", node.span);
        node.visit_children_with(self);
    }

    fn visit_cond_expr(&mut self, node: &CondExpr) {
        self.push("ConditionalExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_assign_expr(&mut self, node: &AssignExpr) {
        self.push("AssignmentExpression", node.span);
        node.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        self.push("VariableDeclarator", node.span);
        node.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, node: &OptChainExpr) {
        self.push("OptionalChainingExpression", node.span);
        node.visit_children_with(self);
    }
}

/// The literals, object keys and identifiers an expression writes.
#[derive(Default, Clone)]
struct Written {
    strings: Vec<String>,
    templates: Vec<String>,
    keys: Vec<String>,
    idents: Vec<(String, Span)>,
}

impl Visit for Written {
    fn visit_str(&mut self, lit: &Str) {
        if let Some(text) = lit.value.as_str() {
            self.strings.push(text.to_string());
        }
    }

    fn visit_tpl(&mut self, tpl: &Tpl) {
        if tpl.exprs.is_empty()
            && let [quasi] = tpl.quasis.as_slice()
            && let Some(cooked) = quasi.cooked.as_ref().and_then(|cooked| cooked.as_str())
        {
            self.templates.push(cooked.to_string());
        }
        tpl.visit_children_with(self);
    }

    fn visit_prop(&mut self, prop: &Prop) {
        match prop {
            Prop::KeyValue(entry) => {
                if let Some(key) = super::prop_name(&entry.key) {
                    self.keys.push(key);
                }
                entry.value.visit_with(self);
                return;
            }
            Prop::Shorthand(ident) => {
                self.keys.push(ident.sym.to_string());
                self.idents.push((ident.sym.to_string(), ident.span));
                return;
            }
            _ => {}
        }
        prop.visit_children_with(self);
    }

    fn visit_member_prop(&mut self, _: &MemberProp) {}

    fn visit_ident(&mut self, ident: &Ident) {
        self.idents.push((ident.sym.to_string(), ident.span));
    }
}

/// Finds the call or construction at a span, and the call it sits inside as
/// a callback.
struct CallFinder<'p> {
    parsed: &'p Parsed,
    target: (u32, u32),
    handing: Vec<(String, u32)>,
    found: Option<CallParts>,
}

impl CallFinder<'_> {
    fn take(
        &mut self,
        span: Span,
        callee: Option<&Expr>,
        args: &[ExprOrSpread],
        constructed: bool,
    ) {
        let mut in_callee = Written::default();
        if let Some(callee) = callee {
            callee.visit_with(&mut in_callee);
        }
        self.found = Some(CallParts {
            arg_exprs: args.iter().map(|arg| (*arg.expr).clone()).collect(),
            span,
            text: self.parsed.text_of(span),
            callee: callee
                .map(|callee| self.parsed.text_of(callee.span()))
                .unwrap_or_default(),
            constructed,
            args: args
                .iter()
                .map(|arg| self.parsed.text_of(arg.span()))
                .collect(),
            in_args: args
                .iter()
                .map(|arg| {
                    let mut written = Written::default();
                    arg.expr.visit_with(&mut written);
                    written
                })
                .collect(),
            in_callee,
            enclosing: self.handing.last().cloned(),
        });
    }

    fn walk_args(&mut self, callee: String, line: u32, args: &[ExprOrSpread]) {
        for arg in args {
            let hands = matches!(&*arg.expr, Expr::Arrow(_) | Expr::Fn(_));
            if hands {
                self.handing.push((callee.clone(), line));
            }
            arg.visit_with(self);
            if hands {
                self.handing.pop();
            }
        }
    }
}

impl Visit for CallFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        let callee = match &call.callee {
            Callee::Expr(expr) => Some(&**expr),
            _ => None,
        };
        if (call.span.lo.0, call.span.hi.0) == self.target {
            self.take(call.span, callee, &call.args, false);
        }
        call.callee.visit_with(self);
        let text = callee
            .map(|c| self.parsed.text_of(c.span()))
            .unwrap_or_default();
        let line = self.parsed.line(call.span.lo.0);
        self.walk_args(text, line, &call.args);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        let args = new.args.as_deref().unwrap_or_default();
        if (new.span.lo.0, new.span.hi.0) == self.target {
            self.take(new.span, Some(&new.callee), args, true);
        }
        new.callee.visit_with(self);
        let text = self.parsed.text_of(new.callee.span());
        let line = self.parsed.line(new.span.lo.0);
        self.walk_args(text, line, args);
    }
}

/// The innermost function written over `line`: its first line, its last,
/// and the strings it writes.
struct FunctionAt {
    line: u32,
    best: Option<(u32, u32, Span)>,
}

impl FunctionAt {
    fn consider(&mut self, parsed: &Parsed, span: Span) {
        let (first, last) = (
            parsed.line(span.lo.0),
            parsed.line(span.hi.0.saturating_sub(1)),
        );
        if first <= self.line && self.line <= last {
            let narrower = self.best.is_none_or(|(a, b, _)| last - first <= b - a);
            if narrower {
                self.best = Some((first, last, span));
            }
        }
    }
}

struct FunctionFinder<'p> {
    parsed: &'p Parsed,
    at: FunctionAt,
}

impl Visit for FunctionFinder<'_> {
    fn visit_function(&mut self, function: &Function) {
        self.at.consider(self.parsed, function.span);
        function.visit_children_with(self);
    }
    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        self.at.consider(self.parsed, arrow.span);
        arrow.visit_children_with(self);
    }
    fn visit_class_method(&mut self, method: &ClassMethod) {
        self.at.consider(self.parsed, method.span);
        method.visit_children_with(self);
    }
}

/// A reached function, as the record shows it.
struct Body {
    at: String,
    code: String,
    strings: Vec<String>,
}

/// The function written over `line` of `file`.
fn body_at(sources: &mut Sources<'_>, file: &str, line: u32) -> Option<Body> {
    let parsed = sources.get(file)?;
    let mut finder = FunctionFinder {
        parsed,
        at: FunctionAt { line, best: None },
    };
    parsed.module.visit_with(&mut finder);
    let (first, last, span) = finder.at.best?;
    let last = last.min(first + MAX_CODE_LINES as u32 - 1);
    let mut written = Written::default();
    parsed.module.visit_with(&mut StringsIn {
        span,
        written: &mut written,
    });
    let mut strings: Vec<String> = written
        .strings
        .into_iter()
        .chain(written.templates)
        .filter(|text| text.chars().count() <= MAX_STATED_CHARS)
        .collect();
    strings.dedup();
    strings.truncate(MAX_STATED);
    Some(Body {
        at: format!("{file}:{first}"),
        code: parsed.numbered(first, last),
        strings,
    })
}

/// Collects the literals written inside `span`.
struct StringsIn<'w> {
    span: Span,
    written: &'w mut Written,
}

impl Visit for StringsIn<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        let span = expr.span();
        if span.lo >= self.span.lo && span.hi <= self.span.hi {
            match expr {
                Expr::Lit(Lit::Str(_)) | Expr::Tpl(_) => {
                    expr.visit_with(self.written);
                    return;
                }
                _ => {}
            }
        }
        if span.hi < self.span.lo || span.lo > self.span.hi {
            return;
        }
        expr.visit_children_with(self);
    }
}

/// The text a string literal type stands for (`"/users"` gives `/users`).
fn literal_of_type(type_text: &str) -> Option<String> {
    if type_text.starts_with('"') && type_text.ends_with('"') && type_text.len() >= 2 {
        return serde_json::from_str::<String>(type_text).ok();
    }
    None
}

/// Every text a string literal type or a union of string literal types
/// stands for (`"a" | "b"` gives `a` and `b`); none when any member is not
/// a string literal.
fn literals_of_type(type_text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = type_text.trim();
    while !rest.is_empty() {
        if !rest.starts_with('"') {
            return Vec::new();
        }
        // The closing quote: the first one no backslash escapes.
        let mut escaped = false;
        let Some(end) = rest
            .char_indices()
            .skip(1)
            .find(|&(_, c)| {
                let closes = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                closes
            })
            .map(|(index, _)| index)
        else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<String>(&rest[..=end]) else {
            return Vec::new();
        };
        values.push(value);
        rest = rest[end + 1..].trim_start();
        match rest.strip_prefix('|') {
            Some(after) => rest = after.trim_start(),
            None if rest.is_empty() => {}
            None => return Vec::new(),
        }
    }
    values
}

/// Whether a row's declaration file is a file of the repo, whose text the
/// scanner may slice (not the default library, not a package).
fn in_repo(decl_file: &str) -> bool {
    !(decl_file.starts_with("lib:")
        || decl_file.starts_with("pkg:")
        || decl_file.split('/').any(|part| part == "node_modules"))
}

/// The declaration at `line` of the repo file `file`, as one line with its
/// keywords off, at most [`MAX_DECLARATION_CHARS`].
fn declaration_text(sources: &mut Sources<'_>, file: &str, line: u32) -> Option<String> {
    let parsed = sources.get(file)?;
    let mut text = parsed.line_text(line)?.trim();
    loop {
        let before = text;
        for keyword in [
            "export ",
            "default ",
            "declare ",
            "const ",
            "let ",
            "var ",
            "readonly ",
        ] {
            text = text.strip_prefix(keyword).unwrap_or(text).trim_start();
        }
        if text == before {
            break;
        }
    }
    let text = text.trim_end_matches(';').trim_end();
    (!text.is_empty()).then(|| cap(text, MAX_DECLARATION_CHARS))
}

/// Each identifier among `idents` as its row names it.
fn compiler_names(
    sources: &mut Sources<'_>,
    positions: Option<&PositionIndex>,
    root: &Path,
    file: &str,
    idents: &[(String, Span)],
) -> Vec<CompilerName> {
    let Some(positions) = positions else {
        return Vec::new();
    };
    let mut names: Vec<CompilerName> = Vec::new();
    for (text, span) in idents {
        if names.iter().any(|name| &name.text == text) {
            continue;
        }
        let Ok(row) = positions.lookup(&root.join(file), span.lo.0, span.hi.0, Kind::Ident) else {
            continue;
        };
        let line = sources.get(file).map_or(0, |parsed| parsed.line(span.lo.0));
        let declaration = match (row.decl_file(), row.decl_line()) {
            (Some(decl), Some(at)) => Some(format!("{}:{at}", declaration_path(decl))),
            (Some(decl), None) => Some(declaration_path(decl)),
            _ => None,
        };
        let declaration_text = match (row.decl_file(), row.decl_line()) {
            (Some(decl), Some(at)) if in_repo(decl) => declaration_text(sources, decl, at),
            _ => None,
        };
        let type_text = row.7.clone();
        names.push(CompilerName {
            text: text.clone(),
            at: format!("{file}:{line}"),
            declaration,
            declaration_text,
            package: row.decl_package().map(str::to_string),
            literal: type_text.as_deref().and_then(literal_of_type),
            type_text,
        });
    }
    names
}

/// The stated values, kept unique and capped as the wire takes them.
#[derive(Default)]
struct StatedBuilder {
    names: Vec<StatedValue>,
    methods: Vec<StatedValue>,
}

impl StatedBuilder {
    fn add(list: &mut Vec<StatedValue>, text: &str, place: Place) {
        if text.is_empty() || text.chars().count() > MAX_STATED_CHARS {
            return;
        }
        if let Some(known) = list.iter_mut().find(|value| value.text == text) {
            if known.places.len() < MAX_PLACES && !known.places.contains(&place) {
                known.places.push(place);
            }
            return;
        }
        if list.len() < MAX_STATED {
            list.push(StatedValue {
                text: text.to_string(),
                places: vec![place],
            });
        }
    }

    /// A value the code states: a method word only as a method.
    fn value(&mut self, text: &str, place: Place) {
        if is_http_method(text) {
            Self::add(&mut self.methods, text, place);
        } else {
            Self::add(&mut self.names, text, place);
        }
    }

    fn method(&mut self, text: &str, place: Place) {
        Self::add(&mut self.methods, text, place);
    }
}

fn place(from: PlaceFrom, at: &str) -> Place {
    Place {
        from,
        at: at.to_string(),
        name: None,
        via: None,
        export: None,
        member: None,
    }
}

/// The kind and role a label's role and op force, where they force one.
pub(super) fn kind_and_role(
    role: &str,
    op: Option<&str>,
) -> (Option<&'static str>, Option<&'static str>) {
    let kind = match role {
        "http_client" | "server_framework" => Some("http"),
        "socket" => Some("socket_event"),
        // A broker carries topics and job or message names alike, and an
        // in-process channel events or messages (a send to a forked child
        // process is a message): the label does not say which.
        _ => None,
    };
    let side = match op {
        Some("request" | "send") => Some("sends"),
        Some("receive") => Some("handles"),
        _ => None,
    };
    (kind, side)
}

/// The service's labelled network roles by the files that use them. A site
/// only the receiver trace finds (carrick#2474 R1, reached by its member's
/// declaration) is left out while the trace is record-only, so no state
/// found without it moves.
fn roles_by_file(decided: &Decided) -> BTreeMap<String, BTreeSet<String>> {
    let mut roles: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for site in decided
        .sites
        .iter()
        .filter(|site| site.reached != Some(super::discovery::ReachedBy::Declaration))
    {
        roles
            .entry(site.end.role.clone())
            .or_default()
            .insert(site.file.clone());
    }
    roles
}

/// A record for every decided site, read with the store's `labels`. Files
/// are named relative to `root`, the scanned repo's root; the compiler's
/// rows are `inputs.positions`, when the run walked them.
pub fn site_records(
    inputs: &RequestSummaryInputs,
    root: &Path,
    decided: &Decided,
    labels: &[SiteLabel],
) -> Vec<SiteRecord> {
    let mut sources = Sources {
        root,
        files: HashMap::new(),
    };
    let roles = roles_by_file(decided);
    decided
        .sites
        .iter()
        .filter_map(|site| {
            let label = SiteLabel::find(labels, &site.end)?;
            record(
                &mut sources,
                inputs.positions.as_ref(),
                root,
                site,
                (label, labels),
                &roles,
            )
        })
        .collect()
}

fn record(
    sources: &mut Sources<'_>,
    positions: Option<&PositionIndex>,
    root: &Path,
    site: &DecidedSite,
    (label, labels): (&SiteLabel, &[SiteLabel]),
    roles: &BTreeMap<String, BTreeSet<String>>,
) -> Option<SiteRecord> {
    let file = site.file.as_str();
    let call_line = {
        let parsed = sources.get(file)?;
        parsed.line(site.span_start)
    };
    let at = format!("{file}:{}", site.line);
    let call_at = format!("{file}:{call_line}");
    let parts = {
        let parsed = sources.get(file)?;
        let mut finder = CallFinder {
            parsed,
            target: (site.span_start, site.span_end),
            handing: Vec::new(),
            found: None,
        };
        parsed.module.visit_with(&mut finder);
        finder.found?
    };
    let point = {
        let parsed = sources.get(file)?;
        Point {
            at: at.clone(),
            code: parsed.numbered(
                site.line.saturating_sub(CONTEXT_BEFORE).max(1),
                site.line + CONTEXT_AFTER,
            ),
        }
    };

    // The compiler's names on the call.
    let mut idents: Vec<(String, Span)> = parts.in_callee.idents.clone();
    for written in &parts.in_args {
        idents.extend(written.idents.iter().cloned());
    }
    let names = compiler_names(sources, positions, root, file, &idents);
    // The literals a name's type stands for: one, or each of a union's.
    let literals = |ident: &str| -> Vec<String> {
        names
            .iter()
            .find(|name| name.text == ident)
            .and_then(|name| name.type_text.as_deref())
            .map(literals_of_type)
            .unwrap_or_default()
    };

    // The functions the call runs, two levels down.
    // Each is the function the call above enters, read at the call inside
    // it that goes on.
    let mut reached: Vec<Reached> = Vec::new();
    let mut via_text = parts.callee.clone();
    for (level, hop) in site.via.iter().take(2).enumerate() {
        let Some((hop_file, hop_line)) = hop.at.rsplit_once(':') else {
            break;
        };
        let Ok(hop_line) = hop_line.parse::<u32>() else {
            break;
        };
        let Some(body) = body_at(sources, hop_file, hop_line) else {
            break;
        };
        reached.push(Reached {
            level: level as u8 + 1,
            via: via_text.clone(),
            function: body.at,
            code: body.code,
            names: Vec::new(),
            strings: body.strings,
        });
        via_text = hop.callee.clone();
    }
    let callee_row = positions.and_then(|positions| {
        positions
            .lookup(&root.join(file), site.span_start, site.span_end, Kind::Call)
            .ok()
    });
    let callees = vec![CompilerCallee {
        text: parts.callee.clone(),
        declaration: match callee_row.and_then(|row| row.decl_file().zip(row.decl_line())) {
            Some((decl, line)) => Some(format!("{}:{line}", declaration_path(decl))),
            None => reached.first().map(|body| body.function.clone()),
        },
        package: match &reached.first() {
            Some(_) => None,
            None => Some(label.package.clone()),
        },
        code: reached.first().map(|body| body.code.clone()),
    }];

    // The values the code states.
    let mut stated = StatedBuilder::default();
    // The whole values the name may take: a value is a candidate only when
    // the code states all of it, never one literal piece of a joined name.
    // `None` where any value it may take is not stated whole.
    let name_exprs: Vec<&Expr> = site
        .name_args
        .iter()
        .filter_map(|index| parts.arg_exprs.get(*index))
        .collect();
    let whole_names: Option<BTreeSet<String>> = match (&site.key, &label.name) {
        (Some(key), _) => {
            stated.value(key, place(PlaceFrom::ObjectKey, &at));
            Some(BTreeSet::from([key.clone()]))
        }
        (None, Some(_)) => {
            if let Some(text) = literal_text(&site.name) {
                stated.value(&text, place(PlaceFrom::StringLiteral, &at));
            } else if let Some(text) = env_based_text(&site.name) {
                stated.value(&text, place(PlaceFrom::TemplateLiteral, &at));
            }
            // Else, for HTTP, the target the request summaries compose from
            // the same pieces (carrick#2519): a base, literal segments, and
            // each parameter or unread value a whole segment.
            stated_whole(sources.get(file), &site.name, &name_exprs, &literals).or_else(|| {
                let text = site.name_text()?;
                stated.value(&text, place(PlaceFrom::TemplateLiteral, &at));
                Some(BTreeSet::from([text]))
            })
        }
        (None, None) => None,
    };
    for written in &parts.in_args {
        for text in &written.strings {
            stated.value(text, place(PlaceFrom::StringLiteral, &call_at));
        }
        for text in &written.templates {
            stated.value(text, place(PlaceFrom::TemplateLiteral, &call_at));
        }
    }
    // A constant's literal, or each literal of a name typed as their union.
    for name in &names {
        for literal in name
            .type_text
            .as_deref()
            .map(literals_of_type)
            .unwrap_or_default()
        {
            let mut checker = place(PlaceFrom::Checker, &name.at);
            checker.name = Some(name.text.clone());
            stated.value(&literal, checker);
        }
    }
    for written in &parts.in_args {
        for key in &written.keys {
            stated.value(key, place(PlaceFrom::ObjectKey, &call_at));
        }
    }
    for body in &reached {
        for text in &body.strings {
            let mut inside = place(PlaceFrom::StringLiteral, &body.function);
            inside.via = Some(body.via.clone());
            stated.value(text, inside);
        }
    }
    let http = matches!(label.role.as_str(), "http_client" | "server_framework");
    let method_label = label.method.clone().unwrap_or_default();
    let label_place = |from: PlaceFrom| Place {
        from,
        at: label.package.clone(),
        name: None,
        via: None,
        export: Some(label.export.clone()),
        member: label.member.clone(),
    };
    if http {
        if let Some(literal) = &method_label.literal {
            stated.method(literal, label_place(PlaceFrom::LabelMethod));
        }
        if let Some(default) = &method_label.default {
            stated.method(default, label_place(PlaceFrom::LabelMethodDefault));
        }
    }

    // The slot rule.
    let mut asked: Vec<Slot> = Vec::new();
    let mut forced: BTreeMap<Slot, String> = BTreeMap::new();
    let mut basis: BTreeMap<Slot, Basis> = BTreeMap::new();
    // One stated value forces a slot; several ask it; none leaves it
    // unstated, neither forced nor asked. A slot chosen among a closed set
    // of options (kind, role, method) is asked when nothing forces it.
    let mut decide =
        |slot: Slot, values: Vec<String>, closed: bool, from: Basis| match values.as_slice() {
            [] if !closed => {}
            [one] => {
                forced.insert(slot, one.clone());
                basis.insert(slot, from);
            }
            _ => asked.push(slot),
        };
    let (kind, role) = kind_and_role(&label.role, label.op.as_deref());
    decide(
        Slot::Kind,
        kind.map(str::to_string).into_iter().collect(),
        true,
        Basis::Label,
    );
    decide(
        Slot::Role,
        role.map(str::to_string).into_iter().collect(),
        true,
        Basis::Label,
    );
    // The name: forced only when the code states one whole value; asked
    // when it states several; a lead when it states none whole. A label
    // that names no name slot forces neither the name nor the argument it
    // is read from: both are asked.
    let name_unstated = match (&label.name, &whole_names) {
        (None, _) => {
            decide(Slot::Name, Vec::new(), true, Basis::Code);
            if site.key.is_none() {
                decide(Slot::NameArgument, Vec::new(), true, Basis::Label);
            }
            false
        }
        (Some(_), Some(values)) if !values.is_empty() => {
            decide(
                Slot::Name,
                values.iter().cloned().collect(),
                false,
                Basis::Code,
            );
            false
        }
        (Some(_), _) => true,
    };
    if site.key.is_none() {
        if label.name.is_some() {
            decide(
                Slot::NameArgument,
                site.name_args.iter().map(usize::to_string).collect(),
                false,
                Basis::Label,
            );
        }
        // Built below the call, no payload at it, or no payload slot in the
        // label: no argument of this call is stated as one.
        if let (Some(_), Some(PayloadBinding::Arg { arg, .. })) = (&label.payload, &site.payload) {
            decide(
                Slot::PayloadArgument,
                vec![arg.to_string()],
                false,
                Basis::Label,
            );
        }
    }
    if http {
        // Read at the label's method slot: the one method the member always
        // sends, the default where nothing writes the slot, or the whole
        // values the code writes there. Forced only when that is a single
        // request method; asked otherwise.
        let methods: Option<BTreeSet<String>> = match (&method_label.literal, &method_label.from) {
            (Some(literal), _) => Some(BTreeSet::from([literal.clone()])),
            (None, Some(_)) => match &site.method {
                Some(MethodValue::Unwritten) => method_label
                    .default
                    .clone()
                    .map(|default| BTreeSet::from([default])),
                Some(MethodValue::Pieces(pieces)) => {
                    let exprs: Vec<&Expr> = parts.arg_exprs.iter().collect();
                    stated_whole(sources.get(file), pieces, &exprs, &literals)
                }
                None => None,
            },
            (None, None) => method_label
                .default
                .clone()
                .map(|default| BTreeSet::from([default])),
        };
        let methods: Vec<String> = methods
            .unwrap_or_default()
            .into_iter()
            .map(|method| method.to_ascii_uppercase())
            .collect();
        // A method the code writes at the slot is the code's; the one the
        // member always sends, or its default, is the label's.
        let method_basis = match (&method_label.literal, &site.method) {
            (None, Some(MethodValue::Pieces(_))) => Basis::Code,
            _ => Basis::Label,
        };
        match methods.as_slice() {
            [one] if is_http_method(one) => {
                decide(Slot::Method, methods.clone(), true, method_basis)
            }
            _ => decide(Slot::Method, Vec::new(), true, method_basis),
        }
    }
    asked.sort();
    asked.dedup();

    let bucket = if name_unstated {
        Bucket::Lead
    } else if asked.is_empty() {
        Bucket::Forced
    } else {
        Bucket::Asked
    };

    let package_network_roles: Vec<RoleFiles> = roles
        .iter()
        .filter_map(|(role, files)| {
            let others = files.iter().filter(|other| *other != file).count();
            (others > 0).then(|| RoleFiles {
                role: role.clone(),
                files: others,
            })
        })
        .collect();

    let call_text = parts.text.clone();
    // The call, then each object entry written in its arguments.
    let mut syntax = vec![SyntaxNode {
        node: if parts.constructed {
            "NewExpression"
        } else {
            "CallExpression"
        }
        .to_string(),
        at: call_at.clone(),
        text: call_text.clone(),
        callee: Some(parts.callee.clone()),
        args: Some(parts.args.clone()),
        key: None,
    }];
    if let Some(parsed) = sources.get(file) {
        let found = SyntaxNodes::of(parsed, file, parts.span);
        syntax.extend(found.into_iter().take(MAX_SYNTAX_NODES - 1));
    }
    // Every op the store labels on the site's export.
    let argument_labels: Vec<ArgumentLabel> = labels
        .iter()
        .filter(|other| other.package == label.package && other.export == label.export)
        .filter_map(|other| {
            Some(ArgumentLabel {
                export: other.export.clone(),
                member: other.member.clone(),
                does: other.op.clone()?,
                name_argument: other.name.as_ref().map(|slot| slot.arg),
                name_key: other.name.as_ref().and_then(|slot| slot.key.clone()),
                payload_argument: other.payload.as_ref().map(|slot| slot.arg),
            })
        })
        .take(MAX_ARGUMENT_LABELS)
        .collect();
    let state = SiteState {
        point,
        compiler: wire_value(&Compiler {
            names,
            callees,
            enclosing: parts.enclosing.map(|(text, line)| Enclosing {
                text,
                at: format!("{file}:{line}"),
            }),
            reached,
        }),
        syntax: wire_value(&syntax),
        libraries: wire_value(&[Library {
            package: label.package.clone(),
            labels: vec![LibraryLabel {
                export: label.export.clone(),
                role: label.role.clone(),
                side: label.side.clone(),
                http: match (&label.name, http) {
                    (Some(path), true) => vec![HttpLabel {
                        member: label.member.clone(),
                        method: method_label.literal.clone(),
                        path_from: path.clone(),
                        method_from: method_label.from.clone(),
                        method_default: method_label.default.clone(),
                    }],
                    _ => Vec::new(),
                },
            }],
            argument_labels,
            in_store: true,
        }]),
        package_network_roles: (!package_network_roles.is_empty())
            .then(|| wire_value(&package_network_roles)),
        call: Some(SiteCall {
            text: call_text,
            at: call_at,
            args: parts.args,
        }),
        stated: Stated {
            names: stated.names,
            methods: stated.methods,
        },
        asked,
        forced,
    };
    let payload_at = match &site.payload {
        Some(PayloadBinding::Arg { arg, key }) => parts
            .arg_exprs
            .get(*arg)
            .and_then(|expr| payload_expr(expr, key.as_deref()))
            .and_then(|span| {
                let parsed = sources.get(file)?;
                Some(AnchorAt {
                    file: file.to_string(),
                    line: parsed.line(span.lo.0),
                    span_start: span.lo.0,
                    span_end: span.hi.0,
                })
            }),
        _ => None,
    };
    let carried = site.payload_slot.as_ref().and_then(|binding| {
        let (arg, key) = match binding {
            SlotBinding::Written { arg, key } | SlotBinding::Unwritten { arg, key } => (*arg, key),
        };
        let expr = parts.arg_exprs.get(arg)?;
        let parsed = sources.get(file)?;
        let at = |span: Span| AnchorAt {
            file: file.to_string(),
            line: parsed.line(span.lo.0),
            span_start: span.lo.0,
            span_end: span.hi.0,
        };
        match binding {
            SlotBinding::Written { .. } => {
                payload_expr(expr, Some(key)).map(|span| PayloadAt::Value(at(span)))
            }
            SlotBinding::Unwritten { .. } => match expr {
                Expr::Object(_) => Some(PayloadAt::Unwritten {
                    at: at(expr.span()),
                    key: key.clone(),
                }),
                _ => None,
            },
        }
    });
    let handler = site.handler.clone().map(
        |HandlerAt {
             file,
             line,
             span_start,
             span_end,
             ..
         }| {
            let param_name = sources
                .get(&file)
                .and_then(|parsed| first_param_at(parsed, span_start, span_end));
            HandlerAnchor {
                at: AnchorAt {
                    file,
                    line,
                    span_start,
                    span_end,
                },
                // The payload a handler is run with arrives in its first
                // parameter.
                param: 0,
                param_name,
            }
        },
    );
    let anchors = TypeAnchors {
        call: AnchorAt {
            file: file.to_string(),
            line: call_line,
            span_start: site.span_start,
            span_end: site.span_end,
        },
        payload: site.payload.clone(),
        payload_at,
        handler,
        key: site.key.clone(),
    };
    let payloads = payload_slots(
        &anchors.call,
        anchors.payload_at.as_ref(),
        carried,
        anchors.handler.as_ref(),
    );
    Some(SiteRecord {
        state,
        anchors,
        payloads,
        basis,
        bucket,
        lead: (bucket == Bucket::Lead).then_some("name_unstated"),
        receiver: site.receiver.clone(),
    })
}

/// The span of a payload written at an argument: the argument itself, or
/// with `key` the value its object literal gives that key. `None` for a key
/// the argument does not write as a literal property.
fn payload_expr(expr: &Expr, key: Option<&str>) -> Option<Span> {
    let Some(key) = key else {
        return Some(expr.span());
    };
    let Expr::Object(object) = expr else {
        return None;
    };
    object.props.iter().find_map(|prop| match prop {
        PropOrSpread::Prop(prop) => match &**prop {
            Prop::KeyValue(pair) if prop_key_text(&pair.key).as_deref() == Some(key) => {
                Some(pair.value.span())
            }
            Prop::Shorthand(name) if name.sym.as_ref() == key => Some(name.span),
            _ => None,
        },
        PropOrSpread::Spread(_) => None,
    })
}

fn prop_key_text(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(name) => Some(name.sym.to_string()),
        PropName::Str(text) => text.value.as_str().map(str::to_string),
        _ => None,
    }
}

/// The first parameter, as written, of the function whose span is
/// `start..end` in `parsed`: a name, or a destructuring pattern without its
/// annotation.
fn first_param_at(parsed: &Parsed, start: u32, end: u32) -> Option<String> {
    struct Finder<'p> {
        parsed: &'p Parsed,
        target: (u32, u32),
        found: Option<String>,
    }
    impl Finder<'_> {
        fn written(&self, pat: &Pat) -> Option<String> {
            let text = match pat {
                Pat::Ident(binding) => binding.id.sym.to_string(),
                Pat::Object(_) => {
                    let raw = self.parsed.raw(pat.span());
                    raw.get(..=raw.rfind('}')?)?.to_string()
                }
                Pat::Array(_) => {
                    let raw = self.parsed.raw(pat.span());
                    raw.get(..=raw.rfind(']')?)?.to_string()
                }
                Pat::Assign(assign) => return self.written(&assign.left),
                _ => return None,
            };
            Some(text)
        }
    }
    impl Visit for Finder<'_> {
        fn visit_function(&mut self, function: &Function) {
            if (function.span.lo.0, function.span.hi.0) == self.target {
                self.found = function
                    .params
                    .first()
                    .and_then(|param| self.written(&param.pat));
            }
            function.visit_children_with(self);
        }
        fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
            if (arrow.span.lo.0, arrow.span.hi.0) == self.target {
                self.found = arrow.params.first().and_then(|param| self.written(param));
            }
            arrow.visit_children_with(self);
        }
    }
    let mut finder = Finder {
        parsed,
        target: (start, end),
        found: None,
    };
    parsed.module.visit_with(&mut finder);
    finder.found
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::decided_sites::{DiscoveryFile, decided_sites, tests::manifest};
    use super::*;
    use crate::position_rows::{FLAG_ANY, FileRows, Header, Row, RowsFile, content_hash};

    const WEB: &str = "services/web/src/";

    /// An identifier row at the first `needle` in `text`, offset `skip` into
    /// it, `len` long.
    fn ident_row(
        text: &str,
        needle: &str,
        ident: &str,
        decl: Option<(&str, u32)>,
        type_text: &str,
        flags: u8,
    ) -> Row {
        let start = (text.find(needle).unwrap() + needle.find(ident).unwrap()) as u32;
        Row(
            Kind::Ident,
            start,
            start + ident.len() as u32,
            decl.map(|_| ident.to_string()),
            decl.map(|(file, _)| file.to_string()),
            decl.map(|(_, line)| line),
            None,
            Some(type_text.to_string()),
            flags,
        )
    }

    /// The discovery fixture with rows for `choices.ts`, as a flag-on walk
    /// writes them: the two constants and the sibling package's path type
    /// as literals, the undeclared import's name as `any`.
    fn records_with_rows(repo: &Path) -> (Vec<SiteRecord>, Decided) {
        records_after(repo, None)
    }

    /// As [`records_with_rows`], with discovery's records for `shifted`
    /// (a file, and the lines and bytes inserted at its top) moved as a
    /// discovery run over the edited file would write them.
    fn records_after(repo: &Path, shifted: Option<(&str, u32, u32)>) -> (Vec<SiteRecord>, Decided) {
        let (_, mut inputs, mut sites, labels) = discovery_fixture_at(repo);
        if let Some((file, lines, bytes)) = shifted {
            for site in &mut sites {
                if site.file == file {
                    site.line += lines;
                    site.span_start += bytes;
                    site.span_end += bytes;
                }
                if let Some(end) = site.end.as_mut().filter(|end| end.file == file) {
                    end.line += lines;
                    end.span_start += bytes;
                    end.span_end += bytes;
                }
                for hop in site.chain.iter_mut().filter(|hop| hop.file == file) {
                    hop.line += lines;
                }
            }
        }
        let choices = "services/web/src/choices.ts";
        let text = std::fs::read_to_string(repo.join(choices)).unwrap();
        let rows = vec![
            ident_row(
                &text,
                "api(USERS_PATH)",
                "USERS_PATH",
                Some(("packages/shared/src/index.ts", 1)),
                "\"/users\"",
                0,
            ),
            ident_row(
                &text,
                "useV2 ? PATH_V2",
                "PATH_V2",
                Some((choices, 7)),
                "\"/v2/report\"",
                0,
            ),
            ident_row(
                &text,
                ": PATH_V1)",
                "PATH_V1",
                Some((choices, 6)),
                "\"/v1/report\"",
                0,
            ),
            ident_row(
                &text,
                "emit(EVENT_NAME",
                "EVENT_NAME",
                None,
                "any",
                FLAG_ANY,
            ),
        ];
        let mut index = PositionIndex::of(
            repo,
            RowsFile {
                header: Header {
                    v: crate::position_rows::FORMAT_VERSION,
                    typescript: "6.0.0".into(),
                    service: "web".into(),
                    files: 1,
                },
                files: vec![FileRows {
                    f: choices.into(),
                    h: Some(content_hash(text.as_bytes())),
                    skip: None,
                    r: rows,
                }],
            },
        );
        index.bind(&repo.join(choices), &text);
        inputs.positions = Some(index);
        let decided = decided_sites(&inputs, repo, &sites, &labels);
        let records = site_records(&inputs, repo, &decided, &labels);
        (records, decided)
    }

    fn discovery_fixture_at(
        repo: &Path,
    ) -> (
        PathBuf,
        RequestSummaryInputs,
        Vec<super::super::decided_sites::DiscoverySite>,
        Vec<SiteLabel>,
    ) {
        let fixture = manifest().join("tests/fixtures/discovery");
        let service = crate::config::Config::load_services(vec![repo.join("carrick.json")])
            .unwrap()
            .remove(0);
        let inputs = crate::engine::discover_service_inputs(
            repo,
            &service,
            Some(PositionIndex::missing(repo)),
        );
        let text = std::fs::read_to_string(fixture.join("sites.expected.json")).unwrap();
        let sites = serde_json::from_str::<DiscoveryFile>(&text).unwrap().sites;
        let answer: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        )
        .unwrap();
        // The method fields label v2 adds (carrick#2434), a cassette until
        // the store's rebuild.
        let methods: Vec<SiteLabel> = serde_json::from_str(
            &std::fs::read_to_string(
                manifest().join("tests/fixtures/decided-sites/method-labels.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut labels = SiteLabel::from_store_answer(&answer);
        for label in &mut labels {
            if let Some(with) = methods.iter().find(|with| {
                with.package == label.package
                    && with.export == label.export
                    && with.member == label.member
            }) {
                label.method = with.method.clone();
            }
        }
        (repo.to_path_buf(), inputs, sites, labels)
    }

    fn repo() -> PathBuf {
        std::fs::canonicalize(manifest().join("tests/fixtures/discovery/repo")).unwrap()
    }

    /// The state's `compiler` part, read back from the wire value.
    fn compiler_of(record: &SiteRecord) -> Compiler {
        record.compiler().unwrap()
    }

    fn record<'r>(records: &'r [SiteRecord], at: &str) -> &'r SiteRecord {
        records
            .iter()
            .find(|record| record.state.point.at == format!("{WEB}{at}"))
            .unwrap_or_else(|| {
                panic!(
                    "{at} in {:?}",
                    records
                        .iter()
                        .map(|r| &r.state.point.at)
                        .collect::<Vec<_>>()
                )
            })
    }

    /// An event name from an import the scan cannot read is one possible
    /// call, labelled with the library call it reaches, and no row: the
    /// other sites of the file are not possible calls (carrick#2438).
    #[test]
    fn a_name_from_an_unreadable_import_is_one_possible_call_and_no_row() {
        let (records, decided) = records_with_rows(&repo());
        let found = crate::possible_calls::from_sites(&decided, &records);
        let in_choices: Vec<_> = found
            .iter()
            .filter(|call| call.file_path.ends_with("choices.ts"))
            .collect();
        assert_eq!(in_choices.len(), 1, "{in_choices:?}");
        let call = in_choices[0];
        let fire = record(&records, "choices.ts:18");
        assert_eq!(fire.bucket, Bucket::Lead);
        assert_eq!(call.line_number, fire.anchors.call.line);
        assert_eq!(call.span_start, fire.anchors.call.span_start);
        assert_eq!(call.file_path, format!("{WEB}choices.ts"));
        assert!(!call.file_path.starts_with('/'), "{call:?}");
        assert_eq!(
            call.unread.reason,
            crate::possible_calls::UnreadReason::UnresolvedImport
        );
        assert_eq!(call.unread.slot, "name");
        assert_eq!(call.kind.as_deref(), Some("socket"), "{call:?}");
        assert_eq!(call.role.as_deref(), Some("producer"), "{call:?}");
        assert!(call.labelled_end.is_some(), "{call:?}");
        // No row: the lead's record states no name, and the forced and
        // asked sites of the file are not possible calls.
        assert!(!fire.state.forced.contains_key(&Slot::Name));
        for bucket_site in ["choices.ts:10", "choices.ts:14"] {
            let line = record(&records, bucket_site).anchors.call.line;
            assert!(in_choices.iter().all(|c| c.line_number != line));
        }
    }

    /// Each bucket: a path constant from a sibling package is one stated
    /// value (forced); a flag choosing between two constants is asked; an
    /// event name from an import that cannot be read is a lead.
    #[test]
    fn each_site_falls_in_its_bucket() {
        let (records, _) = records_with_rows(&repo());
        let users = record(&records, "choices.ts:10");
        assert_eq!(users.bucket, Bucket::Forced, "{:?}", users.state);
        assert_eq!(
            users.state.forced.get(&Slot::Name).map(String::as_str),
            Some("/users")
        );
        assert_eq!(
            users.state.forced.get(&Slot::Kind).map(String::as_str),
            Some("http")
        );
        assert_eq!(
            users.state.forced.get(&Slot::Method).map(String::as_str),
            Some("GET")
        );
        assert!(users.state.asked.is_empty(), "{:?}", users.state.asked);

        let report = record(&records, "choices.ts:14");
        assert_eq!(report.bucket, Bucket::Asked);
        assert_eq!(report.state.asked, vec![Slot::Name]);
        let offered: Vec<&str> = report
            .state
            .stated
            .names
            .iter()
            .map(|v| v.text.as_str())
            .collect();
        assert!(
            offered.contains(&"/v1/report") && offered.contains(&"/v2/report"),
            "{offered:?}"
        );

        let fire = record(&records, "choices.ts:18");
        assert_eq!(fire.bucket, Bucket::Lead);
        assert_eq!(fire.lead, Some("name_unstated"));

        // A socket send whose caller writes the name: every slot forced,
        // the payload at the caller's argument 1.
        let joined = record(&records, "emit.ts:13");
        assert_eq!(joined.bucket, Bucket::Forced, "{:?}", joined.state.asked);
        assert_eq!(
            joined.state.forced.get(&Slot::Name).map(String::as_str),
            Some("user.joined")
        );
        assert_eq!(
            joined.state.forced.get(&Slot::Kind).map(String::as_str),
            Some("socket_event")
        );
        assert_eq!(
            joined
                .state
                .forced
                .get(&Slot::NameArgument)
                .map(String::as_str),
            Some("0")
        );
        assert_eq!(
            joined
                .state
                .forced
                .get(&Slot::PayloadArgument)
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            joined.anchors.payload,
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );

        // A broker's kind is not the label's to force (a topic, a job or a
        // message name): asked, with the name forced.
        let order = record(&records, "events.ts:6");
        assert_eq!(order.state.asked, vec![Slot::Kind]);
        assert_eq!(
            order.state.forced.get(&Slot::Name).map(String::as_str),
            Some("orders.created")
        );

        // A channel whose label names no name slot: the name is asked among
        // what the payload states.
        let bus = record(&records, "bus.ts:7");
        assert_eq!(bus.bucket, Bucket::Asked);
        assert!(bus.state.asked.contains(&Slot::Name));
        // A send to a forked child process is a message by the kind
        // criteria, not an event: the label does not say which.
        assert!(bus.state.asked.contains(&Slot::Kind), "{:?}", bus.state);

        // A handler table's key: the name is the key, the anchor its
        // handler.
        let resize = record(&records, "job-table.ts:5");
        assert_eq!(
            resize.state.forced.get(&Slot::Name).map(String::as_str),
            Some("image.resize")
        );
        assert_eq!(resize.anchors.handler.as_ref().map(|h| h.at.line), Some(5));
    }

    /// A handler table registered in a loop gives every key the loop's call
    /// span; each key's record finds its own site by the key, so each lands
    /// at its own handler's line instead of the first key's (carrick#2730).
    /// The table's kind is asked and unanswered here, so the three keys are
    /// possible calls; the rows case is `site_rows::of`'s on the same lookup.
    #[test]
    fn each_key_of_a_handler_table_finds_its_own_site() {
        let (records, decided) = records_with_rows(&repo());
        let (_, _, _, labels) = discovery_fixture_at(&repo());
        let rows = crate::site_rows::of(&decided, &records, &labels, &Default::default());
        let mut lines: Vec<u32> = rows
            .leads
            .iter()
            .filter(|lead| lead.file_path.ends_with("job-table.ts"))
            .map(|lead| lead.line_number)
            .collect();
        lines.sort();
        assert_eq!(lines, vec![5, 9, 13], "{:?}", rows.leads);
    }

    /// The compiler's names carry the declaration text sliced from the
    /// repo's own file and the literal a constant stands for.
    #[test]
    fn a_name_carries_its_declaration_and_literal() {
        let (records, _) = records_with_rows(&repo());
        let report = record(&records, "choices.ts:14");
        let v2 = compiler_of(report)
            .names
            .into_iter()
            .find(|name| name.text == "PATH_V2")
            .unwrap();
        assert_eq!(
            v2.declaration.as_deref(),
            Some("services/web/src/choices.ts:7")
        );
        assert_eq!(
            v2.declaration_text.as_deref(),
            Some("PATH_V2 = \"/v2/report\"")
        );
        assert_eq!(v2.literal.as_deref(), Some("/v2/report"));

        // The bodies the site's calls run, two levels down, each entered by
        // the callee the level above writes.
        let things = record(&records, "pages/things.ts:4");
        let things_compiler = compiler_of(things);
        let reached: Vec<(u8, &str, &str)> = things_compiler
            .reached
            .iter()
            .map(|body| (body.level, body.via.as_str(), body.function.as_str()))
            .collect();
        assert_eq!(
            reached,
            vec![
                (1, "v1", "services/web/src/api.ts:5"),
                (2, "api", "services/web/src/api.ts:1")
            ]
        );
        assert_eq!(things_compiler.reached[0].strings, vec!["/v1"]);
        assert_eq!(
            things.state.forced.get(&Slot::Name).map(String::as_str),
            Some("/v1/things")
        );
    }

    /// Every record is under 16 KB, and none names the checkout or the
    /// home directory.
    #[test]
    fn records_are_small_and_name_no_absolute_path() {
        let repo = repo();
        let (records, _) = records_with_rows(&repo);
        assert!(records.len() >= 15, "{}", records.len());
        let home = std::env::var("HOME").unwrap_or_default();
        for record in &records {
            assert!(
                record.state_bytes() < 16 * 1024,
                "{}",
                record.state.point.at
            );
            let text = serde_json::to_string(record).unwrap();
            assert!(!text.contains(&*repo.to_string_lossy()), "{text}");
            assert!(home.is_empty() || !text.contains(&home), "{text}");
            assert!(
                !text.contains("\"/Users/") && !text.contains("\"/home/"),
                "{text}"
            );
        }
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    /// Two checkouts of one tree give equal records.
    #[test]
    fn two_checkouts_give_equal_records() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let mut states = Vec::new();
        for dir in [&one, &two] {
            let repo = std::fs::canonicalize(dir.path()).unwrap().join("checkout");
            copy_tree(&repo_source(), &repo);
            let (records, _) = records_with_rows(&repo);
            states.push(
                records
                    .iter()
                    .map(|record| serde_json::to_string(record).unwrap())
                    .collect::<Vec<_>>(),
            );
        }
        assert!(!states[0].is_empty());
        assert_eq!(states[0], states[1]);
    }

    fn repo_source() -> PathBuf {
        manifest().join("tests/fixtures/discovery/repo")
    }

    /// One line inserted at the top of a file changes every record whose
    /// state names a line of that file (its `file:line` positions), and no
    /// other. Measured on the fixture: the count is printed.
    #[test]
    fn an_edit_at_the_top_of_a_file_moves_only_that_files_records() {
        let dir = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(dir.path()).unwrap().join("checkout");
        copy_tree(&repo_source(), &repo);
        let (before, _) = records_with_rows(&repo);
        let api = "services/web/src/api.ts";
        let edited = repo.join(api);
        let text = std::fs::read_to_string(&edited).unwrap();
        let inserted = "// edited\n";
        std::fs::write(&edited, format!("{inserted}{text}")).unwrap();
        let (after, _) = records_after(&repo, Some((api, 1, inserted.len() as u32)));
        assert_eq!(before.len(), after.len());
        let changed: Vec<&str> = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a.state != b.state)
            .map(|(a, _)| a.state.point.at.as_str())
            .collect();
        eprintln!(
            "records changed by one line at the top of api.ts: {} of {}",
            changed.len(),
            before.len()
        );
        for at in &changed {
            let record = before.iter().find(|r| r.state.point.at == *at).unwrap();
            let names_api = serde_json::to_string(&record.state)
                .unwrap()
                .contains("src/api.ts:");
            assert!(names_api, "{at} changed but names no line of api.ts");
        }
        assert!(!changed.is_empty());
    }

    /// The record serialises to the wire shape lane 2 pins: each `state` of
    /// its contract example reads into a [`SiteState`] and writes back the
    /// same JSON.
    #[test]
    fn the_state_round_trips_the_wire_contract() {
        let contract: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                manifest().join("tests/fixtures/site_questions/contract.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let sites = contract["request"]["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 4);
        for site in sites {
            let state = &site["state"];
            let read: SiteState = serde_json::from_value(state.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&read).unwrap(), state);
        }
        // And a record this module builds reads back the same way.
        let (records, _) = records_with_rows(&repo());
        for record in &records {
            let value = serde_json::to_value(&record.state).unwrap();
            let read: SiteState = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&read).unwrap(), value);
        }
        let (records, _) = rooms_records(true);
        for record in &records {
            let value = serde_json::to_value(&record.state).unwrap();
            let read: SiteState = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&read).unwrap(), value);
        }
    }

    // -----------------------------------------------------------------------
    // `decided-sites/src/slots/rooms.ts`, each call its own discovery site.

    use super::super::decided_sites::tests::{
        PAYLOAD_CALLS, PAYLOADS, ROOMS, bottom, decided_fixture, end_for, fetch_label, line_of,
        payloads_label, rooms_sites, span_of,
    };

    const ENV: &str = "src/slots/env.ts";

    /// The records of `rooms.ts`, with or without the compiler's rows: the
    /// local `topic` typed as a union of two literals, and the call it is
    /// sent through declared in the installed package.
    fn rooms_records(with_rows: bool) -> (Vec<SiteRecord>, String) {
        let root = decided_fixture();
        let mut inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(ROOMS)).unwrap();
        if with_rows {
            let at = |needle: &str, inner: &str| {
                (text.find(needle).unwrap() + needle.find(inner).unwrap()) as u32
            };
            let topic = at("wire.emit(topic", "topic");
            let call = "wire.emit(topic, { shipped })";
            let call_start = at(call, call);
            let rows = vec![
                Row(
                    Kind::Ident,
                    topic,
                    topic + 5,
                    Some("topic".into()),
                    Some(ROOMS.into()),
                    Some(line_of(&text, "const topic")),
                    None,
                    Some("\"order.shipped\" | \"order.cancelled\"".into()),
                    0,
                ),
                Row(
                    Kind::Call,
                    call_start,
                    call_start + call.len() as u32,
                    Some("emit".into()),
                    Some("pkg:@fixture/wire/index.d.ts".into()),
                    Some(2),
                    Some("@fixture/wire".into()),
                    Some("void".into()),
                    0,
                ),
            ];
            let mut index = PositionIndex::of(
                &root,
                RowsFile {
                    header: Header {
                        v: crate::position_rows::FORMAT_VERSION,
                        typescript: "6.0.0".into(),
                        service: "rooms".into(),
                        files: 1,
                    },
                    files: vec![FileRows {
                        f: ROOMS.into(),
                        h: Some(content_hash(text.as_bytes())),
                        skip: None,
                        r: rows,
                    }],
                },
            );
            index.bind(&root.join(ROOMS), &text);
            inputs.positions = Some(index);
        }
        let (sites, labels) = rooms_sites(&text);
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        (site_records(&inputs, &root, &decided, &labels), text)
    }

    fn rooms_record<'r>(records: &'r [SiteRecord], text: &str, needle: &str) -> &'r SiteRecord {
        let at = format!("{ROOMS}:{}", line_of(text, needle));
        records
            .iter()
            .find(|record| record.state.point.at == at)
            .unwrap_or_else(|| panic!("{at}"))
    }

    /// The records of `payloads.ts`: each fetch its own discovery site, the
    /// label's payload slot the request's `body` key.
    fn payloads_records() -> (Vec<SiteRecord>, String) {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(PAYLOADS)).unwrap();
        let label = payloads_label();
        let sites: Vec<_> = PAYLOAD_CALLS
            .iter()
            .map(|call| bottom(&end_for(&label, PAYLOADS, &text, call), "fetch"))
            .collect();
        let labels = vec![label];
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        (site_records(&inputs, &root, &decided, &labels), text)
    }

    /// carrick#2674 P3-1a: a request's slot is the value its body key holds
    /// at the deciding call, or the options literal that states the key
    /// absent, wherever a wrapper hands the caller's options on. A control
    /// that is not read leaves the slot empty. The anchors and the forced
    /// payload argument stay as they were: the bound payload is still the one
    /// built inside the wrapper, and the slot is off the wire.
    #[test]
    fn a_request_slot_is_the_carried_body_or_the_literal_that_writes_none() {
        let (records, text) = payloads_records();
        let record = |needle: &str| {
            let at = format!("{PAYLOADS}:{}", line_of(&text, needle));
            records
                .iter()
                .find(|record| record.state.point.at == at)
                .unwrap_or_else(|| panic!("{at}"))
        };
        let anchor = |needle: &str| {
            let (span_start, span_end) = span_of(&text, needle);
            AnchorAt {
                file: PAYLOADS.to_string(),
                line: line_of(&text, needle),
                span_start,
                span_end,
            }
        };
        let note = record("send({ path: \"/notes\"");
        assert_eq!(
            note.payloads.request,
            Some(PayloadAt::Value(anchor("JSON.stringify(note)")))
        );
        let ping = record("send({ path: \"/ping\"");
        assert_eq!(
            ping.payloads.request,
            Some(PayloadAt::Unwritten {
                at: anchor("{ path: \"/ping\", method: \"POST\" }"),
                key: "body".to_string(),
            })
        );
        let logout = record("post(\"/logout\", {})");
        let call = anchor("post(\"/logout\", {})");
        let literal = call.span_start + "post(\"/logout\", ".len() as u32;
        assert_eq!(
            logout.payloads.request,
            Some(PayloadAt::Unwritten {
                at: AnchorAt {
                    span_start: literal,
                    span_end: literal + 2,
                    ..call
                },
                key: "body".to_string(),
            })
        );
        // A direct call states its own body: the locator's slot, unchanged.
        let direct = record("direct/body`");
        assert_eq!(
            direct.payloads.request,
            direct.anchors.payload_at.clone().map(PayloadAt::Value)
        );
        assert!(direct.payloads.request.is_some());
        for needle in ["call(\"/x\"", "wrapped(\"/wrapped\"", "direct/spread`"] {
            assert_eq!(record(needle).payloads.request, None, "{needle}");
        }
        // Nothing else moves.
        assert_eq!(ping.anchors.payload_at, None);
        assert!(matches!(
            ping.anchors.payload,
            Some(PayloadBinding::BuiltInside { .. })
        ));
        assert!(!ping.state.forced.contains_key(&Slot::PayloadArgument));
        let whole = serde_json::to_value(ping).unwrap();
        assert!(!whole.to_string().contains("Unwritten"), "{whole}");
    }

    /// The records of `env.ts`: each call its own discovery site.
    fn env_records() -> (Vec<SiteRecord>, String) {
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let text = std::fs::read_to_string(root.join(ENV)).unwrap();
        let label = fetch_label();
        let sites: Vec<_> = [
            "fetch(`${process.env.API}/items`)",
            "fetch(`${API}/things`)",
            "fetch(`${process.env.API}/items/${suffix}`)",
            "fetch(\"/api\" + path)",
        ]
        .iter()
        .map(|call| bottom(&end_for(&label, ENV, &text, call), "fetch"))
        .collect();
        let labels = vec![label];
        let decided = decided_sites(&inputs, &root, &sites, &labels);
        (site_records(&inputs, &root, &decided, &labels), text)
    }

    fn env_record<'r>(records: &'r [SiteRecord], text: &str, needle: &str) -> &'r SiteRecord {
        let at = format!("{ENV}:{}", line_of(text, needle));
        records
            .iter()
            .find(|record| record.state.point.at == at)
            .unwrap_or_else(|| {
                panic!(
                    "{at} in {:?}",
                    records
                        .iter()
                        .map(|r| &r.state.point.at)
                        .collect::<Vec<_>>()
                )
            })
    }

    /// An environment read leading a name is a stated value, as the request
    /// rows write an env base as a named piece: `${process.env.API}/items`
    /// is whole and forces the name, directly or through a module constant.
    /// A whole segment the code does not read is a path parameter, as the
    /// request summaries compose it (carrick#2519), and a wrapper prefix
    /// over an unread argument stays out of the records.
    #[test]
    fn an_environment_base_is_a_stated_value() {
        let (records, text) = env_records();
        for (needle, stated) in [
            (
                "fetch(`${process.env.API}/items`)",
                "${process.env.API}/items",
            ),
            ("fetch(`${API}/things`)", "${process.env.API}/things"),
        ] {
            let site = env_record(&records, &text, needle);
            assert_eq!(site.bucket, Bucket::Forced, "{needle}: {:?}", site.state);
            assert_eq!(
                site.state.forced.get(&Slot::Name).map(String::as_str),
                Some(stated),
                "{needle}"
            );
            assert!(
                site.state.stated.names.iter().any(|v| v.text == stated),
                "{needle}: {:?}",
                site.state.stated
            );
        }
        let site = env_record(
            &records,
            &text,
            "fetch(`${process.env.API}/items/${suffix}`)",
        );
        assert_eq!(site.bucket, Bucket::Forced, "{:?}", site.state);
        assert_eq!(
            site.state.forced.get(&Slot::Name).map(String::as_str),
            Some("${process.env.API}/items/${suffix}")
        );
        // A prefix over an argument nothing reads is no site's record.
        let at = format!("{ENV}:{}", line_of(&text, "fetch(\"/api\" + path)"));
        assert!(records.iter().all(|record| record.state.point.at != at));
    }

    /// The syntax carries the kinds the concept test kept, not only object
    /// entries: a callback, the call inside it, a member read, a template.
    #[test]
    fn the_syntax_carries_the_concepts_node_kinds() {
        let (records, text) = rooms_records(false);
        let listen = rooms_record(&records, &text, "wire.on(\"user.joined\"");
        let kinds: BTreeSet<&str> = listen
            .state
            .syntax
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["type"].as_str().unwrap())
            .collect();
        for kind in [
            "CallExpression",
            "StringLiteral",
            "ArrowFunctionExpression",
            "MemberExpression",
        ] {
            assert!(kinds.contains(kind), "{kind} in {kinds:?}");
        }
        let nodes = listen.state.syntax.as_array().unwrap();
        let log = nodes
            .iter()
            .find(|node| node["callee"] == "console.log")
            .expect("the call inside the callback");
        assert_eq!(log["args"], serde_json::json!(["data"]));
        let (records, text) = env_records();
        let items = env_record(&records, &text, "fetch(`${process.env.API}/items`)");
        let kinds: BTreeSet<&str> = items
            .state
            .syntax
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["type"].as_str().unwrap())
            .collect();
        assert!(
            kinds.contains("TemplateLiteral") && kinds.contains("MemberExpression"),
            "{kinds:?}"
        );
    }

    /// A name built from a literal and a value the code does not state is
    /// not that literal: it is never forced, and with no whole value stated
    /// the site is a lead. The literal is still offered.
    #[test]
    fn a_partly_stated_name_is_never_forced() {
        for with_rows in [false, true] {
            let (records, text) = rooms_records(with_rows);
            let join = rooms_record(&records, &text, "wire.emit(\"room:\"");
            assert_eq!(join.state.forced.get(&Slot::Name), None, "{:?}", join.state);
            assert_eq!(join.bucket, Bucket::Lead);
            assert!(
                join.state.stated.names.iter().any(|v| v.text == "room:"),
                "{:?}",
                join.state.stated
            );
        }
    }

    /// The method is read at the label's method slot: a literal written
    /// there is forced, and a value the slot holds that the code does not
    /// state is never the label's default.
    #[test]
    fn a_method_is_forced_only_when_one_is_stated() {
        let (records, text) = rooms_records(false);
        let create = rooms_record(&records, &text, "fetch(\"/items\",");
        assert_eq!(
            create.state.forced.get(&Slot::Method).map(String::as_str),
            Some("POST"),
            "{:?}",
            create.state
        );
        let remove = rooms_record(&records, &text, "fetch(\"/items/1\"");
        // The options object is a module constant the value flow reads.
        assert_eq!(
            remove.state.forced.get(&Slot::Method).map(String::as_str),
            Some("DELETE"),
            "{:?}",
            remove.state
        );
        // Options a caller hands in: the method is not stated here.
        let forward = rooms_record(&records, &text, "fetch(\"/items/3\"");
        assert_eq!(forward.state.forced.get(&Slot::Method), None);
        assert!(
            forward.state.asked.contains(&Slot::Method),
            "{:?}",
            forward.state
        );
    }

    /// A name typed as a union of literals states several values: asked,
    /// each offered from the checker. Without the rows nothing states it: a
    /// lead.
    #[test]
    fn a_union_typed_name_is_asked() {
        let (records, text) = rooms_records(true);
        let settle = rooms_record(&records, &text, "wire.emit(topic");
        assert_eq!(settle.bucket, Bucket::Asked, "{:?}", settle.state);
        assert!(settle.state.asked.contains(&Slot::Name));
        for value in ["order.shipped", "order.cancelled"] {
            let stated = settle
                .state
                .stated
                .names
                .iter()
                .find(|v| v.text == value)
                .unwrap_or_else(|| panic!("{value} in {:?}", settle.state.stated));
            assert!(
                stated
                    .places
                    .iter()
                    .any(|p| p.from == PlaceFrom::Checker && p.name.as_deref() == Some("topic")),
                "{stated:?}"
            );
        }
        let topic = compiler_of(settle)
            .names
            .into_iter()
            .find(|name| name.text == "topic")
            .unwrap();
        assert_eq!(topic.literal, None);

        let (records, text) = rooms_records(false);
        let settle = rooms_record(&records, &text, "wire.emit(topic");
        assert_eq!(settle.bucket, Bucket::Lead, "{:?}", settle.state);
    }

    /// A label that names no name slot forces neither the name nor the
    /// argument it is read from, however few values the call states, and a
    /// process channel's kind is asked.
    #[test]
    fn a_label_with_no_name_slot_forces_no_name() {
        let (records, text) = rooms_records(false);
        let ping = rooms_record(&records, &text, "worker.send(");
        for slot in [Slot::Name, Slot::NameArgument, Slot::Kind] {
            assert!(!ping.state.forced.contains_key(&slot), "{:?}", ping.state);
            assert!(ping.state.asked.contains(&slot), "{:?}", ping.state);
        }
        assert_eq!(ping.bucket, Bucket::Asked);
        assert_eq!(
            ping.state
                .forced
                .get(&Slot::PayloadArgument)
                .map(String::as_str),
            Some("0")
        );
    }

    /// A receive site's anchors name its handler, in its own file, and the
    /// argument the payload arrives through.
    #[test]
    fn a_receive_record_anchors_its_handler() {
        let (records, text) = rooms_records(false);
        let inline = "(data) => {\n    console.log(data);\n  }";
        let joined = rooms_record(&records, &text, "wire.on(\"user.joined\"");
        let handler = joined.anchors.handler.as_ref().unwrap();
        assert_eq!(
            (
                handler.at.file.as_str(),
                handler.at.line,
                handler.at.span_start
            ),
            (ROOMS, line_of(&text, inline), span_of(&text, inline).0)
        );
        assert_eq!(
            joined.anchors.payload,
            Some(PayloadBinding::Arg { arg: 1, key: None })
        );
        let left = rooms_record(&records, &text, "wire.on(\"user.left\"");
        assert_eq!(
            left.anchors.handler.as_ref().map(|h| h.at.line),
            Some(line_of(&text, "function onLeft"))
        );
    }

    /// A handler's anchor names its first parameter as written, whether
    /// the function is written at the call or declared by name, so the type
    /// capture can read the payload's type there (carrick#2439).
    #[test]
    fn a_receive_record_names_its_handlers_parameter() {
        let (records, text) = rooms_records(false);
        let joined = rooms_record(&records, &text, "wire.on(\"user.joined\"");
        let left = rooms_record(&records, &text, "wire.on(\"user.left\"");
        for record in [joined, left] {
            assert_eq!(
                record
                    .anchors
                    .handler
                    .as_ref()
                    .and_then(|handler| handler.param_name.as_deref()),
                Some("data"),
                "{:?}",
                record.anchors
            );
        }
    }

    /// A send's payload written at an argument of the deciding call is
    /// anchored at that expression's own span (carrick#2439).
    #[test]
    fn a_send_record_anchors_its_payload_expression() {
        let (records, text) = rooms_records(false);
        let settle = rooms_record(&records, &text, "wire.emit(topic");
        let payload = settle
            .anchors
            .payload_at
            .as_ref()
            .expect("a payload anchor");
        let (start, end) = span_of(&text, "{ shipped }");
        assert_eq!(
            (
                payload.file.as_str(),
                payload.line,
                payload.span_start,
                payload.span_end
            ),
            (ROOMS, line_of(&text, "{ shipped }"), start, end)
        );
    }

    /// A send's slots (phase 3 step 1): its request is the body argument the
    /// label names, its response the call's own result. A receive's request
    /// is its handler's parameter, and nothing states what it responds with.
    /// Neither moves the state.
    #[test]
    fn a_sites_payload_slots_are_its_body_its_result_and_its_handlers_parameter() {
        let (records, text) = rooms_records(false);
        let settle = rooms_record(&records, &text, "wire.emit(topic");
        let (start, end) = span_of(&text, "{ shipped }");
        let body = AnchorAt {
            file: ROOMS.to_string(),
            line: line_of(&text, "{ shipped }"),
            span_start: start,
            span_end: end,
        };
        assert_eq!(settle.payloads.request, Some(PayloadAt::Value(body)));
        assert_eq!(
            settle.payloads.response,
            Some(PayloadAt::Result(settle.anchors.call.clone()))
        );

        let joined = rooms_record(&records, &text, "wire.on(\"user.joined\"");
        let handler = joined.anchors.handler.clone().expect("a handler");
        assert_eq!(
            joined.payloads.request,
            Some(PayloadAt::Param {
                at: handler.at,
                name: "data".to_string(),
            })
        );
        assert_eq!(joined.payloads.response, None);

        let state = serde_json::to_value(&settle.state).unwrap();
        let whole = serde_json::to_value(settle).unwrap();
        assert!(whole.get("payloads").is_none(), "{whole}");
        assert!(!state.to_string().contains("payloads"), "{state}");
    }

    /// The state's parts as the contract writes them: the object entries
    /// the call is handed are syntax nodes, a package's declaration is
    /// named under `node_modules/`, and the package's argument labels are
    /// listed.
    #[test]
    fn the_state_carries_the_contracts_parts() {
        let (records, text) = rooms_records(true);
        let create = rooms_record(&records, &text, "fetch(\"/items\",");
        let line = line_of(&text, "fetch(\"/items\",");
        assert_eq!(
            create.state.syntax.clone(),
            serde_json::json!([
                {
                    "type": "CallExpression",
                    "at": format!("{ROOMS}:{line}"),
                    "text": "fetch(\"/items\", { method: \"POST\" })",
                    "callee": "fetch",
                    "args": ["\"/items\"", "{ method: \"POST\" }"]
                },
                {
                    "type": "StringLiteral",
                    "at": format!("{ROOMS}:{line}"),
                    "text": "\"/items\""
                },
                {
                    "type": "ObjectExpression",
                    "at": format!("{ROOMS}:{line}"),
                    "text": "{ method: \"POST\" }"
                },
                {
                    "type": "KeyValueProperty",
                    "at": format!("{ROOMS}:{line}"),
                    "text": "method: \"POST\"",
                    "key": "method"
                },
                {
                    "type": "StringLiteral",
                    "at": format!("{ROOMS}:{line}"),
                    "text": "\"POST\""
                }
            ])
        );
        let settle = rooms_record(&records, &text, "wire.emit(topic");
        assert_eq!(
            compiler_of(settle).callees[0].declaration.as_deref(),
            Some("node_modules/@fixture/wire/index.d.ts:2")
        );
        assert_eq!(
            settle.state.libraries[0]["argument_labels"].clone(),
            serde_json::json!([
                {
                    "export": "connect",
                    "member": "emit",
                    "does": "send",
                    "name_argument": 0,
                    "name_key": null,
                    "payload_argument": 1
                },
                {
                    "export": "connect",
                    "member": "on",
                    "does": "receive",
                    "name_argument": 0,
                    "name_key": null,
                    "payload_argument": null
                }
            ])
        );
    }

    /// A path the summaries compose with a path parameter forces the name,
    /// and the record says what each forced slot rests on: the name and a
    /// method the code writes are the code's, kind and role the label's
    /// (carrick#2519, carrick#2520).
    #[test]
    fn a_composed_path_forces_the_name_and_the_basis_is_recorded() {
        use crate::request_summary::decided_sites::tests::{PATHS, paths_decided};
        let root = decided_fixture();
        let inputs = crate::engine::discover_request_inputs(&root);
        let (decided, text) = paths_decided();
        let records = site_records(&inputs, &root, &decided, &[fetch_label()]);
        let at = format!("{PATHS}:{}", line_of(&text, "send(`/tasks/"));
        let record = records
            .iter()
            .find(|record| record.state.point.at == at)
            .unwrap_or_else(|| panic!("{at}"));
        assert_eq!(record.bucket, Bucket::Forced, "{:?}", record.state);
        assert_eq!(
            record.state.forced.get(&Slot::Name).map(String::as_str),
            Some("${process.env.API}/tasks/${taskId}/status")
        );
        assert_eq!(
            record.state.forced.get(&Slot::Method).map(String::as_str),
            Some("PUT")
        );
        assert_eq!(record.basis.get(&Slot::Name), Some(&Basis::Code));
        assert_eq!(record.basis.get(&Slot::Method), Some(&Basis::Code));
        assert_eq!(record.basis.get(&Slot::Kind), Some(&Basis::Label));
        assert_eq!(record.basis.get(&Slot::Role), Some(&Basis::Label));
        // A method nothing writes is the label's default.
        let at = format!("{PATHS}:{}", line_of(&text, "fetch(url)"));
        let download = records
            .iter()
            .find(|record| record.state.point.at == at)
            .unwrap_or_else(|| panic!("{at}"));
        assert_eq!(download.basis.get(&Slot::Method), Some(&Basis::Label));
        assert_eq!(download.basis.get(&Slot::Name), Some(&Basis::Code));
    }
}
