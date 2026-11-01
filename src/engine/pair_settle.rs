//! A pull request run takes main's stored outcome for a pair whose two
//! sides its change cannot reach (carrick#3104).
//!
//! A main scan stores each consumer's final pair outcomes on its blob
//! (`CloudRepoData::pair_outcomes`), each with the hash of everything the
//! pair was judged on. A pull request run settles a pair, instead of judging
//! and retyping it, when:
//! - each side is a service the run carries or that kept main's types
//!   (carrick#3103), or an alias its type-reach decision left unreached
//!   (carrick#3102);
//! - both services' stored generations are at one commit, written by this
//!   run's build, from a clean tree;
//! - the consumer's stored record was written at that commit by this build,
//!   and holds an outcome under the pair's key with an equal hash that came
//!   from the two types (`judged`).
//!
//! A pair with a reached side settles on the same terms when the judge would
//! read the same input as main's did (carrick#3135): each side's
//! [`stamp_surface_hashes`] hash equals the one stored with main's outcome,
//! and main's judge did not leave the pair to the consumer's own code, which
//! the retype reads and no hash covers.
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

/// Whose run is settling: the pull request's own check, or main's side of
/// the comparison (carrick#3127).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettleSide {
    PullRequest,
    MainSide,
}

/// What a pull request run settles pairs from.
pub(crate) struct SettleFrom {
    main: HashMap<String, MainGeneration>,
    carried: HashSet<String>,
    reach: HashMap<String, ReachDecision>,
    side: SettleSide,
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
            side: SettleSide::PullRequest,
        }
    }

    /// What main's side of a pull request run settles from (carrick#3127).
    /// Every service of main's copy is main's by definition, so none is
    /// reached: a pair settles when `settled` accepts main's record for it.
    /// Peers are never in `main`, so a pair with a side in another repo never
    /// settles and is judged as before.
    pub(crate) fn main_side(
        main: &[CloudRepoData],
        this_build: Option<&ScannerBuild>,
        cache_version: u32,
    ) -> Self {
        let carried = main.iter().map(|blob| {
            blob.service_name
                .clone()
                .unwrap_or_else(|| blob.repo_name.clone())
        });
        Self {
            side: SettleSide::MainSide,
            ..Self::new(main, carried, HashMap::new(), this_build, cache_version)
        }
    }

    pub(crate) fn side(&self) -> SettleSide {
        self.side
    }

    /// Whether this run's change cannot reach `alias` of `service`. A service
    /// that kept main's types (carrick#3103) counts as carried: its rows are
    /// main's, so its pairs' aliases need not be the ones its decision named.
    fn unreached(&self, service: &str, alias: &str) -> bool {
        self.carried.contains(service)
            || self.reach.get(service).is_some_and(|decision| {
                decision.keeps_main_types()
                    || matches!(
                        decision,
                        ReachDecision::Partial { unreached, .. } if unreached.contains(alias)
                    )
            })
    }

    /// Main's outcome for `pair`, when the pair settles, and whether it
    /// settled with a reached side (carrick#3135).
    pub(crate) fn settled(&self, pair: &BuiltPair) -> Option<(&StoredPairOutcome, Settled)> {
        let producer = self.main.get(&pair.producer_service)?;
        let consumer = self.main.get(&pair.consumer_service)?;
        if !producer.usable || !consumer.usable || producer.commit != consumer.commit {
            return None;
        }
        let stored = consumer.outcomes.as_ref()?.get(&pair.spec.pair_key)?;
        if !stored.judged || stored.spec_hash != spec_hash(pair) {
            return None;
        }
        if self.unreached(&pair.producer_service, &pair.producer_alias)
            && self.unreached(&pair.consumer_service, &pair.consumer_alias)
        {
            Some((stored, Settled::Unreached))
        } else if same_judge_input(pair, stored) {
            Some((stored, Settled::SameJudgeInput))
        } else {
            None
        }
    }
}

/// How a pair settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Settled {
    /// Neither side is reached (carrick#3104).
    Unreached,
    /// A side is reached, and the judge would read main's input (carrick#3135).
    SameJudgeInput,
}

