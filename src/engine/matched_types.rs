//! The compiler engine's type stage and judge over a given list of
//! call-to-server pairs.
//!
//! A model finds which call reaches which server code; this module reads the
//! types at both ends of each pair with the same type stage a scan runs, and
//! judges the pair with the one judge ([`type_compat_v2::judge_pairs`]). It
//! locates nothing: the caller of this module hands in each end's span, and
//! says which pairs go through the repository's own code.
//!
//! Only HTTP pairs are read, and only their response sides (the caller's
//! result and the server's response body). A pair of any other protocol
//! states `protocol_not_read` at both ends; a model-found type for such an end
//! is not this module's.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::type_compat_v2::{self, JudgeStub, LocalConsumer, LocalConsumers};
use crate::cloud_storage::{
    ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence, TypeManifestEntry,
};
use crate::operation::OperationKey;
use crate::services::TypeSidecar;
use crate::services::type_sidecar::{
    CaptureAliasRecord, ExtractionConfig, InferKind, InferRequestItem, ServerClaims, VerdictBucket,
    VerdictSide,
};
use crate::type_manifest::{build_manifest_type_alias_with_site_id, build_site_id};

/// Reason words this module states itself. Each is a key of
/// `src/sidecar/src/capture/type-reasons.json`, the one list of reasons;
/// [`state`] reads the list and panics on a word it does not hold.
const NOT_TYPESCRIPT_SOURCE: &str = "not_typescript_source";
const PROTOCOL_NOT_READ: &str = "protocol_not_read";
const NOT_LOCATED: &str = "not_located";
const BUDGET_EXHAUSTED: &str = "budget_exhausted";
const SITE_UNREADABLE: &str = "site_unreadable";
const NOT_RECORDED: &str = "not_recorded";

/// Why a pair's caller was typed and its verdict skipped when the located
/// call goes into the repository's own code and that code reshapes the
/// response before the caller reads it.
pub(crate) const OWN_CODE_SKIP: &str = "the call goes through the repository's own code, which reshapes the response before the caller reads it; the pair at that code's request line is the one judged";

/// Extensions the compiler engine reads as source.
const SOURCE_EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// One service of the repository: where its compiler program is rooted and
/// what the scan used to read its types.
pub(crate) struct MatchedService {
    pub service_id: String,
    /// The root the compiler program of this service is initialised at.
    pub root: PathBuf,
    /// The repository root every pair path is relative to.
    pub scan_root: PathBuf,
    pub tsconfig: Option<String>,
    pub extraction_config: Option<ExtractionConfig>,
    pub server_claims: Option<ServerClaims>,
}

/// One end of a pair: the service that owns its file and where it sits.
/// The caller of this module locates the end; this module never does.
pub(crate) struct MatchedEnd {
    pub service: String,
    /// Path relative to the repository root.
    pub file: String,
    /// 1-based.
    pub line: u32,
    /// The caller's call as the compiler's UTF-16 offsets `[start, end)`. A
    /// caller with none is `not_located`. Unused on a server end.
    pub span: Option<(u32, u32)>,
    /// A reason already decided for this end (for example `not_located`,
    /// `several_sites_on_line`): the end is not asked.
    pub reason: Option<&'static str>,
    /// Caller only: whether the repository's own code the located call goes
    /// into hands back the parsed body. `Some(false)` types the caller and
    /// skips the verdict. `None` and `Some(true)` are judged.
    pub hands_back_body: Option<bool>,
}

pub(crate) struct MatchedPair {
    pub pair_id: String,
    pub verb: String,
    pub operation: String,
    /// Whether the pair is an HTTP pair. Any other pair states
    /// `protocol_not_read` at both ends.
    pub http: bool,
    pub caller: MatchedEnd,
    pub server: MatchedEnd,
}

/// The type stored at one end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedType {
    pub alias: String,
    pub definition: String,
    pub expanded: String,
}

