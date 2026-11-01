//! A pull request run takes main's stored outcome for a pair whose two
//! sides its change cannot reach (carrick#3104).
//!
//! A main scan stores each consumer's final pair outcomes on its blob
//! (`CloudRepoData::pair_outcomes`), each with the hash of everything the
//! pair was judged on. A pull request run settles a pair, instead of judging
//! and retyping it, when:
//! - each side is a service the run carries, or an alias its type-reach
//!   decision left unreached (carrick#3102);
//! - both services' stored generations are at one commit, written by this
//!   run's build, from a clean tree;
//! - the consumer's stored record was written at that commit by this build,
//!   and holds an outcome under the pair's key with an equal hash that came
//!   from the two types (`judged`).
//!
//! Anything else is judged as before. Only a pull request run settles: a run
//! that uploads never builds a [`SettleFrom`].

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::type_compat_v2::{BuiltPair, PreVerdict};
use super::type_reach::{ReachDecision, same_build, written_by_this_build};
use crate::cloud_storage::{
    CloudRepoData, ManifestTypeKind, PAIR_OUTCOMES_VERSION, ScannerBuild, StoredPairOutcome,
};
use crate::services::type_sidecar::{CheckPairSpec, VerdictSide};

/// The `spec_hash` preimage: every input of a pair's verdict the scan holds,
/// in a fixed order. Enums in their serde spelling, no maps.
#[derive(Serialize)]
struct SpecPreimage<'a> {
    spec: &'a CheckPairSpec,
    pseudo_method: &'a str,
    identity: &'a str,
    consumer_file: &'a str,
    consumer_line: u32,
    type_kind: ManifestTypeKind,
    producer_alias: &'a str,
    consumer_alias: &'a str,
    producer_service: &'a str,
    consumer_service: &'a str,
    pre_verdict: Option<String>,
    pre_verdict_side: Option<VerdictSide>,
    producer_expanded: Option<&'a str>,
    producer_unwidened: Option<&'a str>,
    consumer_expanded: Option<&'a str>,
    unstated_mode: Option<&'a str>,
    unwidened_out_of_budget: bool,
}

/// Lowercase hex SHA-256 of `"carrick-pair-spec/<version>\n"` and the pair's
/// [`SpecPreimage`] as JSON.
pub(crate) fn spec_hash(pair: &BuiltPair) -> String {
    let preimage = SpecPreimage {
        spec: &pair.spec,
        pseudo_method: &pair.pseudo_method,
        identity: &pair.identity,
        consumer_file: &pair.consumer_file,
        consumer_line: pair.consumer_line,
        type_kind: pair.type_kind,
        producer_alias: &pair.producer_alias,
        consumer_alias: &pair.consumer_alias,
        producer_service: &pair.producer_service,
        consumer_service: &pair.consumer_service,
        pre_verdict: pair.pre_verdict.as_ref().map(|pre| match pre {
            PreVerdict::NotCompared { reason } => format!("not_compared:{reason}"),
            PreVerdict::NoBodyEitherSide => "no_body_either_side".to_string(),
        }),
        pre_verdict_side: pair.pre_verdict_side,
        producer_expanded: pair.producer_expanded.as_deref(),
        producer_unwidened: pair.producer_unwidened.as_deref(),
        consumer_expanded: pair.consumer_expanded.as_deref(),
        unstated_mode: pair.unstated_mode.as_deref(),
        unwidened_out_of_budget: pair.unwidened_out_of_budget,
    };
    let mut hasher = Sha256::new();
    hasher.update(format!("carrick-pair-spec/{PAIR_OUTCOMES_VERSION}\n").as_bytes());
    hasher.update(serde_json::to_vec(&preimage).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

/// Main's stored generation of one service, as settling reads it.
struct MainGeneration {
    commit: String,
    /// Written by this run's build from a clean tree.
    usable: bool,
    /// Its record's outcomes by pair key, when the record is this run's to
    /// read: this format, at the blob's commit, by this build, each key once.
    outcomes: Option<HashMap<String, StoredPairOutcome>>,
}

/// What a pull request run settles pairs from.
pub(crate) struct SettleFrom {
    main: HashMap<String, MainGeneration>,
    carried: HashSet<String>,
    reach: HashMap<String, ReachDecision>,
}

/// A build that names tracked changes its commit does not hold is no build
/// to trust a stored answer from.
fn clean(build: Option<&ScannerBuild>) -> bool {
    !build.is_some_and(|build| build.dirty)
}

impl SettleFrom {
    /// `main` is main's stored generation of each service of this repo,
    /// `carried` the services this run carries, `reach` each analysed
    /// service's decision.
    pub(crate) fn new<'a>(
        main: impl IntoIterator<Item = &'a CloudRepoData>,
        carried: impl IntoIterator<Item = String>,
        reach: HashMap<String, ReachDecision>,
        this_build: Option<&ScannerBuild>,
        cache_version: u32,
    ) -> Self {
        let main = main
            .into_iter()
            .map(|blob| {
                let id = blob
                    .service_name
                    .clone()
                    .unwrap_or_else(|| blob.repo_name.clone());
                let usable = written_by_this_build(blob, this_build, cache_version)
                    && clean(blob.scanner_build.as_ref())
                    && blob.dirty != Some(true);
                let outcomes = blob
                    .pair_outcomes
                    .as_ref()
                    .filter(|record| {
                        record.version == PAIR_OUTCOMES_VERSION
                            && record.commit == blob.commit_hash
                            && same_build(record.scanner_build.as_ref(), this_build)
                            && clean(record.scanner_build.as_ref())
                    })
                    .and_then(|record| {
                        let mut by_key = HashMap::with_capacity(record.outcomes.len());
                        for outcome in &record.outcomes {
                            if by_key
                                .insert(outcome.pair_key.clone(), outcome.clone())
                                .is_some()
                            {
                                return None;
                            }
                        }
                        Some(by_key)
                    });
                (
                    id,
                    MainGeneration {
                        commit: blob.commit_hash.clone(),
                        usable,
                        outcomes,
                    },
                )
            })
            .collect();
        Self {
            main,
            carried: carried.into_iter().collect(),
            reach,
        }
    }

    /// Whether this run's change cannot reach `alias` of `service`.
    fn unreached(&self, service: &str, alias: &str) -> bool {
        self.carried.contains(service)
            || matches!(
                self.reach.get(service),
                Some(ReachDecision::Partial { unreached, .. }) if unreached.contains(alias)
            )
    }

    /// Main's outcome for `pair`, when the pair settles.
    pub(crate) fn settled(&self, pair: &BuiltPair) -> Option<&StoredPairOutcome> {
        if !self.unreached(&pair.producer_service, &pair.producer_alias)
            || !self.unreached(&pair.consumer_service, &pair.consumer_alias)
        {
            return None;
        }
        let producer = self.main.get(&pair.producer_service)?;
        let consumer = self.main.get(&pair.consumer_service)?;
        if !producer.usable || !consumer.usable || producer.commit != consumer.commit {
            return None;
        }
        let stored = consumer.outcomes.as_ref()?.get(&pair.spec.pair_key)?;
        (stored.judged && stored.spec_hash == spec_hash(pair)).then_some(stored)
    }
}