/// Whether the judge would read for `pair` what it read for main's `stored`
/// outcome: both sides hashed alike, and nothing left to the consumer's code.
fn same_judge_input(pair: &BuiltPair, stored: &StoredPairOutcome) -> bool {
    let equal = |ours: &Option<String>, main: &Option<String>| ours.is_some() && ours == main;
    !stored.left_to_consumer
        && equal(&pair.producer_surface_hash, &stored.producer_surface_hash)
        && equal(&pair.consumer_surface_hash, &stored.consumer_surface_hash)
}

/// Set each pair's per-side hash of what the judge reads (carrick#3135).
///
/// A side's hash covers, in this order: the capture's `surface_hash` for the
/// alias the probe reads, the one for its unwidened reading (the probe reads
/// it on a sending side, `check.ts`), and the dependencies of every stub the
/// judge's workspace holds, since one stub's pins move the version another
/// stub installs (`computeDedupeOverrides`). A side whose alias has no
/// recorded hash, or whose service id names two stubs, gets `None` and never
/// settles that way.
pub(crate) fn stamp_surface_hashes(pairs: &mut [BuiltPair], all_repo_data: &[CloudRepoData]) {
    let mut records: HashMap<&str, Option<HashMap<String, Option<String>>>> = HashMap::new();
    let mut workspace = Sha256::new();
    workspace.update(format!("carrick-workspace/{PAIR_OUTCOMES_VERSION}\n").as_bytes());
    let mut stubs: Vec<(&str, &crate::cloud_storage::CaptureStubArtifact)> = all_repo_data
        .iter()
        .filter_map(|repo| {
            let stub = repo.capture_stub.as_ref()?;
            (stub.artifact_version == crate::cloud_storage::CAPTURE_ARTIFACT_VERSION).then(|| {
                let id = repo.service_name.as_deref().unwrap_or(&repo.repo_name);
                (id, stub)
            })
        })
        .collect();
    stubs.sort_by(|a, b| a.0.cmp(b.0));
    for (id, stub) in &stubs {
        let parsed = stub
            .files
            .get(CAPTURE_MANIFEST_FILE)
            .and_then(|text| serde_json::from_str::<StubManifest>(text).ok())
            .map(|manifest| {
                manifest
                    .aliases
                    .into_iter()
                    .map(|record| (record.alias, record.surface_hash))
                    .collect()
            });
        // A service id that names two stubs: which one the judge reads is
        // not known, so neither side hashes.
        records
            .entry(id)
            .and_modify(|seen| *seen = None)
            .or_insert(parsed);
        workspace.update(
            serde_json::to_vec(&(
                id,
                stub.files.get("package.json"),
                stub.files.get(RESOLUTION_FILE),
            ))
            .unwrap_or_default(),
        );
    }
    let workspace = format!("{:x}", workspace.finalize());
    let side = |service: &str, alias: &str| -> Option<String> {
        let aliases = records.get(service)?.as_ref()?;
        let own = aliases.get(alias)?.as_ref()?;
        let unwidened = super::type_compat_v2::unwidened_alias(alias);
        let unwidened = match aliases.get(&unwidened) {
            None => None,
            Some(hash) => Some(hash.as_ref()?),
        };
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&(own, unwidened, &workspace)).unwrap_or_default());
        Some(format!("{:x}", hasher.finalize()))
    };
    for pair in pairs.iter_mut() {
        pair.producer_surface_hash =
            side(&pair.spec.producer.service_name, &pair.spec.producer.alias);
        pair.consumer_surface_hash =
            side(&pair.spec.consumer.service_name, &pair.spec.consumer.alias);
    }
}

/// The capture's per-alias records file inside a stub.
const CAPTURE_MANIFEST_FILE: &str = "carrick-manifest.json";
/// The stub's recorded dependency edges (the sidecar's `RESOLUTION_FILE`).
const RESOLUTION_FILE: &str = "carrick-resolution.json";

#[derive(serde::Deserialize)]
struct StubManifest {
    #[serde(default)]
    aliases: Vec<crate::services::type_sidecar::CaptureAliasRecord>,
}
