//! A consumer service's final pair outcomes, stored on its own blob by a main
//! scan (carrick#3104). A pull request scan takes main's outcome for a pair
//! whose two sides its change cannot reach, instead of judging and retyping
//! it again.
//!
//! Only the verdict half of each outcome is stored. Its identity (operation,
//! call site, aliases, services, the texts judged) is pinned by `spec_hash`,
//! so a pull request scan rebuilds the outcome from its own pair and takes
//! only the verdict. The wire is pinned in the Step 0 comment on
//! carrick#3104.

use super::ScannerBuild;
use crate::services::type_sidecar::VerdictBucket;
use serde::{Deserialize, Deserializer, Serialize};

/// The record's own format and the `spec_hash` preimage's. Bump it when
/// either changes: a record of another version settles nothing.
pub const PAIR_OUTCOMES_VERSION: u32 = 1;

/// The largest serialised record a main scan stores. Over it the consumer
/// stores none, and a pull request scan judges each of its pairs.
pub const PAIR_OUTCOMES_CAP_BYTES: usize = 8 * 1024 * 1024;

/// The check's outcomes for every pair whose consumer is the blob's service
/// and whose producer was scanned in the same run.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StoredPairOutcomes {
    /// [`PAIR_OUTCOMES_VERSION`] when written.
    pub version: u32,
    /// The commit the check ran at. A carried blob can be restamped, so the
    /// record names its own run.
    pub commit: String,
    /// The scanner build that judged, as [`ScannerBuild::current`] read it.
    #[serde(default)]
    pub scanner_build: Option<ScannerBuild>,
    /// Sorted by `pair_key`.
    pub outcomes: Vec<StoredPairOutcome>,
}

/// One pair's final outcome, verdict half only.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StoredPairOutcome {
    pub pair_key: String,
    /// Lowercase hex SHA-256 of the pair's spec (`type_compat_v2::spec_hash`).
    pub spec_hash: String,
    /// False when the outcome came from how the run went, not from the two
    /// types: such a pair is judged again.
    pub judged: bool,
    pub bucket: VerdictBucket,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_reads: Vec<u32>,
}

/// Read the field leniently: a record this build cannot read (a later
/// bucket, another shape) is no record. Peer blobs are read strictly
/// ([`super::read_peer_blob`]), so a strict read here would stop every scan
/// that reads the blob as a peer.
pub(super) fn lenient_pair_outcomes<'de, D>(
    deserializer: D,
) -> Result<Option<StoredPairOutcomes>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer).unwrap_or(None);
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Set each payload's record from this run's check (carrick#3104): the
/// outcomes whose consumer is the payload's service and whose producer is
/// one of `payloads`, at `commit`. Every payload's record is replaced,
/// carried ones included, so a stored clone's record never rides under this
/// run's stamp. No outcomes, no record; a record over
/// [`PAIR_OUTCOMES_CAP_BYTES`] is not stored, and its pairs are judged by
/// every pull request run.
pub fn attach_pair_outcomes(
    payloads: &mut [super::CloudRepoData],
    outcomes: &[crate::analyzer::PairCheckOutcome],
    commit: &str,
    scanner_build: Option<&ScannerBuild>,
) {
    let id = |payload: &super::CloudRepoData| {
        payload
            .service_name
            .clone()
            .unwrap_or_else(|| payload.repo_name.clone())
    };
    let ids: std::collections::HashSet<String> = payloads.iter().map(id).collect();
    for payload in payloads.iter_mut() {
        let service = id(payload);
        let mut rows: Vec<StoredPairOutcome> = outcomes
            .iter()
            .filter(|outcome| {
                outcome.consumer_service == service && ids.contains(&outcome.producer_service)
            })
            .map(|outcome| StoredPairOutcome {
                pair_key: outcome.pair_key.clone(),
                spec_hash: outcome.spec_hash.clone(),
                judged: outcome.judged,
                bucket: outcome.bucket,
                gate: outcome.gate.clone(),
                diagnostic: outcome.diagnostic.clone(),
                resolved: outcome.resolved,
                unresolved_reason: outcome.unresolved_reason.clone(),
                notes: outcome.notes.clone(),
                consumer_reads: outcome.consumer_reads.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.pair_key.cmp(&b.pair_key));
        let record = (!rows.is_empty()).then(|| StoredPairOutcomes {
            version: PAIR_OUTCOMES_VERSION,
            commit: commit.to_string(),
            scanner_build: scanner_build.cloned(),
            outcomes: rows,
        });
        payload.pair_outcomes = record.filter(|record| {
            let bytes = serde_json::to_vec(record).map_or(0, |bytes| bytes.len());
            let fits = bytes <= PAIR_OUTCOMES_CAP_BYTES;
            if !fits {
                tracing::warn!(
                    "The pair outcomes of service {service} are {bytes} bytes, over the {} \
                     stored, so a pull request scan judges each of its pairs",
                    PAIR_OUTCOMES_CAP_BYTES
                );
            }
            fits
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Holder {
        #[serde(default, deserialize_with = "lenient_pair_outcomes")]
        pair_outcomes: Option<StoredPairOutcomes>,
    }

    fn read(json: &str) -> Option<StoredPairOutcomes> {
        serde_json::from_str::<Holder>(json)
            .expect("the holder always reads")
            .pair_outcomes
    }

    #[test]
    fn a_record_this_build_cannot_read_is_no_record() {
        let good = r#"{"pair_outcomes":{"version":1,"commit":"c","outcomes":[
            {"pair_key":"k","spec_hash":"h","judged":true,"bucket":"compatible","resolved":true}]}}"#;
        let record = read(good).expect("a record of known shape reads");
        assert_eq!(record.outcomes[0].bucket, VerdictBucket::Compatible);
        assert!(record.outcomes[0].notes.is_empty());

        let later_bucket = good.replace("\"compatible\"", "\"a_later_bucket\"");
        assert_eq!(read(&later_bucket), None);
        assert_eq!(read(r#"{"pair_outcomes":{"version":"one"}}"#), None);
        assert_eq!(read(r#"{"pair_outcomes":[]}"#), None);
        assert_eq!(read(r#"{"pair_outcomes":null}"#), None);
        assert_eq!(read("{}"), None);
    }
}
