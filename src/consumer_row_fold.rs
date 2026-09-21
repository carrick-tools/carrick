//! One consumer row per request (carrick#1371).
//!
//! A single outbound request is written across several lines: the line that
//! sets a fetch up, the line that calls the client method, the line that opens
//! the connection, and the line that reads the body back. Each of those lines
//! can raise its own HTTP candidate, and the file-analyzer answers each of them
//! with a target, so one request arrives in the index as several consumers. A
//! reader — human or agent — counts them as independent call sites.
//!
//! This pass folds the lines that are not the request into the line that is.
//! Both rules are read off the AST or off spans the scanner already recorded;
//! neither names a library, a framework or a hook.
//!
//! 1. **A response read is not a request.** A member call that takes no
//!    arguments, whose receiver is the value a call in the same file produced,
//!    is reading that call's result: `const r = await send(url); return
//!    r.json();`. The rule is the shape, not the member's name — nothing here
//!    lists `json`/`text`/`blob`, because the receiver's origin is what settles
//!    it. When the producing call has a row of its own, the read's row is
//!    dropped and its type anchor moves to the producing row, which is
//!    routinely the side with no anchor (a deterministic wrapper row states a
//!    method and a target and no type). A read whose producing call has no row
//!    keeps its own row rather than losing the request: that case is counted,
//!    not silently swallowed.
//!
//! 2. **A row that lexically encloses another row for the same operation is
//!    the setup, not a second request.** A hook or a helper that takes the
//!    request as an argument or a callback spans the call it wraps, so one
//!    row's span strictly contains the other's. When the two agree on the
//!    operation — same method, same consumer path after normalization — they
//!    are one request seen twice and the OUTER row is dropped. The inner row
//!    sits at the call that issues the request, and the outer one's type is a
//!    projection of the response rather than the response (carrick#1375), so
//!    nothing is carried up.
//!
//! Only a row the model stated is ever dropped. A deterministic pass that
//! emits a row has read the request off the source; this pass does not
//! second-guess it.
//!
//! What this pass does NOT do: a call THROUGH a client method and the request
//! that method's own body issues are two rows in two files, and both are
//! wanted (carrick#1146). Telling a reader which of the two is the network
//! request needs a role on the indexed row, which is a change to what the
//! index blob carries.

use std::collections::HashMap;
use std::path::Path;

use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{CallExpr, Callee, Expr, Module, Pat};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult, ResolutionSource};
use crate::parser::parse_file;
use crate::swc_scanner::SWC_SPAN_BASE;
use crate::url_normalizer::UrlNormalizer;

/// What the pass folded, by the rule that decided it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerRowFolds {
    /// Rows dropped because the line reads the result of a request whose own
    /// call already has a row.
    pub response_reads: usize,
    /// Rows dropped because they enclose another row for the same operation.
    pub enclosing_setup: usize,
    /// Type anchors moved from a response read onto the request it reads.
    pub anchors_carried: usize,
    /// Response reads left in place because the call they read has no row —
    /// dropping them would lose the request altogether.
    pub response_reads_kept: usize,
}

impl ConsumerRowFolds {
    pub fn total(&self) -> usize {
        self.response_reads + self.enclosing_setup
    }
}

/// Fold every file's consumer rows down to one row per request.
pub fn fold_consumer_rows(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    normalizer: &UrlNormalizer,
) -> ConsumerRowFolds {
    let mut folds = ConsumerRowFolds::default();

    let mut keys: Vec<String> = file_results
        .iter()
        // Nothing can fold in a file with fewer than two spanned rows: both
        // rules need a row to fold ONTO. This is also what keeps the pass from
        // re-parsing most of the repo.
        .filter(|(_, result)| {
            result
                .data_calls
                .iter()
                .filter(|call| call.call_expression_span_start.is_some())
                .take(2)
                .count()
                == 2
        })
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();

    for key in keys {
        let Some(result) = file_results.get_mut(&key) else {
            continue;
        };
        let reads = response_reads(Path::new(&key));
        fold_response_reads(result, &reads, &key, &mut folds);
        fold_enclosing_setup(result, normalizer, &key, &mut folds);
    }

    folds
}

