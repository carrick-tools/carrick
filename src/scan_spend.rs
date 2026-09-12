//! What a paid scan cost, from the cloud's answer to the lines a person reads
//! (carrick#995).
//!
//! The last write action of a laptop run answers with one `carrick.scan-spend/0`
//! object: this scan's Vertex spend, and what is left of the two budgets it was
//! charged against (carrick-cloud#806/#813,
//! `docs/internal/reference/laptop-scan-seam.md` §5.7). It rides a `cli`
//! credential only, so a CI upload never carries it and nothing here is on the
//! Action's path.
//!
//! Three rules the shape enforces, and every surface below obeys:
//!
//! * **`priced: false` means print nothing about money.** One flag covers all
//!   three figures on purpose: an unpriced model suspends enforcement, so the
//!   run's own cost and both remainings are under-counts, and one printed
//!   beside a missing other is a number the reader cannot place.
//! * **A null amount is "not set", not "unlimited".** The clause is left out
//!   rather than printed with a blank or a zero in it.
//! * **The ceiling is about a first index.** A later scan of the same repo is
//!   charged to the month, so its ceiling remaining says nothing about it and
//!   is not printed — the same rule the dashboard's card follows.
//!
//! The scan that pays is a subprocess of `carrick index` whose stdout the
//! indexer drops, so it states the figure on stderr in a line the parent reads
//! ([`MARKER`]), exactly as progress crosses that boundary.

use serde::{Deserialize, Serialize};
use tracing::info;

/// The tag the cloud answers under. Anything else is a cloud saying something
/// this scanner does not understand, and an unread tag prints nothing rather
/// than dollars read off a shape that has moved.
pub const SCHEMA: &str = "carrick.scan-spend/0";

/// The stderr line a scan states its spend on, for the parent that started it.
/// Chosen to be something no log line starts with, like the progress marker.
const MARKER: &str = "@carrick-spend ";

/// One scan's spend, as the cloud reports it. Every field is defaulted, so a
/// body carrying more than this parses and a body carrying less does too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ScanSpend {
    pub schema: String,
    /// The slot `start-scan` minted: the same id this run's requests log under.
    #[serde(default)]
    pub scan_id: String,
    /// Whether this run was charged to the repo's first-index ceiling rather
    /// than to the monthly pool.
    #[serde(default)]
    pub first_index: bool,
    /// False when any model in any meter read has no price on file.
    #[serde(default)]
    pub priced: bool,
    /// Which models, so the gap is legible rather than looking like a zero.
    #[serde(default)]
    pub unpriced_models: Vec<String>,
    /// This scan's Vertex spend. Null when `priced` is false.
    #[serde(default)]
    pub usd: Option<f64>,
    #[serde(default)]
    pub input_tokens: u64,
    /// Includes thinking tokens.
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub calls: u64,
    /// The ceiling in force, or null when the amount is unset.
    #[serde(default)]
    pub first_index_ceiling_usd: Option<f64>,
    /// What is left of it after this scan, floored at 0.
    #[serde(default)]
    pub first_index_remaining_usd: Option<f64>,
    #[serde(default)]
    pub monthly_allowance_usd: Option<f64>,
    #[serde(default)]
    pub monthly_remaining_usd: Option<f64>,
    /// `YYYY-MM`, the month the monthly figure is for.
    #[serde(default)]
    pub period: String,
}

impl ScanSpend {
    /// Whether this is the shape this scanner reads. A cloud that answers
    /// under a new tag is telling the scanner it has changed, and the answer
    /// to that is silence, not a guess.
    pub fn understood(&self) -> bool {
        self.schema == SCHEMA
    }

    /// This scan's cost, when there is one to state. `None` covers both an
    /// unpriced run and a cloud that sent no figure: neither can be printed,
    /// and treating them alike keeps one path.
    fn dollars(&self) -> Option<f64> {
        if !self.priced {
            return None;
        }
        self.usd
    }

    /// The budget clauses that follow the run's own cost, in the order the
    /// ticket names them. Each is present only when the amount behind it is.
    fn budget_clauses(&self) -> Vec<String> {
        let mut clauses = Vec::new();
        // A repo past its first index is charged to the month, so its ceiling
        // remaining is a figure about some earlier scan.
        if self.first_index
            && let Some(left) = self.first_index_remaining_usd
        {
            clauses.push(format!("First-index ceiling left: {}.", usd(left)));
        }
        if let (Some(left), Some(allowance)) =
            (self.monthly_remaining_usd, self.monthly_allowance_usd)
        {
            clauses.push(format!(
                "Laptop allowance this month: {} of {}.",
                usd(left),
                usd(allowance)
            ));
        }
        clauses
    }
}

