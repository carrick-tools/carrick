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
    /// A scan building the index the caller asked for, when one is running.
    /// "There is no index" and "one is being built right now" are different
    /// answers, and a reader given only the first starts a second scan
    /// (carrick#992).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_scans: Vec<super::scan_state::ScanState>,
    /// What the last paid scan of this workspace cost. Carried on the error
    /// body too: a first run killed before it wrote an index still spent the
    /// money, and this is the only surface that can say so (carrick#995).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan: Option<crate::scan_spend::RunSpend>,
}

impl ErrorOutput {
    /// The schema is the one the CALLER asked under: a `status` failure is a
    /// `carrick.status/0` body, so a reader that rejects any other marker
    /// still gets an answer it can parse.
    pub fn new(error: ReadError, schema: &str) -> Self {
        Self {
            schema: schema.to_string(),
            error: error.wire().to_string(),
            running_scans: Vec::new(),
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
}

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
    /// How many of those no service in this repo reads.
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
    /// ordinary case; this is what a detached `carrick index --infer` is
    /// visible through while it runs (carrick#992).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_scans: Vec<super::scan_state::ScanState>,
    /// What the last paid scan of this workspace cost, one entry per repo it
    /// scanned (carrick#995). Absent until one has run: the free pass pays for
    /// nothing, and a scan that has not been priced yet states no figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scan: Option<crate::scan_spend::RunSpend>,
    pub services: Vec<StatusService>,
}

impl StatusOutput {
    /// The human form: one block per service, boundary last, same order as
    /// every other local answer.
    pub fn render(&self) -> String {
        let mut out = String::new();
        // First, because it is the only line about right now: everything below
        // it describes the index as it was when the last build finished.
        for scan in &self.running_scans {
            out.push_str(&scan.line());
            out.push('\n');
        }
        if !self.running_scans.is_empty() {
            out.push('\n');
        }
        // The paid scan prints this when it finishes, and a detached one
        // prints it into a log nobody is tailing. So it is repeated here,
        // dated, for the reader who is asking afterwards (carrick#995).
        if let Some(spend) = &self.last_scan {
            for line in spend.lines(Some(&spend.updated_at)) {
                out.push_str(&line);
                out.push('\n');
            }
            if !spend.is_empty() {
                out.push('\n');
            }
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
            repos: vec![StatusRepo {
                repo: "/repos/monorepo".to_string(),
                name: "monorepo".to_string(),
                changed_since_index: 3,
                outside_every_service: 2,
                stale_files: vec![
                    "carrick.json".to_string(),
                    ".github/workflows/x.yml".to_string(),
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
            text.contains("3 candidate(s) waiting for --infer"),
            "{text}"
        );
        assert!(
            text.contains("monorepo: 2 file(s) changed outside every service"),
            "{text}"
        );
        assert!(text.contains("changed  carrick.json"), "{text}");
    }

    /// `carrick status` repeats the last paid scan's line, because the scan
    /// that paid printed it into a log nobody is tailing (carrick#995). It
    /// leads the index, which describes a moment that has already passed.
    #[test]
    fn the_status_render_repeats_what_the_last_paid_scan_cost() {
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
        output.last_scan = Some(spend);

        let text = output.render();
        let line = text.lines().next().expect("the money line leads");
        assert!(line.starts_with("The last paid scan, "), "{text}");
        assert!(
            line.ends_with(
                "US$4.32. First-index ceiling left: US$10.68. Laptop allowance this month: \
                 US$10.00 of US$10.00."
            ),
            "{text}"
        );
    }

    /// A workspace with no paid scan behind it says nothing about money: there
    /// is no placeholder for a figure that does not exist.
    #[test]
    fn the_status_render_says_nothing_about_money_when_nothing_was_paid() {
        assert!(!status_output().render().contains("US$"));
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
        let body = ErrorOutput::new(ReadError::NotIndexed, STATUS_SCHEMA)
            .with_scans(Vec::new())
            .with_last_scan(Some(spend));
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["last_scan"]["scans"][0]["repo"], "api");
        assert_eq!(json["last_scan"]["scans"][0]["spend"]["usd"], 4.32);
        // And absent entirely when there is none, rather than null.
        let empty =
            serde_json::to_value(ErrorOutput::new(ReadError::NotIndexed, STATUS_SCHEMA)).unwrap();
        assert!(empty.get("last_scan").is_none(), "{empty}");
    }
}
