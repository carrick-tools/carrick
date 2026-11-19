//! Carry an edited file's model answers onto the lines the edit left alone
//! (carrick#2894).
//!
//! A no-model scan (`check`, `check --changed`, `refresh`, the local read-in)
//! replays a previous model answer only for a file whose bytes equal the
//! previous generation's commit (`reusable_model_answers`). The file a user is
//! editing is exactly the file that fails that test, so its routes, mounts and
//! calls vanished from the one scan meant to judge the edit.
//!
//! This module re-anchors each row of such a file by the lines the edit did
//! not touch. The row's call must sit on an unchanged line and be found again
//! there (same callee, same first argument, same place among its twins); its
//! hints are kept where their line and text still stand. Whatever cannot be
//! proven is NOT carried and is counted with its reason, so a row is never
//! dropped without a trace.
//!
//! Carried answers are joined as the model's answer and are never stored: they
//! are keyed by candidate ids of THIS tree, and the next scan re-maps from the
//! same previous generation. See `docs/local-mode-output.md` for what a reader
//! sees.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::agents::file_analyzer_agent::{
    DataCallResult, EmissionStyle, EndpointResult, FileAnalysisResult, GraphqlOperation,
    MountResult, PubsubOperation,
};
use crate::dispatch::DispatchTable;
use crate::git_state::{Hunk, changed_hunks, file_at_commit, map_line};
use crate::swc_scanner::{CandidateTarget, SwcScanner};

/// A row whose registration line the edit replaced.
pub const REGISTRATION_LINE_CHANGED: &str = "registration_line_changed";
/// No single call on the mapped line matches the one the answer named.
pub const NO_MATCHING_CALL: &str = "no_matching_call";
/// A hint outside the registration call changed: a locator there could read
/// the wrong function, so the row is left out rather than risk a false failure.
pub const HINT_OUTSIDE_REGISTRATION_CHANGED: &str = "hint_outside_registration_changed";
/// Carried, but the response is not typed: the send line was edited.
pub const CARRIED_UNTYPED: &str = "carried_untyped";
/// The previous commit's copy of the file is not in this clone.
pub const OLD_SOURCE_UNAVAILABLE: &str = "old_source_unavailable";
/// Prefix of the reason for a kind of row that is not carried yet
/// (`not_carried_<field>`).
const NOT_CARRIED_PREFIX: &str = "not_carried_";

/// One reason above as a person reads it, for `carrick status`. The stored
/// boundary keeps the code; a code with no words here (one a newer scanner
/// wrote) is returned as it stands.
pub fn reason_words(code: &str) -> &str {
    match code {
        REGISTRATION_LINE_CHANGED => "the edit changed the line that registers it",
        NO_MATCHING_CALL => "no single call on its line matches the one the index named",
        HINT_OUTSIDE_REGISTRATION_CHANGED => "the edit changed a handler it names",
        CARRIED_UNTYPED => "kept, but its send line was edited, so it has no response type",
        OLD_SOURCE_UNAVAILABLE => "this clone does not hold the indexed copy of the file",
        _ if code.starts_with(NOT_CARRIED_PREFIX) => "rows of this kind are not kept yet",
        _ => code,
    }
}

/// What carrying did for the files of one scan.
#[derive(Debug, Default)]
pub struct Carried {
    /// The carried answers, by the key the orchestrator holds the file under.
    pub answers: HashMap<String, FileAnalysisResult>,
    /// The repo-relative paths of the files in `answers`.
    pub files: HashSet<String>,
    /// Rows of edited files that were carried (including `carried_untyped`).
    pub carried: usize,
    /// Rows of edited files that had a previous answer, carried or not.
    pub examined: usize,
    /// One `file:line reason` per row not carried, or carried untyped.
    pub not_carried: Vec<String>,
}

/// The carried rows of one file, and what became of the rest.
#[derive(Debug, Default)]
pub struct FileCarry {
    pub answer: FileAnalysisResult,
    pub carried: usize,
    pub examined: usize,
    pub not_carried: Vec<String>,
}

/// Carry every edited file's previous answer that the edit left standing.
///
/// `files` yields `(orchestrator key, repo-relative path)`. `unchanged` is the
/// set [`crate::git_state::unchanged_since`] returned for `prev_commit`; a file
/// outside it that has a previous answer is the edited file this carries.
pub fn carry_edited_files(
    repo_path: &str,
    prev_commit: &str,
    files: impl Iterator<Item = (String, String)>,
    previous: &HashMap<String, FileAnalysisResult>,
    unchanged: &HashSet<String>,
) -> Carried {
    let mut out = Carried::default();
    for (key, relative) in files {
        if unchanged.contains(&relative) {
            continue;
        }
        let Some(answer) = previous.get(&relative) else {
            continue;
        };
        let rows = row_count(answer);
        if rows == 0 {
            continue;
        }
        let git_path = format!("./{relative}");
        let old_text = file_at_commit(repo_path, prev_commit, &git_path);
        let hunks = changed_hunks(repo_path, prev_commit, &relative, None);
        let new_text = std::fs::read_to_string(&key).ok();
        let carry = match (old_text, hunks, new_text) {
            (Some(old), Ok(hunks), Some(new)) => carry_file(&relative, &old, &new, &hunks, answer),
            _ => unavailable(&relative, answer),
        };
        out.examined += carry.examined;
        out.carried += carry.carried;
        out.not_carried.extend(carry.not_carried);
        // An answer with no carried row would only mask the file as answered.
        if carry.carried > 0 {
            out.files.insert(relative);
            out.answers.insert(key, carry.answer);
        }
    }
    if out.examined > 0 {
        let tally = tally_by_reason(&out.not_carried);
        tracing::debug!(
            "Edited files: {} row(s) carried of {} examined; not carried by reason: {:?}",
            out.carried,
            out.examined,
            tally
        );
    }
    out
}

