//! The `carrick.check/0` output: what `touch` and `check` print.
//!
//! The contract these types serialize is `docs/local-mode-output.md`, and it
//! is read by surfaces outside this repo (the editor hook, the LSP shim). A
//! field may be added here; renaming or removing one is a change to that
//! document and to every reader of it.
//!
//! Both commands print the same shape. `touch` states locations and
//! counterparts with every verdict null; `check` fills the verdicts in from
//! what the index already computed. Locations come first and the boundary
//! comes last, so a reader that stops early has the facts.

use serde::{Deserialize, Serialize};

use crate::boundary::ServiceBoundary;

/// The version marker on every response.
pub const SCHEMA: &str = "carrick.check/0";

/// Why a read-only command could not answer. Never an exit code: a hook that
/// fails an edit because an index is missing is worse than one that says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// No `.carrick/` for this file.
    NotIndexed,
    /// The file is not under any repo this workspace covers: not listed, or
    /// listed and excluded.
    NotInWorkspace,
    /// The index is there and could not be read.
    IndexUnreadable,
}

impl ReadError {
    pub fn wire(self) -> &'static str {
        match self {
            ReadError::NotIndexed => "not_indexed",
            ReadError::NotInWorkspace => "not_in_workspace",
            ReadError::IndexUnreadable => "index_unreadable",
        }
    }

    /// The one line a human (or a model) gets on stderr, naming the next move.
    pub fn message(self) -> &'static str {
        match self {
            ReadError::NotIndexed => {
                "no local index for this file. Run `carrick index --workspace <dir>` in the \
                 folder holding your repos."
            }
            ReadError::NotInWorkspace => {
                "this file is not under any repo this workspace covers. Check the repos and \
                 the exclude list in carrick-workspace.json, then re-index."
            }
            ReadError::IndexUnreadable => {
                "the local index could not be read. Re-run `carrick index --workspace <dir>`."
            }
        }
    }
}

/// Why a read could not answer, with the exact sentence the user was given.
///
/// [`ReadError`] is the machine code and it is deliberately coarse: three
/// values cover every refusal. The sentence is not. "The index could not be
/// read" and "this index was written by carrick 0.3.58, which wrote format 2,
/// and this build reads 3" are the same code and different answers, and until
/// carrick#1009 the second one existed only on stderr — so the language server,
/// which reads the JSON body, published nothing at all and the editor went
/// quiet. Every refusal now carries its sentence on the wire.
#[derive(Debug, Clone)]
pub struct ReadFailure {
    pub error: ReadError,
    message: String,
}

impl ReadFailure {
    /// A refusal with nothing more specific to say than its kind.
    pub fn new(error: ReadError) -> Self {
        Self {
            error,
            message: error.message().to_string(),
        }
    }

    /// A refusal that knows more than its kind does — the sentence the code
    /// that refused wrote, which names the file, the versions, or the move.
    pub fn detailed(error: ReadError, message: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
        }
    }

    /// The one sentence every surface prints for this refusal.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The error body, printed to stdout so a reader parsing JSON always gets JSON.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorOutput {
    pub schema: String,
    pub error: String,
    /// The sentence, beside the code. Always present: a reader that wants to
    /// show a human why there is no answer should never have to reconstruct
    /// one from an enum value (carrick#1009).
    pub message: String,
    /// A scan building the index the caller asked for, when one is running.
    /// "There is no index" and "one is being built right now" are different
    /// answers, and a reader given only the first starts a second scan
    /// (carrick#992).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_scans: Vec<super::scan_state::ScanState>,
    /// What the last scan of this workspace reported, for a reader parsing
    /// `--json`. Carried on the error body too, because a run killed before
    /// it wrote an index still uploaded the repos it got through
    /// (carrick#995). Nothing rendered here says anything about it: our
    /// inference cost is ours (carrick#1236).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan: Option<crate::scan_spend::RunSpend>,
    /// The same as [`StatusOutput::analysing`], carried on the error body too:
    /// "there is no index" and "the analysis that builds it is running in the
    /// cloud" are different answers (carrick#1229).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysing: Vec<String>,
}

impl ErrorOutput {
    /// The schema is the one the CALLER asked under: a `status` failure is a
    /// `carrick.status/0` body, so a reader that rejects any other marker
    /// still gets an answer it can parse.
    pub fn new(failure: &ReadFailure, schema: &str) -> Self {
        Self {
            schema: schema.to_string(),
            error: failure.error.wire().to_string(),
            message: failure.message().to_string(),
            running_scans: Vec::new(),
            analysing: Vec::new(),
            last_scan: None,
        }
    }

    pub fn with_scans(mut self, scans: Vec<super::scan_state::ScanState>) -> Self {
        self.running_scans = scans;
        self
    }

    pub fn with_last_scan(mut self, last_scan: Option<crate::scan_spend::RunSpend>) -> Self {
        self.last_scan = last_scan;
        self
    }
}

/// The other side of a contract, with where to find it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Counterpart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub role: String,
    pub service: String,
    /// Relative to the counterpart's own repo, which is a different repo from
    /// the one the queried file lives in.
    pub file: String,
    pub line: Option<u32>,
    /// The absolute path of that repo on this machine, so a reader can open
    /// `repo/file` instead of guessing which directory `file` hangs off
    /// (carrick#709). `None` when the index no longer holds that repo.
    pub repo: Option<String>,
}

/// What the index concluded about one row.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The type layer's word, in the same vocabulary as `verdict_state` on the
    /// PR-result payload (carrick#727, carrick#731): `resolved` = a compiler
    /// verdict with no `any`/`unknown`/error on either side; `unresolved` = a
    /// verdict was attempted and a side would not resolve; `not_checked` = no
    /// type verdict bears on this row.
    ///
    /// Never a statement about freshness: `stale` and `changed_since_index` at
    /// the top level say whether the tree has moved.
    pub state: String,
    pub result: Option<String>,
    pub detail: String,
}

/// One route or call in the queried file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Item {
    pub kind: String,
    pub method: String,
    pub path: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub source: String,
    pub resolution_source: Option<String>,
    pub evidence: Option<String>,
    pub counterparts: Vec<Counterpart>,
    pub verdict: Option<Verdict>,
    /// The type the READING side of [`Item::direction`] declares: the
    /// producer's request type on a `request`, what the call site reads on a
    /// `response`. Capped at [`MAX_TYPE_TEXT_CHARS`] characters, with `...`
    /// where the cap cut it.
    ///
    /// `expected` and `actual` are the two ends of one assignability check,
    /// the same ends the verdict's own detail names when it says one type is
    /// "not assignable to" another: `actual` is the source, `expected` is the
    /// target. Which service holds which flips with the direction, so a reader
    /// pairing them must read [`Item::direction`] too (carrick#1033).
    ///
    /// Absent whenever the index does not hold it — nothing was compared, the
    /// check stated no direction, or the pair's types were never resolved. An
    /// absent field is "this run did not state it", never a type of `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_type: Option<String>,
    /// The type the SENDING side of [`Item::direction`] states: what the
    /// consumer sends on a `request`, the producer's response type on a
    /// `response`. Capped and absent on the same terms as
    /// [`Item::expected_type`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_type: Option<String>,
    /// Which half of the contract the two types above belong to: `request` or
    /// `response`. The [`crate::cloud_storage::ManifestTypeKind`] spelling,
    /// because it is the same fact the type check keyed its outcome on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