/// What one run of `carrick index --infer` spent: one entry per repo scanned,
/// because each repo is its own scan with its own id and its own first-index
/// ceiling. Written to `.carrick/last-scan.json` as each figure arrives —
/// money is a fact at upload time, so a run killed after paying still leaves
/// the receipt behind.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RunSpend {
    /// RFC 3339, when the last figure in it landed.
    #[serde(default)]
    pub updated_at: String,
    pub scans: Vec<RepoSpend>,
}

/// One repo's scan, and what it cost. The repo is named here rather than in
/// the cloud's object: only the indexer knows which repo a scan id was for.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RepoSpend {
    /// The repo as the index labels it: its directory name.
    pub repo: String,
    pub spend: ScanSpend,
}

impl RunSpend {
    /// Take one repo's figure. The newest entry is the one whose monthly
    /// remaining is current, so order is kept.
    pub fn record(&mut self, repo: &str, spend: ScanSpend) {
        self.scans.push(RepoSpend {
            repo: repo.to_string(),
            spend,
        });
        self.updated_at = timestamp();
    }

    pub fn is_empty(&self) -> bool {
        self.scans.is_empty()
    }

    /// The run's own cost, or `None` when any scan in it could not be priced.
    ///
    /// Not a partial sum: the month's meter is shared, so one unpriced model
    /// makes every figure in the run an under-count, and a total that silently
    /// dropped a scan would be the most misleading number of all.
    fn total(&self) -> Option<f64> {
        self.scans
            .iter()
            .map(|entry| entry.spend.dollars())
            .sum::<Option<f64>>()
    }

    /// Every model this run met no price for, in one sorted list.
    fn unpriced_models(&self) -> Vec<String> {
        let mut models: Vec<String> = self
            .scans
            .iter()
            .flat_map(|entry| entry.spend.unpriced_models.iter().cloned())
            .collect();
        models.sort();
        models.dedup();
        models
    }

    /// What the run's figures are about. `reported_at` is the time a surface
    /// is repeating a run it did not just perform — `carrick status` — and its
    /// absence means the run has this moment finished.
    fn head(&self, reported_at: Option<&str>) -> String {
        let one = self.scans.len() == 1;
        match reported_at {
            None if one => "This scan".to_string(),
            None => "This run".to_string(),
            Some(at) if one => format!("The last paid scan, {at}"),
            Some(at) => format!("The last paid run, {at}"),
        }
    }

    /// The lines a surface prints for this run, or nothing at all.
    ///
    /// Nothing is the answer when no figure arrived: a free pass pays for
    /// nothing, and a cloud deployed before this field existed sends nothing,
    /// and a placeholder in either case is a claim about money that nobody
    /// made.
    pub fn lines(&self, reported_at: Option<&str>) -> Vec<String> {
        if self.scans.is_empty() {
            return Vec::new();
        }
        let head = self.head(reported_at);
        let Some(total) = self.total() else {
            let models = self.unpriced_models();
            let named = if models.is_empty() {
                "a model it used has no price on file".to_string()
            } else {
                format!("no price on file for {}", models.join(", "))
            };
            return vec![format!("{head}: cost not available, {named}.")];
        };
        // One scan is the ordinary case, and the ticket's sentence: the run,
        // its ceiling and the month, in one line.
        if let [only] = self.scans.as_slice() {
            let mut line = format!("{head}: {}.", usd(total));
            for clause in only.spend.budget_clauses() {
                line.push(' ');
                line.push_str(&clause);
            }
            return vec![line];
        }
        // Several repos are several scans, each charged to its own repo's
        // ceiling, so the ceilings are stated per repo and never added up. The
        // month is one pool, and the last scan's remaining is what is left of
        // it.
        let mut lines = vec![format!(
            "{head}: {} across {} scans.",
            usd(total),
            self.scans.len()
        )];
        for entry in &self.scans {
            let cost = entry.spend.dollars().map(usd).unwrap_or_default();
            let ceiling = match (
                entry.spend.first_index,
                entry.spend.first_index_remaining_usd,
            ) {
                (true, Some(left)) => format!(", first-index ceiling left: {}", usd(left)),
                _ => String::new(),
            };
            lines.push(format!("  {:<28} {cost}{ceiling}", entry.repo));
        }
        if let Some(last) = self.scans.last()
            && let (Some(left), Some(allowance)) = (
                last.spend.monthly_remaining_usd,
                last.spend.monthly_allowance_usd,
            )
        {
            lines.push(format!(
                "Laptop allowance this month: {} of {}.",
                usd(left),
                usd(allowance)
            ));
        }
        lines
    }