/// Rule 1: a member call reading the result of another call in this file.
fn fold_response_reads(
    result: &mut FileAnalysisResult,
    reads: &HashMap<u32, u32>,
    file: &str,
    folds: &mut ConsumerRowFolds,
) {
    if reads.is_empty() {
        return;
    }

    // The producing spans that actually carry a row, so a read can be told
    // from a read of something the index knows nothing about.
    let request_spans: Vec<u32> = result
        .data_calls
        .iter()
        .filter_map(|call| call.call_expression_span_start)
        .collect();

    let mut drop_reads: Vec<(usize, u32)> = Vec::new();
    for (index, call) in result.data_calls.iter().enumerate() {
        if !is_model_row(call) {
            continue;
        }
        let Some(span) = call.call_expression_span_start else {
            continue;
        };
        let Some(&produced_by) = reads.get(&span) else {
            continue;
        };
        if request_spans.contains(&produced_by) {
            drop_reads.push((index, produced_by));
        } else {
            folds.response_reads_kept += 1;
            debug!(
                "  - {file}: line {} reads a call with no row of its own; keeping it",
                call.line_number
            );
        }
    }

    // Highest index first, so each removal leaves the rest addressable.
    for &(index, produced_by) in drop_reads.iter().rev() {
        let anchor = result.data_calls[index]
            .primary_type_symbol
            .as_ref()
            .map(|symbol| {
                (
                    symbol.clone(),
                    result.data_calls[index].type_import_source.clone(),
                )
            });
        if let Some((symbol, import)) = anchor
            && let Some(request) = result
                .data_calls
                .iter_mut()
                .find(|call| call.call_expression_span_start == Some(produced_by))
            && request.primary_type_symbol.is_none()
        {
            request.primary_type_symbol = Some(symbol);
            request.type_import_source = import;
            folds.anchors_carried += 1;
        }
        let dropped = result.data_calls.remove(index);
        folds.response_reads += 1;
        debug!(
            "  - {file}: line {} reads the result of the request at span {produced_by}; dropped",
            dropped.line_number
        );
    }
}

/// Rule 2: a row whose span strictly contains another row for the same
/// operation.
fn fold_enclosing_setup(
    result: &mut FileAnalysisResult,
    normalizer: &UrlNormalizer,
    file: &str,
    folds: &mut ConsumerRowFolds,
) {
    let spanned: Vec<(usize, u32, u32)> = result
        .data_calls
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            Some((
                index,
                call.call_expression_span_start?,
                call.call_expression_span_end?,
            ))
        })
        .collect();

    let mut drop_indexes: Vec<usize> = Vec::new();
    for &(outer_index, outer_start, outer_end) in &spanned {
        let outer = &result.data_calls[outer_index];
        if !is_model_row(outer) {
            continue;
        }
        for &(inner_index, inner_start, inner_end) in &spanned {
            if inner_index == outer_index {
                continue;
            }
            // STRICT containment: a twin sharing the span is the model's
            // answer for the same call site, which the emit/join phase already
            // folds.
            let contains = outer_start <= inner_start
                && outer_end >= inner_end
                && (outer_start, outer_end) != (inner_start, inner_end);
            if !contains {
                continue;
            }
            let inner = &result.data_calls[inner_index];
            if !same_operation(outer, inner, normalizer) {
                continue;
            }
            drop_indexes.push(outer_index);
            debug!(
                "  - {file}: line {} sets up the request at line {}; dropped",
                outer.line_number, inner.line_number
            );
            break;
        }
    }

    // Highest index first, so each removal leaves the rest addressable.
    for &index in drop_indexes.iter().rev() {
        result.data_calls.remove(index);
        folds.enclosing_setup += 1;
    }
}

/// Whether two rows state the same operation: the same method, and the same
/// consumer path once the target is normalized, so two spellings of one base
/// do not read as two operations.
fn same_operation(a: &DataCallResult, b: &DataCallResult, normalizer: &UrlNormalizer) -> bool {
    let method = |call: &DataCallResult| {
        crate::agents::file_orchestrator::FileOrchestrator::normalize_consumer_method(
            call.method.as_deref(),
        )
    };
    method(a) == method(b)
        && normalizer.consumer_call_path(&a.target) == normalizer.consumer_call_path(&b.target)
}