/// How many rows each reason left out: the one answer to "why", read by the
/// boundary and the debug line. A line is `file:line reason` or `file reason`.
pub fn tally_by_reason(lines: &[String]) -> std::collections::BTreeMap<String, usize> {
    let mut tally = std::collections::BTreeMap::new();
    for line in lines {
        if let Some((_, reason)) = line.rsplit_once(' ') {
            *tally.entry(reason.to_string()).or_default() += 1;
        }
    }
    tally
}

fn row_count(answer: &FileAnalysisResult) -> usize {
    answer.endpoints.len()
        + answer.data_calls.len()
        + answer.mounts.len()
        + answer.graphql_operations.len()
        + answer.pubsub_operations.len()
        + answer.graphql_consumer_locates.len()
        + answer.dispatch_tables.len()
}

/// Every row of `answer`, none carried, because the old text or the diff could
/// not be read.
fn unavailable(relative: &str, answer: &FileAnalysisResult) -> FileCarry {
    let mut carry = FileCarry {
        examined: row_count(answer),
        ..FileCarry::default()
    };
    let lines = answer
        .endpoints
        .iter()
        .map(|row| row.line_number)
        .chain(answer.data_calls.iter().map(|row| row.line_number))
        .chain(answer.mounts.iter().map(|row| row.line_number));
    for line in lines {
        carry
            .not_carried
            .push(format!("{relative}:{line} {OLD_SOURCE_UNAVAILABLE}"));
    }
    other_lists_not_carried(relative, answer, &mut carry);
    carry
}

fn other_lists_not_carried(relative: &str, answer: &FileAnalysisResult, carry: &mut FileCarry) {
    for (field, count) in [
        ("graphql_operations", answer.graphql_operations.len()),
        ("pubsub_operations", answer.pubsub_operations.len()),
        (
            "graphql_consumer_locates",
            answer.graphql_consumer_locates.len(),
        ),
        ("dispatch_tables", answer.dispatch_tables.len()),
    ] {
        for _ in 0..count {
            count_not_carried(relative, field, carry);
        }
    }
}

fn count_not_carried(relative: &str, field: &str, carry: &mut FileCarry) {
    carry
        .not_carried
        .push(format!("{relative} {NOT_CARRIED_PREFIX}{field}"));
}

/// Carry one file's rows. Pure over its inputs: the old text, the new text and
/// the hunks between them.
pub fn carry_file(
    relative: &str,
    old_text: &str,
    new_text: &str,
    hunks: &[Hunk],
    answer: &FileAnalysisResult,
) -> FileCarry {
    // Every call in each text, not the calls the detection lists let the
    // scanner raise: a row stands on the call it names, whichever signal the
    // model's answer came from (N24).
    let scanner = SwcScanner::new();
    let path = Path::new(relative);
    let old_scan = scanner.scan_every_call(path, old_text);
    let new_scan = scanner.scan_every_call(path, new_text);
    let new_lines: Vec<&str> = new_text.lines().collect();
    let ctx = Context {
        hunks,
        old: &old_scan.candidates,
        new: &new_scan.candidates,
        new_lines: &new_lines,
    };
    let mut carry = FileCarry {
        examined: row_count(answer),
        ..FileCarry::default()
    };

    for row in &answer.endpoints {
        match ctx.endpoint(row) {
            Ok((carried, untyped)) => {
                carry.carried += 1;
                if untyped {
                    carry.not_carried.push(format!(
                        "{relative}:{} {CARRIED_UNTYPED}",
                        carried.line_number
                    ));
                }
                carry.answer.endpoints.push(carried);
            }
            Err(reason) => carry.not_carried.push(format!(
                "{relative}:{} {reason}",
                ctx.report_line(row.line_number)
            )),
        }
    }
    for row in &answer.data_calls {
        match ctx.data_call(row) {
            Ok(carried) => {
                carry.carried += 1;
                carry.answer.data_calls.push(carried);
            }
            Err(reason) => carry.not_carried.push(format!(
                "{relative}:{} {reason}",
                ctx.report_line(row.line_number)
            )),
        }
    }
    for row in &answer.mounts {
        match ctx.mount(row) {
            Some(carried) => {
                carry.carried += 1;
                carry.answer.mounts.push(carried);
            }
            None => carry.not_carried.push(format!(
                "{relative}:{} {REGISTRATION_LINE_CHANGED}",
                ctx.report_line(row.line_number)
            )),
        }
    }
    for row in &answer.graphql_operations {
        match ctx.graphql_operation(row) {
            Some(carried) => {
                carry.carried += 1;
                carry.answer.graphql_operations.push(carried);
            }
            None => count_not_carried(relative, "graphql_operations", &mut carry),
        }
    }
    for row in &answer.pubsub_operations {
        match ctx.pubsub_operation(row) {
            Some(carried) => {
                carry.carried += 1;
                carry.answer.pubsub_operations.push(carried);
            }
            None => count_not_carried(relative, "pubsub_operations", &mut carry),
        }
    }
    for row in &answer.dispatch_tables {
        match ctx.dispatch_table(row) {
            Some(carried) => {
                carry.carried += 1;
                carry.answer.dispatch_tables.push(carried);
            }
            None => count_not_carried(relative, "dispatch_tables", &mut carry),
        }
    }
    // A consumer locate states a field and a type symbol and no line or text
    // the carry could check against the edited file: it is never carried.
    for _ in &answer.graphql_consumer_locates {
        count_not_carried(relative, "graphql_consumer_locates", &mut carry);
    }
    carry
}