    /// Read the receipt a previous run left, or nothing where there is none.
    pub fn read(file: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Write the receipt where a reader can never see half of it.
    pub fn write(&self, file: &std::path::Path) {
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let pending = file.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&pending, json).is_ok() {
            let _ = std::fs::rename(&pending, file);
        }
        let _ = std::fs::remove_file(pending);
    }
}

/// US dollars, to the cent, the way every Carrick surface prints a figure.
fn usd(value: f64) -> String {
    format!("US${value:.2}")
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Say what this scan cost, to whoever is listening.
///
/// A scan the indexer started has a parent reading its stderr, and that parent
/// owns the wording — so the figure crosses as JSON and is rendered once, at
/// the end of the build. A scan nobody is parsing says it itself.
pub fn report(spend: &ScanSpend) {
    if !spend.understood() {
        return;
    }
    if crate::progress::parent_is_reading() {
        if let Ok(line) = serde_json::to_string(spend) {
            eprintln!("{MARKER}{line}");
        }
        return;
    }
    // One scan, so the repo label is never rendered: the sentence is about the
    // run this process IS.
    let mut run = RunSpend::default();
    run.record("", spend.clone());
    for line in run.lines(None) {
        info!("{line}");
    }
}

/// Read one spend out of a line of a scan's stderr, if that is what it is.
pub fn parse(line: &str) -> Option<ScanSpend> {
    let payload = line.trim_start().strip_prefix(MARKER)?;
    let spend: ScanSpend = serde_json::from_str(payload).ok()?;
    spend.understood().then_some(spend)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spend() -> ScanSpend {
        ScanSpend {
            schema: SCHEMA.to_string(),
            scan_id: "scan_01J".to_string(),
            first_index: true,
            priced: true,
            unpriced_models: Vec::new(),
            usd: Some(4.32),
            input_tokens: 1_170_432,
            output_tokens: 288_114,
            cached_tokens: 0,
            calls: 412,
            first_index_ceiling_usd: Some(15.0),
            first_index_remaining_usd: Some(10.68),
            monthly_allowance_usd: Some(10.0),
            monthly_remaining_usd: Some(10.0),
            period: "2026-09".to_string(),
        }
    }

    fn run_of(scans: Vec<(&str, ScanSpend)>) -> RunSpend {
        let mut run = RunSpend::default();
        for (repo, spend) in scans {
            run.record(repo, spend);
        }
        run
    }

    /// The line the ticket names, in the ordinary case: one repo, a first
    /// index, both budgets set.
    #[test]
    fn one_priced_scan_states_all_three_figures() {
        let lines = run_of(vec![("api", spend())]).lines(None);
        assert_eq!(
            lines,
            vec![
                "This scan: US$4.32. First-index ceiling left: US$10.68. Laptop allowance this \
                 month: US$10.00 of US$10.00."
                    .to_string()
            ]
        );
    }

    /// `carrick status` repeats it, and says which run it is repeating: the
    /// scan that paid printed to a log nobody is tailing.
    #[test]
    fn status_repeats_the_last_scan_and_dates_it() {
        let lines = run_of(vec![("api", spend())]).lines(Some("2026-09-12T21:03:11Z"));
        assert!(
            lines[0].starts_with("The last paid scan, 2026-09-12T21:03:11Z: US$4.32."),
            "{lines:?}"
        );
    }

    /// A repo past its first index is charged to the month, so the ceiling
    /// remaining is not a figure about this scan and is not printed.
    #[test]
    fn a_later_scan_of_the_same_repo_leaves_the_ceiling_out() {
        let mut later = spend();
        later.first_index = false;
        later.monthly_remaining_usd = Some(5.68);
        let lines = run_of(vec![("api", later)]).lines(None);
        assert_eq!(
            lines,
            vec![
                "This scan: US$4.32. Laptop allowance this month: US$5.68 of US$10.00.".to_string()
            ]
        );
    }

    /// The shipped default for both amounts is unset, which the cloud sends as
    /// null. Nothing has been decided, so there is nothing to print: the cost
    /// stands alone rather than beside a blank.
    #[test]
    fn an_unset_amount_prints_no_budget_sentence() {
        let mut unset = spend();
        unset.first_index_ceiling_usd = None;
        unset.first_index_remaining_usd = None;
        unset.monthly_allowance_usd = None;
        unset.monthly_remaining_usd = None;
        assert_eq!(
            run_of(vec![("api", unset)]).lines(None),
            vec!["This scan: US$4.32.".to_string()]
        );
    }

    /// One unpriced model suspends enforcement for the whole month, so every
    /// dollar figure is an under-count. Say the cost is not available, name the
    /// gap, and print no money at all.
    #[test]
    fn an_unpriced_run_prints_no_dollars_and_names_the_gap() {
        let mut unpriced = spend();
        unpriced.priced = false;
        unpriced.usd = None;
        unpriced.unpriced_models = vec!["a-preview-model".to_string()];
        unpriced.first_index_remaining_usd = None;
        unpriced.monthly_remaining_usd = None;
        let lines = run_of(vec![("api", unpriced)]).lines(None);
        assert_eq!(
            lines,
            vec![
                "This scan: cost not available, no price on file for a-preview-model.".to_string()
            ]
        );
        assert!(!lines[0].contains("US$"), "{lines:?}");
    }

    /// The same, with the list empty: the flag is the statement, and a run
    /// that cannot name the model still must not read as free.
    #[test]
    fn an_unpriced_run_that_names_no_model_still_withholds_the_figure() {
        let mut unpriced = spend();
        unpriced.priced = false;
        unpriced.usd = None;
        let lines = run_of(vec![("api", unpriced)]).lines(None);
        assert_eq!(
            lines,
            vec![
                "This scan: cost not available, a model it used has no price on file.".to_string()
            ]
        );
    }

    /// A workspace is several repos, and each is its own scan charged to its
    /// own repo's ceiling. The ceilings are stated per repo and never added;
    /// the month is one pool, so the newest remaining is what is left of it.
    #[test]
    fn a_workspace_states_each_repo_and_one_month() {
        let mut second = spend();
        second.scan_id = "scan_02K".to_string();
        second.first_index = false;
        second.usd = Some(8.64);
        second.monthly_remaining_usd = Some(1.36);
        let lines = run_of(vec![("api", spend()), ("web", second)]).lines(None);
        assert_eq!(lines[0], "This run: US$12.96 across 2 scans.");
        assert!(lines[1].contains("api"), "{lines:?}");
        assert!(
            lines[1].contains("US$4.32") && lines[1].contains("first-index ceiling left: US$10.68"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("web") && !lines[2].contains("ceiling"),
            "the second repo is not a first index: {lines:?}"
        );
        assert_eq!(
            lines[3], "Laptop allowance this month: US$1.36 of US$10.00.",
            "the month is the newest remaining, not a sum"
        );
    }

    /// One unpriced scan in a run makes the whole run's total an under-count,
    /// so no total is printed for it.
    #[test]
    fn one_unpriced_scan_withholds_the_whole_run_total() {
        let mut unpriced = spend();
        unpriced.priced = false;
        unpriced.usd = None;
        unpriced.unpriced_models = vec!["a-preview-model".to_string()];
        let lines = run_of(vec![("api", spend()), ("web", unpriced)]).lines(None);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("This run: cost not available"),
            "{lines:?}"
        );
    }

    /// A run that paid for nothing says nothing. There is no placeholder for a
    /// figure that does not exist.
    #[test]
    fn a_run_with_no_figure_prints_nothing() {
        assert!(RunSpend::default().lines(None).is_empty());
        assert!(
            RunSpend::default()
                .lines(Some("2026-09-12T21:03:11Z"))
                .is_empty()
        );
    }

    #[test]
    fn a_spend_survives_the_crossing_from_the_scan_that_paid_for_it() {
        let line = format!("{MARKER}{}", serde_json::to_string(&spend()).unwrap());
        assert_eq!(parse(&line), Some(spend()));
    }

    /// A tag this scanner does not read is a cloud that has moved, and the
    /// answer to that is silence rather than dollars off an unknown shape.
    #[test]
    fn a_line_that_is_not_a_spend_is_not_read_as_one() {
        let mut future = spend();
        future.schema = "carrick.scan-spend/1".to_string();
        let line = format!("{MARKER}{}", serde_json::to_string(&future).unwrap());
        assert!(parse(&line).is_none());
        assert!(parse("@carrick-spend not json").is_none());
        assert!(parse("Uploading results...").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn the_receipt_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("last-scan.json");
        let run = run_of(vec![("api", spend())]);
        run.write(&file);
        assert_eq!(RunSpend::read(&file), Some(run));
        assert_eq!(RunSpend::read(&dir.path().join("nothing-here.json")), None);
    }
}
