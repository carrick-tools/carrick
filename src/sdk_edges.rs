//! The SDK-mediated consumer edge: consumer → package → member → producer.
//!
//! A consumer that calls a service through its published client writes no
//! route, so the HTTP matcher has nothing to match. The three facts that
//! bridge the gap are already computed elsewhere and this module joins them:
//!
//! 1. the consumer's [`crate::external_call_candidates::CallMechanism::Sdk`]
//!    row, which names the package, the export the receiver came from, and the
//!    callee as written;
//! 2. the SDK repo's [`crate::sdk_surface`], which maps that export and member
//!    chain to the source span implementing it;
//! 3. the [`crate::analyzer::CrossRepoMatch`] the SDK repo's OWN outbound call
//!    inside that span already formed with the producer endpoint.
//!
//! Nothing new is matched here. Step 3 reads the edges the cross-repo analyzer
//! produced this run — it merges every repo's calls and endpoints into one
//! analyzer, so a peer SDK repo's calls are matched against producers exactly
//! like the current repo's are — and the result is a relationship of its own,
//! never folded back into `endpoints`/`calls`.
//!
//! # The type verdict (#525)
//!
//! An edge's `type_compatible` is the verdict for the SDK→producer pair, not
//! for a comparison this run performs. Cross-repo type checking only evaluates
//! pairs where a CURRENT service is the consumer, and in the consumer's run the
//! SDK repo is a peer — so the `CrossRepoMatch` its call formed carries no
//! verdict. Two sources are read, in order:
//!
//! 1. the verdict overlaid on the match this run (present only when the SDK
//!    repo is itself one of the current services);
//! 2. the verdict the SDK repo's OWN scan persisted on its blob
//!    ([`CompatVerdict`]), looked up by the four canonical fields of that same
//!    match: `(producer_repo, producer_key, consumer_repo, consumer_key)`.
//!
//! `sdk_location` is what selects WHICH match — the member's span is what the
//! SDK's own call site has to fall inside — but it is not part of the lookup
//! key, because `attach_compat_verdicts` stores one verdict per canonical pair
//! and no location. Two members whose calls collapse onto the same canonical
//! producer/consumer pair therefore share one verdict (incompatible-wins, as
//! stored).
//!
//! Neither source is a fresh comparison against the producer as it stands
//! today: a stored verdict was computed when the SDK repo last scanned, and
//! nothing in [`CompatVerdict`] records which producer revision it judged. A
//! producer that changed since then is not re-judged here (#530).
//! Absent both sources the edge stays `None` — "no verdict stored", never
//! "compatible" (#324).
//!
//! # Where it stops
//!
//! Each dead end is recorded as an [`SdkUnresolved`] reason rather than
//! dropped:
//!
//! - `no_sdk_repo_in_project` — no scanned repo declares that package name, or
//!   several do (which one publishes it is then a guess).
//! - `receiver_unresolved` — the receiver is a namespace import, or the callee
//!   chain runs through a call (`getClient().payments.create`), so no single
//!   member is named.
//! - `member_not_found` — the SDK repo publishes no such member; also the
//!   answer for a subpath import (`pkg/edge`), whose entry module is not the
//!   root one the surface was walked from, and for a peer scanned before
//!   `sdk_surface` existed.
//! - `no_matching_producer` — the member's span contains no outbound call that
//!   matched a producer. Three structural causes are worth naming, because
//!   each is a real shape rather than a bug here. The cross-repo analyzer
//!   drops same-identity pairs (#397), so an SDK that wraps its OWN service's
//!   endpoints forms no `CrossRepoMatch` to carry. A vendor client whose
//!   hardcoded host the project does not declare internal is classified
//!   external at extraction, so it never matches either. And
//!   `MountGraph::merge_from_repos` dedupes data calls on
//!   `method:target_url:file_location` without repo identity, so two repos
//!   whose relative paths, method and target all coincide collapse to one
//!   merged call and the second repo's consumer attribution is lost.
//!
//! `export { default as Ledger } from './client'` with no `export default`
//! publishes under the name `Ledger`, not `default` — deliberately, because
//! that is what the consumer's `import_symbol` carries for a named import of
//! it. A default import of such a package carries `default` and correctly
//! finds no member.

use crate::analyzer::CrossRepoMatch;
use crate::cloud_storage::{CloudRepoData, CompatVerdict, SdkEdge, SdkUnresolved};
use crate::external_call_candidates::{CallMechanism, ExternalCallCandidate};
use crate::findings::Finding;
use crate::sdk_surface::SdkMember;
use crate::type_manifest::parse_file_location;
use std::collections::BTreeMap;
use tracing::debug;

const NO_SDK_REPO: &str = "no_sdk_repo_in_project";
const MEMBER_NOT_FOUND: &str = "member_not_found";
const RECEIVER_UNRESOLVED: &str = "receiver_unresolved";
const NO_MATCHING_PRODUCER: &str = "no_matching_producer";