/// What was read at one end. A type, a reason, or both (a type that is not
/// concrete carries the reason it is not); never neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedEndTypes {
    pub service: String,
    pub alias: Option<String>,
    pub type_read: Option<MatchedType>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedVerdict {
    pub bucket: VerdictBucket,
    pub gate: Option<String>,
    pub diagnostic: Option<String>,
    pub resolved: bool,
    pub unresolved_reason: Option<String>,
    pub unresolved_side: Option<VerdictSide>,
    pub judged: bool,
    /// The sentence the pair was skipped with; the verdict was not run.
    pub skipped: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedPairTypes {
    pub pair_id: String,
    pub caller: MatchedEndTypes,
    pub server: MatchedEndTypes,
    /// `None` when an end has no alias to judge.
    pub verdict: Option<MatchedVerdict>,
}

/// The word's own row in the one list, read at the point of use: a word the
/// list does not hold is a broken build, never a silent default.
fn state(word: &'static str) -> String {
    static LIST: std::sync::OnceLock<BTreeMap<String, serde_json::Value>> =
        std::sync::OnceLock::new();
    let list = LIST.get_or_init(|| {
        serde_json::from_str(include_str!("../sidecar/src/capture/type-reasons.json"))
            .expect("src/sidecar/src/capture/type-reasons.json parses")
    });
    assert!(
        list.contains_key(word),
        "type-reasons.json holds no word `{word}`"
    );
    word.to_string()
}

fn is_compiler_source(file: &str) -> bool {
    if file.ends_with(".d.ts") || file.ends_with(".d.mts") || file.ends_with(".d.cts") {
        return false;
    }
    Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Caller,
    Server,
}

/// An end the pass asks the compiler about.
struct Ask {
    service: String,
    alias: String,
    entry: TypeManifestEntry,
    infer: InferRequestItem,
}

enum Plan {
    /// Nothing is asked: this is why.
    Stated(String),
    Ask(Box<Ask>),
}

fn key_of(pair: &MatchedPair) -> OperationKey {
    OperationKey::http(&pair.verb, &pair.operation)
}

fn plan_end(pair: &MatchedPair, role: Role, services: &HashMap<&str, &MatchedService>) -> Plan {
    let end = match role {
        Role::Caller => &pair.caller,
        Role::Server => &pair.server,
    };
    if !pair.http {
        return Plan::Stated(state(PROTOCOL_NOT_READ));
    }
    if let Some(reason) = end.reason {
        return Plan::Stated(state(reason));
    }
    if role == Role::Server && !is_compiler_source(&end.file) {
        return Plan::Stated(state(NOT_TYPESCRIPT_SOURCE));
    }
    if role == Role::Caller && end.span.is_none() {
        return Plan::Stated(state(NOT_LOCATED));
    }
    let Some(service) = services.get(end.service.as_str()) else {
        return Plan::Stated(state(NOT_LOCATED));
    };
    let key = key_of(pair);
    let scan_root = service.scan_root.to_string_lossy().into_owned();
    let site_id = build_site_id(&end.file, end.line, &key, &scan_root);
    let (manifest_role, infer_kind, span) = match role {
        Role::Caller => (ManifestRole::Consumer, InferKind::CallResult, end.span),
        Role::Server => (ManifestRole::Producer, InferKind::ResponseBody, None),
    };
    let alias = build_manifest_type_alias_with_site_id(
        &key,
        manifest_role,
        ManifestTypeKind::Response,
        Some(&site_id),
    );
    let absolute = absolute_path(&service.scan_root, &end.file);
    let infer = InferRequestItem {
        member: None,
        result: None,
        file_path: absolute,
        line_number: end.line,
        span_start: span.map(|(start, _)| start),
        span_end: span.map(|(_, end)| end),
        expression_text: None,
        expression_line: None,
        infer_kind: infer_kind.clone(),
        alias: Some(alias.clone()),
        param_name: None,
        route_path: None,
        at_registration: false,
        result_unanswered: false,
        through_call: None,
        handler_lines: None,
    };
    let entry = TypeManifestEntry {
        key,
        role: manifest_role,
        type_kind: ManifestTypeKind::Response,
        type_alias: alias.clone(),
        file_path: end.file.clone(),
        line_number: end.line,
        is_explicit: false,
        type_state: ManifestTypeState::Unknown,
        evidence: TypeEvidence {
            file_path: end.file.clone(),
            span_start: span.map(|(start, _)| start),
            span_end: span.map(|(_, end)| end),
            expression_text: None,
            expression_line: None,
            line_number: end.line,
            infer_kind,
            is_explicit: false,
            type_state: ManifestTypeState::Unknown,
            value_spans: Vec::new(),
        },
        resolved_definition: None,
        expanded_definition: None,
        primary_type_symbol: None,
        defined_in: None,
        any_provenance: Vec::new(),
        unwidened_definition: None,
        unwidened_out_of_budget: false,
        response_modes: None,
        stated_values: Default::default(),
        graphql_root: None,
        v1_before_demotion: None,
    };
    Plan::Ask(Box::new(Ask {
        service: end.service.clone(),
        alias,
        entry,
        infer,
    }))
}

fn absolute_path(scan_root: &Path, file: &str) -> String {
    let joined = scan_root.join(file);
    joined
        .canonicalize()
        .unwrap_or(joined)
        .to_string_lossy()
        .into_owned()
}

/// What one service's type stage left: its capture stub and its entries, with
/// the definitions and provenance the capture joined onto them.
struct ServiceTypes {
    artifact: crate::cloud_storage::CaptureStubArtifact,
    entries: Vec<TypeManifestEntry>,
}

/// Read the types of every asked end in one service with the scan's own type
/// stage, in the scan's order, calling each of its functions.
fn type_service(
    sidecar_path: &Path,
    service: &MatchedService,
    asks: &[&Ask],
    local_consumers: &mut LocalConsumers,
) -> Result<ServiceTypes, String> {
    let infer: Vec<InferRequestItem> = asks.iter().map(|ask| ask.infer.clone()).collect();
    let mut entries: Vec<TypeManifestEntry> = asks.iter().map(|ask| ask.entry.clone()).collect();

    let sidecar = TypeSidecar::spawn(sidecar_path).map_err(|e| e.to_string())?;
    sidecar.start_init(&service.root, service.tsconfig.as_deref());
    sidecar
        .wait_ready(crate::services::type_sidecar::ready_budget())
        .map_err(|e| e.to_string())?;
    let resolution = sidecar
        .resolve_all_types(
            &[],
            &infer,
            service.extraction_config.as_ref(),
            service.server_claims.as_ref(),
        )
        .map_err(|e| e.to_string())?;

    // The calls the retype judges, prescreened in the program that answered
    // the asks, before the capture restarts the process.
    let mut calls = type_compat_v2::consumer_call_locators(&infer);
    type_compat_v2::prescreen_calls(&sidecar, &service.service_id, &mut calls);
    local_consumers.insert(
        service.service_id.clone(),
        LocalConsumer {
            root: service.root.clone(),
            tsconfig: service.tsconfig.clone(),
            calls,
        },
    );

    let capture_root = service
        .root
        .canonicalize()
        .unwrap_or_else(|_| service.root.clone());
    let capture_root = capture_root.to_string_lossy().into_owned();
    let manifest_aliases: Vec<String> = entries
        .iter()
        .map(|entry| entry.type_alias.clone())
        .collect();
    let mut anchors = type_compat_v2::derive_capture_anchors(
        &[],
        &infer,
        &[],
        &resolution.inferred_types,
        &manifest_aliases,
        &capture_root,
    );
    type_compat_v2::state_unresolved_imports(&mut anchors, &HashSet::new());
    let backfill_texts = type_compat_v2::derive_backfill_texts(
        &resolution.explicit_manifest,
        &resolution.inferred_types,
        &[],
    );
    let (stub_dir, artifact) = type_compat_v2::run_capture(
        &sidecar,
        &capture_root,
        &service.service_id,
        &anchors,
        &backfill_texts,
        service.tsconfig.as_deref(),
    )
    .map_err(|failure| failure.to_string())?;

    let records: HashMap<String, CaptureAliasRecord> = super::read_capture_records(&stub_dir);
    let aliases = super::aliases_to_resolve(&entries, &records, &[]);
    let resolved = if aliases.is_empty() {
        Vec::new()
    } else {
        sidecar
            .resolve_definitions(&stub_dir.to_string_lossy(), &aliases, None)
            .unwrap_or_else(|e| {
                warn!("matched pairs: definition resolution failed: {e}");
                Vec::new()
            })
    };
    super::join_capture_answers(&mut entries, resolved, &records);
    let _ = std::fs::remove_dir_all(&stub_dir);
    Ok(ServiceTypes { artifact, entries })
}

/// The end's reading once its service's type stage has run.
fn read_end(service: &str, alias: &str, entries: &[TypeManifestEntry]) -> MatchedEndTypes {
    let Some(entry) = entries.iter().find(|entry| entry.type_alias == alias) else {
        return stated(service, Some(alias), state(SITE_UNREADABLE));
    };
    let cause = || {
        entry
            .any_provenance
            .iter()
            .find(|finding| finding.path.is_empty())
            .map(|finding| finding.reason.clone())
            .unwrap_or_else(|| state(NOT_RECORDED))
    };
    match (&entry.resolved_definition, &entry.expanded_definition) {
        (Some(definition), Some(expanded)) => MatchedEndTypes {
            service: service.to_string(),
            alias: Some(alias.to_string()),
            type_read: Some(MatchedType {
                alias: alias.to_string(),
                definition: definition.clone(),
                expanded: expanded.clone(),
            }),
            reason: type_compat_v2::contains_disqualifying_top_type(expanded).then(cause),
        },
        _ => stated(service, Some(alias), cause()),
    }
}

fn stated(service: &str, alias: Option<&str>, reason: String) -> MatchedEndTypes {
    MatchedEndTypes {
        service: service.to_string(),
        alias: alias.map(str::to_string),
        type_read: None,
        reason: Some(reason),
    }
}

/// Read both ends of every pair and judge the pairs whose ends are both read.
///
/// Services run one at a time, in the order the pairs name them. A service not
/// started once `ceiling` has passed gives every end it owns `budget_exhausted`.
/// The result has one row per input pair, in input order.
pub(crate) fn type_matched_pairs(
    sidecar_path: &Path,
    services: &[MatchedService],
    pairs: &[MatchedPair],
    ceiling: Duration,
) -> Vec<MatchedPairTypes> {
    let started = Instant::now();
    let by_id: HashMap<&str, &MatchedService> = services
        .iter()
        .map(|service| (service.service_id.as_str(), service))
        .collect();

    // What each end of each pair is asked, or why it is not.
    let plans: Vec<(Plan, Plan)> = pairs
        .iter()
        .map(|pair| {
            (
                plan_end(pair, Role::Caller, &by_id),
                plan_end(pair, Role::Server, &by_id),
            )
        })
        .collect();

    // Asks per service in first-named order, one per alias.
    let mut order: Vec<&str> = Vec::new();
    let mut asks_of: HashMap<&str, Vec<&Ask>> = HashMap::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (caller, server) in &plans {
        for plan in [caller, server] {
            if let Plan::Ask(ask) = plan {
                if !order.contains(&ask.service.as_str()) {
                    order.push(ask.service.as_str());
                }
                if seen.insert(ask.alias.as_str()) {
                    asks_of.entry(ask.service.as_str()).or_default().push(ask);
                }
            }
        }
    }

    let mut local_consumers: LocalConsumers = HashMap::new();
    let mut typed: HashMap<&str, Result<ServiceTypes, String>> = HashMap::new();
    for service_id in order {
        let service = by_id[service_id];
        let result = if started.elapsed() >= ceiling {
            Err(state(BUDGET_EXHAUSTED))
        } else {
            type_service(
                sidecar_path,
                service,
                &asks_of[service_id],
                &mut local_consumers,
            )
        };
        typed.insert(service_id, result);
    }

    let read = |plan: &Plan, fallback_service: &str| -> MatchedEndTypes {
        match plan {
            Plan::Stated(reason) => stated(fallback_service, None, reason.clone()),
            Plan::Ask(ask) => match &typed[ask.service.as_str()] {
                Ok(types) => read_end(&ask.service, &ask.alias, &types.entries),
                Err(reason) => {
                    debug!("matched pairs: {} could not be read: {reason}", ask.service);
                    let word = if reason == BUDGET_EXHAUSTED {
                        state(BUDGET_EXHAUSTED)
                    } else {
                        state(SITE_UNREADABLE)
                    };
                    stated(&ask.service, Some(&ask.alias), word)
                }
            },
        }
    };

    let mut rows: Vec<MatchedPairTypes> = pairs
        .iter()
        .zip(&plans)
        .map(|(pair, (caller_plan, server_plan))| MatchedPairTypes {
            pair_id: pair.pair_id.clone(),
            caller: read(caller_plan, &pair.caller.service),
            server: read(server_plan, &pair.server.service),
            verdict: None,
        })
        .collect();

    // The judge: only a pair with both ends asked and both services typed.
    let entry_of = |service: &str, alias: &str| -> Option<&TypeManifestEntry> {
        typed
            .get(service)?
            .as_ref()
            .ok()?
            .entries
            .iter()
            .find(|entry| entry.type_alias == alias)
    };
    let mut built = Vec::new();
    let mut row_of_key: HashMap<String, usize> = HashMap::new();
    for (index, ((pair, row), (caller_plan, server_plan))) in
        pairs.iter().zip(rows.iter_mut()).zip(&plans).enumerate()
    {
        let (Plan::Ask(caller), Plan::Ask(server)) = (caller_plan, server_plan) else {
            continue;
        };
        let (Some(consumer_entry), Some(producer_entry)) = (
            entry_of(&caller.service, &caller.alias),
            entry_of(&server.service, &server.alias),
        ) else {
            continue;
        };
        if pair.caller.hands_back_body == Some(false) {
            row.verdict = Some(MatchedVerdict {
                bucket: VerdictBucket::Unverifiable,
                gate: None,
                diagnostic: None,
                resolved: false,
                unresolved_reason: Some(OWN_CODE_SKIP.to_string()),
                unresolved_side: None,
                judged: false,
                skipped: Some(OWN_CODE_SKIP),
            });
            continue;
        }
        if let Some(built_pair) = type_compat_v2::build_matched_pair(
            &server.service,
            producer_entry,
            &caller.service,
            consumer_entry,
        ) {
            row_of_key.insert(built_pair.spec.pair_key.clone(), index);
            built.push(built_pair);
        }
    }
    if built.is_empty() {
        return rows;
    }

    let stubs: Vec<JudgeStub<'_>> = typed
        .iter()
        .filter_map(|(service_id, types)| {
            Some(JudgeStub {
                service_id,
                artifact: &types.as_ref().ok()?.artifact,
                graphql_types: None,
            })
        })
        .collect();
    let Some(first) = services.first() else {
        return rows;
    };
    let judge_sidecar = match TypeSidecar::spawn(sidecar_path) {
        Ok(sidecar) => sidecar,
        Err(e) => {
            warn!("matched pairs: the judge could not start: {e}");
            return rows;
        }
    };
    judge_sidecar.start_init(&first.root, first.tsconfig.as_deref());
    if let Err(e) = judge_sidecar.wait_ready(crate::services::type_sidecar::ready_budget()) {
        warn!("matched pairs: the judge was not ready: {e}");
        return rows;
    }
    let judged =
        type_compat_v2::judge_pairs(&judge_sidecar, &stubs, &built, &local_consumers, started);
    for outcome in judged.outcomes {
        let Some(&index) = row_of_key.get(&outcome.pair_key) else {
            continue;
        };
        rows[index].verdict = Some(MatchedVerdict {
            bucket: outcome.bucket,
            gate: outcome.gate.clone(),
            diagnostic: outcome.diagnostic.clone(),
            resolved: outcome.resolved,
            unresolved_reason: outcome.unresolved_reason.clone(),
            unresolved_side: judged.unresolved_sides.get(&outcome.pair_key).copied(),
            judged: outcome.judged,
            skipped: None,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[derive(serde::Deserialize)]
    struct FixtureEnd {
        service: String,
        file: String,
        line: u32,
        #[serde(default)]
        span: Option<(u32, u32)>,
        #[serde(default)]
        span_text: Option<String>,
        #[serde(default)]
        hands_back_body: Option<bool>,
    }

    #[derive(serde::Deserialize)]
    struct FixturePair {
        pair_id: String,
        protocol: String,
        verb: String,
        operation: String,
        caller: FixtureEnd,
        server: FixtureEnd,
    }

    #[derive(serde::Deserialize)]
    struct FixturePairs {
        pairs: Vec<FixturePair>,
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn end_of(end: &FixtureEnd) -> MatchedEnd {
        MatchedEnd {
            service: end.service.clone(),
            file: end.file.clone(),
            line: end.line,
            span: end.span,
            reason: None,
            hands_back_body: end.hands_back_body,
        }
    }

    /// Every pair of the fixture, read once and shared by the tests: the pass
    /// starts several compiler processes, so one run answers them all.
    fn all_rows() -> Option<&'static HashMap<String, MatchedPairTypes>> {
        static ROWS: std::sync::OnceLock<Option<HashMap<String, MatchedPairTypes>>> =
            std::sync::OnceLock::new();
        ROWS.get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
            if !sidecar_path.exists() {
                eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
                return None;
            }
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("matched-pairs");
            copy_tree(&manifest_dir.join("tests/fixtures/matched-pairs"), &root);
            let fixture: FixturePairs =
                serde_json::from_str(&std::fs::read_to_string(root.join("pairs.json")).unwrap())
                    .unwrap();
            for pair in &fixture.pairs {
                // The hand-filled spans still name the call they say they do.
                if let (Some((start, end)), Some(text)) =
                    (pair.caller.span, pair.caller.span_text.as_deref())
                {
                    let source = std::fs::read_to_string(root.join(&pair.caller.file)).unwrap();
                    assert_eq!(
                        &source[start as usize..end as usize],
                        text,
                        "{}",
                        pair.pair_id
                    );
                }
            }
            let services: Vec<MatchedService> = [("web", "apps/web"), ("api", "apps/api")]
                .iter()
                .map(|(name, directory)| MatchedService {
                    service_id: name.to_string(),
                    root: root.join(directory),
                    scan_root: root.clone(),
                    tsconfig: None,
                    extraction_config: None,
                    server_claims: None,
                })
                .collect();
            let pairs: Vec<MatchedPair> = fixture
                .pairs
                .iter()
                .map(|pair| MatchedPair {
                    pair_id: pair.pair_id.clone(),
                    verb: pair.verb.clone(),
                    operation: pair.operation.clone(),
                    http: pair.protocol == "http",
                    caller: end_of(&pair.caller),
                    server: end_of(&pair.server),
                })
                .collect();
            let rows =
                type_matched_pairs(&sidecar_path, &services, &pairs, Duration::from_secs(600));
            Some(
                rows.into_iter()
                    .map(|row| (row.pair_id.clone(), row))
                    .collect(),
            )
        })
        .as_ref()
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn matched_pair_with_both_ends_typed_is_judged() {
        let Some(rows) = all_rows() else { return };
        let ok = &rows["typed_ok"];
        let verdict = ok.verdict.as_ref().expect("the typed pair is judged");
        assert_eq!(verdict.bucket, VerdictBucket::Compatible, "{verdict:?}");
        let bad = &rows["mismatch"];
        let verdict = bad.verdict.as_ref().expect("the mismatch is judged");
        assert_eq!(verdict.bucket, VerdictBucket::Incompatible, "{verdict:?}");
        assert!(
            verdict.diagnostic.as_deref().unwrap_or("").contains("id"),
            "the diagnostic names the field: {verdict:?}"
        );
        assert!(bad.server.type_read.is_some(), "{:?}", bad.server);
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn untyped_caller_goes_to_the_retype() {
        let Some(rows) = all_rows() else { return };
        let row = &rows["untyped_generic"];
        let verdict = row.verdict.as_ref().expect("the retype judges it");
        assert_eq!(verdict.bucket, VerdictBucket::Incompatible, "{row:?}");
        assert!(
            verdict.diagnostic.as_deref().unwrap_or("").contains("name"),
            "the diagnostic names the missing field: {verdict:?}"
        );
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn sql_server_end_states_the_reason() {
        let Some(rows) = all_rows() else { return };
        let row = &rows["sql_server"];
        assert_eq!(row.server.reason.as_deref(), Some("not_typescript_source"));
        assert!(row.server.type_read.is_none());
        assert!(row.server.alias.is_none(), "no ask was made at the server");
        assert!(row.verdict.is_none());
        assert!(row.caller.type_read.is_some(), "{:?}", row.caller);
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn own_code_that_reshapes_is_not_judged() {
        let Some(rows) = all_rows() else { return };
        let row = &rows["own_wrapper"];
        assert!(row.caller.type_read.is_some(), "{:?}", row.caller);
        let verdict = row.verdict.as_ref().expect("a stated skip");
        assert_eq!(verdict.skipped, Some(OWN_CODE_SKIP));
        assert!(!verdict.judged);
    }

    #[test]
    #[serial(v2_capture_sidecar)]
    fn protocol_not_read_ends() {
        let Some(rows) = all_rows() else { return };
        let row = &rows["pubsub"];
        for end in [&row.caller, &row.server] {
            assert_eq!(end.reason.as_deref(), Some("protocol_not_read"));
            assert!(end.alias.is_none() && end.type_read.is_none());
        }
        assert!(row.verdict.is_none());
    }

    #[test]
    fn an_unknown_reason_word_is_a_broken_build() {
        let caught = std::panic::catch_unwind(|| state("no_such_word_in_the_list"));
        assert!(caught.is_err());
    }
}
