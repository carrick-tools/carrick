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
//! The workspace join settles too (carrick#3884): every scan the join reads
//! already judged and retyped its own pairs and stored each consumer's final
//! outcome, so the join takes that outcome instead of judging the pair again.
//! Its terms are [`SettleFrom::join`]'s, not a pull request's: the record must
//! be this build's, at its blob's commit, with an equal `spec_hash` and
//! `judged` set. A dirty tree and a pair left to the consumer's own code both
//! settle, because the record was written from the tree the join reads.
//!
//! Anything else is judged as before. A run that uploads never builds a
//! [`SettleFrom`].

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
    /// carrick#4403: absent on every pair that reads no GraphQL type table.
    #[serde(skip_serializing_if = "Option::is_none")]
    producer_graphql_types: Option<&'a str>,
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
            PreVerdict::NoSurface { reason } => format!("no_surface:{reason}"),
            PreVerdict::NoBodyEitherSide => "no_body_either_side".to_string(),
        }),
        pre_verdict_side: pair.pre_verdict_side,
        producer_expanded: pair.producer_expanded.as_deref(),
        producer_unwidened: pair.producer_unwidened.as_deref(),
        consumer_expanded: pair.consumer_expanded.as_deref(),
        unstated_mode: pair.unstated_mode.as_deref(),
        unwidened_out_of_budget: pair.unwidened_out_of_budget,
        producer_graphql_types: pair.producer_graphql_types.as_deref(),
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
    /// The workspace join, from the scans' own records (carrick#3884).
    Join,
}

/// What a run settles pairs from.
pub(crate) struct SettleFrom {
    main: HashMap<String, MainGeneration>,
    carried: HashSet<String>,
    reach: HashMap<String, ReachDecision>,
    side: SettleSide,
    /// The join's side: each service's record, or why it cannot be read.
    join: HashMap<String, Result<HashMap<String, StoredPairOutcome>, JoinMiss>>,
    /// The join also judges each settled pair and counts the differences.
    verify: bool,
}

/// Why the join judged a pair instead of taking its stored outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum JoinMiss {
    /// The consumer's blob holds no record, or no outcome under the pair's
    /// key. A record over `PAIR_OUTCOMES_CAP_BYTES` was dropped at write
    /// time and reads as this.
    NoRecord,
    /// The record was written by another build, or by a build with tracked
    /// changes its commit does not hold.
    OtherBuild,
    /// The record is another format, at another commit than its blob, holds a
    /// key twice, or its service id names two blobs.
    Unreadable,
    /// The pair's inputs differ from the ones the stored outcome was judged on.
    Hash,
    /// The stored outcome was caused by the run, not the two types.
    NotJudged,
    /// The producer's blob is not in the join's input.
    NoProducer,
}

impl JoinMiss {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            JoinMiss::NoRecord => "no record",
            JoinMiss::OtherBuild => "other build",
            JoinMiss::Unreadable => "unreadable record",
            JoinMiss::Hash => "hash",
            JoinMiss::NotJudged => "not judged",
            JoinMiss::NoProducer => "no producer",
        }
    }
}

/// `blob`'s record by pair key, when the join may read it: this format, at
/// the blob's commit, by this clean build, each key once.
fn join_record(
    blob: &CloudRepoData,
    this_build: Option<&ScannerBuild>,
) -> Result<HashMap<String, StoredPairOutcome>, JoinMiss> {
    let record = blob.pair_outcomes.as_ref().ok_or(JoinMiss::NoRecord)?;
    if record.version != PAIR_OUTCOMES_VERSION || record.commit != blob.commit_hash {
        return Err(JoinMiss::Unreadable);
    }
    if !same_build(record.scanner_build.as_ref(), this_build)
        || !clean(record.scanner_build.as_ref())
    {
        return Err(JoinMiss::OtherBuild);
    }
    let mut by_key = HashMap::with_capacity(record.outcomes.len());
    for outcome in &record.outcomes {
        if by_key
            .insert(outcome.pair_key.clone(), outcome.clone())
            .is_some()
        {
            return Err(JoinMiss::Unreadable);
        }
    }
    Ok(by_key)
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
            join: HashMap::new(),
            verify: false,
        }
    }

    /// What the workspace join settles from (carrick#3884): every blob it
    /// reads. A pair settles when [`Self::join_settled`] accepts its
    /// consumer's record. Unlike a pull request's terms, neither a dirty tree,
    /// two commits, nor a pair left to the consumer's code refuses it: the
    /// record was written by the scan that read that tree, with this build.
    /// `verify` asks the join to judge every settled pair too and count the
    /// differences (`CARRICK_JOIN_SETTLE=verify`).
    pub(crate) fn join<'a>(
        blobs: impl IntoIterator<Item = &'a CloudRepoData>,
        this_build: Option<&ScannerBuild>,
        verify: bool,
    ) -> Self {
        let mut join = HashMap::new();
        for blob in blobs {
            let id = blob
                .service_name
                .clone()
                .unwrap_or_else(|| blob.repo_name.clone());
            // A service id that names two blobs: which record is the pair's
            // is not known, so neither is read.
            join.entry(id)
                .and_modify(|seen| *seen = Err(JoinMiss::Unreadable))
                .or_insert_with(|| join_record(blob, this_build));
        }
        Self {
            main: HashMap::new(),
            carried: HashSet::new(),
            reach: HashMap::new(),
            side: SettleSide::Join,
            join,
            verify,
        }
    }

    /// The join's stored outcome for `pair`, or why it is judged instead.
    pub(crate) fn join_settled(&self, pair: &BuiltPair) -> Result<&StoredPairOutcome, JoinMiss> {
        if !self.join.contains_key(&pair.producer_service) {
            return Err(JoinMiss::NoProducer);
        }
        let record = self
            .join
            .get(&pair.consumer_service)
            .ok_or(JoinMiss::NoRecord)?
            .as_ref()
            .map_err(|miss| *miss)?;
        let stored = record.get(&pair.spec.pair_key).ok_or(JoinMiss::NoRecord)?;
        if !stored.judged {
            return Err(JoinMiss::NotJudged);
        }
        if stored.spec_hash != spec_hash(pair) {
            return Err(JoinMiss::Hash);
        }
        Ok(stored)
    }

    pub(crate) fn verifies(&self) -> bool {
        self.verify
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
        if self.side == SettleSide::Join {
            return self
                .join_settled(pair)
                .ok()
                .map(|stored| (stored, Settled::ThisRun));
        }
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
    /// The join took the outcome the scan of its consumer stored (carrick#3884).
    ThisRun,
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