/// How much printed type text one item carries.
///
/// A resolved shape can run to thousands of characters, and the two surfaces
/// this feeds — an LSP diagnostic and a hook line — are read in one glance. The
/// cap is applied where the row is built, so what a reader receives is already
/// bounded and nothing downstream has to re-truncate it.
pub const MAX_TYPE_TEXT_CHARS: usize = 200;

/// The whole answer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CheckOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_checked_at: Option<String>,
    #[serde(default)]
    pub hosted: Option<super::hosted::HostedProvenance>,
    #[serde(default)]
    pub hosted_state: super::hosted::HostedState,
    pub schema: String,
    /// Relative to the repo that owns it.
    pub file: String,
    /// The absolute path of that repo on this machine. `repo` + `file` is the
    /// path to open; `file` alone is what the index keys on.
    pub repo: String,
    pub service: String,
    pub index_commit: String,
    pub indexed_at: String,
    pub scanner_version: String,
    pub changed_since_index: usize,
    pub stale: bool,
    pub deleted: bool,
    pub items: Vec<Item>,
    pub boundary: Option<ServiceBoundary>,
    /// What this index could not classify at all, in one sentence. Not part of
    /// the boundary block (which counts what the scan itself counted); this is
    /// the statement that a local index has no model behind it, so a reader
    /// never mistakes a thin answer for a quiet one.
    pub boundary_note: String,
    /// The boundary as this command prints it, line by line: the note above
    /// and then the counts the scan kept. A reader that renders the boundary
    /// prints these bytes rather than re-wording the struct, so a hook and a
    /// terminal say the same sentence about the same number (carrick#709).
    /// The struct stays beside it for a reader that wants the numbers.
    pub boundary_lines: Vec<String>,
    /// What a `--recheck` call did about the file having moved since the index
    /// (carrick#1036). Present on every `check --recheck`, including the ones
    /// where nothing ran — a file the tree has not changed, or one that is no
    /// longer on disk — because a reader told to read `ran` must not meet an
    /// absent key (carrick#1374). Absent only when no re-check was asked for,
    /// which is every `touch` and every language-server read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck: Option<Recheck>,
    /// What other files of this repo use from this one, as the index recorded
    /// it: the callers of its functions and the operations that read its
    /// types (carrick#2067). Locations only, never a verdict, so `touch`
    /// carries it too. Absent when the index predates the field; present with
    /// empty lists when the index recorded nothing for this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses: Option<Uses>,
    /// [`Uses::lines`] for the block above, the bytes a hook and a terminal
    /// both print. Always present, and empty when there is nothing to say.
    #[serde(default)]
    pub uses_lines: Vec<String>,
}

/// What other files use from the queried one.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Uses {
    pub functions: Vec<FunctionUse>,
    pub types: Vec<TypeUse>,
}

/// A function this file declares, and its callers in other files.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FunctionUse {
    /// `Class.member` where a call site qualifies it, the bare name otherwise.
    pub name: String,
    /// Where it starts; zero when the scan recorded no line.
    pub line: u32,
    /// Every distinct caller; `callers` lists at most 20 of them.
    pub callers_total: usize,
    pub callers: Vec<Caller>,
}

/// One call site, relative to the same repo as `file` at the top level.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub service: String,
    pub file: String,
    pub line: u32,
    /// The calling function, or `<module>` for a call at the top of a file.
    pub function: String,
}

/// A type this file declares, and the operations in other files that read it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeUse {
    pub symbol: String,
    pub line: u32,
    pub operations: Vec<TypeOperation>,
}

/// One route or call whose request or response type is the type above.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeOperation {
    /// `route` or `call`.
    pub kind: String,
    /// `request` or `response`.
    pub direction: String,
    pub service: String,
    pub method: String,
    pub path: String,
    pub file: String,
    pub line: u32,
    /// The other side of that operation, as [`Item::counterparts`] states it.
    pub counterparts: Vec<Counterpart>,
}

/// How many locations one uses line names before it counts the rest.
const MAX_USE_LOCATORS: usize = 4;

impl Uses {
    /// One line per type use and per used function, as the terminal and the
    /// hook print them. `service` is the queried file's service: a location in
    /// any other service is prefixed with its own.
    ///
    /// Types first, in line order, one line per operation (kind, direction,
    /// method, path); then functions, most callers first and then by line. A
    /// function or type nothing outside the file uses has no line.
    pub fn lines(&self, service: &str) -> Vec<String> {
        let mut lines = Vec::new();
        for used in &self.types {
            let mut groups: Vec<(&TypeOperation, Vec<&TypeOperation>)> = Vec::new();
            for operation in &used.operations {
                let same = |first: &&TypeOperation| {
                    (&first.kind, &first.direction, &first.method, &first.path)
                        == (
                            &operation.kind,
                            &operation.direction,
                            &operation.method,
                            &operation.path,
                        )
                };
                match groups.iter_mut().find(|(first, _)| same(first)) {
                    Some((_, members)) => members.push(operation),
                    None => groups.push((operation, vec![operation])),
                }
            }
            for (first, members) in groups {
                let sites: Vec<String> = members
                    .iter()
                    .map(|operation| {
                        locator(&operation.service, service, &operation.file, operation.line)
                    })
                    .collect();
                let mut counterparts: Vec<&Counterpart> = Vec::new();
                for counterpart in members.iter().flat_map(|op| &op.counterparts) {
                    if !counterparts.contains(&counterpart) {
                        counterparts.push(counterpart);
                    }
                }
                let noun = if first.kind == "route" {
                    "route(s)"
                } else {
                    "call(s)"
                };
                let mut line = format!(
                    "- type {}{} is the {} type of {} {} at {} {}: {}",
                    used.symbol,
                    at_line(used.line),
                    first.direction,
                    first.method,
                    first.path,
                    sites.len(),
                    noun,
                    capped(&sites, sites.len())
                );
                if !counterparts.is_empty() {
                    let named: Vec<String> = counterparts
                        .iter()
                        .map(|counterpart| counterpart_locator(counterpart))
                        .collect();
                    line.push_str(&format!(
                        ". {}: {}",
                        role_label(&counterparts),
                        capped(&named, named.len())
                    ));
                }
                lines.push(line);
            }
        }

        let mut functions: Vec<&FunctionUse> = self
            .functions
            .iter()
            .filter(|function| function.callers_total > 0)
            .collect();
        functions.sort_by(|a, b| {
            b.callers_total
                .cmp(&a.callers_total)
                .then(a.line.cmp(&b.line))
                .then(a.name.cmp(&b.name))
        });
        for function in functions {
            let callers: Vec<String> = function
                .callers
                .iter()
                .map(|caller| locator(&caller.service, service, &caller.file, caller.line))
                .collect();
            lines.push(format!(
                "- {}{} has {} caller(s) outside this file: {}",
                function.name,
                at_line(function.line),
                function.callers_total,
                capped(&callers, function.callers_total)
            ));
        }
        lines
    }
}