/// Everything the join reads out of the repo blobs.
///
/// Collected before `all_repo_data` and `current_services_data` are moved into
/// the cross-repo analyzer, because the join's other input — the analyzer's
/// `CrossRepoMatch` edges — only exists after that. A compact projection
/// rather than a clone of the blobs: the blobs carry megabytes of caches and
/// bundled types this needs none of.
#[derive(Debug, Default)]
pub struct SdkJoinInput {
    peers: Vec<SdkPeer>,
    consumers: Vec<SdkConsumer>,
}

/// A repo that might publish a package some consumer calls.
#[derive(Debug)]
struct SdkPeer {
    service_id: String,
    package_names: Vec<String>,
    /// `None` = scanned before the channel existed, which is not the same as
    /// an empty surface and is logged differently.
    surface: Option<Vec<SdkMember>>,
    /// `(file, line)` of every outbound call the SDK repo makes.
    data_calls: Vec<(String, u32)>,
    /// The verdicts the SDK repo's own scan persisted for the pairs where it
    /// is the consumer. Empty when it stored none — which includes every blob
    /// written before the field existed, and every scan whose type check did
    /// not evaluate the pair. Absent is never read as compatible (#324).
    compat_verdicts: Vec<CompatVerdict>,
}

/// A current service whose SDK calls are being resolved.
#[derive(Debug)]
struct SdkConsumer {
    service_id: String,
    candidates: Vec<ExternalCallCandidate>,
}

/// The join's output, addressable per consumer service (for the blobs) and in
/// full (for the report).
#[derive(Debug, Default)]
pub struct SdkJoin {
    edges: Vec<SdkEdge>,
    /// `(consumer service id, aggregated reason)`.
    unresolved: Vec<(String, SdkUnresolved)>,
}

impl SdkJoin {
    pub fn edges(&self) -> &[SdkEdge] {
        &self.edges
    }

    pub fn unresolved(&self) -> Vec<SdkUnresolved> {
        self.unresolved
            .iter()
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty() && self.unresolved.is_empty()
    }

    fn edges_for(&self, service_id: &str) -> Vec<SdkEdge> {
        self.edges
            .iter()
            .filter(|edge| edge.consumer_repo == service_id)
            .cloned()
            .collect()
    }

    fn unresolved_for(&self, service_id: &str) -> Vec<SdkUnresolved> {
        self.unresolved
            .iter()
            .filter(|(consumer, _)| consumer == service_id)
            .map(|(_, entry)| entry.clone())
            .collect()
    }
}

fn service_id(repo: &CloudRepoData) -> String {
    repo.service_name
        .clone()
        .unwrap_or_else(|| repo.repo_name.clone())
}