/// Whether the row is the model's own statement. A deterministic pass read its
/// row off the source, and this pass never overrules one.
fn is_model_row(call: &DataCallResult) -> bool {
    matches!(call.resolution_source, None | Some(ResolutionSource::Model))
}

/// Read one file's response reads: the span of each member call that takes no
/// arguments and whose receiver is the value another call produced, mapped to
/// that call's span.
///
/// Spans are in the same units the scanner's candidates carry: file-relative
/// byte offsets plus [`SWC_SPAN_BASE`].
fn response_reads(file: &Path) -> HashMap<u32, u32> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let Some(module) = parse_file(file, &cm, &handler) else {
        return HashMap::new();
    };
    read_module(&module, &cm)
}

fn read_module(module: &Module, cm: &Lrc<SourceMap>) -> HashMap<u32, u32> {
    let mut bindings = BindingOrigins::default();
    module.visit_with(&mut bindings);
    let mut reads = ReadCollector {
        bindings: &bindings.origins,
        reads: Vec::new(),
    };
    module.visit_with(&mut reads);

    let offset = |pos: swc_common::BytePos| -> Option<u32> {
        cm.lookup_byte_offset(pos).pos.0.checked_add(SWC_SPAN_BASE)
    };
    reads
        .reads
        .into_iter()
        .filter_map(|(read, produced_by)| Some((offset(read)?, offset(produced_by)?)))
        .collect()
}

/// The call a bare `const` binding's value came out of, for every name bound
/// exactly once in the module. A name bound twice states nothing: which
/// binding a use reaches is a scope question this does not answer, so it is
/// dropped rather than guessed at (the same discipline as
/// [`crate::receiver_origin`]).
#[derive(Default)]
struct BindingOrigins {
    origins: HashMap<String, Option<swc_common::BytePos>>,
}

impl Visit for BindingOrigins {
    fn visit_var_declarator(&mut self, declarator: &swc_ecma_ast::VarDeclarator) {
        if let Pat::Ident(ident) = &declarator.name {
            let name = ident.id.sym.to_string();
            let origin = declarator
                .init
                .as_deref()
                .and_then(call_behind)
                .map(|call| call.span.lo);
            self.origins
                .entry(name)
                .and_modify(|existing| {
                    if *existing != origin {
                        *existing = None;
                    }
                })
                .or_insert(origin);
        }
        declarator.visit_children_with(self);
    }
}

struct ReadCollector<'a> {
    bindings: &'a HashMap<String, Option<swc_common::BytePos>>,
    /// (the read's span start, the producing call's span start)
    reads: Vec<(swc_common::BytePos, swc_common::BytePos)>,
}

impl Visit for ReadCollector<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if call.args.is_empty()
            && let Callee::Expr(callee) = &call.callee
            && let Expr::Member(member) = &**callee
        {
            let produced_by = match call_behind(&member.obj) {
                Some(producer) => Some(producer.span.lo),
                None => match strip_value_wrappers(&member.obj) {
                    Expr::Ident(ident) => self.bindings.get(ident.sym.as_ref()).copied().flatten(),
                    _ => None,
                },
            };
            if let Some(produced_by) = produced_by
                && produced_by != call.span.lo
            {
                self.reads.push((call.span.lo, produced_by));
            }
        }
        call.visit_children_with(self);
    }
}

/// Strip the forms that pass a value along without replacing it.
fn strip_value_wrappers(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => strip_value_wrappers(&paren.expr),
        Expr::Await(await_expr) => strip_value_wrappers(&await_expr.arg),
        Expr::TsAs(as_expr) => strip_value_wrappers(&as_expr.expr),
        Expr::TsNonNull(non_null) => strip_value_wrappers(&non_null.expr),
        Expr::TsConstAssertion(assertion) => strip_value_wrappers(&assertion.expr),
        Expr::TsSatisfies(satisfies) => strip_value_wrappers(&satisfies.expr),
        other => other,
    }
}