/// ` (line N)`, or nothing where the scan recorded no line.
fn at_line(line: u32) -> String {
    if line > 0 {
        format!(" (line {line})")
    } else {
        String::new()
    }
}

/// `file:line`, prefixed with the service when it is not the queried file's.
fn locator(of: &str, queried: &str, file: &str, line: u32) -> String {
    let prefix = if of == queried {
        String::new()
    } else {
        format!("{of} ")
    };
    if line > 0 {
        format!("{prefix}{file}:{line}")
    } else {
        format!("{prefix}{file}")
    }
}

/// A counterpart always carries its service, as an item line does.
fn counterpart_locator(counterpart: &Counterpart) -> String {
    let place = match counterpart.line {
        Some(line) if !counterpart.file.is_empty() => format!("{}:{line}", counterpart.file),
        _ if !counterpart.file.is_empty() => counterpart.file.clone(),
        _ => "an unnamed file".to_string(),
    };
    format!("{} {place}", counterpart.service)
}

/// The first [`MAX_USE_LOCATORS`] of `shown`, then `, and N more` against
/// `total`, the exact count.
fn capped(shown: &[String], total: usize) -> String {
    let named = &shown[..shown.len().min(MAX_USE_LOCATORS)];
    let rest = total.saturating_sub(named.len());
    if rest > 0 {
        format!("{}, and {rest} more", named.join(", "))
    } else {
        named.join(", ")
    }
}

/// What to call the other side, as the hook's item lines call it.
fn role_label(counterparts: &[&Counterpart]) -> &'static str {
    let first = counterparts
        .first()
        .map(|counterpart| counterpart.role.as_str());
    let one_role = counterparts
        .iter()
        .all(|counterpart| Some(counterpart.role.as_str()) == first);
    match first {
        Some("consumer") if one_role => "Consumers",
        Some("producer") if one_role => "Producers",
        Some("peer") if one_role => "Same contract in",
        _ => "Counterparts",
    }
}

/// What the re-check behind an edit did, and how old the answer is if it did
/// nothing.
///
/// The rows above are either this run's or the index's, never a mixture, and
/// this block is the only place that says which. A reader that does not know
/// the field treats the answer as the indexed one, which is what it was before
/// this existed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Recheck {
    /// `extraction+types` — the file was re-extracted, re-joined against the
    /// blobs the index holds, and at least one of its rows carries a type
    /// verdict. `extraction` — the same, and no row of this file carries one:
    /// nothing here pairs with anything, or the pairs it has were not both
    /// resolved. Which of those it is, is in each row's own detail. `none` —
    /// the rows above are the indexed ones, and `reason` says why: the file
    /// has not changed since the index, it is no longer on disk, or a re-check
    /// was warranted and did not finish.
    pub ran: String,
    /// Wall time of the re-check, including the run that missed its budget.
    /// Zero where no re-check was attempted at all.
    pub elapsed_ms: u64,
    /// When the rows above were computed, on a `none`. Absent when they are
    /// this run's, because then the answer is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_since: Option<String>,
    /// Why the re-check did not run, on a `none`. One sentence, for a log, and
    /// the only thing that tells a re-check that failed from one that was not
    /// needed (carrick#1374).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Functions this file declares in the working tree that the index does
    /// not hold under that name (carrick#1330).
    ///
    /// The re-check already re-extracts the file, so this costs one set
    /// difference on top of work that has been done. Empty on a `none` — a
    /// re-check that did not run extracted nothing to compare — and empty,
    /// which is the usual answer, when the edit added no function.
    ///
    /// It is a NAME comparison against one commit, and both halves of that
    /// matter to a reader: the index holds no body hash, so a renamed or
    /// rewritten function is a new name here, and the commit it is compared
    /// against is `index_commit` on the answer around this block.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_functions: Vec<NewFunction>,
}

/// One function the working tree declares and the index does not.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NewFunction {
    /// As the source spells it: `name`, or `Class.member`.
    pub name: String,
    /// Where it starts, so a reader can open it without searching.
    pub line: u32,
}

impl Recheck {
    /// The line a terminal and a hook both print for this block. Keyed on
    /// `ran`, which is the field that says whose rows these are.
    pub fn line(&self) -> String {
        match self.ran.as_str() {
            // The reason is what tells the two `none`s apart: a re-check that
            // was warranted and did not finish, and one that had nothing to do
            // because the file has not moved (carrick#1374). Printing the
            // reason rather than one hardcoded sentence is what keeps the line
            // true for both.
            "none" => format!(
                "re-check: {}, so these verdicts are the indexed ones{}.",
                match self.reason.as_deref() {
                    Some(reason) => format!("did not run — {reason}"),
                    None => format!("did not finish inside its budget ({} ms)", self.elapsed_ms),
                },
                match self.stale_since.as_deref() {
                    Some(since) => format!(", computed at {since}"),
                    None => String::new(),
                }
            ),
            "extraction" => format!(
                "re-check: this file was re-extracted and re-joined from your working tree in {} ms; no type verdict bears on its rows.",
                self.elapsed_ms
            ),
            _ => format!(
                "re-check: these verdicts are from your working tree, re-extracted and type-checked in {} ms.",
                self.elapsed_ms
            ),
        }
    }

    /// The line naming the functions the index does not hold, or none.
    ///
    /// Separate from [`Self::line`] so a reader that prints one does not have
    /// to print the other: this says nothing about a verdict, and the two
    /// answer different questions about the same re-check.
    pub fn new_functions_line(&self, index_commit: &str) -> Option<String> {
        if self.new_functions.is_empty() {
            return None;
        }
        let names: Vec<&str> = self
            .new_functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        Some(format!(
            "new since the index at {}: {}. Not in the index, so nothing has been compared against them.",
            short_commit(index_commit),
            names.join(", ")
        ))
    }
}

impl CheckOutput {
    /// Whether the rows in this answer came from the working tree rather than
    /// from the index.
    pub fn rechecked(&self) -> bool {
        self.recheck
            .as_ref()
            .is_some_and(|recheck| recheck.ran != "none")
    }

