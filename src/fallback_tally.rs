//! How much of each service the scan read on the old path, for the parent
//! that started it (David, 2026-10-21).
//!
//! While the compiler's per-position rows and the call-graph request
//! summaries both run, a service falls back to the old path in these ways,
//! each counted here:
//!
//! * its rows were never walked (`rows: off`), its walk wrote none or they
//!   could not be read (`rows: missing`), or binding them ran past
//!   [`crate::request_summary::ROWS_JOIN_BUDGET`] (`rows: join_budget`,
//!   carrick#2564): the whole service reads as without rows;
//! * with rows, a call site read from the call graph because the compiler
//!   could not resolve its import (`unresolved_import`, carrick#2557);
//! * with rows, a call site the call graph follows and the rows cannot, by
//!   the rows' reason (`unfollowed_<reason>`);
//! * the rows file's own files, by state (`files_<state>`:
//!   [`crate::position_rows::PositionIndex::file_states`]).
//!
//! Each old piece is deleted when its weekly fleet count reaches zero. The
//! counts cross on stderr as one [`MARKER`] line per service, as the scan's
//! spend and timing do, and only when a parent reads them
//! ([`crate::progress::parent_is_reading`]). The cloud runner logs each
//! count as a row of its own. `carrick index` drops them. None of it is for a
//! reader.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::request_summary::{RequestSummaryIndex, RequestSummaryInputs};

/// The stderr line a scan states one service's fallbacks on.
const MARKER: &str = "@carrick-fallbacks ";

/// What a service's compiler rows came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rows {
    /// The service's analysis had no walk to read: every scan walks since
    /// carrick#3283, so a scan reaches this only where no walk slot exists.
    /// The word stays, because the cloud runner and the old-path dashboard
    /// read it.
    Off,
    /// The rows are bound to what discovery read.
    Read,
    /// The walk wrote none, or they could not be read: every lookup is a
    /// hole.
    Missing,
    /// Binding them ran past its budget, and they were dropped.
    JoinBudget,
}

/// One service's fallbacks. Every count is a top-level field, so each is a
/// column for whoever totals them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceFallbacks {
    /// The service as `X-Carrick-Service` names it; `None` for an unnamed
    /// one.
    pub service: Option<String>,
    pub rows: Rows,
    /// Call sites read from the call graph because the compiler could not
    /// resolve the import they call through.
    pub unresolved_import: usize,
    /// `unfollowed_<reason>`: call sites the call graph follows that the
    /// rows cannot, by the rows' reason. `files_<state>`: the rows file's
    /// files by state.
    #[serde(flatten)]
    pub counts: BTreeMap<String, usize>,
}

impl ServiceFallbacks {
    /// What `service`'s summaries, composed from `inputs`, read on the old
    /// path.
    pub fn of(
        service: Option<String>,
        inputs: &RequestSummaryInputs,
        summaries: &RequestSummaryIndex,
    ) -> Self {
        let rows = match &inputs.positions {
            _ if inputs.rows_ran_out => Rows::JoinBudget,
            None => Rows::Off,
            Some(index) if index.file_count() == 0 => Rows::Missing,
            Some(_) => Rows::Read,
        };
        let mut counts: BTreeMap<String, usize> = summaries
            .unfollowed()
            .iter()
            .map(|(reason, count)| (format!("unfollowed_{reason}"), *count))
            .collect();
        if let Some(index) = &inputs.positions {
            counts.extend(
                index
                    .file_states()
                    .into_iter()
                    .map(|(state, count)| (format!("files_{state}"), count)),
            );
        }
        Self {
            service,
            rows,
            unresolved_import: summaries.fell_back(),
            counts,
        }
    }
}

/// Hand one service's fallbacks to the parent that started the scan, when
/// there is one. A scan nobody is parsing says nothing.
pub fn report(fallbacks: &ServiceFallbacks) {
    if !crate::progress::parent_is_reading() {
        return;
    }
    if let Ok(line) = serde_json::to_string(fallbacks) {
        crate::errln!("{MARKER}{line}");
    }
}

/// One service's fallbacks, stated ([`report`]) once, when the service's
/// scan is done or leaves early: counts added after the analysis, such as
/// the route sites the receiver trace leaves to the reader (carrick#2474 R3
/// S1), reach the line without an exit that skips it.
pub struct Pending(ServiceFallbacks);

impl Pending {
    pub fn new(fallbacks: ServiceFallbacks) -> Self {
        Self(fallbacks)
    }

    pub fn counts(&mut self) -> &mut BTreeMap<String, usize> {
        &mut self.0.counts
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        report(&self.0);
    }
}