/// The call an expression's value came out of, through the wrappers above.
fn call_behind(expr: &Expr) -> Option<&CallExpr> {
    match strip_value_wrappers(expr) {
        Expr::Call(call) => Some(call),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::FileName;
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

    fn reads(source: &str) -> HashMap<u32, u32> {
        let cm: Lrc<SourceMap> = Default::default();
        let file = cm.new_source_file(
            Lrc::new(FileName::Real("read.ts".into())),
            source.to_string(),
        );
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax {
                tsx: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(&*file),
            None,
        );
        let module = Parser::new_from(lexer)
            .parse_module()
            .expect("fixture source parses");
        read_module(&module, &cm)
    }

    /// The offset a candidate span would carry for the first occurrence of
    /// `needle`.
    fn span_of(source: &str, needle: &str) -> u32 {
        u32::try_from(source.find(needle).expect("needle is in the source")).unwrap()
            + SWC_SPAN_BASE
    }

    fn row(line: i32, span: (u32, u32), target: &str, method: &str) -> DataCallResult {
        DataCallResult {
            candidate_id: format!("span:{}-{}", span.0, span.1),
            line_number: line,
            target: target.to_string(),
            method: Some(method.to_string()),
            call_kind: None,
            pattern_matched: "test".to_string(),
            call_expression_span_start: Some(span.0),
            call_expression_span_end: Some(span.1),
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
        }
    }

    fn lines(result: &FileAnalysisResult) -> Vec<i32> {
        result
            .data_calls
            .iter()
            .map(|call| call.line_number)
            .collect()
    }

    #[test]
    fn a_response_read_folds_its_type_anchor_onto_the_request() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(17, (100, 140), "${BASE}/v1/things", "GET"),
                DataCallResult {
                    primary_type_symbol: Some("ShelfSummary".to_string()),
                    type_import_source: Some("./types".to_string()),
                    ..row(21, (200, 216), "${BASE}/v1/things", "GET")
                },
            ],
            ..Default::default()
        };
        let reads = HashMap::from([(200u32, 100u32)]);
        let mut folds = ConsumerRowFolds::default();
        fold_response_reads(&mut result, &reads, "shelves.ts", &mut folds);

        assert_eq!(lines(&result), vec![17], "the read's row is folded away");
        assert_eq!(
            result.data_calls[0].primary_type_symbol.as_deref(),
            Some("ShelfSummary"),
            "the request keeps the anchor the read was carrying"
        );
        assert_eq!(
            result.data_calls[0].type_import_source.as_deref(),
            Some("./types")
        );
        assert_eq!(folds.response_reads, 1);
        assert_eq!(folds.anchors_carried, 1);
    }

    #[test]
    fn a_response_read_of_a_call_with_no_row_is_kept() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(17, (100, 140), "${BASE}/v1/things", "GET"),
                row(21, (200, 216), "${BASE}/v1/other", "GET"),
            ],
            ..Default::default()
        };
        // The read at 200 reads a call at 900, which has no row.
        let reads = HashMap::from([(200u32, 900u32)]);
        let mut folds = ConsumerRowFolds::default();
        fold_response_reads(&mut result, &reads, "shelves.ts", &mut folds);

        assert_eq!(
            lines(&result),
            vec![17, 21],
            "dropping this would lose the only record of the request"
        );
        assert_eq!(folds.response_reads, 0);
        assert_eq!(folds.response_reads_kept, 1);
    }

    #[test]
    fn a_deterministic_row_at_a_read_is_never_folded() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(17, (100, 140), "${BASE}/v1/things", "GET"),
                DataCallResult {
                    resolution_source: Some(ResolutionSource::SameFileWrapper),
                    ..row(21, (200, 216), "${BASE}/v1/things", "GET")
                },
            ],
            ..Default::default()
        };
        let reads = HashMap::from([(200u32, 100u32)]);
        let mut folds = ConsumerRowFolds::default();
        fold_response_reads(&mut result, &reads, "shelves.ts", &mut folds);

        assert_eq!(lines(&result), vec![17, 21]);
        assert_eq!(folds.response_reads, 0);
    }

    #[test]
    fn an_enclosing_row_for_the_same_operation_is_folded() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(6, (100, 300), "${BASE}/v1/things", "GET"),
                row(8, (180, 220), "${BASE}/v1/things", "GET"),
            ],
            ..Default::default()
        };
        let mut folds = ConsumerRowFolds::default();
        fold_enclosing_setup(
            &mut result,
            &UrlNormalizer::default_permissive(),
            "useThings.ts",
            &mut folds,
        );

        assert_eq!(lines(&result), vec![8], "the inner call is the request");
        assert_eq!(folds.enclosing_setup, 1);
    }

    #[test]
    fn an_enclosing_row_for_a_different_operation_is_kept() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(6, (100, 300), "${BASE}/v1/things", "POST"),
                row(8, (180, 220), "${BASE}/v1/others", "GET"),
            ],
            ..Default::default()
        };
        let mut folds = ConsumerRowFolds::default();
        fold_enclosing_setup(
            &mut result,
            &UrlNormalizer::default_permissive(),
            "post-with-a-nested-get.ts",
            &mut folds,
        );

        assert_eq!(
            lines(&result),
            vec![6, 8],
            "a request whose argument is another request is two requests"
        );
        assert_eq!(folds.enclosing_setup, 0);
    }

    #[test]
    fn a_twin_sharing_the_span_is_not_folded() {
        let mut result = FileAnalysisResult {
            data_calls: vec![
                row(6, (100, 300), "${BASE}/v1/things", "GET"),
                row(6, (100, 300), "${BASE}/v1/things", "GET"),
            ],
            ..Default::default()
        };
        let mut folds = ConsumerRowFolds::default();
        fold_enclosing_setup(
            &mut result,
            &UrlNormalizer::default_permissive(),
            "twins.ts",
            &mut folds,
        );

        assert_eq!(folds.enclosing_setup, 0, "containment must be strict");
    }

    #[test]
    fn a_read_off_a_bound_request_names_the_request() {
        let source = "\
async function load() {
  const response = await send(`${BASE}/v1/things`);
  return response.json();
}
";
        let reads = reads(source);
        assert_eq!(
            reads.get(&span_of(source, "response.json()")),
            Some(&span_of(source, "send(`${BASE}/v1/things`)")),
            "the read must name the call whose value it reads: {reads:?}"
        );
    }

    #[test]
    fn a_read_off_an_inline_request_names_the_request() {
        let source = "const data = await (await fetch(\"/v1/things\")).json();\n";
        let reads = reads(source);
        assert_eq!(
            reads.get(&span_of(source, "(await fetch(\"/v1/things\")).json()")),
            Some(&span_of(source, "fetch(\"/v1/things\")")),
            "an inline read must name the call it wraps: {reads:?}"
        );
    }

    #[test]
    fn a_call_carrying_arguments_is_not_a_read() {
        let source = "\
async function load() {
  const client = await connect();
  return client.get(\"/v1/things\");
}
";
        assert!(
            reads(source).is_empty(),
            "a member call with arguments states a request of its own"
        );
    }

    #[test]
    fn a_read_off_a_parameter_names_nothing() {
        let source = "async function parse(response: Response) { return response.json(); }\n";
        assert!(
            reads(source).is_empty(),
            "a receiver this file did not produce cannot be folded onto anything"
        );
    }

    #[test]
    fn a_name_bound_twice_names_nothing() {
        let source = "\
async function load(which: boolean) {
  if (which) {
    const response = await send(\"/a\");
    return response.json();
  }
  const response = await other(\"/b\");
  return response.json();
}
";
        assert!(
            reads(source).is_empty(),
            "an ambiguous binding is dropped, never picked between"
        );
    }

    #[test]
    fn a_read_off_a_binding_that_is_not_a_call_names_nothing() {
        let source = "\
async function load() {
  const response = cached;
  return response.json();
}
";
        assert!(
            reads(source).is_empty(),
            "a binding holding no call result produced no request"
        );
    }
}