    /// The human form: the same content in the same order, for a model reading
    /// a terminal.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} ({}, indexed at {})\n",
            self.file,
            self.service,
            short_commit(&self.index_commit)
        ));
        if self.deleted {
            out.push_str("this file is no longer on disk; the rows below are what the index still holds for it.\n");
        }
        out.push('\n');

        if self.items.is_empty() {
            out.push_str("  no routes or calls indexed in this file.\n\n");
        }
        for item in &self.items {
            let line = item
                .line
                .map(|line| format!("line {line}"))
                .unwrap_or_else(|| "line unknown".to_string());
            let source = match &item.resolution_source {
                Some(source) => format!("[{}: {}]", item.source, source),
                None => format!("[{}]", item.source),
            };
            out.push_str(&format!(
                "  {:<5}  {} {}  {}  {}\n",
                item.kind, item.method, item.path, line, source
            ));
            for counterpart in &item.counterparts {
                let where_ = match counterpart.line {
                    Some(line) if !counterpart.file.is_empty() => {
                        format!("{}:{}", counterpart.file, line)
                    }
                    _ if !counterpart.file.is_empty() => counterpart.file.clone(),
                    _ => "location not recorded".to_string(),
                };
                out.push_str(&format!(
                    "    {:<9} {}  {}\n",
                    counterpart.role, counterpart.service, where_
                ));
            }
            if item.counterparts.is_empty() {
                out.push_str("    no counterpart in this workspace\n");
            }
            if let Some(verdict) = &item.verdict {
                out.push_str(&format!(
                    "    verdict   {} — {}\n",
                    verdict.result.as_deref().unwrap_or(&verdict.state),
                    verdict.detail
                ));
            }
            out.push('\n');
        }

        if !self.uses_lines.is_empty() {
            out.push_str("used outside this file, as indexed:\n");
            for line in &self.uses_lines {
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }

        out.push_str(&format!(
            "changed since index: {} file(s){}\n",
            self.changed_since_index,
            // A re-check that ran has already answered from the working tree,
            // so the sentence that sends a reader to re-index would be false.
            if self.stale && !self.rechecked() {
                "; this file is one of them, so its rows are unresolved since your edit"
            } else {
                ""
            }
        ));
        if let Some(recheck) = &self.recheck {
            out.push_str(&recheck.line());
            out.push('\n');
            if let Some(line) = recheck.new_functions_line(&self.index_commit) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        for line in &self.boundary_lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// The version marker on a `carrick status` answer. Its own schema, because a
/// status answer is about a WORKSPACE and every `carrick.check/0` response is
/// about one file — relaxing that document to admit a fileless shape would
/// make `file` optional for readers that always have one.
pub const STATUS_SCHEMA: &str = "carrick.status/0";

/// How many stale paths a service lists before the list is a sample. The exact
/// total is stated either way, so a reader can always tell "here are all 6"
/// from "here are 50 of 900".
pub const MAX_STALE_FILES: usize = 50;

/// One service of the workspace, as `carrick status` reports it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StatusService {
    #[serde(default)]
    pub hosted: Option<super::hosted::HostedProvenance>,
    #[serde(default)]
    pub hosted_state: super::hosted::HostedState,
    pub service: String,
    /// Absolute path of the repo this service belongs to. Services of one repo
    /// share a commit and a changed-file count, and this is what says so.
    pub repo: String,
    pub index_commit: String,
    pub indexed_at: String,
    pub routes: usize,
    pub calls: usize,
    /// Files THIS SERVICE's scan reads that differ from `index_commit`, or
    /// that git does not track: its own directory and its `include` roots.
    /// Files in the repo that no service reads are under `repos` instead — a
    /// workflow file is not a change to every service in the monorepo
    /// (carrick#997 item 4).
    ///
    /// Source files only, like `StatusRepo::outside_every_service`: this is a
    /// count of rows that may have gone stale, and `carrick.json`, a workflow
    /// or an editor's settings hold none. A single-service repo has no service
    /// directory, so without the filter its one service owns every one of them
    /// (carrick#1007 item 5).
    pub changed_since_index: usize,
    /// Up to [`MAX_STALE_FILES`] of them, repo-relative.
    pub stale_files: Vec<String>,
    /// The exact number, whatever the list length.
    pub stale_files_total: usize,
    /// Whether `stale_files` is a sample rather than the whole set.
    pub stale_files_truncated: bool,
    pub boundary: Option<ServiceBoundary>,
    pub boundary_note: String,
    pub boundary_lines: Vec<String>,
}

/// One repo of the workspace: what moved in it that belongs to no service.
///
/// Every count here is about the tree, not about an API. The services carry
/// what a service's own scan reads; this carries the rest of the repo, once,
/// so a repo-level file is neither attributed to a service that never reads it
/// nor dropped from the answer entirely.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StatusRepo {
    /// Absolute path on this machine.
    pub repo: String,
    /// Its directory name, which is what the index keys blobs by.
    pub name: String,
    /// Everything in the repo that differs from the indexed commit or that git
    /// does not track, services included.
    pub changed_since_index: usize,
    /// How many of those are source files no service in this repo reads.
    ///
    /// Source files only: a changed `carrick.json`, workflow or editor setting
    /// holds no indexed row and cannot make the index stale, and the line this
    /// number renders is about rows going out of date (carrick#1007 item 5).
    /// `changed_since_index` above stays the whole repo's count.
    pub outside_every_service: usize,
    /// Up to [`MAX_STALE_FILES`] of THOSE, repo-relative.
    pub stale_files: Vec<String>,
    pub stale_files_truncated: bool,
}

/// What `carrick status` answers: the workspace, not a file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StatusOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos_detected_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos_added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos_excluded: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosted_checked_at: Option<String>,
    pub schema: String,
    /// The workspace root: the folder holding `carrick-workspace.json` and
    /// `.carrick/`.
    pub workspace: String,
    pub indexed_at: String,
    pub scanner_version: String,
    /// One entry per indexed repo, whatever its services hold.
    #[serde(default)]
    pub repos: Vec<StatusRepo>,
    /// Scans running right now, or stopped without finishing. Empty in the
    /// ordinary case; this is what a detached `carrick index` is
    /// visible through while it runs (carrick#992).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_scans: Vec<super::scan_state::ScanState>,
    /// What the last scan of this workspace reported, one entry per repo it
    /// scanned (carrick#995). Absent until one has run, and read by whoever
    /// parses `--json`; the human render states none of it (carrick#1236).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan: Option<crate::scan_spend::RunSpend>,
    /// Analysis Carrick Cloud is doing for this workspace right now, one entry
    /// per repo handed over (carrick#1229). Empty in the ordinary case, and
    /// the only part of any read command that touches the network.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub analysing: Vec<String>,
    pub services: Vec<StatusService>,
    /// What the last build's type check did: whether it ran, what it judged,
    /// and which verdicts are not in Carrick Cloud (carrick#1490). Absent
    /// for an index that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_check: Option<super::read_model::TypeCheckSummary>,
}

