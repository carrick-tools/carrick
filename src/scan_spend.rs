//! What a scan reported to the cloud, from the cloud's answer to the receipt
//! this run leaves behind (carrick#995).
//!
//! The last write action of a laptop run answers with one `carrick.scan-spend/0`
//! object (carrick-cloud#806/#813,
//! `docs/internal/reference/laptop-scan-seam.md` §5.7). It rides a `cli`
//! credential only, so a CI upload never carries it and nothing here is on the
//! Action's path.
//!
//! Nothing in this module renders: what a run costs us is our figure and never
//! reaches a customer's terminal (carrick#1236). The object is parsed, carried
//! across the process boundary the indexer puts between a scan and itself, and
//! written to `.carrick/last-scan.json`, which `carrick status --json` reports
//! for a reader that asked for the machine answer.
//!
//! The scan that reports is a subprocess of `carrick index` whose stdout the
//! indexer drops, so it states the object on stderr in a line the parent reads
//! ([`MARKER`]), exactly as progress crosses that boundary.

use serde::{Deserialize, Serialize};

/// The tag the cloud answers under. Anything else is a cloud saying something
/// this scanner does not understand, and an unread tag is dropped rather than
/// read off a shape that has moved.
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
}

/// What one run of `carrick index` reported, one entry per repo scanned,
/// because each repo is its own scan with its own id. Written to
/// `.carrick/last-scan.json` as each object arrives, so a run killed partway
/// through still leaves behind a record of the repos it uploaded.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RunSpend {
    /// RFC 3339, when the last figure in it landed.
    #[serde(default)]
    pub updated_at: String,
    pub scans: Vec<RepoSpend>,
}

/// One repo's scan. The repo is named here rather than in the cloud's object,
/// because only the indexer knows which repo a scan id was for.
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

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Hand this scan's object to the parent that started it, when there is one.
///
/// The indexer swallows a scan's stdout, so the object crosses on stderr as
/// JSON for the parent to record. A scan nobody is parsing has nowhere to put
/// it and says nothing, because none of this is for a reader (carrick#1236).
pub fn report(spend: &ScanSpend) {
    if !spend.understood() || !crate::progress::parent_is_reading() {
        return;
    }
    if let Ok(line) = serde_json::to_string(spend) {
        eprintln!("{MARKER}{line}");
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

    #[test]
    fn a_spend_survives_the_crossing_from_the_scan_that_reported_it() {
        let line = format!("{MARKER}{}", serde_json::to_string(&spend()).unwrap());
        assert_eq!(parse(&line), Some(spend()));
    }

    /// A tag this scanner does not read is a cloud that has moved, and the
    /// answer to that is silence rather than a guess off an unknown shape.
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
        let mut run = RunSpend::default();
        run.record("api", spend());
        run.write(&file);
        assert_eq!(RunSpend::read(&file), Some(run));
        assert_eq!(RunSpend::read(&dir.path().join("nothing-here.json")), None);
    }
}
