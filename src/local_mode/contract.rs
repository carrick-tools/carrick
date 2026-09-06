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
    /// The file is not under any repo this workspace lists.
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
                "this file is not under any repo the workspace lists. Add its repo to \
                 carrick-workspace.json and re-index."
            }
            ReadError::IndexUnreadable => {
                "the local index could not be read. Re-run `carrick index --workspace <dir>`."
            }
        }
    }
}

/// The error body, printed to stdout so a reader parsing JSON always gets JSON.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorOutput {
    pub schema: String,
    pub error: String,
}

impl ErrorOutput {
    /// The schema is the one the CALLER asked under: a `status` failure is a
    /// `carrick.status/0` body, so a reader that rejects any other marker
    /// still gets an answer it can parse.
    pub fn new(error: ReadError, schema: &str) -> Self {
        Self {
            schema: schema.to_string(),
            error: error.wire().to_string(),
        }
    }
}

/// The other side of a contract, with where to find it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Counterpart {
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
}

/// The whole answer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CheckOutput {
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
}

impl CheckOutput {
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

        out.push_str(&format!(
            "changed since index: {} file(s){}\n",
            self.changed_since_index,
            if self.stale {
                "; this file is one of them, so its rows are unresolved since your edit"
            } else {
                ""
            }
        ));
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
    pub service: String,
    /// Absolute path of the repo this service belongs to. Services of one repo
    /// share a commit and a changed-file count, and this is what says so.
    pub repo: String,
    pub index_commit: String,
    pub indexed_at: String,
    pub routes: usize,
    pub calls: usize,
    /// Files in this service's repo that differ from `index_commit`, or that
    /// git does not track.
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

/// What `carrick status` answers: the workspace, not a file.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StatusOutput {
    pub schema: String,
    /// The workspace root: the folder holding `carrick-workspace.json` and
    /// `.carrick/`.
    pub workspace: String,
    pub indexed_at: String,
    pub scanner_version: String,
    pub services: Vec<StatusService>,
}

impl StatusOutput {
    /// The human form: one block per service, boundary last, same order as
    /// every other local answer.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{} — {} service(s), indexed at {} by carrick {}\n\n",
            self.workspace,
            self.services.len(),
            self.indexed_at,
            self.scanner_version
        );
        for service in &self.services {
            out.push_str(&format!(
                "  {:<28} {:>4} route(s)  {:>4} call(s)  {}  changed since index: {}\n",
                service.service,
                service.routes,
                service.calls,
                short_commit(&service.index_commit),
                service.changed_since_index
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

    fn output() -> CheckOutput {
        CheckOutput {
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
                    role: "consumer".to_string(),
                    service: "admin-ui".to_string(),
                    file: "src/api.ts".to_string(),
                    line: Some(44),
                    repo: Some("/repos/admin-ui".to_string()),
                }],
                verdict: None,
            }],
            boundary: None,
            boundary_note: super::super::NOT_CLASSIFIED_LOCALLY.to_string(),
            boundary_lines: vec![format!(
                "boundary (webapp): {}",
                super::super::NOT_CLASSIFIED_LOCALLY
            )],
        }
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
    fn a_short_commit_is_safe_on_a_short_string() {
        assert_eq!(short_commit("abc"), "abc");
    }
}