impl StatusOutput {
    /// The human form: one block per service, boundary last, same order as
    /// every other local answer.
    pub fn render(&self) -> String {
        let mut out = String::new();
        // First, because it is the only line about right now: everything below
        // it describes the index as it was when the last build finished.
        let index_dir = std::path::Path::new(&self.workspace).join(super::workspace::INDEX_DIR);
        for line in super::scan_state::status_lines(&self.running_scans, &index_dir) {
            out.push_str(&line);
            out.push('\n');
        }
        if !self.running_scans.is_empty() {
            out.push('\n');
        }
        // Above the index for the same reason: an analysis in flight is about
        // now, and the index below it is about the last build that finished.
        for line in &self.analysing {
            out.push_str(line);
            out.push('\n');
        }
        if !self.analysing.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{} — {} service(s), indexed at {} by carrick {}\n\n",
            self.workspace,
            self.services.len(),
            self.indexed_at,
            self.scanner_version
        ));
        for service in &self.services {
            let waiting = service
                .boundary
                .as_ref()
                .and_then(|boundary| boundary.awaiting_model())
                .map(|sentence| format!("  {sentence}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {:<28} {:>4} route(s)  {:>4} call(s)  {}  changed since index: {}{}\n",
                service.service,
                service.routes,
                service.calls,
                short_commit(&service.index_commit),
                service.changed_since_index,
                waiting
            ));
            for file in &service.stale_files {
                out.push_str(&format!("      changed  {file}\n"));
            }
            if service.stale_files_truncated {
                out.push_str(&format!(
                    "      ... and {} more\n",
                    service.stale_files_total - service.stale_files.len()
                ));
            }
        }
        for repo in &self.repos {
            if repo.outside_every_service == 0 {
                continue;
            }
            out.push_str(&format!(
                "  {}: {} file(s) changed outside every service\n",
                repo.name, repo.outside_every_service
            ));
            for file in &repo.stale_files {
                out.push_str(&format!("      changed  {file}\n"));
            }
            if repo.stale_files_truncated {
                out.push_str(&format!(
                    "      ... and {} more\n",
                    repo.outside_every_service - repo.stale_files.len()
                ));
            }
        }
        for line in self.type_check.iter().flat_map(|check| check.lines()) {
            out.push_str(&format!("  {line}\n"));
        }
        out.push('\n');
        for service in &self.services {
            for line in &service.boundary_lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}

fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn output() -> CheckOutput {
        CheckOutput {
            hosted: None,
            hosted_state: Default::default(),
            hosted_checked_at: None,
            schema: SCHEMA.to_string(),
            file: "app/routes/orders.ts".to_string(),
            repo: "/repos/webapp".to_string(),
            service: "webapp".to_string(),
            index_commit: "a1b2c3d4e5f6".to_string(),
            indexed_at: "2026-09-06T21:00:00Z".to_string(),
            scanner_version: "0.3.41".to_string(),
            changed_since_index: 2,
            stale: true,
            deleted: false,
            items: vec![Item {
                kind: "route".to_string(),
                method: "GET".to_string(),
                path: "/api/orders/:id".to_string(),
                line: Some(12),
                col: None,
                source: "fact".to_string(),
                resolution_source: Some("file_based_route".to_string()),
                evidence: None,
                counterparts: vec![Counterpart {
                    remote: None,
                    role: "consumer".to_string(),
                    service: "admin-ui".to_string(),
                    file: "src/api.ts".to_string(),
                    line: Some(44),
                    repo: Some("/repos/admin-ui".to_string()),
                }],
                verdict: None,
                expected_type: None,
                actual_type: None,
                direction: None,
            }],
            boundary: None,
            boundary_note: super::super::NOT_CLASSIFIED_LOCALLY.to_string(),
            boundary_lines: vec![format!(
                "boundary (webapp): {}",
                super::super::NOT_CLASSIFIED_LOCALLY
            )],
            recheck: None,
            uses: None,
            uses_lines: Vec::new(),
        }
    }

    fn caller(service: &str, file: &str, line: u32) -> Caller {
        Caller {
            service: service.into(),
            file: file.into(),
            line,
            function: "f".into(),
        }
    }

    fn function_use(name: &str, line: u32, callers: Vec<Caller>, total: usize) -> FunctionUse {
        FunctionUse {
            name: name.into(),
            line,
            callers_total: total,
            callers,
        }
    }

    fn counterpart(role: &str, service: &str, file: &str, line: u32) -> Counterpart {
        Counterpart {
            remote: None,
            role: role.into(),
            service: service.into(),
            file: file.into(),
            line: Some(line),
            repo: None,
        }
    }

    fn operation(
        kind: &str,
        service: &str,
        path: &str,
        site: (&str, u32),
        counterparts: Vec<Counterpart>,
    ) -> TypeOperation {
        TypeOperation {
            kind: kind.into(),
            direction: "response".into(),
            service: service.into(),
            method: "GET".into(),
            path: path.into(),
            file: site.0.into(),
            line: site.1,
            counterparts,
        }
    }

    #[test]
    fn a_function_line_names_its_callers_and_counts_past_four() {
        let uses = Uses {
            functions: vec![
                function_use(
                    "send",
                    9,
                    vec![caller("inventory-svc", "src/client.ts", 14)],
                    1,
                ),
                function_use(
                    "formatCents",
                    1,
                    vec![
                        caller("web", "app/lib/receipt.ts", 5),
                        caller("web", "app/routes/a.ts", 3),
                        caller("web", "app/routes/b.ts", 8),
                        caller("payments", "app/x.ts", 2),
                        caller("web", "app/routes/c.ts", 1),
                    ],
                    7,
                ),
                function_use("lonely", 30, Vec::new(), 0),
            ],
            types: Vec::new(),
        };
        assert_eq!(
            uses.lines("web"),
            vec![
                // Most callers first; a caller in another service names it.
                "- formatCents (line 1) has 7 caller(s) outside this file: app/lib/receipt.ts:5, app/routes/a.ts:3, app/routes/b.ts:8, payments app/x.ts:2, and 3 more",
                "- send (line 9) has 1 caller(s) outside this file: inventory-svc src/client.ts:14",
            ]
        );
    }

    #[test]
    fn functions_with_as_many_callers_are_in_line_order() {
        let uses = Uses {
            functions: vec![
                function_use("later", 20, vec![caller("svc", "src/b.ts", 2)], 1),
                function_use(
                    "CatalogClient.readWidget",
                    12,
                    vec![caller("svc", "src/a.ts", 9)],
                    1,
                ),
            ],
            types: Vec::new(),
        };
        assert_eq!(
            uses.lines("svc"),
            vec![
                "- CatalogClient.readWidget (line 12) has 1 caller(s) outside this file: src/a.ts:9",
                "- later (line 20) has 1 caller(s) outside this file: src/b.ts:2",
            ]
        );
    }

    #[test]
    fn a_type_line_names_its_sites_then_the_other_side() {
        let consumers = vec![
            counterpart("consumer", "billing-svc", "src/billing.ts", 9),
            counterpart("consumer", "billing-svc", "src/billing.ts", 17),
        ];
        let uses = Uses {
            functions: vec![function_use(
                "send",
                9,
                vec![caller("web", "src/x.ts", 1)],
                1,
            )],
            types: vec![
                TypeUse {
                    symbol: "OrderSummary".into(),
                    line: 4,
                    operations: vec![operation(
                        "route",
                        "web",
                        "/api/v1/orders/:orderId",
                        ("app/routes/api.v1.orders.$orderId.ts", 12),
                        consumers,
                    )],
                },
                TypeUse {
                    symbol: "Widget".into(),
                    line: 3,
                    operations: vec![
                        operation(
                            "call",
                            "inventory-svc",
                            "/api/v1/widgets/:encoded",
                            ("src/inventory.ts", 9),
                            vec![counterpart("producer", "catalog-web", "app/w.ts", 11)],
                        ),
                        operation(
                            "call",
                            "inventory-svc",
                            "/api/v1/widgets/:encoded",
                            ("src/inventory.ts", 17),
                            vec![counterpart("producer", "catalog-web", "app/w.ts", 11)],
                        ),
                    ],
                },
                TypeUse {
                    symbol: "Health".into(),
                    line: 8,
                    operations: vec![operation(
                        "route",
                        "web",
                        "/health",
                        ("app/h.ts", 2),
                        Vec::new(),
                    )],
                },
            ],
        };
        assert_eq!(
            uses.lines("web"),
            vec![
                // Types first, as the index stored them; the same producer
                // named by two sites is one counterpart.
                "- type OrderSummary (line 4) is the response type of GET /api/v1/orders/:orderId at 1 route(s): app/routes/api.v1.orders.$orderId.ts:12. Consumers: billing-svc src/billing.ts:9, billing-svc src/billing.ts:17",
                "- type Widget (line 3) is the response type of GET /api/v1/widgets/:encoded at 2 call(s): inventory-svc src/inventory.ts:9, inventory-svc src/inventory.ts:17. Producers: catalog-web app/w.ts:11",
                "- type Health (line 8) is the response type of GET /health at 1 route(s): app/h.ts:2",
                "- send (line 9) has 1 caller(s) outside this file: src/x.ts:1",
            ]
        );
    }

    #[test]
    fn mixed_or_peer_counterparts_take_their_own_label() {
        let mixed = vec![
            counterpart("consumer", "a", "a.ts", 1),
            counterpart("producer", "b", "b.ts", 2),
        ];
        let peers = vec![counterpart("peer", "c", "c.ts", 3)];
        let line = |counterparts: Vec<Counterpart>| {
            Uses {
                functions: Vec::new(),
                types: vec![TypeUse {
                    symbol: "T".into(),
                    line: 1,
                    operations: vec![operation("call", "s", "/p", ("x.ts", 5), counterparts)],
                }],
            }
            .lines("s")
        };
        assert_eq!(
            line(mixed),
            vec![
                "- type T (line 1) is the response type of GET /p at 1 call(s): x.ts:5. Counterparts: a a.ts:1, b b.ts:2"
            ]
        );
        assert_eq!(
            line(peers),
            vec![
                "- type T (line 1) is the response type of GET /p at 1 call(s): x.ts:5. Same contract in: c c.ts:3"
            ]
        );
    }

    #[test]
    fn the_terminal_prints_the_uses_block_before_the_change_count() {
        let mut out = output();
        let text = out.render();
        assert!(!text.contains("used outside this file"), "{text}");
        out.uses_lines =
            vec!["- send (line 9) has 1 caller(s) outside this file: src/client.ts:14".into()];
        let text = out.render();
        let block = text
            .find("used outside this file, as indexed:\n- send (line 9) has 1 caller(s) outside this file: src/client.ts:14\n")
            .expect("the block, verbatim");
        let changed = text.find("changed since index").expect("change count");
        let item = text.find("src/api.ts:44").expect("item");
        assert!(item < block && block < changed, "{text}");
    }

    #[test]
    fn the_human_form_leads_with_locations_and_ends_with_the_boundary() {
        let text = output().render();
        let locations = text.find("src/api.ts:44").expect("counterpart location");
        let boundary = text.find("boundary (webapp)").expect("boundary line");
        assert!(locations < boundary, "boundary must come last:\n{text}");
    }

    #[test]
    fn a_stale_file_says_so_in_the_words_a_reader_greps_for() {
        assert!(output().render().contains("unresolved since your edit"));
    }

    #[test]
    fn a_rechecked_answer_says_the_rows_are_this_run_s() {
        let mut fresh = output();
        fresh.recheck = Some(Recheck {
            ran: "extraction+types".to_string(),
            elapsed_ms: 2543,
            stale_since: None,
            reason: None,
            new_functions: Vec::new(),
        });
        let text = fresh.render();
        assert!(
            text.contains("re-extracted and type-checked in 2543 ms"),
            "{text}"
        );
        // The sentence that sends a reader to re-index would be false here.
        assert!(!text.contains("unresolved since your edit"), "{text}");
    }

    #[test]
    fn a_re_check_that_did_not_run_says_how_old_the_answer_is() {
        let mut degraded = output();
        degraded.recheck = Some(Recheck {
            ran: "none".to_string(),
            elapsed_ms: 10_004,
            stale_since: Some("2026-09-13T22:26:26Z".to_string()),
            reason: Some("the re-scan ran past the budget".to_string()),
            new_functions: Vec::new(),
        });
        let text = degraded.render();
        assert!(text.contains("computed at 2026-09-13T22:26:26Z"), "{text}");
        assert!(text.contains("unresolved since your edit"), "{text}");
    }

    /// carrick#1374: a re-check that had nothing to do is a `none` too, and
    /// the line must not tell the reader it ran out of time.
    #[test]
    fn a_re_check_with_nothing_to_do_says_that_rather_than_a_budget() {
        let mut unchanged = output();
        unchanged.stale = false;
        unchanged.recheck = Some(Recheck {
            ran: "none".to_string(),
            elapsed_ms: 0,
            stale_since: Some("2026-09-13T22:26:26Z".to_string()),
            reason: Some("the file has not changed since the index".to_string()),
            new_functions: Vec::new(),
        });
        let text = unchanged.render();
        assert!(
            text.contains("did not run — the file has not changed since the index"),
            "{text}"
        );
        assert!(!text.contains("inside its budget"), "{text}");
    }

    #[test]
    fn a_short_commit_is_safe_on_a_short_string() {
        assert_eq!(short_commit("abc"), "abc");
    }
}

#[cfg(test)]
mod hosted_wire_tests {
    use super::*;