impl SdkJoinInput {
    /// Project the blobs the join needs.
    ///
    /// `all_repos` is every blob in the run (peers plus the current services):
    /// any of them can be the SDK publisher. `consumers` is the current
    /// services only, because an edge is stored on the consumer's blob and this
    /// run only writes its own.
    pub fn collect<'a>(
        all_repos: impl Iterator<Item = &'a CloudRepoData>,
        consumers: impl Iterator<Item = &'a CloudRepoData>,
    ) -> Self {
        let peers = all_repos
            .map(|repo| SdkPeer {
                service_id: service_id(repo),
                package_names: repo
                    .packages
                    .as_ref()
                    .map(|packages| {
                        packages
                            .package_jsons
                            .iter()
                            .filter_map(|manifest| manifest.name.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
                surface: repo.sdk_surface.clone(),
                data_calls: repo
                    .mount_graph
                    .as_ref()
                    .map(|graph| {
                        graph
                            .data_calls
                            .iter()
                            .map(|call| parse_file_location(&call.file_location))
                            .collect()
                    })
                    .unwrap_or_default(),
                compat_verdicts: repo.compat_verdicts.clone().unwrap_or_default(),
            })
            .collect();
        let consumers = consumers
            .map(|repo| SdkConsumer {
                service_id: service_id(repo),
                candidates: repo
                    .external_call_candidates
                    .as_ref()
                    .map(|rows| {
                        rows.iter()
                            .filter(|row| row.mechanism == CallMechanism::Sdk)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect();
        Self { peers, consumers }
    }
}

/// Resolve every current service's SDK call candidates against the peers'
/// published surfaces and the cross-repo edges those peers' own calls formed.
pub fn join(input: &SdkJoinInput, matches: &[CrossRepoMatch]) -> SdkJoin {
    let scanner_version = env!("CARGO_PKG_VERSION");

    // Producer side of every match, keyed by the consumer that formed it. The
    // consumer here is the SDK repo: its own outbound call is what matched.
    let mut producers_by_call: BTreeMap<(String, String, u32), Vec<&CrossRepoMatch>> =
        BTreeMap::new();
    for edge in matches {
        let Some(location) = edge.consumer_location.as_deref() else {
            continue;
        };
        let (file, line) = parse_file_location(location);
        producers_by_call
            .entry((edge.consumer_repo.clone(), file, line))
            .or_default()
            .push(edge);
    }

    // Deduped by the pair the cloud joins on, which also fixes emission order.
    let mut edges: BTreeMap<(String, String, String), SdkEdge> = BTreeMap::new();
    let mut unresolved: BTreeMap<(String, String, String), usize> = BTreeMap::new();

    for consumer in &input.consumers {
        for candidate in &consumer.candidates {
            let mut record = |reason: &str| {
                *unresolved
                    .entry((
                        consumer.service_id.clone(),
                        candidate.package.clone(),
                        reason.to_string(),
                    ))
                    .or_insert(0) += 1;
            };

            // 1. Which scanned repo publishes this package?
            let publishers: Vec<&SdkPeer> = input
                .peers
                .iter()
                .filter(|peer| peer.package_names.iter().any(|n| n == &candidate.package))
                .collect();
            let peer = match publishers.as_slice() {
                [only] => *only,
                [] => {
                    record(NO_SDK_REPO);
                    continue;
                }
                several => {
                    debug!(
                        "{} repos declare the package name '{}'; which one publishes it is a \
                         guess, so no SDK edge is emitted",
                        several.len(),
                        candidate.package
                    );
                    record(NO_SDK_REPO);
                    continue;
                }
            };

            // 2. Which member does the callee name?
            let Some(import_symbol) = candidate.import_symbol.as_deref() else {
                // A namespace import binds the module, not one of its exports,
                // so there is no export to anchor the chain to.
                record(RECEIVER_UNRESOLVED);
                continue;
            };
            if candidate.subpath.is_some() {
                // The surface is walked from the package's ROOT entry module.
                // A subpath entry publishes a different module, so matching a
                // subpath receiver against the root surface would be a guess.
                debug!(
                    "SDK call to '{}/{}' names a subpath entry, which the published surface does \
                     not cover",
                    candidate.package,
                    candidate.subpath.as_deref().unwrap_or_default()
                );
                record(MEMBER_NOT_FOUND);
                continue;
            }
            let Some(chain) = member_chain(&candidate.callee) else {
                record(RECEIVER_UNRESOLVED);
                continue;
            };
            if chain.is_empty() {
                record(MEMBER_NOT_FOUND);
                continue;
            }
            let Some(surface) = peer.surface.as_deref() else {
                debug!(
                    "Peer '{}' publishes '{}' but was scanned before sdk_surface existed; its \
                     members cannot be resolved until it is re-scanned",
                    peer.service_id, candidate.package
                );
                record(MEMBER_NOT_FOUND);
                continue;
            };
            let Some(member) = surface
                .iter()
                .find(|m| m.export == import_symbol && m.chain == chain)
            else {
                record(MEMBER_NOT_FOUND);
                continue;
            };

            // 3. Which producer does the SDK's own call inside that member reach?
            let mut matched = false;
            for (file, line) in &peer.data_calls {
                if file != &member.file || *line < member.line || *line > member.end_line {
                    continue;
                }
                let Some(producers) =
                    producers_by_call.get(&(peer.service_id.clone(), file.clone(), *line))
                else {
                    continue;
                };
                for producer in producers {
                    matched = true;
                    let consumer_location = format!("{}:{}", candidate.file, candidate.line);
                    let (type_compatible, mismatch_reason) = pair_verdict(peer, producer);
                    edges.insert(
                        (
                            consumer.service_id.clone(),
                            consumer_location.clone(),
                            producer.producer_key.clone(),
                        ),
                        SdkEdge {
                            consumer_repo: consumer.service_id.clone(),
                            consumer_location,
                            package: candidate.package.clone(),
                            import_symbol: Some(import_symbol.to_string()),
                            callee: candidate.callee.clone(),
                            sdk_repo: peer.service_id.clone(),
                            sdk_member: member.chain.clone(),
                            sdk_location: format!("{}:{}", member.file, member.line),
                            producer_repo: producer.producer_repo.clone(),
                            producer_key: producer.producer_key.clone(),
                            type_compatible,
                            mismatch_reason,
                            scanner_version: scanner_version.to_string(),
                        },
                    );
                }
            }
            if !matched {
                record(NO_MATCHING_PRODUCER);
            }
        }
    }

    SdkJoin {
        edges: edges.into_values().collect(),
        unresolved: unresolved
            .into_iter()
            .map(|((consumer, package, reason), count)| {
                (
                    consumer,
                    SdkUnresolved {
                        package,
                        count,
                        reason,
                    },
                )
            })
            .collect(),
    }
}

/// The type verdict for one SDK→producer pair (#525).
///
/// Prefers the verdict this run overlaid on the match — present only when the
/// SDK repo is itself a current service, because compat is evaluated for
/// current-service consumers only — and otherwise reads the verdict the SDK
/// repo's own scan persisted for the same canonical pair. Returns
/// `(None, None)` when neither source has one, which downstream reads as "no
/// verdict stored", never as compatible (#324).
///
/// The lookup key is the match's four canonical fields. `CompatVerdict` stores
/// no location, so this cannot key on the SDK call site; see the module docs.
fn pair_verdict(peer: &SdkPeer, producer: &CrossRepoMatch) -> (Option<bool>, Option<String>) {
    if producer.type_compatible.is_some() {
        return (producer.type_compatible, producer.mismatch_reason.clone());
    }
    let stored = peer.compat_verdicts.iter().find(|verdict| {
        verdict.producer_repo == producer.producer_repo
            && verdict.producer_key == producer.producer_key
            && verdict.consumer_repo == producer.consumer_repo
            && verdict.consumer_key == producer.consumer_key
    });
    match stored {
        Some(verdict) => (
            Some(verdict.compatible),
            if verdict.compatible {
                None
            } else {
                verdict.mismatch_reason.clone()
            },
        ),
        None => {
            debug!(
                "No type verdict for {} → {} ({}): neither this run nor '{}'s own scan stored one",
                producer.consumer_key,
                producer.producer_key,
                producer.producer_repo,
                peer.service_id
            );
            (None, None)
        }
    }
}

/// Project every INCOMPATIBLE SDK edge into a [`Finding::TypeMismatch`], so a
/// contract break reached through a published client counts toward the PR
/// verdict exactly like a direct incompatible pair (#525).
///
/// This is the only route to the verdict: the PR result payload the cloud
/// renders the comment from carries `findings`, not `sdk_edges`, so an edge
/// that stayed inside the SDK section would render a red row under a green
/// headline.
///
/// `producer_type` / `consumer_type` are SIDE LABELS here, not type symbols:
/// the SDK repo's scan persists a verdict and a diagnostic but no type names,
/// so naming a symbol would mean inventing one. The hop the break travels
/// through is spelled out in `detail`, ahead of the stored diagnostic.
///
/// Nothing is deduped against the direct findings, and in one shape that shows:
/// a `carrick.json` whose `services` array holds BOTH the SDK service and a
/// service that consumes it. The SDK's own pair is then checked this run and
/// emits its own risk row, alongside this one. Two rows for one broken field,
/// deliberately — they cite different call sites, and in that layout both are
/// the reader's to fix.
pub fn type_mismatch_findings(edges: &[SdkEdge]) -> Vec<Finding> {
    edges
        .iter()
        .filter(|edge| edge.type_compatible == Some(false))
        .map(|edge| {
            let (method, path) = key_labels(&edge.producer_key);
            let reason = edge
                .mismatch_reason
                .as_deref()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("producer and consumer types are incompatible");
            Finding::type_mismatch(
                method,
                path,
                None,
                vec![edge.consumer_location.clone()],
                edge.producer_repo.clone(),
                format!("{} ({})", edge.package, edge.sdk_member),
                &format!(
                    "reached through `{}` (`{}` at {}:{}): {}",
                    edge.package, edge.sdk_member, edge.sdk_repo, edge.sdk_location, reason
                ),
            )
        })
        .collect()
}

/// Split an `OperationKey::canonical()` into the `(method, path)` display
/// labels a finding carries. Three-segment keys (`http|POST|/v1/payments`,
/// `graphql|query|orders`, `socket|emit|tick`) name both; the two-segment
/// pub/sub key names only a topic, so the protocol stands in for the method.
fn key_labels(producer_key: &str) -> (String, String) {
    let parts: Vec<&str> = producer_key.splitn(3, '|').collect();
    match parts.as_slice() {
        [_protocol, method, path] => (method.to_string(), path.to_string()),
        [protocol, topic] => (protocol.to_string(), topic.to_string()),
        _ => (String::new(), producer_key.to_string()),
    }
}

/// The member path a callee names, with the root binding dropped.
///
/// `ledger.payments.create` → `payments.create`. `None` when a segment after
/// the root is itself a call (`client.transport().send`): the value the chain
/// continues from only exists at runtime, so no published member is named.
/// The root segment's own `()` is not read — what bound the receiver is
/// already answered by `import_symbol`.
fn member_chain(callee: &str) -> Option<String> {
    let mut segments = callee.split('.');
    segments.next()?;
    let rest: Vec<&str> = segments.collect();
    if rest.iter().any(|segment| segment.contains("()")) {
        return None;
    }
    Some(rest.join("."))
}

/// Attach each service's SDK edges to its upload payload, mirroring
/// [`crate::cloud_storage::attach_compat_verdicts`]: consumer-side only, and
/// absent rather than empty when there is nothing to say.
pub fn attach_sdk_edges(payloads: &mut [CloudRepoData], join: &SdkJoin) {
    for payload in payloads.iter_mut() {
        let id = service_id(payload);
        let edges = join.edges_for(&id);
        let unresolved = join.unresolved_for(&id);
        payload.sdk_edges = (!edges.is_empty()).then_some(edges);
        payload.sdk_unresolved = (!unresolved.is_empty()).then_some(unresolved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::CrossRepoMatch;
    use crate::external_call_candidates::CallMechanism;
    use crate::mount_graph::{DataFetchingCall, MountGraph};
    use crate::packages::{PackageJson, Packages};
    use crate::sdk_surface::SdkMember;
    use std::collections::HashMap;

    const SDK_PACKAGE: &str = "@fixture/ledger-sdk";
    const PRODUCER_KEY: &str = "http|POST|/v1/payments";

    fn blob(repo_name: &str) -> CloudRepoData {
        CloudRepoData {
            repo_name: repo_name.to_string(),
            service_name: None,
            endpoints: vec![],
            calls: vec![],
            mounts: vec![],
            apps: HashMap::new(),
            imported_handlers: vec![],
            function_definitions: HashMap::new(),
            config_json: None,
            package_json: None,
            packages: None,
            last_updated: chrono::Utc::now(),
            commit_hash: "abc123".to_string(),
            mount_graph: None,
            bundled_types: None,
            type_manifest: None,
            file_results: None,
            cached_detection: None,
            cached_guidance: None,
            cached_extraction_config: None,
            package_json_hash: None,
            cache_version: None,
            type_extraction_status: None,
            compat_verdicts: None,
            capture_stub: None,
            external_call_candidates: None,
            sdk_surface: None,
            sdk_edges: None,
            sdk_unresolved: None,
        }
    }

    /// The producer: it owns `POST /v1/payments`. Only its identity is read by
    /// the join (the endpoint itself reaches the join through the match).
    fn producer() -> CloudRepoData {
        blob("payments-api")
    }

    /// The SDK repo: it declares the package name, publishes one member, and
    /// makes its own outbound call inside that member's span.
    fn sdk_repo() -> CloudRepoData {
        let mut data = blob("ledger-sdk");
        let mut packages = Packages::default();
        packages.package_jsons.push(PackageJson {
            name: Some(SDK_PACKAGE.to_string()),
            version: Some("1.0.0".to_string()),
            dependencies: HashMap::new(),
            dev_dependencies: HashMap::new(),
            peer_dependencies: HashMap::new(),
            optional_dependencies: HashMap::new(),
            resolutions: HashMap::new(),
        });
        data.packages = Some(packages);
        data.sdk_surface = Some(vec![SdkMember {
            export: "default".to_string(),
            chain: "payments.create".to_string(),
            file: "src/resources/payments.ts".to_string(),
            line: 28,
            end_line: 33,
        }]);
        let mut graph = MountGraph::new();
        graph.data_calls = vec![DataFetchingCall {
            method: "POST".to_string(),
            target_url: "/v1/payments".to_string(),
            canonical_path: "/v1/payments".to_string(),
            client: "fetch(".to_string(),
            file_location: "src/resources/payments.ts:32".to_string(),
            call_kind: None,
            repo_name: None,
            service_name: None,
            host: None,
            line: Some(32),
        }];
        data.mount_graph = Some(graph);
        data
    }

    /// The consumer: it calls `ledger.payments.create(...)`, and its source
    /// contains no route at all.
    fn consumer_with(
        candidate: crate::external_call_candidates::ExternalCallCandidate,
    ) -> CloudRepoData {
        let mut data = blob("checkout");
        data.external_call_candidates = Some(vec![candidate]);
        data
    }

    fn candidate(callee: &str) -> crate::external_call_candidates::ExternalCallCandidate {
        crate::external_call_candidates::ExternalCallCandidate {
            file: "src/checkout.ts".to_string(),
            line: 42,
            callee: callee.to_string(),
            package: SDK_PACKAGE.to_string(),
            mechanism: CallMechanism::Sdk,
            import_symbol: Some("default".to_string()),
            subpath: None,
        }
    }

    /// The `CrossRepoMatch` the SDK repo's own call already formed with the
    /// producer. Nothing in the join re-matches; this is the edge it reads.
    fn sdk_to_producer_match() -> CrossRepoMatch {
        CrossRepoMatch {
            producer_repo: "payments-api".to_string(),
            producer_key: PRODUCER_KEY.to_string(),
            consumer_repo: "ledger-sdk".to_string(),
            consumer_key: "http|POST|/v1/payments".to_string(),
            consumer_location: Some("src/resources/payments.ts:32".to_string()),
            match_score: 1.0,
            type_compatible: Some(true),
            type_verdict: None,
            mismatch_reason: None,
            producer_provenance: Default::default(),
            relationship: carrick_match::MatchRelationship::ProducerConsumer,
        }
    }

    fn run(
        repos: &[CloudRepoData],
        consumers: &[CloudRepoData],
        matches: &[CrossRepoMatch],
    ) -> SdkJoin {
        let all: Vec<&CloudRepoData> = repos.iter().chain(consumers.iter()).collect();
        let input = SdkJoinInput::collect(all.into_iter(), consumers.iter());
        join(&input, matches)
    }

    fn reasons(join: &SdkJoin) -> Vec<(String, String, usize)> {
        join.unresolved()
            .into_iter()
            .map(|entry| (entry.package, entry.reason, entry.count))
            .collect()
    }

    #[test]
    fn resolves_a_consumer_call_through_the_sdk_to_the_producer_endpoint() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[sdk_to_producer_match()],
        );

        assert_eq!(joined.edges().len(), 1, "{:?}", joined);
        let edge = &joined.edges()[0];
        assert_eq!(edge.consumer_repo, "checkout");
        assert_eq!(edge.consumer_location, "src/checkout.ts:42");
        assert_eq!(edge.package, SDK_PACKAGE);
        assert_eq!(edge.import_symbol.as_deref(), Some("default"));
        assert_eq!(edge.callee, "ledger.payments.create");
        assert_eq!(edge.sdk_repo, "ledger-sdk");
        assert_eq!(edge.sdk_member, "payments.create");
        assert_eq!(edge.sdk_location, "src/resources/payments.ts:28");
        assert_eq!(edge.producer_repo, "payments-api");
        // Byte-identical to the producer endpoint's own canonical key: the
        // cloud de-orphans by exact match on this string.
        assert_eq!(edge.producer_key, PRODUCER_KEY);
        assert_eq!(edge.type_compatible, Some(true));
        assert!(joined.unresolved().is_empty());
    }

    /// The edge lands on the CONSUMER's blob, the same storage convention the
    /// compat verdicts follow — never on the producer's or the SDK's.
    #[test]
    fn edges_attach_to_the_consumer_payload_only() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[sdk_to_producer_match()],
        );
        let mut payloads = vec![producer(), sdk_repo(), blob("checkout")];
        attach_sdk_edges(&mut payloads, &joined);

        assert!(payloads[0].sdk_edges.is_none());
        assert!(payloads[1].sdk_edges.is_none());
        assert_eq!(payloads[2].sdk_edges.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn no_scanned_repo_publishes_the_package() {
        let joined = run(
            &[producer()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), NO_SDK_REPO.to_string(), 1)]
        );
    }

    /// Two repos declaring one package name make the publisher a guess, so the
    /// join declines rather than picking one.
    #[test]
    fn two_repos_declaring_one_package_resolve_to_neither() {
        let mut twin = sdk_repo();
        twin.repo_name = "ledger-sdk-fork".to_string();
        let joined = run(
            &[producer(), sdk_repo(), twin],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), NO_SDK_REPO.to_string(), 1)]
        );
    }

    /// A namespace import binds the module, not one of its exports, so there
    /// is no export to anchor the member chain to.
    #[test]
    fn a_namespace_receiver_is_unresolved() {
        let mut row = candidate("ledger.payments.create");
        row.import_symbol = None;
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(row)],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), RECEIVER_UNRESOLVED.to_string(), 1)]
        );
    }

    /// A call in the middle of the chain means the value the chain continues
    /// from only exists at runtime.
    #[test]
    fn a_call_inside_the_chain_is_unresolved() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.transport().create"))],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), RECEIVER_UNRESOLVED.to_string(), 1)]
        );
    }

    #[test]
    fn a_member_the_sdk_does_not_publish_is_not_found() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.refund"))],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), MEMBER_NOT_FOUND.to_string(), 1)]
        );
    }

    /// A peer scanned before this channel existed carries no surface at all,
    /// which cannot be told apart from a repo that publishes nothing.
    #[test]
    fn a_peer_without_a_published_surface_is_not_found() {
        let mut stale = sdk_repo();
        stale.sdk_surface = None;
        let joined = run(
            &[producer(), stale],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), MEMBER_NOT_FOUND.to_string(), 1)]
        );
    }

    /// A subpath entry publishes a different module from the root one the
    /// surface was walked from, so matching it against the root surface would
    /// be a guess.
    #[test]
    fn a_subpath_import_is_not_matched_against_the_root_surface() {
        let mut row = candidate("ledger.payments.create");
        row.subpath = Some("edge".to_string());
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(row)],
            &[sdk_to_producer_match()],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), MEMBER_NOT_FOUND.to_string(), 1)]
        );
    }

    /// The member resolves, but the SDK's own call inside it matched no
    /// producer — the shape that fires when the cross-repo analyzer dropped
    /// the pair (same identity, #397) or classified the call external.
    #[test]
    fn a_member_whose_call_matched_no_producer_has_no_edge() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), NO_MATCHING_PRODUCER.to_string(), 1)]
        );
    }

    /// A data call outside the member's span belongs to a different member and
    /// must not be attributed to this one.
    #[test]
    fn a_call_outside_the_member_span_is_not_attributed_to_it() {
        let mut sdk = sdk_repo();
        if let Some(graph) = sdk.mount_graph.as_mut() {
            graph.data_calls[0].file_location = "src/resources/payments.ts:99".to_string();
        }
        let mut edge = sdk_to_producer_match();
        edge.consumer_location = Some("src/resources/payments.ts:99".to_string());
        let joined = run(
            &[producer(), sdk],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[edge],
        );
        assert!(joined.edges().is_empty());
        assert_eq!(
            reasons(&joined),
            vec![(SDK_PACKAGE.to_string(), NO_MATCHING_PRODUCER.to_string(), 1)]
        );
    }

    /// Two call sites onto one pair collapse; two producers behind one member
    /// each get their own edge.
    #[test]
    fn edges_dedup_on_the_pair_the_cloud_joins_on() {
        let mut consumer = consumer_with(candidate("ledger.payments.create"));
        // The SAME call site, extracted twice.
        if let Some(rows) = consumer.external_call_candidates.as_mut() {
            rows.push(candidate("ledger.payments.create"));
        }
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer],
            &[sdk_to_producer_match()],
        );
        assert_eq!(joined.edges().len(), 1);
    }

    // ---- #525: the type verdict on an SDK edge ----

    /// The same match, with compat NOT overlaid — what the CONSUMER's run
    /// actually sees, because the check only evaluates pairs whose consumer is
    /// a current service and the SDK repo is a peer there.
    fn unjudged_match() -> CrossRepoMatch {
        let mut edge = sdk_to_producer_match();
        edge.type_compatible = None;
        edge.mismatch_reason = None;
        edge
    }

    /// A verdict as the SDK repo's own scan persisted it, keyed by the four
    /// canonical fields `attach_compat_verdicts` writes.
    fn stored_verdict(compatible: bool) -> CompatVerdict {
        CompatVerdict {
            producer_repo: "payments-api".to_string(),
            producer_key: PRODUCER_KEY.to_string(),
            consumer_repo: "ledger-sdk".to_string(),
            consumer_key: "http|POST|/v1/payments".to_string(),
            compatible,
            mismatch_reason: (!compatible)
                .then(|| "Property 'amountCents' is missing in type 'Payment'".to_string()),
            scanner_version: "0.0.0-test".to_string(),
        }
    }

    fn sdk_repo_storing(verdicts: Vec<CompatVerdict>) -> CloudRepoData {
        let mut data = sdk_repo();
        data.compat_verdicts = Some(verdicts);
        data
    }

    #[test]
    fn an_incompatible_stored_verdict_reaches_the_edge() {
        let joined = run(
            &[producer(), sdk_repo_storing(vec![stored_verdict(false)])],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[unjudged_match()],
        );

        let edge = &joined.edges()[0];
        assert_eq!(edge.type_compatible, Some(false));
        assert_eq!(
            edge.mismatch_reason.as_deref(),
            Some("Property 'amountCents' is missing in type 'Payment'")
        );
    }

    #[test]
    fn a_compatible_stored_verdict_reaches_the_edge_without_a_reason() {
        let joined = run(
            &[producer(), sdk_repo_storing(vec![stored_verdict(true)])],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[unjudged_match()],
        );

        let edge = &joined.edges()[0];
        assert_eq!(edge.type_compatible, Some(true));
        assert!(edge.mismatch_reason.is_none());
    }

    /// This run's own overlay is the fresher fact, so it wins over anything the
    /// SDK repo stored on a previous scan.
    #[test]
    fn this_runs_verdict_wins_over_the_stored_one() {
        let mut judged = sdk_to_producer_match();
        judged.type_compatible = Some(false);
        judged.mismatch_reason = Some("checked this run".to_string());

        let joined = run(
            &[producer(), sdk_repo_storing(vec![stored_verdict(true)])],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[judged],
        );

        let edge = &joined.edges()[0];
        assert_eq!(edge.type_compatible, Some(false));
        assert_eq!(edge.mismatch_reason.as_deref(), Some("checked this run"));
    }

    /// The whole reason the lookup key is a four-tuple: a verdict stored for a
    /// DIFFERENT pair must never be carried onto this edge. Fabricating a
    /// `true` here is the #324 fail-open trap.
    #[test]
    fn a_verdict_for_another_pair_is_never_borrowed() {
        let other_consumer_key = {
            let mut verdict = stored_verdict(true);
            verdict.consumer_key = "http|POST|/v1/refunds".to_string();
            verdict
        };
        let other_producer_repo = {
            let mut verdict = stored_verdict(true);
            verdict.producer_repo = "ledger-api".to_string();
            verdict
        };
        let other_producer_key = {
            let mut verdict = stored_verdict(true);
            verdict.producer_key = "http|POST|/v1/refunds".to_string();
            verdict
        };
        let other_consumer_repo = {
            let mut verdict = stored_verdict(true);
            verdict.consumer_repo = "billing-sdk".to_string();
            verdict
        };

        for verdict in [
            other_consumer_key,
            other_producer_repo,
            other_producer_key,
            other_consumer_repo,
        ] {
            let joined = run(
                &[producer(), sdk_repo_storing(vec![verdict.clone()])],
                &[consumer_with(candidate("ledger.payments.create"))],
                &[unjudged_match()],
            );
            assert_eq!(
                joined.edges()[0].type_compatible,
                None,
                "borrowed a verdict from {verdict:?}"
            );
        }
    }

    /// No verdict anywhere: the edge stays `None` — "no verdict stored", never
    /// "compatible".
    #[test]
    fn no_verdict_anywhere_leaves_the_edge_unverified() {
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[unjudged_match()],
        );

        let edge = &joined.edges()[0];
        assert_eq!(edge.type_compatible, None);
        assert!(edge.mismatch_reason.is_none());
    }

    /// A shared-external-contract match is two call sites encoding the same
    /// externally-served contract; compat is never overlaid on one and the
    /// stored verdicts are keyed by producer identity, so no verdict is
    /// fabricated from it.
    #[test]
    fn a_shared_external_contract_edge_carries_no_verdict() {
        let mut shared = unjudged_match();
        shared.relationship = carrick_match::MatchRelationship::SharedExternalContract;
        let joined = run(
            &[producer(), sdk_repo()],
            &[consumer_with(candidate("ledger.payments.create"))],
            &[shared],
        );
        assert_eq!(joined.edges()[0].type_compatible, None);
    }

    #[test]
    fn only_incompatible_edges_become_findings() {
        let edge = |compatible: Option<bool>| SdkEdge {
            consumer_repo: "checkout".to_string(),
            consumer_location: "src/checkout.ts:42".to_string(),
            package: SDK_PACKAGE.to_string(),
            import_symbol: Some("default".to_string()),
            callee: "ledger.payments.create".to_string(),
            sdk_repo: "ledger-sdk".to_string(),
            sdk_member: "payments.create".to_string(),
            sdk_location: "src/resources/payments.ts:28".to_string(),
            producer_repo: "payments-api".to_string(),
            producer_key: PRODUCER_KEY.to_string(),
            type_compatible: compatible,
            mismatch_reason: (compatible == Some(false))
                .then(|| "Property 'amountCents' is missing".to_string()),
            scanner_version: "0.0.0-test".to_string(),
        };

        let findings = type_mismatch_findings(&[edge(Some(true)), edge(None), edge(Some(false))]);
        assert_eq!(findings.len(), 1);
        match &findings[0] {
            Finding::TypeMismatch {
                method,
                path,
                call_sites,
                producer_type,
                consumer_type,
                detail,
                ..
            } => {
                assert_eq!(method, "POST");
                assert_eq!(path, "/v1/payments");
                // The consumer's own call site is the actionable location.
                assert_eq!(call_sites, &["src/checkout.ts:42".to_string()]);
                assert_eq!(producer_type, "payments-api");
                assert_eq!(consumer_type, "@fixture/ledger-sdk (payments.create)");
                assert!(detail.contains("reached through"));
                assert!(detail.contains("src/resources/payments.ts:28"));
                assert!(detail.contains("Property 'amountCents' is missing"));
            }
            other => panic!("expected a type mismatch, got {other:?}"),
        }
    }

    /// A finding is a contract risk, and risks are what the headline counts —
    /// so an incompatible SDK edge reaches the PR verdict by the same route a
    /// direct incompatible pair does.
    #[test]
    fn an_sdk_finding_is_a_contract_risk() {
        let edge = SdkEdge {
            consumer_repo: "checkout".to_string(),
            consumer_location: "src/checkout.ts:42".to_string(),
            package: SDK_PACKAGE.to_string(),
            import_symbol: Some("default".to_string()),
            callee: "ledger.payments.create".to_string(),
            sdk_repo: "ledger-sdk".to_string(),
            sdk_member: "payments.create".to_string(),
            sdk_location: "src/resources/payments.ts:28".to_string(),
            producer_repo: "payments-api".to_string(),
            producer_key: PRODUCER_KEY.to_string(),
            type_compatible: Some(false),
            mismatch_reason: None,
            scanner_version: "0.0.0-test".to_string(),
        };
        let findings = type_mismatch_findings(&[edge]);
        assert_eq!(
            findings[0].severity(),
            crate::findings::Severity::Risk,
            "an SDK break must weigh the same as a direct one"
        );
    }

    #[test]
    fn key_labels_split_every_protocol_key() {
        assert_eq!(
            key_labels("http|POST|/v1/payments"),
            ("POST".to_string(), "/v1/payments".to_string())
        );
        assert_eq!(
            key_labels("graphql|query|orders"),
            ("query".to_string(), "orders".to_string())
        );
        // Pub/sub identity is the topic alone, so the protocol stands in.
        assert_eq!(
            key_labels("pubsub|orders.created"),
            ("pubsub".to_string(), "orders.created".to_string())
        );
    }

    #[test]
    fn member_chain_drops_the_root_binding() {
        assert_eq!(
            member_chain("ledger.payments.create").as_deref(),
            Some("payments.create")
        );
        assert_eq!(member_chain("ledger.scrape").as_deref(), Some("scrape"));
        // The root's own `()` is not read: what bound the receiver is already
        // answered by `import_symbol`.
        assert_eq!(
            member_chain("getLedger().scrape").as_deref(),
            Some("scrape")
        );
        assert_eq!(member_chain("ledger.transport().send"), None);
    }
}