/// Read one service's fallbacks out of a line of a scan's stderr.
pub fn parse(line: &str) -> Option<ServiceFallbacks> {
    let payload = line.trim_start().strip_prefix(MARKER)?;
    serde_json::from_str(payload).ok()
}

/// Whether a line of a scan's stderr is a fallbacks line, read or not: a
/// parent that does not forward them drops every one, so none reaches a
/// person.
pub fn is_marker(line: &str) -> bool {
    line.trim_start().starts_with(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_semantics::LibrarySemantics;
    use crate::position_rows::PositionIndex;
    use crate::request_summary::fixture_walk::{NODENEXT, walked, walked_within};
    use crate::request_summary::summarize;

    fn tally(inputs: &RequestSummaryInputs) -> ServiceFallbacks {
        let summaries = summarize(inputs, &LibrarySemantics::default());
        ServiceFallbacks::of(Some("api".to_string()), inputs, &summaries)
    }

    /// A run that walks no rows reads the whole service on the old path.
    #[test]
    fn a_service_without_rows_reads_as_off() {
        let fallbacks = tally(&RequestSummaryInputs::default());
        assert_eq!(fallbacks.rows, Rows::Off);
        assert_eq!(fallbacks.unresolved_import, 0);
        assert!(fallbacks.counts.is_empty(), "{:?}", fallbacks.counts);
    }

    /// A walk that wrote no rows leaves an index with no files.
    #[test]
    fn a_service_whose_walk_wrote_no_rows_reads_as_missing() {
        let root = tempfile::tempdir().unwrap();
        let inputs = RequestSummaryInputs {
            positions: Some(PositionIndex::missing(root.path())),
            ..RequestSummaryInputs::default()
        };
        assert_eq!(tally(&inputs).rows, Rows::Missing);
    }

    /// The line names every count at its top level, so a log row built from
    /// it has one column per count, and it reads back as it was written.
    #[test]
    fn the_counts_cross_flat_and_read_back() {
        let fallbacks = ServiceFallbacks {
            service: None,
            rows: Rows::JoinBudget,
            unresolved_import: 17,
            counts: BTreeMap::from([
                ("unfollowed_unresolved_name".to_string(), 3),
                ("files_stale".to_string(), 2),
            ]),
        };
        let line = format!("{MARKER}{}", serde_json::to_string(&fallbacks).unwrap());
        let object: serde_json::Value =
            serde_json::from_str(line.strip_prefix(MARKER).unwrap()).unwrap();
        assert_eq!(
            object,
            serde_json::json!({
                "service": null,
                "rows": "join_budget",
                "unresolved_import": 17,
                "unfollowed_unresolved_name": 3,
                "files_stale": 2,
            })
        );
        assert_eq!(parse(&line), Some(fallbacks));
        assert!(is_marker(&line));
    }

    /// carrick#2557's fixture: a service rooted above the directory holding
    /// its tsconfig, so the compiler resolves none of its imports. With rows
    /// read, every call into a function another module declares is read from
    /// the call graph, and the tally counts each one.
    #[test]
    fn a_call_read_from_the_call_graph_is_counted_beside_rows_that_were_read() {
        let Some(on) = walked("position-rows-wrapper", ".", "web", &[NODENEXT], true) else {
            return;
        };
        let fallbacks = tally(&on.inputs);
        assert_eq!(fallbacks.rows, Rows::Read);
        assert_eq!(fallbacks.unresolved_import, 17);
        assert!(
            !fallbacks
                .counts
                .keys()
                .any(|key| key.starts_with("unfollowed_")),
            "{:?}",
            fallbacks.counts
        );
        assert!(
            fallbacks.counts.get("files_fresh").is_some_and(|n| *n > 0),
            "{:?}",
            fallbacks.counts
        );
    }

    /// A join with no time to bind its rows drops them, and the service is
    /// counted as read without rows for that reason.
    #[test]
    fn a_join_past_its_budget_is_counted_as_join_budget() {
        let Some(forced) = walked_within(
            "position-rows-wrapper",
            ".",
            "web",
            &[],
            true,
            std::time::Duration::ZERO,
        ) else {
            return;
        };
        let fallbacks = tally(&forced.inputs);
        assert_eq!(fallbacks.rows, Rows::JoinBudget);
        assert_eq!(
            fallbacks.unresolved_import, 0,
            "no rows, nothing read around them"
        );
    }

    #[test]
    fn a_line_that_is_not_a_fallbacks_line_is_not_read_as_one() {
        assert!(parse("@carrick-fallbacks not json").is_none());
        assert!(is_marker("@carrick-fallbacks not json"));
        assert!(parse("@carrick-spend {}").is_none());
        assert!(!is_marker("@carrick-spend {}"));
        assert!(!is_marker("request summaries: 3 row(s)"));
    }
}