    #[test]
    fn hosted_projection_additions_are_sparse_and_older_payloads_still_parse() {
        let mut value = serde_json::to_value(super::tests::output()).unwrap();
        value["hosted"] = serde_json::json!({"commit":"abc123","indexed_at":"2026-09-10T10:00:00Z","scanner_version":"0.3.58","project":"fixture"});
        value["hosted_state"] = serde_json::json!("enriched");
        value["hosted_checked_at"] = serde_json::json!("2026-09-10T11:00:00Z");
        value["items"][0]["counterparts"][0]["remote"] = serde_json::json!("example/api");
        value["items"][0]["counterparts"][0]["repo"] = serde_json::Value::Null;
        let current: CheckOutput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(current).unwrap(), value);
        for field in ["hosted", "hosted_state", "hosted_checked_at"] {
            value.as_object_mut().unwrap().remove(field);
        }
        value["items"][0]["counterparts"][0]
            .as_object_mut()
            .unwrap()
            .remove("remote");
        let old: CheckOutput = serde_json::from_value(value).unwrap();
        assert!(old.hosted.is_none());
        assert!(old.hosted_checked_at.is_none());
        assert!(old.items[0].counterparts[0].remote.is_none());
        assert!(
            !serde_json::to_value(old).unwrap()["items"][0]["counterparts"][0]
                .as_object()
                .unwrap()
                .contains_key("remote")
        );
    }

    /// The three provenance fields ride INSIDE `hosted`, in these exact
    /// spellings, and only when the row carries them.
    ///
    /// `dirty` is the one a reader acts on — it is what says the generation was
    /// built from bytes no commit describes — and carrick#1255 made the scanner
    /// stamp it from the blob as well as the stored row, so a generation
    /// written before the field existed now reads as dirty. This pins both
    /// halves: `true` is carried and spelled `dirty`, and a hosted object
    /// without the key parses and re-serialises without it rather than as a
    /// `null` a consumer has to interpret.
    #[test]
    fn the_hosted_provenance_fields_are_sparse_and_exactly_spelled() {
        let mut value = serde_json::to_value(super::tests::output()).unwrap();
        value["hosted"] = serde_json::json!({
            "commit":"abc123","indexed_at":"2026-09-10T10:00:00Z",
            "scanner_version":"0.3.58","project":"fixture",
            "source":"laptop","uploaded_by":"ihor","dirty":true
        });
        value["hosted_state"] = serde_json::json!("read_failed");
        let parsed: CheckOutput = serde_json::from_value(value.clone()).unwrap();
        let hosted = parsed.hosted.clone().expect("a hosted row");
        assert_eq!(hosted.dirty, Some(true));
        assert_eq!(hosted.source.as_deref(), Some("laptop"));
        assert_eq!(hosted.uploaded_by.as_deref(), Some("ihor"));
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);

        for field in ["source", "uploaded_by", "dirty"] {
            value["hosted"].as_object_mut().unwrap().remove(field);
        }
        let older: CheckOutput = serde_json::from_value(value.clone()).unwrap();
        let hosted = older.hosted.clone().expect("a hosted row");
        assert_eq!(hosted.dirty, None);
        assert_eq!(hosted.source, None);
        assert_eq!(hosted.uploaded_by, None);
        let written = serde_json::to_value(older).unwrap();
        for field in ["source", "uploaded_by", "dirty"] {
            assert!(
                !written["hosted"].as_object().unwrap().contains_key(field),
                "{field} was written onto a row that never carried it"
            );
        }
        assert_eq!(written, value);
    }

    /// The three carrick#1033 fields ride on the ITEM, in these exact
    /// spellings, and a payload written before they existed still parses with
    /// all three absent rather than empty.
    #[test]
    fn the_two_types_and_the_direction_are_sparse_item_fields() {
        let bare = serde_json::to_value(super::tests::output()).unwrap();
        // The block a re-check adds rides the same rule: present when it ran,
        // absent on every answer written before it existed (carrick#1036).
        let mut with_recheck = serde_json::to_value(super::tests::output()).unwrap();
        with_recheck["recheck"] =
            serde_json::json!({"ran": "extraction+types", "elapsed_ms": 2543});
        let parsed: CheckOutput = serde_json::from_value(with_recheck.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), with_recheck);
        assert!(
            !serde_json::to_value(super::tests::output())
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("recheck"),
            "an answer with no re-check states no block at all"
        );

        for field in ["expected_type", "actual_type", "direction"] {
            assert!(
                bare["items"][0].get(field).is_none(),
                "{field} is written only when the index holds it"
            );
        }

        let mut stated = super::tests::output();
        stated.items[0].expected_type = Some("number".to_string());
        stated.items[0].actual_type = Some("UsersResponse { users: UserV2[] }".to_string());
        stated.items[0].direction = Some("response".to_string());
        let value = serde_json::to_value(&stated).unwrap();
        assert_eq!(
            value["items"][0]["expected_type"],
            serde_json::json!("number")
        );
        assert_eq!(
            value["items"][0]["actual_type"],
            serde_json::json!("UsersResponse { users: UserV2[] }")
        );
        assert_eq!(
            value["items"][0]["direction"],
            serde_json::json!("response")
        );

        let round_tripped: CheckOutput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(round_tripped).unwrap(), value);

        // An item as it was written before the three fields existed.
        let older: Item = serde_json::from_value(serde_json::json!({
            "kind": "route",
            "method": "GET",
            "path": "/api/users",
            "line": 12,
            "col": null,
            "source": "fact",
            "resolution_source": null,
            "evidence": null,
            "counterparts": [],
            "verdict": null
        }))
        .expect("an item written before the fields existed still parses");
        assert!(older.expected_type.is_none());
        assert!(older.actual_type.is_none());
        assert!(older.direction.is_none());
    }

    /// The direction spelling is the type check's own
    /// [`crate::cloud_storage::ManifestTypeKind`] wire value, not a second
    /// vocabulary invented here.
    #[test]
    fn the_direction_words_are_the_type_checks_own() {
        assert_eq!(
            serde_json::to_value(crate::cloud_storage::ManifestTypeKind::Request).unwrap(),
            serde_json::json!("request")
        );
        assert_eq!(
            serde_json::to_value(crate::cloud_storage::ManifestTypeKind::Response).unwrap(),
            serde_json::json!("response")
        );
    }

    #[test]
    fn older_status_and_boundary_payloads_default_only_the_new_fields() {
        let value = serde_json::json!({"schema":"carrick.status/0","workspace":"/fixture","indexed_at":"now","scanner_version":"test","services":[{
            "service":"api","repo":"/fixture/api","index_commit":"abc","indexed_at":"now","routes":0,"calls":0,"changed_since_index":0,"stale_files":[],"stale_files_total":0,"stale_files_truncated":false,"boundary":null,"boundary_note":"test","boundary_lines":[]}]});
        let status: StatusOutput = serde_json::from_value(value).unwrap();
        assert!(status.hosted_checked_at.is_none());
        assert!(status.services[0].hosted.is_none());
        assert!(status.repos.is_empty(), "a payload written before `repos`");
        let boundary = ServiceBoundary::default();
        let mut value = serde_json::to_value(boundary).unwrap();
        assert!(value.get("candidates_withheld_changed_files").is_none());
        value["candidates_withheld_changed_files"] = serde_json::json!(3);
        let parsed: ServiceBoundary = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(parsed.candidates_withheld_changed_files, Some(3));
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }

    /// The three fields carrick#997 adds to the boundary are additive and
    /// sparse, and nothing that was a number stopped being one: the cloud's
    /// MCP server reads `unemitted_literal_candidates` as an integer.
    #[test]
    fn the_boundary_additions_are_sparse_and_change_no_existing_type() {
        let empty = serde_json::to_value(ServiceBoundary::default()).unwrap();
        for field in [
            "unemitted_literal_sites",
            "candidates_awaiting_model",
            "type_extraction_status",
        ] {
            assert!(empty.get(field).is_none(), "{field} is written when set");
        }
        assert!(empty["unemitted_literal_candidates"].is_number());

        let stated = ServiceBoundary {
            unemitted_literal_candidates: 1,
            unemitted_literal_sites: vec!["src/a.ts:4".to_string()],
            candidates_awaiting_model: Some(8),
            type_extraction_status: Some("sidecar unavailable".to_string()),
            ..Default::default()
        };
        let value = serde_json::to_value(&stated).unwrap();
        assert!(value["unemitted_literal_candidates"].is_number());
        let round_tripped: ServiceBoundary = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(round_tripped).unwrap(), value);
    }

    /// One workspace answer, for the renderer tests to vary one thing at
    /// a time against.
    fn status_output() -> StatusOutput {
        StatusOutput {
            analysing: Vec::new(),
            repos_detected_by: None,
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            hosted_checked_at: None,
            schema: STATUS_SCHEMA.to_string(),
            workspace: "/repos".to_string(),
            indexed_at: "2026-09-12T10:00:00Z".to_string(),
            scanner_version: "test".to_string(),
            running_scans: Vec::new(),
            last_scan: None,
            type_check: None,
            repos: vec![StatusRepo {
                repo: "/repos/monorepo".to_string(),
                name: "monorepo".to_string(),
                changed_since_index: 3,
                outside_every_service: 2,
                // Source files: the line is about rows going out of date, and
                // a changed `carrick.json` or workflow holds none
                // (carrick#1007 item 5).
                stale_files: vec![
                    "tools/release.ts".to_string(),
                    "scripts/seed.ts".to_string(),
                ],
                stale_files_truncated: false,
            }],
            services: vec![StatusService {
                hosted: None,
                hosted_state: Default::default(),
                service: "gateway".to_string(),
                repo: "/repos/monorepo".to_string(),
                index_commit: "abc1234".to_string(),
                indexed_at: "2026-09-12T10:00:00Z".to_string(),
                routes: 0,
                calls: 0,
                changed_since_index: 1,
                stale_files: vec!["apps/gateway/main.ts".to_string()],
                stale_files_total: 1,
                stale_files_truncated: false,
                boundary: Some(ServiceBoundary {
                    candidates_awaiting_model: Some(3),
                    ..Default::default()
                }),
                boundary_note: "note".to_string(),
                boundary_lines: Vec::new(),
            }],
        }
    }

    /// A repo-level file is the repo's, stated once, and a service reports
    /// only what its own scan reads (carrick#997 item 4).
    #[test]
    fn the_status_render_states_repo_level_changes_under_the_repo() {
        let output = status_output();
        let text = output.render();
        assert!(text.contains("changed since index: 1"), "{text}");
        assert!(
            text.contains("3 candidate(s) waiting for `carrick index`"),
            "{text}"
        );
        assert!(
            text.contains("monorepo: 2 file(s) changed outside every service"),
            "{text}"
        );
        assert!(text.contains("changed  tools/release.ts"), "{text}");
    }

    /// A spend on the output changes nothing a person reads. What a run costs
    /// us is our figure, never a line in a customer's terminal (carrick#1236);
    /// the receipt is on `--json` for whoever parses it.
    #[test]
    fn the_status_render_says_nothing_about_what_a_scan_cost() {
        let mut spend = crate::scan_spend::RunSpend::default();
        spend.record(
            "api",
            crate::scan_spend::ScanSpend {
                schema: crate::scan_spend::SCHEMA.to_string(),
                scan_id: "scan_01J".to_string(),
                first_index: true,
                priced: true,
                usd: Some(4.32),
                first_index_ceiling_usd: Some(15.0),
                first_index_remaining_usd: Some(10.68),
                monthly_allowance_usd: Some(10.0),
                monthly_remaining_usd: Some(10.0),
                ..Default::default()
            },
        );
        let mut output = status_output();
        let bare = output.render();
        output.last_scan = Some(spend);

        let text = output.render();
        assert_eq!(text, bare, "a spend must not add a line");
        for banned in ["US$", "4.32", "allowance", "ceiling", "paid"] {
            assert!(!text.contains(banned), "{banned} in {text}");
        }
    }

    /// The receipt rides the error body too. A first paid run killed before it
    /// wrote an index still spent the money, and this is the only answer that
    /// can say so.
    #[test]
    fn an_unindexed_workspace_still_reports_what_its_scan_cost() {
        let mut spend = crate::scan_spend::RunSpend::default();
        spend.record(
            "api",
            crate::scan_spend::ScanSpend {
                schema: crate::scan_spend::SCHEMA.to_string(),
                priced: true,
                usd: Some(4.32),
                ..Default::default()
            },
        );
        let body = ErrorOutput::new(&ReadFailure::new(ReadError::NotIndexed), STATUS_SCHEMA)
            .with_scans(Vec::new())
            .with_last_scan(Some(spend));
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["last_scan"]["scans"][0]["repo"], "api");
        assert_eq!(json["last_scan"]["scans"][0]["spend"]["usd"], 4.32);
        // And absent entirely when there is none, rather than null.
        let empty = serde_json::to_value(ErrorOutput::new(
            &ReadFailure::new(ReadError::NotIndexed),
            STATUS_SCHEMA,
        ))
        .unwrap();
        assert!(empty.get("last_scan").is_none(), "{empty}");
    }

    /// The refusal every scanner release creates for every existing
    /// workspace: the editor showed nothing at all because this sentence was
    /// on stderr and the language server reads the body (carrick#1009).
    #[test]
    fn a_refusal_carries_its_own_sentence_on_the_wire() {
        let detailed = ReadFailure::detailed(
            ReadError::IndexUnreadable,
            "/w/.carrick/index.json was written by a different scanner (index format 2, this \
             build reads 3). Re-run `carrick index`.",
        );
        let json = serde_json::to_value(ErrorOutput::new(&detailed, SCHEMA)).unwrap();
        assert_eq!(json["error"], "index_unreadable");
        assert!(
            json["message"]
                .as_str()
                .is_some_and(|message| message.contains("index format 2")),
            "{json}"
        );
        // A refusal with nothing more specific to say still carries one, so a
        // reader never has to reconstruct a sentence from the code.
        let plain = serde_json::to_value(ErrorOutput::new(
            &ReadFailure::new(ReadError::NotIndexed),
            SCHEMA,
        ))
        .unwrap();
        assert_eq!(plain["message"], ReadError::NotIndexed.message());
    }
}