struct Context<'a> {
    hunks: &'a [Hunk],
    old: &'a [CandidateTarget],
    new: &'a [CandidateTarget],
    new_lines: &'a [&'a str],
}

/// The place of a call among the calls that look the same on its line.
struct Place<'a> {
    candidate: &'a CandidateTarget,
    ordinal: usize,
    twins: usize,
}

fn same_call(a: &CandidateTarget, b: &CandidateTarget) -> bool {
    a.line_number == b.line_number
        && a.callee_object == b.callee_object
        && a.callee_property == b.callee_property
        && a.path_snippet == b.path_snippet
}

/// What became of one hint.
enum Hint {
    /// Kept, at this line (`None` when the hint has none).
    Kept(Option<i32>),
    /// Changed by the edit.
    Changed,
}

impl Context<'_> {
    fn old_place(&self, candidate_id: &str) -> Option<Place<'_>> {
        let candidate = self.old.iter().find(|c| c.candidate_id == candidate_id)?;
        let mut twins: Vec<&CandidateTarget> = self
            .old
            .iter()
            .filter(|other| same_call(candidate, other))
            .collect();
        twins.sort_by_key(|twin| twin.span_start);
        let ordinal = twins
            .iter()
            .position(|twin| twin.candidate_id == candidate.candidate_id)?;
        Some(Place {
            candidate,
            ordinal,
            twins: twins.len(),
        })
    }

    /// The call in the edited text that `place` is, or why there is none.
    fn new_candidate(&self, place: &Place<'_>) -> Result<&CandidateTarget, &'static str> {
        let old_line = place.candidate.line_number as u32;
        let new_line = map_line(self.hunks, old_line).ok_or(REGISTRATION_LINE_CHANGED)? as usize;
        let mut twins: Vec<&CandidateTarget> = self
            .new
            .iter()
            .filter(|other| {
                other.line_number == new_line
                    && other.callee_object == place.candidate.callee_object
                    && other.callee_property == place.candidate.callee_property
                    && other.path_snippet == place.candidate.path_snippet
            })
            .collect();
        twins.sort_by_key(|twin| twin.span_start);
        if twins.len() != place.twins {
            return Err(NO_MATCHING_CALL);
        }
        twins.get(place.ordinal).copied().ok_or(NO_MATCHING_CALL)
    }

    /// The line to name a row at in the working tree: where the line went, or
    /// where the edit that replaced it now stands.
    fn report_line(&self, old_line: i32) -> i32 {
        let Ok(line) = u32::try_from(old_line) else {
            return old_line;
        };
        if let Some(mapped) = map_line(self.hunks, line) {
            return mapped as i32;
        }
        self.hunks
            .iter()
            .find(|h| h.old_count > 0 && line >= h.old_start && line < h.old_start + h.old_count)
            .map(|h| {
                let within = (line - h.old_start).min(h.new_count.saturating_sub(1));
                (h.new_start + within).max(1) as i32
            })
            .unwrap_or(old_line)
    }

    /// Whether `text` still stands in the source where the hint's line starts,
    /// ignoring whitespace (a model's "verbatim" text is not always
    /// byte-exact, and the expression opens part-way along its line).
    fn text_stands(&self, line: u32, text: &str) -> bool {
        let want: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if want.is_empty() {
            return true;
        }
        let mut lines = self.new_lines.iter().skip(line.saturating_sub(1) as usize);
        let mut have: String = match lines.next() {
            Some(first) => first.chars().filter(|c| !c.is_whitespace()).collect(),
            None => return false,
        };
        // The expression may run on past the line it starts on.
        let reach = have.len() + want.len();
        for source in lines {
            if have.len() >= reach {
                break;
            }
            have.extend(source.chars().filter(|c| !c.is_whitespace()));
        }
        have.contains(&want)
    }

    fn hint(&self, text: &Option<String>, line: Option<i32>) -> Hint {
        match (text, line) {
            (None, None) => Hint::Kept(None),
            (_, Some(line)) if line > 0 => match map_line(self.hunks, line as u32) {
                Some(mapped)
                    if text
                        .as_deref()
                        .is_none_or(|text| self.text_stands(mapped, text)) =>
                {
                    Hint::Kept(Some(mapped as i32))
                }
                _ => Hint::Changed,
            },
            // Text with no line to check it by.
            _ => Hint::Changed,
        }
    }

    /// Whether `line` lies inside the call the row was registered at.
    fn inside_call(place: &Place<'_>, line: Option<i32>) -> bool {
        line.is_some_and(|line| {
            line >= place.candidate.line_number as i32 && line <= place.candidate.end_line as i32
        })
    }

    /// The endpoint carried onto the edited text, and whether its response is
    /// left untyped; or the reason it is not carried.
    fn endpoint(&self, row: &EndpointResult) -> Result<(EndpointResult, bool), &'static str> {
        let place = self.old_place(&row.candidate_id).ok_or(NO_MATCHING_CALL)?;
        let new = self.new_candidate(&place)?;
        let mut carried = row.clone();
        carried.candidate_id = new.candidate_id.clone();
        carried.line_number = new.line_number as i32;
        carried.call_expression_span_start = row.call_expression_span_start.map(|_| new.span_start);
        carried.call_expression_span_end = row.call_expression_span_end.map(|_| new.span_end);

        let mut untyped = false;
        match self.hint(&row.response_expression_text, row.response_expression_line) {
            Hint::Kept(line) => carried.response_expression_line = line,
            Hint::Changed => {
                // Inside the registration call the handler is inline: with no
                // hint, the response is read from the function at the
                // registration line. Outside it, a locator could read another
                // function.
                if !Self::inside_call(&place, row.response_expression_line) {
                    return Err(HINT_OUTSIDE_REGISTRATION_CHANGED);
                }
                carried.response_expression_text = None;
                carried.response_expression_line = None;
                untyped = !matches!(row.emission_style, Some(EmissionStyle::ReturnValue));
            }
        }
        match self.hint(&row.payload_expression_text, row.payload_expression_line) {
            Hint::Kept(line) => carried.payload_expression_line = line,
            Hint::Changed => {
                if !Self::inside_call(&place, row.payload_expression_line) {
                    return Err(HINT_OUTSIDE_REGISTRATION_CHANGED);
                }
                carried.payload_expression_text = None;
                carried.payload_expression_line = None;
            }
        }
        Ok((carried, untyped))
    }

    /// A data call is carried only when every line of its call is unchanged
    /// and every hint stands: its target is read off those lines.
    fn data_call(&self, row: &DataCallResult) -> Result<DataCallResult, &'static str> {
        let place = self.old_place(&row.candidate_id).ok_or(NO_MATCHING_CALL)?;
        let first = place.candidate.line_number as u32;
        let last = place.candidate.end_line.max(place.candidate.line_number) as u32;
        let new = self.new_candidate(&place)?;
        for line in first..=last {
            let expected = new.line_number as u32 + (line - first);
            if map_line(self.hunks, line) != Some(expected) {
                return Err(REGISTRATION_LINE_CHANGED);
            }
        }
        let mut carried = row.clone();
        carried.candidate_id = new.candidate_id.clone();
        carried.line_number = new.line_number as i32;
        carried.call_expression_span_start = row.call_expression_span_start.map(|_| new.span_start);
        carried.call_expression_span_end = row.call_expression_span_end.map(|_| new.span_end);
        match self.hint(&row.call_expression_text, row.call_expression_line) {
            Hint::Kept(line) => carried.call_expression_line = line,
            Hint::Changed => return Err(HINT_OUTSIDE_REGISTRATION_CHANGED),
        }
        match self.hint(&row.payload_expression_text, row.payload_expression_line) {
            Hint::Kept(line) => carried.payload_expression_line = line,
            Hint::Changed => return Err(HINT_OUTSIDE_REGISTRATION_CHANGED),
        }
        Ok(carried)
    }

    /// The line `old_line` moved to, when the edit left it alone and every
    /// text in `texts` still stands there. `None` when any of it cannot be
    /// proven.
    fn anchored_at(&self, old_line: Option<i64>, texts: &[&str]) -> Option<i32> {
        let line = u32::try_from(old_line?).ok().filter(|line| *line > 0)?;
        let mapped = map_line(self.hunks, line)?;
        texts
            .iter()
            .all(|text| self.text_stands(mapped, text))
            .then_some(mapped as i32)
    }

    /// A resolver is carried when its function line is unchanged and still
    /// names the function and the return type the row states. A resolver-less
    /// row (no function, no line) has no anchor and is not carried.
    fn graphql_operation(&self, row: &GraphqlOperation) -> Option<GraphqlOperation> {
        let name = row.resolver_function.as_deref()?;
        let mut texts = vec![name];
        texts.extend(row.primary_type_symbol.as_deref());
        let line = self.anchored_at(row.resolver_line.map(i64::from), &texts)?;
        let mut carried = row.clone();
        carried.resolver_line = Some(line);
        Some(carried)
    }

    /// A pub/sub row is carried when its line is unchanged and still holds the
    /// topic and the payload type symbol, and its payload hint stands.
    fn pubsub_operation(&self, row: &PubsubOperation) -> Option<PubsubOperation> {
        let mut texts = vec![row.topic.as_str()];
        texts.extend(row.primary_type_symbol.as_deref());
        let line = self.anchored_at(Some(i64::from(row.line_number)), &texts)?;
        let mut carried = row.clone();
        carried.line_number = line;
        match self.hint(&row.payload_expression_text, row.payload_expression_line) {
            Hint::Kept(at) => carried.payload_expression_line = at,
            Hint::Changed => return None,
        }
        Some(carried)
    }

    /// A dispatch table is carried only when the edit provably left the
    /// switch alone: every hunk ends strictly above the table's line in the
    /// old file, so everything from the switch line down is byte-identical and
    /// only shifted, and the line still names the field. Any hunk at or below
    /// the switch line (an added case label, or an edit after the switch's
    /// end, which is refused on purpose: the switch's extent is not known)
    /// means not carried. The handler name sits on the function line, which is
    /// not checked.
    fn dispatch_table(&self, row: &DispatchTable) -> Option<DispatchTable> {
        let old_line = row.line_number?;
        // A hunk with no old lines is an insertion after `old_start`.
        if !self
            .hunks
            .iter()
            .all(|h| h.old_start + h.old_count.max(1) - 1 < old_line)
        {
            return None;
        }
        let line = self.anchored_at(row.line_number.map(i64::from), &[row.field.as_str()])?;
        let mut carried = row.clone();
        carried.line_number = Some(line as u32);
        Some(carried)
    }

    fn mount(&self, row: &MountResult) -> Option<MountResult> {
        let line = u32::try_from(row.line_number).ok()?;
        let mapped = map_line(self.hunks, line)?;
        let mut carried = row.clone();
        carried.line_number = mapped as i32;
        Some(carried)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_state::{Hunk, parse_hunk_header};
    use serde_json::json;

    const OLD: &str = "\
export async function itemRoutes(api: Api) {
  api.get('/items/:id', async (request) => {
    return { id: request.id, connected: true };
  });
  api.get('/items/:id/status', async (request, reply) => {
    reply.send({ id: request.id, state: 'ready' });
  });
  api.get('/items', listItems);
}
function listItems() {
  return [{ id: '1', connected: true }];
}
";

    fn id_at(text: &str, line: usize) -> String {
        SwcScanner::new()
            .scan_every_call(Path::new("routes.ts"), text)
            .candidates
            .iter()
            .find(|c| c.line_number == line)
            .unwrap_or_else(|| panic!("no candidate at line {line}"))
            .candidate_id
            .clone()
    }

    fn endpoint(
        line: usize,
        path: &str,
        resp: Option<(&str, usize)>,
        style: &str,
    ) -> serde_json::Value {
        json!({
            "candidate_id": id_at(OLD, line),
            "line_number": line,
            "owner_node": "api",
            "method": "GET",
            "path": path,
            "handler_name": "inline",
            "pattern_matched": ".get(",
            "payload_expression_text": null,
            "payload_expression_line": null,
            "response_expression_text": resp.map(|r| r.0),
            "response_expression_line": resp.map(|r| r.1),
            "emission_style": style,
            "primary_type_symbol": null,
            "type_import_source": null,
        })
    }

    fn answer() -> FileAnalysisResult {
        serde_json::from_value(json!({
            "mounts": [{
                "line_number": 1, "parent_node": "api", "child_node": "x",
                "mount_path": "/m", "import_source": null, "pattern_matched": ".register(",
            }],
            "endpoints": [
                endpoint(2, "/items/:id", Some(("{ id: request.id, connected: true }", 3)), "return-value"),
                endpoint(5, "/items/:id/status", Some(("{ id: request.id, state: 'ready' }", 6)), "imperative-send"),
                endpoint(8, "/items", Some(("[{ id: '1', connected: true }]", 11)), "return-value"),
            ],
            "data_calls": [],
        }))
        .expect("answer parses")
    }

    fn run(new: &str, hunks: &[Hunk]) -> FileCarry {
        carry_file("routes.ts", OLD, new, hunks, &answer())
    }

    fn insert_two_lines_above() -> (String, Vec<Hunk>) {
        let new = format!("// one\n// two\n{OLD}");
        let hunks = vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 2,
        }];
        (new, hunks)
    }

    fn hunks_of(headers: &[&str]) -> Vec<Hunk> {
        headers
            .iter()
            .map(|h| parse_hunk_header(h).unwrap_or_else(|| panic!("header {h}")))
            .collect()
    }

    #[test]
    fn hunk_headers_parse_in_every_form() {
        assert_eq!(
            parse_hunk_header("@@ -0,0 +1,2 @@ fn"),
            Some(Hunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -4 +4 @@"),
            Some(Hunk {
                old_start: 4,
                old_count: 1,
                new_start: 4,
                new_count: 1
            })
        );
        assert_eq!(
            parse_hunk_header("@@ -7,3 +6,0 @@"),
            Some(Hunk {
                old_start: 7,
                old_count: 3,
                new_start: 6,
                new_count: 0
            })
        );
        assert_eq!(parse_hunk_header("+++ b/x"), None);
    }

    #[test]
    fn the_line_map_shifts_by_the_hunks_above() {
        // two lines inserted after line 0, one line at 5 replaced by two
        let hs = hunks_of(&["@@ -0,0 +1,2 @@", "@@ -5 +7,2 @@"]);
        assert_eq!(map_line(&hs, 1), Some(3));
        assert_eq!(map_line(&hs, 4), Some(6));
        assert_eq!(map_line(&hs, 5), None);
        assert_eq!(map_line(&hs, 6), Some(9));
        // old lines 3-4 deleted
        let hs = hunks_of(&["@@ -3,2 +2,0 @@"]);
        assert_eq!(map_line(&hs, 2), Some(2));
        assert_eq!(map_line(&hs, 3), None);
        assert_eq!(map_line(&hs, 5), Some(3));
        // an insertion after line 2 leaves line 2 where it was
        let hs = hunks_of(&["@@ -2,0 +3,1 @@"]);
        assert_eq!(map_line(&hs, 2), Some(2));
        assert_eq!(map_line(&hs, 3), Some(4));
    }

    #[test]
    fn rows_below_an_insertion_move_and_keep_their_hints() {
        let (new, hunks) = insert_two_lines_above();
        let carry = run(&new, &hunks);
        assert_eq!(carry.carried, 4, "{:?}", carry.not_carried);
        assert!(carry.not_carried.is_empty(), "{:?}", carry.not_carried);
        let lines: Vec<i32> = carry
            .answer
            .endpoints
            .iter()
            .map(|e| e.line_number)
            .collect();
        assert_eq!(lines, vec![4, 7, 10]);
        assert_eq!(carry.answer.endpoints[0].response_expression_line, Some(5));
        assert_eq!(carry.answer.mounts[0].line_number, 3);
        assert_eq!(carry.answer.endpoints[0].candidate_id, id_at(&new, 4));
    }

    #[test]
    fn an_inline_hint_the_edit_touched_is_cleared_and_a_return_value_row_is_carried() {
        let (shifted, _) = insert_two_lines_above();
        let new = shifted.replace("connected: true };", "isConnected: true };");
        // old line 3 replaced by one new line (5), two lines inserted above
        let hunks = vec![
            Hunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2,
            },
            Hunk {
                old_start: 3,
                old_count: 1,
                new_start: 5,
                new_count: 1,
            },
        ];
        let carry = run(&new, &hunks);
        let first = &carry.answer.endpoints[0];
        assert_eq!(first.response_expression_text, None);
        assert_eq!(first.response_expression_line, None);
        assert_eq!(first.line_number, 4);
        assert!(carry.not_carried.is_empty(), "{:?}", carry.not_carried);
    }

    #[test]
    fn an_imperative_send_whose_line_changed_is_carried_untyped() {
        let new = OLD.replace("state: 'ready'", "state: 'done'");
        let hunks = vec![Hunk {
            old_start: 6,
            old_count: 1,
            new_start: 6,
            new_count: 1,
        }];
        let carry = run(&new, &hunks);
        assert_eq!(carry.carried, 4);
        assert_eq!(
            carry.not_carried,
            vec!["routes.ts:5 carried_untyped".to_string()]
        );
        assert_eq!(
            tally_by_reason(&carry.not_carried),
            std::collections::BTreeMap::from([(CARRIED_UNTYPED.to_string(), 1)])
        );
    }

    #[test]
    fn a_changed_registration_line_is_not_carried_and_is_named() {
        let new = OLD.replace("/items/:id/status", "/items/:id/state");
        let hunks = vec![Hunk {
            old_start: 5,
            old_count: 1,
            new_start: 5,
            new_count: 1,
        }];
        let carry = run(&new, &hunks);
        assert_eq!(carry.carried, 3);
        assert_eq!(
            carry.not_carried,
            vec!["routes.ts:5 registration_line_changed".to_string()]
        );
        assert_eq!(carry.answer.endpoints.len(), 2);
    }

    #[test]
    fn a_named_handler_whose_body_changed_is_not_carried() {
        let new = OLD.replace("connected: true }];", "isConnected: true }];");
        let hunks = vec![Hunk {
            old_start: 11,
            old_count: 1,
            new_start: 11,
            new_count: 1,
        }];
        let carry = run(&new, &hunks);
        assert_eq!(carry.carried, 3);
        assert_eq!(
            carry.not_carried,
            vec![format!("routes.ts:8 {HINT_OUTSIDE_REGISTRATION_CHANGED}")]
        );
    }

    #[test]
    fn a_call_found_twice_or_not_at_all_is_not_carried() {
        // Two identical registrations now stand where one did.
        let new = OLD.replace(
            "  api.get('/items', listItems);\n",
            "  api.get('/items', listItems); api.get('/items', listItems);\n",
        );
        let hunks = vec![Hunk {
            old_start: 8,
            old_count: 1,
            new_start: 8,
            new_count: 1,
        }];
        let carry = run(&new, &hunks);
        assert_eq!(carry.carried, 3);
        assert!(carry.not_carried[0].ends_with(REGISTRATION_LINE_CHANGED));
        // An unchanged line per the diff that holds no such call (a stale diff).
        let gone = OLD.replace("api.get('/items', listItems);", "// removed");
        let carry = run(&gone, &[]);
        assert_eq!(
            carry.not_carried,
            vec!["routes.ts:8 no_matching_call".to_string()]
        );
    }

    #[test]
    fn rows_of_other_protocols_are_counted_not_carried() {
        let mut answer = answer();
        answer.graphql_operations = serde_json::from_value(json!([{
            "kind": "query", "field": "item",
            "primary_type_symbol": null, "type_import_source": null,
        }]))
        .expect("graphql row parses");
        let carry = carry_file("routes.ts", OLD, OLD, &[], &answer);
        assert_eq!(carry.examined, 5);
        assert_eq!(carry.carried, 4);
        assert_eq!(
            carry.not_carried,
            vec!["routes.ts not_carried_graphql_operations".to_string()]
        );
    }

    #[test]
    fn unreadable_old_text_counts_every_row() {
        let carry = unavailable("routes.ts", &answer());
        assert_eq!(carry.carried, 0);
        assert_eq!(carry.not_carried.len(), 4);
        assert!(
            carry
                .not_carried
                .iter()
                .all(|r| r.ends_with(OLD_SOURCE_UNAVAILABLE))
        );
    }

    const SUBSCRIBE: &str = "\
import { Kafka } from 'kafkajs';
const kafka = new Kafka({ brokers: [] });
const consumer = kafka.consumer({ groupId: 'g' });
export async function run() {
  await consumer.subscribe({ topic: 'order.placed' });
}
";

    /// A model row on an import-gated messaging subscribe is carried with no
    /// detection list: the carry places the row by the calls in the file, not
    /// by re-running the scanner behind the lists (N24).
    #[test]
    fn a_messaging_subscribe_row_is_carried_without_detection_lists() {
        let id = SwcScanner::new()
            .scan_content(
                Path::new("consumer.ts"),
                SUBSCRIBE,
                &[],
                &["kafkajs".to_string()],
            )
            .candidates
            .iter()
            .find(|c| c.callee_property.as_deref() == Some("subscribe"))
            .expect("the gated scan raises the subscribe")
            .candidate_id
            .clone();
        let answer: FileAnalysisResult = serde_json::from_value(json!({
            "mounts": [],
            "endpoints": [],
            "data_calls": [{
                "candidate_id": id,
                "line_number": 5,
                "target": "order.placed",
                "method": null,
                "pattern_matched": ".subscribe(",
                "payload_expression_text": null,
                "payload_expression_line": null,
                "primary_type_symbol": null,
                "type_import_source": null,
            }],
        }))
        .expect("answer parses");
        let new = format!("// one\n{SUBSCRIBE}");
        let hunks = vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 1,
        }];
        let carry = carry_file("consumer.ts", SUBSCRIBE, &new, &hunks, &answer);
        assert_eq!(carry.not_carried, Vec::<String>::new());
        assert_eq!(carry.carried, 1);
        assert_eq!(carry.answer.data_calls[0].line_number, 6);
    }

    const OTHER: &str = "\
export function resolveWidget(): WidgetView {
  return { id: '1' };
}
export async function emit(bus: Bus) {
  bus.publish('widget.made', { id: '1' });
}
export async function handle(body: Body) {
  switch (body.verb) {
    case 'make':
      return 1;
    case 'drop':
      return 2;
  }
}
";

    fn other_answer() -> FileAnalysisResult {
        serde_json::from_value(json!({
            "mounts": [], "endpoints": [], "data_calls": [],
            "graphql_operations": [{
                "kind": "query", "field": "widget",
                "resolver_function": "resolveWidget", "resolver_line": 1,
                "primary_type_symbol": "WidgetView", "type_import_source": null,
            }],
            "pubsub_operations": [{
                "topic": "widget.made", "role": "publisher", "line_number": 5,
                "primary_type_symbol": null, "type_import_source": null,
                "payload_expression_text": "{ id: '1' }", "payload_expression_line": 5,
            }],
            "graphql_consumer_locates": [{
                "kind": "query", "field": "widget",
                "result_type_symbol": "WidgetView", "result_type_source": null,
            }],
            "dispatch_tables": [{
                "location": "body", "field": "verb", "values": ["make", "drop"],
                "handler_name": "handle", "file_path": "other.ts", "line_number": 8,
            }],
        }))
        .expect("answer parses")
    }

    fn two_lines_above() -> Vec<Hunk> {
        vec![Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 2,
        }]
    }

    fn replaced(old_line: u32) -> Vec<Hunk> {
        vec![Hunk {
            old_start: old_line,
            old_count: 1,
            new_start: old_line,
            new_count: 1,
        }]
    }

    fn other(new: &str, hunks: &[Hunk]) -> FileCarry {
        carry_file("other.ts", OTHER, new, hunks, &other_answer())
    }

    #[test]
    fn a_resolver_row_moves_with_its_line_and_is_dropped_when_its_name_changes() {
        let new = format!("// one\n// two\n{OTHER}");
        let carry = other(&new, &two_lines_above());
        assert_eq!(carry.answer.graphql_operations[0].resolver_line, Some(3));
        let edited = OTHER.replace("resolveWidget", "resolveGadget");
        let carry = other(&edited, &replaced(1));
        assert!(carry.answer.graphql_operations.is_empty());
        assert!(
            carry
                .not_carried
                .contains(&"other.ts not_carried_graphql_operations".to_string())
        );
        // The return type the row states changed on the same line.
        let edited = OTHER.replace("WidgetView {", "GadgetView {");
        let carry = other(&edited, &replaced(1));
        assert!(carry.answer.graphql_operations.is_empty());
    }

    #[test]
    fn a_pubsub_row_moves_with_its_line_and_is_dropped_when_its_topic_changes() {
        let new = format!("// one\n// two\n{OTHER}");
        let carry = other(&new, &two_lines_above());
        let row = &carry.answer.pubsub_operations[0];
        assert_eq!((row.line_number, row.payload_expression_line), (7, Some(7)));
        let edited = OTHER.replace("widget.made", "widget.gone");
        let carry = other(&edited, &replaced(5));
        assert!(carry.answer.pubsub_operations.is_empty());
        assert!(
            carry
                .not_carried
                .contains(&"other.ts not_carried_pubsub_operations".to_string())
        );
    }

    #[test]
    fn a_dispatch_table_moves_with_its_line_and_is_dropped_when_its_field_changes() {
        let new = format!("// one\n// two\n{OTHER}");
        let carry = other(&new, &two_lines_above());
        assert_eq!(carry.answer.dispatch_tables[0].line_number, Some(10));
        let edited = OTHER.replace("body.verb", "body.kind");
        let carry = other(&edited, &replaced(8));
        assert!(carry.answer.dispatch_tables.is_empty());
        assert!(
            carry
                .not_carried
                .contains(&"other.ts not_carried_dispatch_tables".to_string())
        );
    }

    fn dispatch_not_carried(carry: &FileCarry) -> bool {
        carry.answer.dispatch_tables.is_empty()
            && carry
                .not_carried
                .contains(&"other.ts not_carried_dispatch_tables".to_string())
    }

    #[test]
    fn a_case_label_added_inside_the_switch_is_not_carried() {
        let edited = OTHER.replace(
            "    case 'drop':",
            "    case 'move':\n      return 3;\n    case 'drop':",
        );
        let hunks = vec![Hunk {
            old_start: 10,
            old_count: 0,
            new_start: 11,
            new_count: 2,
        }];
        assert!(dispatch_not_carried(&other(&edited, &hunks)));
    }

    #[test]
    fn an_edit_below_the_switch_is_not_carried_by_design() {
        // Conservative: the switch's end is not known, so any hunk at or below
        // its line refuses the carry, even one after the closing brace.
        let edited = format!("{OTHER}// tail\n");
        let hunks = vec![Hunk {
            old_start: 14,
            old_count: 0,
            new_start: 15,
            new_count: 1,
        }];
        assert!(dispatch_not_carried(&other(&edited, &hunks)));
    }

    #[test]
    fn a_consumer_locate_has_no_anchor_and_is_always_counted() {
        let new = format!("// one\n// two\n{OTHER}");
        let carry = other(&new, &two_lines_above());
        assert!(carry.answer.graphql_consumer_locates.is_empty());
        assert_eq!(
            carry
                .not_carried
                .iter()
                .filter(|r| r.ends_with("not_carried_graphql_consumer_locates"))
                .count(),
            1
        );
        assert_eq!((carry.examined, carry.carried), (4, 3));
        assert_eq!(carry.answer.dispatch_tables.len(), 1);
    }

    #[test]
    fn an_unedited_file_carries_the_four_lists_as_before() {
        let carry = other(OTHER, &[]);
        let a = &carry.answer;
        assert_eq!(a.graphql_operations[0].resolver_line, Some(1));
        assert_eq!(a.pubsub_operations[0].line_number, 5);
        assert_eq!(a.dispatch_tables[0].line_number, Some(8));
        assert_eq!(carry.carried, 3);
    }
}
