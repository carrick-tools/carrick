//! The compiler engine's type stage and judge over a given list of
//! call-to-server pairs.
//!
//! A model finds which call reaches which server code; this module reads the
//! types at both ends of each pair with the same type stage a scan runs, and
//! judges the pair with the one judge ([`type_compat_v2::judge_pairs`]). It
//! locates nothing: the caller of this module hands in each end's span, and
//! says which pairs go through the repository's own code.
//!
//! The caller of this module says which pairs are of a kind the pass asks
//! about (`MatchedPair::asked`); only those are read, and only their response
//! sides (the caller's result and, where the server end is asked, its
//! response body). Every other pair, whatever its protocol, states
//! `protocol_not_read` at both ends; a model-found type for such an end is
//! not this module's.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::type_compat_v2::{self, JudgeStub, LocalConsumer, LocalConsumers};
use crate::cloud_storage::{
    ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence, TypeManifestEntry,
};
use crate::graphql::{GraphqlRoot, GraphqlType, GraphqlTypes};
use crate::operation::{GraphqlOperationKind, OperationKey};
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

/// Why a pair with both ends read has no verdict. Each is stated on the pair
/// (`verdict_reason`); a pair with no verdict always carries one.
pub(crate) const ENDS_NOT_READ: &str = "an end of the pair was not read, so there are no two types to judge; the end's reason says why";
pub(crate) const PAIR_NOT_BUILT: &str =
    "the pair could not be built from the two types that were read, so it was not judged";
pub(crate) const JUDGE_COULD_NOT_START: &str =
    "the judge's compiler process could not be started, so the pair was not judged";
pub(crate) const JUDGE_NOT_READY: &str =
    "the judge's compiler process did not become ready in time, so the pair was not judged";
pub(crate) const JUDGE_NO_ANSWER: &str = "the judge returned no outcome for the pair";

/// Why a GraphQL pair whose ends were both read is not judged: judged without
/// the producer's schema types, a built-in scalar leaf is compared in its JSON
/// form, which reads a false mismatch (a resolver's `bigint` under a `String`
/// field), so the pair is judged only when the types hold the root field.
pub(crate) const GRAPHQL_NOT_JUDGED: &str = "the producer has no GraphQL schema types for this field, so the response was not judged: a field resolver can supply what the root resolver does not return";

/// Extensions the compiler engine reads as source.
const SOURCE_EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// One service of the repository: where its compiler program is rooted and
/// what the scan used to read its types.
pub(crate) struct MatchedService {
    pub service_id: String,
    /// The root the compiler program of this service is initialised at.
    pub root: PathBuf,
    /// The repository root every pair path is relative to, and the scan root
    /// the compiler engine is given, as the scan gives it.
    pub scan_root: PathBuf,
    pub tsconfig: Option<String>,
    pub extraction_config: Option<ExtractionConfig>,
    pub server_claims: Option<ServerClaims>,
    /// The service's GraphQL type table, as the scan stores it for the
    /// service (`graphql::graphql_types`); `None` when it has none. A GraphQL
    /// pair whose producer is this service is judged only when it holds the
    /// pair's root field.
    pub graphql_types: Option<GraphqlTypes>,
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
    /// skips the verdict. `None` means there is no own code between the call
    /// and the client (the call is the client's own), so the pair is judged,
    /// as it is for `Some(true)`.
    pub hands_back_body: Option<bool>,
}

/// The protocol a pair is read as. `Http` (the default) keeps the asks and
/// the key a route pair has always had.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum MatchedProtocol {
    #[default]
    Http,
    Graphql(GraphqlPair),
}

/// A GraphQL field pair: the operation it names and where each end is read.
/// The caller of this module locates both places, as the scan does (the
/// resolver's span from `graphql_resolver_anchor`, the declared field type's
/// span from the document reader); this module asks the compiler there and
/// locates nothing. An end with no place is `not_located`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphqlPair {
    pub kind: GraphqlOperationKind,
    /// The root field the pair is about.
    pub field: String,
    /// The schema's root type for `kind` (renamed or not). The service's
    /// type table does not say which type is a root, so the caller names it.
    pub root_type: String,
    /// Where the caller's result type is declared: the document's field
    /// type, as the scan asks it (`Expression`).
    pub caller: Option<ReadAt>,
    /// Where the resolver function is: the scan's producer ask
    /// (`FunctionReturn`).
    pub server: Option<ReadAt>,
}

/// A place a GraphQL end is read at, in the file's own numbering: the
/// compiler's UTF-16 offsets `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadAt {
    /// Path relative to the repository root; read only through [`repo_file`].
    pub file: String,
    /// 1-based.
    pub line: u32,
    pub span: (u32, u32),
}

pub(crate) struct MatchedPair {
    pub pair_id: String,
    pub verb: String,
    pub operation: String,
    pub protocol: MatchedProtocol,
    /// Whether the pair's kind is one the pass asks about. The caller of
    /// this module decides which kinds those are, and states a reason on an
    /// end it does not want asked. A pair that is not asked states
    /// `protocol_not_read` at both ends.
    pub asked: bool,
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
    /// The judge's verdict, or `None`: the pair was not judged, and
    /// `verdict_reason` says why. A skipped pair (own code) is `Some` with
    /// `skipped` set, never `None`.
    pub verdict: Option<MatchedVerdict>,
    /// Why `verdict` is `None`; `None` exactly when `verdict` is `Some`.
    pub verdict_reason: Option<&'static str>,
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
    match &pair.protocol {
        MatchedProtocol::Http => OperationKey::http(&pair.verb, &pair.operation),
        MatchedProtocol::Graphql(graphql) => {
            OperationKey::graphql(graphql.kind, graphql.field.clone())
        }
    }
}

/// Where an end is read and with what ask: an HTTP end at its own line (a
/// caller at its call's span, as `call_result`; a server by line, as
/// `response_body`), a GraphQL end at the place its pair names (a caller's
/// declared field type, as `expression`; a resolver, as `function_return`),
/// the two asks the scan makes for a GraphQL field. `Err` is the reason the
/// end is not asked.
struct ReadPlace {
    file: String,
    line: u32,
    span: Option<(u32, u32)>,
    infer_kind: InferKind,
}

fn read_place(pair: &MatchedPair, role: Role, end: &MatchedEnd) -> Result<ReadPlace, String> {
    if !is_compiler_source(&end.file) {
        return Err(state(NOT_TYPESCRIPT_SOURCE));
    }
    match (&pair.protocol, role) {
        (MatchedProtocol::Http, Role::Caller) => Ok(ReadPlace {
            file: end.file.clone(),
            line: end.line,
            span: Some(end.span.ok_or_else(|| state(NOT_LOCATED))?),
            infer_kind: InferKind::CallResult,
        }),
        (MatchedProtocol::Http, Role::Server) => Ok(ReadPlace {
            file: end.file.clone(),
            line: end.line,
            span: None,
            infer_kind: InferKind::ResponseBody,
        }),
        (MatchedProtocol::Graphql(graphql), role) => {
            let (at, infer_kind) = match role {
                Role::Caller => (&graphql.caller, InferKind::Expression),
                Role::Server => (&graphql.server, InferKind::FunctionReturn),
            };
            let at = at.as_ref().ok_or_else(|| state(NOT_LOCATED))?;
            if !is_compiler_source(&at.file) {
                return Err(state(NOT_TYPESCRIPT_SOURCE));
            }
            Ok(ReadPlace {
                file: at.file.clone(),
                line: at.line,
                span: Some(at.span),
                infer_kind,
            })
        }
    }
}

fn plan_end(pair: &MatchedPair, role: Role, services: &HashMap<&str, &MatchedService>) -> Plan {
    let end = match role {
        Role::Caller => &pair.caller,
        Role::Server => &pair.server,
    };
    if !pair.asked {
        return Plan::Stated(state(PROTOCOL_NOT_READ));
    }
    if let Some(reason) = end.reason {
        return Plan::Stated(state(reason));
    }
    let place = match read_place(pair, role, end) {
        Ok(place) => place,
        Err(reason) => return Plan::Stated(reason),
    };
    let Some(service) = services.get(end.service.as_str()) else {
        return Plan::Stated(state(NOT_LOCATED));
    };
    let key = key_of(pair);
    let scan_root = service.scan_root.to_string_lossy().into_owned();
    let site_id = build_site_id(&place.file, place.line, &key, &scan_root);
    let manifest_role = match role {
        Role::Caller => ManifestRole::Consumer,
        Role::Server => ManifestRole::Producer,
    };
    let (infer_kind, span) = (place.infer_kind, place.span);
    let graphql_root = match (&pair.protocol, role) {
        (MatchedProtocol::Graphql(graphql), Role::Server) => Some(GraphqlRoot {
            type_name: graphql.root_type.clone(),
            field: graphql.field.clone(),
        }),
        _ => None,
    };
    let alias = build_manifest_type_alias_with_site_id(
        &key,
        manifest_role,
        ManifestTypeKind::Response,
        Some(&site_id),
    );
    let Some(absolute) = repo_file(&service.scan_root, &place.file) else {
        return Plan::Stated(state(NOT_LOCATED));
    };
    let absolute = absolute.to_string_lossy().into_owned();
    let infer = InferRequestItem {
        member: None,
        result: None,
        file_path: absolute,
        line_number: place.line,
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
        file_path: place.file.clone(),
        line_number: place.line,
        is_explicit: false,
        type_state: ManifestTypeState::Unknown,
        evidence: TypeEvidence {
            file_path: place.file.clone(),
            span_start: span.map(|(start, _)| start),
            span_end: span.map(|(_, end)| end),
            expression_text: None,
            expression_line: None,
            line_number: place.line,
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
        graphql_root,
        v1_before_demotion: None,
    };
    Plan::Ask(Box::new(Ask {
        service: end.service.clone(),
        alias,
        entry,
        infer,
    }))
}

/// The file a pair names, as its absolute path inside the repository, or
/// `None` when the name could reach anything else. A pair's file names come
/// from a record written outside this process, and whatever file is asked
/// has its type text stored, so a name is read only when it is a plain
/// repository-relative path (no absolute path, no backslash or NUL, and no
/// empty, `.`, `..` or `.git` segment) whose real path, symlinks followed,
/// is under the repository's real root and not under a `.git` folder. This
/// is the one gate for a file name of the record: the pass's entry asks it
/// too (`checked_path` in `matched_pass.rs`).
pub(crate) fn repo_file(scan_root: &Path, file: &str) -> Option<PathBuf> {
    let name = Path::new(file);
    if name.is_absolute() || name.has_root() || file.contains(['\\', '\0']) {
        return None;
    }
    if file
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".." | ".git"))
    {
        return None;
    }
    let root = scan_root.canonicalize().ok()?;
    let path = root.join(name).canonicalize().ok()?;
    let inside = path.strip_prefix(&root).ok()?;
    if inside.components().any(|part| part.as_os_str() == ".git") {
        return None;
    }
    Some(path)
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
    // As the scan names it (`spawn_sidecar`, `main.rs`): the bound of the
    // search for a tsconfig above a service that has none of its own, in this
    // process's init and in the capture process it starts.
    sidecar.set_scan_root(&service.scan_root);
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

/// The stubs of the services that were typed, in service-id order: the order
/// feeds the stub folder names (a repeated name is suffixed in the order it is
/// met) and the judge's answer cache, so it must not depend on the map's.
///
/// Each stub carries its service's own schema types: the judge reads a GraphQL
/// response's built-in scalar leaves in the scalar's form from them.
fn ordered_stubs<'a>(
    typed: &'a HashMap<&'a str, Result<ServiceTypes, String>>,
    services: &HashMap<&str, &'a MatchedService>,
) -> Vec<JudgeStub<'a>> {
    let mut stubs: Vec<JudgeStub<'a>> = typed
        .iter()
        .filter_map(|(service_id, types)| {
            Some(JudgeStub {
                service_id,
                artifact: &types.as_ref().ok()?.artifact,
                graphql_types: services
                    .get(service_id)
                    .and_then(|service| service.graphql_types.as_ref()),
            })
        })
        .collect();
    stubs.sort_by(|a, b| a.service_id.cmp(b.service_id));
    stubs
}

/// Whether the producer's schema types hold the pair's root field: the root
/// type is an object or interface of the table and declares the field.
fn schema_holds_root(schema: Option<&GraphqlTypes>, graphql: &GraphqlPair) -> bool {
    matches!(
        schema.and_then(|types| types.get(&graphql.root_type)),
        Some(GraphqlType::Fields(fields)) if fields.contains_key(&graphql.field)
    )
}

/// The end's reading once its service's type stage has run.
fn read_end(service: &str, alias: &str, entries: &[TypeManifestEntry]) -> MatchedEndTypes {
    let Some(entry) = entries.iter().find(|entry| entry.type_alias == alias) else {
        return stated(service, Some(alias), state(SITE_UNREADABLE));
    };
    let cause = || nested_cause(&entry.any_provenance);
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

/// The capture's word for why a read type is not wholly concrete: its cause
/// at the type's own position; else the first position inside it whose top
/// type the source does not declare itself (an import that did not resolve,
/// say); else `declared`, when every top type inside it is one the source
/// writes (a member declared `unknown`). `not_recorded` only when the capture
/// recorded no position at all.
fn nested_cause(findings: &[crate::services::type_sidecar::TypeProvenance]) -> String {
    const DECLARED: &str = "declared";
    findings
        .iter()
        .find(|finding| finding.path.is_empty())
        .or_else(|| findings.iter().find(|finding| finding.reason != DECLARED))
        .or_else(|| findings.first())
        .map(|finding| finding.reason.clone())
        .unwrap_or_else(|| state(NOT_RECORDED))
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
/// Services run one at a time, in the order the pairs name them. `ceiling` is
/// checked before each service starts: a service not started once it has passed
/// gives every end it owns `budget_exhausted`. It is not a bound on the whole
/// run: a service that has started runs to its end, and the judge runs after
/// the last service whatever the time.
/// The result has one row per input pair, in input order. A row's `verdict` is
/// `None` only with a `verdict_reason`.
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
            verdict_reason: Some(ENDS_NOT_READ),
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
            row.verdict_reason = None;
            continue;
        }
        // A GraphQL pair is judged with the producer's schema types or not at
        // all: both types are stored either way.
        let schema = by_id
            .get(server.service.as_str())
            .and_then(|service| service.graphql_types.as_ref());
        if let MatchedProtocol::Graphql(graphql) = &pair.protocol
            && !schema_holds_root(schema, graphql)
        {
            row.verdict = Some(MatchedVerdict {
                bucket: VerdictBucket::Unverifiable,
                gate: None,
                diagnostic: None,
                resolved: false,
                unresolved_reason: Some(GRAPHQL_NOT_JUDGED.to_string()),
                unresolved_side: None,
                judged: false,
                skipped: Some(GRAPHQL_NOT_JUDGED),
            });
            row.verdict_reason = None;
            continue;
        }
        match type_compat_v2::build_matched_pair(
            &server.service,
            producer_entry,
            &caller.service,
            consumer_entry,
            schema,
        ) {
            Some(built_pair) => {
                row_of_key.insert(built_pair.spec.pair_key.clone(), index);
                built.push(built_pair);
            }
            None => row.verdict_reason = Some(PAIR_NOT_BUILT),
        }
    }
    // A pair that reached the judge and has no verdict says why not.
    let set_reason = |reason: &'static str, rows: &mut [MatchedPairTypes]| {
        for &index in row_of_key.values() {
            rows[index].verdict_reason = Some(reason);
        }
    };
    if built.is_empty() {
        return rows;
    }

    let stubs = ordered_stubs(&typed, &by_id);
    let Some(first) = services.first() else {
        return rows;
    };
    let judge_sidecar = match TypeSidecar::spawn(sidecar_path) {
        Ok(sidecar) => sidecar,
        Err(e) => {
            warn!("matched pairs: the judge could not start: {e}");
            set_reason(JUDGE_COULD_NOT_START, &mut rows);
            return rows;
        }
    };
    // The retype re-initialises this process at each consumer's root, so it
    // carries the scan root as each service's own process did.
    judge_sidecar.set_scan_root(&first.scan_root);
    judge_sidecar.start_init(&first.root, first.tsconfig.as_deref());
    if let Err(e) = judge_sidecar.wait_ready(crate::services::type_sidecar::ready_budget()) {
        warn!("matched pairs: the judge was not ready: {e}");
        set_reason(JUDGE_NOT_READY, &mut rows);
        return rows;
    }
    set_reason(JUDGE_NO_ANSWER, &mut rows);
    let judged =
        type_compat_v2::judge_pairs(&judge_sidecar, &stubs, &built, &local_consumers, started);
    for outcome in judged.outcomes {
        let Some(&index) = row_of_key.get(&outcome.pair_key) else {
            continue;
        };
        rows[index].verdict_reason = None;
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

    /// The rows of one run, in the order the pairs were given.
    struct Run {
        ids: Vec<String>,
        by_id: HashMap<String, MatchedPairTypes>,
        fixture_ids: Vec<String>,
    }

    impl std::ops::Index<&str> for Run {
        type Output = MatchedPairTypes;
        fn index(&self, id: &str) -> &MatchedPairTypes {
            &self.by_id[id]
        }
    }

    /// Whether the built sidecar is there. A local run without the build skips
    /// the test; in CI a missing build is a failure, because a skip there
    /// would make a green tick mean nothing (the convention of the
    /// integration tests: `CI` set means the build must exist).
    fn sidecar_built(sidecar_path: &Path) -> bool {
        let built = sidecar_path.exists();
        assert!(
            built || std::env::var("CI").is_err(),
            "the built sidecar is missing in CI, so these tests would silently not run \
             (cd src/sidecar && npm run build)"
        );
        if !built {
            eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        }
        built
    }

    /// Every pair of the fixture, read once and shared by the tests: the pass
    /// starts several compiler processes, so one run answers them all.
    fn all_rows() -> Option<&'static Run> {
        static ROWS: std::sync::OnceLock<Option<Run>> = std::sync::OnceLock::new();
        ROWS.get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
            if !sidecar_built(&sidecar_path) {
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
                    graphql_types: None,
                })
                .collect();
            let pairs: Vec<MatchedPair> = fixture
                .pairs
                .iter()
                .map(|pair| MatchedPair {
                    pair_id: pair.pair_id.clone(),
                    verb: pair.verb.clone(),
                    operation: pair.operation.clone(),
                    protocol: MatchedProtocol::Http,
                    asked: pair.protocol == "http",
                    caller: end_of(&pair.caller),
                    server: end_of(&pair.server),
                })
                .collect();
            let rows =
                type_matched_pairs(&sidecar_path, &services, &pairs, Duration::from_secs(600));
            Some(Run {
                ids: rows.iter().map(|row| row.pair_id.clone()).collect(),
                fixture_ids: fixture
                    .pairs
                    .iter()
                    .map(|pair| pair.pair_id.clone())
                    .collect(),
                by_id: rows
                    .into_iter()
                    .map(|row| (row.pair_id.clone(), row))
                    .collect(),
            })
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
    #[serial(v2_capture_sidecar)]
    fn rows_come_back_in_input_order_and_a_missing_verdict_has_its_reason() {
        let Some(rows) = all_rows() else { return };
        assert_eq!(rows.ids, rows.fixture_ids);
        for row in rows.by_id.values() {
            assert_eq!(
                row.verdict.is_none(),
                row.verdict_reason.is_some(),
                "{}: a verdict is missing only with its reason",
                row.pair_id
            );
        }
        assert_eq!(rows["sql_server"].verdict_reason, Some(ENDS_NOT_READ));
        assert_eq!(rows["pubsub"].verdict_reason, Some(ENDS_NOT_READ));
    }

    fn stated_end(service: &str, file: &str, reason: Option<&'static str>) -> MatchedEnd {
        MatchedEnd {
            service: service.to_string(),
            file: file.to_string(),
            line: 1,
            span: Some((0, 1)),
            reason,
            hands_back_body: None,
        }
    }

    fn stated_pair(id: &str, caller: MatchedEnd, server: MatchedEnd) -> MatchedPair {
        MatchedPair {
            pair_id: id.to_string(),
            verb: "GET".to_string(),
            operation: "/x".to_string(),
            protocol: MatchedProtocol::Http,
            asked: true,
            caller,
            server,
        }
    }

    /// Nothing in these pairs is asked of the compiler, so no sidecar is
    /// needed: the path below is never spawned.
    fn run_stated(pairs: &[MatchedPair]) -> Vec<MatchedPairTypes> {
        type_matched_pairs(
            Path::new("/nonexistent/sidecar/index.js"),
            &[],
            pairs,
            Duration::from_secs(1),
        )
    }

    #[test]
    fn an_empty_pair_list_gives_an_empty_answer() {
        assert!(run_stated(&[]).is_empty());
    }

    #[test]
    fn a_pair_naming_an_unknown_service_states_not_located() {
        let rows = run_stated(&[stated_pair(
            "p",
            stated_end("nope", "a.ts", None),
            stated_end("nowhere", "b.ts", None),
        )]);
        for end in [&rows[0].caller, &rows[0].server] {
            assert_eq!(end.reason.as_deref(), Some("not_located"));
            assert!(end.type_read.is_none() && end.alias.is_none());
        }
        assert!(rows[0].verdict.is_none());
        assert_eq!(rows[0].verdict_reason, Some(ENDS_NOT_READ));
    }

    #[test]
    fn a_declaration_file_server_end_and_a_caller_with_a_reason_state_theirs() {
        let rows = run_stated(&[stated_pair(
            "p",
            stated_end("web", "a.ts", Some("several_sites_on_line")),
            stated_end("api", "types/api.d.ts", None),
        )]);
        assert_eq!(
            rows[0].caller.reason.as_deref(),
            Some("several_sites_on_line")
        );
        assert_eq!(
            rows[0].server.reason.as_deref(),
            Some("not_typescript_source")
        );
        assert!(rows[0].verdict.is_none() && rows[0].verdict_reason.is_some());
    }

    #[test]
    fn a_caller_in_a_file_that_is_not_source_states_not_typescript_source() {
        let rows = run_stated(&[
            stated_pair(
                "sql",
                stated_end("web", "db/001.sql", None),
                stated_end("api", "b.ts", Some("not_located")),
            ),
            stated_pair(
                "decl",
                stated_end("web", "types/web.d.ts", None),
                stated_end("api", "b.ts", Some("not_located")),
            ),
        ]);
        for row in &rows {
            assert_eq!(
                row.caller.reason.as_deref(),
                Some("not_typescript_source"),
                "{}",
                row.pair_id
            );
        }
    }

    #[test]
    fn output_order_is_input_order() {
        let ids = ["z", "a", "m", "b", "y"];
        let pairs: Vec<MatchedPair> = ids
            .iter()
            .map(|id| {
                stated_pair(
                    id,
                    stated_end("nope", "a.ts", None),
                    stated_end("nope", "b.ts", None),
                )
            })
            .collect();
        let rows = run_stated(&pairs);
        let got: Vec<&str> = rows.iter().map(|row| row.pair_id.as_str()).collect();
        assert_eq!(got, ids);
    }

    #[test]
    fn stub_inputs_are_the_same_on_every_run() {
        use crate::cloud_storage::{CAPTURE_ARTIFACT_VERSION, CaptureStubArtifact};
        // `a/b` and `a_b` sanitize to one folder name, so the order decides
        // which gets the suffix.
        let names = ["web", "a/b", "api", "a_b", "worker", "zeta", "alpha"];
        let artifact = || CaptureStubArtifact {
            artifact_version: CAPTURE_ARTIFACT_VERSION,
            package_name: "@carrick/test".to_string(),
            ts_version: "5.8.3".to_string(),
            bare_checkout: true,
            files: BTreeMap::new(),
        };
        let inputs = |order: &[&str]| -> Vec<(String, String)> {
            let typed: HashMap<&str, Result<ServiceTypes, String>> = order
                .iter()
                .map(|name| {
                    (
                        *name,
                        Ok(ServiceTypes {
                            artifact: artifact(),
                            entries: Vec::new(),
                        }),
                    )
                })
                .collect();
            let dir = tempfile::tempdir().unwrap();
            type_compat_v2::materialize_stubs(&ordered_stubs(&typed, &HashMap::new()), dir.path())
                .into_iter()
                .map(|stub| {
                    let folder = Path::new(&stub.stub_dir)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    (stub.service_name, folder)
                })
                .collect()
        };
        let first = inputs(&names);
        let mut sorted = names.to_vec();
        sorted.sort();
        assert_eq!(
            first
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            sorted
        );
        for round in 0..40 {
            let mut shuffled = names.to_vec();
            shuffled.rotate_left(round % names.len());
            assert_eq!(inputs(&shuffled), first, "round {round}");
        }
    }

    /// The callers of `tests/fixtures/matched-pairs-nested`, read once: a
    /// service with no tsconfig of its own, whose only tsconfig sits above it
    /// with the `paths` alias its callers' import needs.
    fn nested_rows() -> Option<&'static HashMap<String, MatchedPairTypes>> {
        static ROWS: std::sync::OnceLock<Option<HashMap<String, MatchedPairTypes>>> =
            std::sync::OnceLock::new();
        ROWS.get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let sidecar_path = manifest_dir.join("src/sidecar/dist/src/index.js");
            if !sidecar_built(&sidecar_path) {
                return None;
            }
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("matched-pairs-nested");
            copy_tree(
                &manifest_dir.join("tests/fixtures/matched-pairs-nested"),
                &root,
            );
            let file = "apps/portal/ui/web/src/page.ts";
            let source = std::fs::read_to_string(root.join(file)).unwrap();
            let pair = |id: &str, call: &str, operation: &str| {
                let start = source.find(call).expect("the fixture holds the call");
                MatchedPair {
                    pair_id: id.to_string(),
                    verb: "GET".to_string(),
                    operation: operation.to_string(),
                    protocol: MatchedProtocol::Http,
                    asked: true,
                    caller: MatchedEnd {
                        service: "web".to_string(),
                        file: file.to_string(),
                        line: source[..start].matches('\n').count() as u32 + 1,
                        span: Some((start as u32, (start + call.len()) as u32)),
                        reason: None,
                        hands_back_body: None,
                    },
                    server: MatchedEnd {
                        service: "web".to_string(),
                        file: "apps/portal/shared/widgets.ts".to_string(),
                        line: 1,
                        span: None,
                        reason: Some(NOT_LOCATED),
                        hands_back_body: None,
                    },
                }
            };
            let pairs = [
                pair("widget", "readWidget(id)", "/widgets/:id"),
                pair("gadget", "readGadget(id)", "/gadgets/:id"),
            ];
            let services = [MatchedService {
                service_id: "web".to_string(),
                root: root.join("apps/portal/ui/web"),
                scan_root: root.clone(),
                tsconfig: None,
                extraction_config: None,
                server_claims: None,
                graphql_types: None,
            }];
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

    /// The scan finds a tsconfig above a service because it names the scan
    /// root (`set_scan_root`); the pass must too, or the import does not
    /// resolve and the caller has no type.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_service_whose_tsconfig_sits_above_it_reads_its_caller_type() {
        let Some(rows) = nested_rows() else { return };
        let caller = &rows["widget"].caller;
        assert_eq!(caller.reason, None, "{caller:?}");
        let read = caller.type_read.as_ref().expect("the caller is typed");
        assert!(
            read.expanded.contains("label: string"),
            "the type is the shared Widget: {read:?}"
        );
    }

    /// A type with one member the source declares `unknown` is read, and the
    /// reason beside it is the capture's word for that member, not a word
    /// saying no cause was recorded.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn a_member_declared_unknown_keeps_the_type_and_states_the_captures_reason() {
        let Some(rows) = nested_rows() else { return };
        let caller = &rows["gadget"].caller;
        let read = caller.type_read.as_ref().expect("the caller is typed");
        assert!(read.expanded.contains("id: string"), "{read:?}");
        assert_eq!(caller.reason.as_deref(), Some("declared"), "{caller:?}");
    }

    /// A repository with one source file, and a file outside it that a
    /// symlink inside points at.
    fn guarded_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(repo.join("apps/web/src")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(repo.join("apps/web/src/a.ts"), "export const a = 1;\n").unwrap();
        let secret = outside.join("secret.ts");
        std::fs::write(&secret, "export const secret = 'kept out';\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, repo.join("apps/web/src/out.ts")).unwrap();
        (dir, repo, secret)
    }

    /// The pair's caller names `file`; the result is that caller's reading.
    /// The sidecar path does not exist, so an end that is asked reads
    /// `site_unreadable` with an alias, and one that is refused reads
    /// `not_located` with none.
    fn caller_named(repo: &Path, file: &str) -> MatchedEndTypes {
        let services = [MatchedService {
            service_id: "web".to_string(),
            root: repo.join("apps/web"),
            scan_root: repo.to_path_buf(),
            tsconfig: None,
            extraction_config: None,
            server_claims: None,
            graphql_types: None,
        }];
        let pair = stated_pair(
            "p",
            stated_end("web", file, None),
            stated_end("web", "apps/web/src/a.ts", Some("not_located")),
        );
        type_matched_pairs(
            Path::new("/nonexistent/sidecar/index.js"),
            &services,
            &[pair],
            Duration::from_secs(1),
        )
        .remove(0)
        .caller
    }

    fn assert_refused(end: &MatchedEndTypes, file: &str) {
        assert_eq!(
            end.reason.as_deref(),
            Some("not_located"),
            "{file}: {end:?}"
        );
        assert!(
            end.alias.is_none() && end.type_read.is_none(),
            "{file} was asked: {end:?}"
        );
    }

    #[test]
    fn an_absolute_file_name_is_not_asked() {
        let (_dir, repo, secret) = guarded_repo();
        let file = secret.to_string_lossy().into_owned();
        assert_refused(&caller_named(&repo, &file), &file);
        // Absolute and inside the repository is still not a repository name.
        let inside = repo
            .join("apps/web/src/a.ts")
            .to_string_lossy()
            .into_owned();
        assert_refused(&caller_named(&repo, &inside), &inside);
    }

    #[test]
    fn a_file_name_with_a_parent_segment_is_not_asked() {
        let (_dir, repo, _secret) = guarded_repo();
        for file in [
            "../outside/secret.ts",
            "apps/web/../../outside/secret.ts",
            "apps/web/../web/src/a.ts",
            "./apps/web/src/a.ts",
            "apps//web/src/a.ts",
            ".git/hooks/a.ts",
        ] {
            assert_refused(&caller_named(&repo, file), file);
        }
        // The plain name of the same file is asked.
        let asked = caller_named(&repo, "apps/web/src/a.ts");
        assert!(asked.alias.is_some(), "{asked:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_repository_is_not_asked() {
        let (_dir, repo, _secret) = guarded_repo();
        assert_refused(
            &caller_named(&repo, "apps/web/src/out.ts"),
            "apps/web/src/out.ts",
        );
    }

    /// The GraphQL fixture: a schema-first service and a client that executes
    /// typed documents, plus one route pair. Spans are found by text, except
    /// the resolver's, which is the scan's own reader's (`resolver_anchor`).
    struct GraphqlFixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        pairs: Vec<MatchedPair>,
        schema: GraphqlTypes,
    }

    fn line_of(source: &str, offset: usize) -> u32 {
        source[..offset].matches('\n').count() as u32 + 1
    }

    fn graphql_fixture() -> GraphqlFixture {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("matched-pairs-graphql");
        copy_tree(
            &manifest_dir.join("tests/fixtures/matched-pairs-graphql"),
            &root,
        );
        let read = |file: &str| std::fs::read_to_string(root.join(file)).unwrap();

        // The producer's schema types and root type names, as the scan reads
        // them from its SDL.
        let extraction = crate::graphql::extract_from_document_text(
            &read("apps/api/src/schema.graphql"),
            Path::new("apps/api/src/schema.graphql"),
            1,
        );
        let schema = crate::graphql::graphql_types(&extraction.schema_types).unwrap();
        let root_type = |kind: GraphqlOperationKind| -> String {
            extraction
                .schema_types
                .roots
                .iter()
                .find(|(_, root_kind)| **root_kind == kind)
                .map(|(name, _)| name.clone())
                .unwrap()
        };

        // The declared field type of a document's result: the text after
        // `property?: ` up to the end of its type statement.
        let documents = "apps/web/src/documents.ts";
        let documents_source = read(documents);
        let field_type = |property: &str, document_type: &str| -> ReadAt {
            let declared = documents_source
                .find(&format!("export type {document_type} = {{"))
                .unwrap();
            let marker = format!("  {property}?: ");
            let anchor = declared + documents_source[declared..].find(&marker).unwrap();
            let start = anchor + marker.len();
            let end = start + documents_source[start..].find(";\n};").unwrap();
            ReadAt {
                file: documents.to_string(),
                line: line_of(&documents_source, anchor),
                span: (start as u32, end as u32),
            }
        };

        // The resolver function of the field declared at `field_text`.
        let resolvers = "apps/api/src/graphql.ts";
        let resolvers_source = read(resolvers);
        let resolver = |field_text: &str| -> ReadAt {
            let line = line_of(
                &resolvers_source,
                resolvers_source.find(field_text).unwrap(),
            );
            let crate::graphql_resolver_anchor::ResolverAnchor::Function { lo, hi } =
                crate::graphql_resolver_anchor::resolver_anchor(
                    Path::new(resolvers),
                    &resolvers_source,
                    line,
                )
            else {
                panic!("the fixture's resolver is a function literal");
            };
            ReadAt {
                file: resolvers.to_string(),
                line,
                span: (
                    crate::utils::sidecar_position(&resolvers_source, lo),
                    crate::utils::sidecar_position(&resolvers_source, hi),
                ),
            }
        };

        let widgets = "apps/web/src/widgets.ts";
        let widgets_source = read(widgets);
        let graphql_pair = |id: &str,
                            kind: GraphqlOperationKind,
                            field: &str,
                            call: &str,
                            caller: ReadAt,
                            server: ReadAt| {
            let call_line = line_of(&widgets_source, widgets_source.find(call).unwrap());
            MatchedPair {
                pair_id: id.to_string(),
                // The record's own words: neither names the root field.
                verb: "QUERY".to_string(),
                operation: "SomeDocument".to_string(),
                protocol: MatchedProtocol::Graphql(GraphqlPair {
                    kind,
                    field: field.to_string(),
                    root_type: root_type(kind),
                    caller: Some(caller),
                    server: Some(server.clone()),
                }),
                asked: true,
                caller: MatchedEnd {
                    service: "web".to_string(),
                    file: widgets.to_string(),
                    line: call_line,
                    span: None,
                    reason: None,
                    hands_back_body: None,
                },
                server: MatchedEnd {
                    service: "api".to_string(),
                    file: resolvers.to_string(),
                    line: server.line,
                    span: None,
                    reason: None,
                    hands_back_body: None,
                },
            }
        };

        let client = "apps/web/src/client.ts";
        let client_source = read(client);
        let call = "(await fetch('/g/1')).json()";
        let start = client_source.find(call).unwrap();
        let http = MatchedPair {
            pair_id: "http_ok".to_string(),
            verb: "GET".to_string(),
            operation: "/g/:id".to_string(),
            protocol: MatchedProtocol::Http,
            asked: true,
            caller: MatchedEnd {
                service: "web".to_string(),
                file: client.to_string(),
                line: line_of(&client_source, start),
                span: Some((start as u32, (start + call.len()) as u32)),
                reason: None,
                hands_back_body: None,
            },
            server: MatchedEnd {
                service: "api".to_string(),
                file: "apps/api/src/server.ts".to_string(),
                line: 6,
                span: None,
                reason: None,
                hands_back_body: None,
            },
        };
        let query = GraphqlOperationKind::Query;
        let pairs = vec![
            graphql_pair(
                "gql_ok",
                query,
                "widget",
                "execute(WidgetDocument)",
                field_type("widget", "WidgetQuery"),
                resolver("widget: {"),
            ),
            graphql_pair(
                "gql_mismatch",
                GraphqlOperationKind::Mutation,
                "renameWidget",
                "execute(RenameWidgetDocument",
                field_type("renameWidget", "RenameWidgetMutation"),
                resolver("renameWidget: {"),
            ),
            graphql_pair(
                "gql_declared",
                query,
                "widget",
                "execute(WidgetWithMetaDocument)",
                field_type("widget", "WidgetWithMetaQuery"),
                resolver("widget: {"),
            ),
            http,
        ];
        GraphqlFixture {
            _dir: dir,
            root,
            pairs,
            schema,
        }
    }

    /// One run of the GraphQL fixture's pairs (`only_http` keeps the route
    /// pair alone), with the api service's schema types or none.
    fn graphql_rows(
        with_schema: bool,
        only_http: bool,
    ) -> Option<HashMap<String, MatchedPairTypes>> {
        let sidecar_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
        if !sidecar_built(&sidecar_path) {
            return None;
        }
        let fixture = graphql_fixture();
        let services: Vec<MatchedService> = [("web", "apps/web"), ("api", "apps/api")]
            .iter()
            .map(|(name, directory)| MatchedService {
                service_id: name.to_string(),
                root: fixture.root.join(directory),
                scan_root: fixture.root.clone(),
                tsconfig: None,
                extraction_config: None,
                server_claims: None,
                graphql_types: (with_schema && *name == "api").then(|| fixture.schema.clone()),
            })
            .collect();
        let pairs: Vec<MatchedPair> = fixture
            .pairs
            .into_iter()
            .filter(|pair| !only_http || pair.protocol == MatchedProtocol::Http)
            .collect();
        let rows = type_matched_pairs(&sidecar_path, &services, &pairs, Duration::from_secs(600));
        Some(
            rows.into_iter()
                .map(|row| (row.pair_id.clone(), row))
                .collect(),
        )
    }

    fn graphql_with_schema() -> Option<&'static HashMap<String, MatchedPairTypes>> {
        static ROWS: std::sync::OnceLock<Option<HashMap<String, MatchedPairTypes>>> =
            std::sync::OnceLock::new();
        ROWS.get_or_init(|| graphql_rows(true, false)).as_ref()
    }

    fn graphql_without_schema() -> Option<&'static HashMap<String, MatchedPairTypes>> {
        static ROWS: std::sync::OnceLock<Option<HashMap<String, MatchedPairTypes>>> =
            std::sync::OnceLock::new();
        ROWS.get_or_init(|| graphql_rows(false, false)).as_ref()
    }

    /// Both ends of a field pair are read where the pair names them, and the
    /// resolver's imported named type is resolved through to its members.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn graphql_pair_reads_the_resolver_and_the_declared_field() {
        let Some(rows) = graphql_with_schema() else {
            return;
        };
        let row = &rows["gql_ok"];
        let server = row.server.type_read.as_ref().expect("the resolver is read");
        assert!(
            server.expanded.contains("total: number") && server.expanded.contains("name: string"),
            "the imported Owner is resolved: {server:?}"
        );
        assert_eq!(row.server.reason, None, "{:?}", row.server);
        let caller = row
            .caller
            .type_read
            .as_ref()
            .expect("the declared field is read");
        assert!(caller.expanded.contains("total"), "{caller:?}");
        assert_eq!(row.caller.reason, None, "{:?}", row.caller);
        // A member the document declares `unknown` states the capture's word.
        let declared = &rows["gql_declared"].caller;
        assert!(declared.type_read.is_some(), "{declared:?}");
        assert_eq!(declared.reason.as_deref(), Some("declared"), "{declared:?}");
    }

    /// With the producer's schema types the pair is judged: a resolver's
    /// `number` under an `ID!` field reads as the client's `string`, and a
    /// client that reads an `Int!` field as a `string` is a mismatch (what a
    /// resolver leaves out or converts is the schema's to state, not the
    /// resolver's).
    #[test]
    #[serial(v2_capture_sidecar)]
    fn graphql_pair_with_schema_types_is_judged() {
        let Some(rows) = graphql_with_schema() else {
            return;
        };
        let ok = rows["gql_ok"].verdict.as_ref().expect("judged");
        assert_eq!(ok.bucket, VerdictBucket::Compatible, "{ok:?}");
        assert!(ok.judged && ok.skipped.is_none(), "{ok:?}");
        let bad = rows["gql_mismatch"].verdict.as_ref().expect("judged");
        assert_eq!(bad.bucket, VerdictBucket::Incompatible, "{bad:?}");
        assert!(
            bad.diagnostic.as_deref().unwrap_or("").contains("total"),
            "the diagnostic names the field: {bad:?}"
        );
    }

    /// Without schema types both types are stored and the verdict is skipped
    /// with its sentence (RC20): never a verdict from the JSON form.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn graphql_pair_without_schema_types_is_not_judged() {
        let Some(rows) = graphql_without_schema() else {
            return;
        };
        for id in ["gql_ok", "gql_mismatch"] {
            let row = &rows[id];
            assert!(row.caller.type_read.is_some(), "{id}: {:?}", row.caller);
            assert!(row.server.type_read.is_some(), "{id}: {:?}", row.server);
            let verdict = row.verdict.as_ref().expect("a stated skip");
            assert_eq!(verdict.skipped, Some(GRAPHQL_NOT_JUDGED), "{id}");
            assert!(!verdict.judged, "{id}");
            assert_eq!(row.verdict_reason, None, "{id}");
        }
    }

    /// The route pair in the run reads what it reads alone, with or without
    /// the service's schema types: the GraphQL path never touches it.
    #[test]
    #[serial(v2_capture_sidecar)]
    fn an_http_pair_beside_graphql_pairs_reads_what_it_read_before() {
        let Some(alone) = graphql_rows(true, true) else {
            return;
        };
        let (Some(with), Some(without)) = (graphql_with_schema(), graphql_without_schema()) else {
            return;
        };
        let expected = &alone["http_ok"];
        assert_eq!(
            expected.verdict.as_ref().map(|verdict| verdict.bucket),
            Some(VerdictBucket::Compatible),
            "{expected:?}"
        );
        assert_eq!(&with["http_ok"], expected);
        assert_eq!(&without["http_ok"], expected);
    }

    /// An end with no place states `not_located`; a place whose file fails
    /// the one path gate is never asked.
    #[test]
    fn a_graphql_end_with_no_ask_or_an_ungated_file_is_not_located() {
        let (_dir, repo, secret) = guarded_repo();
        let services = [MatchedService {
            service_id: "web".to_string(),
            root: repo.join("apps/web"),
            scan_root: repo.clone(),
            tsconfig: None,
            extraction_config: None,
            server_claims: None,
            graphql_types: None,
        }];
        let at = |file: &str| ReadAt {
            file: file.to_string(),
            line: 1,
            span: (0, 1),
        };
        let pair = |id: &str, caller: Option<ReadAt>, server: Option<ReadAt>| {
            let mut pair = stated_pair(
                id,
                stated_end("web", "apps/web/src/a.ts", None),
                stated_end("web", "apps/web/src/a.ts", None),
            );
            pair.protocol = MatchedProtocol::Graphql(GraphqlPair {
                kind: GraphqlOperationKind::Query,
                field: "widget".to_string(),
                root_type: "Query".to_string(),
                caller,
                server,
            });
            pair
        };
        let pairs = [
            pair("none", None, None),
            pair(
                "ungated",
                Some(at("../outside/secret.ts")),
                Some(at(&secret.to_string_lossy())),
            ),
            pair("link", Some(at("apps/web/src/out.ts")), None),
            pair("gated", Some(at("apps/web/src/a.ts")), None),
        ];
        let rows = type_matched_pairs(
            Path::new("/nonexistent/sidecar/index.js"),
            &services,
            &pairs,
            Duration::from_secs(1),
        );
        for (row, file) in rows.iter().take(2).zip(["none", "ungated"]) {
            for end in [&row.caller, &row.server] {
                assert_refused(end, file);
            }
        }
        #[cfg(unix)]
        assert_refused(&rows[2].caller, "apps/web/src/out.ts");
        // A place that passes the gate is asked (the sidecar is absent, so it
        // reads `site_unreadable`, with its alias).
        assert!(rows[3].caller.alias.is_some(), "{:?}", rows[3].caller);
        assert_refused(&rows[3].server, "a server with no place");
    }

    /// The schema types answer for a pair only when its root type declares
    /// its field: a missing table, a root type the table lacks, a renamed
    /// root and an absent field all leave it unjudged.
    #[test]
    fn schema_types_hold_a_root_field_only_when_the_root_type_declares_it() {
        let pair = |root: &str, field: &str| GraphqlPair {
            kind: GraphqlOperationKind::Query,
            field: field.to_string(),
            root_type: root.to_string(),
            caller: None,
            server: None,
        };
        let fields = |names: &[&str]| {
            GraphqlType::Fields(
                names
                    .iter()
                    .map(|name| {
                        (
                            name.to_string(),
                            crate::graphql::Leaf::Scalar("String".to_string()),
                        )
                    })
                    .collect(),
            )
        };
        let table: GraphqlTypes = [
            ("RootQuery".to_string(), fields(&["widget"])),
            (
                "Members".to_string(),
                GraphqlType::Members(vec!["RootQuery".to_string()]),
            ),
        ]
        .into_iter()
        .collect();
        assert!(schema_holds_root(
            Some(&table),
            &pair("RootQuery", "widget")
        ));
        assert!(!schema_holds_root(
            Some(&table),
            &pair("RootQuery", "gadget")
        ));
        assert!(!schema_holds_root(Some(&table), &pair("Query", "widget")));
        assert!(!schema_holds_root(Some(&table), &pair("Members", "widget")));
        assert!(!schema_holds_root(None, &pair("RootQuery", "widget")));
    }

    #[test]
    fn a_nested_top_type_the_source_does_not_declare_names_the_cause() {
        let finding = |path: &str, reason: &str| crate::services::type_sidecar::TypeProvenance {
            path: path.to_string(),
            kind: "unknown".to_string(),
            reason: reason.to_string(),
            detail: None,
        };
        assert_eq!(nested_cause(&[]), "not_recorded");
        assert_eq!(nested_cause(&[finding("meta", "declared")]), "declared");
        assert_eq!(
            nested_cause(&[
                finding("meta", "declared"),
                finding("owner", "unresolved_import")
            ]),
            "unresolved_import"
        );
        assert_eq!(
            nested_cause(&[
                finding("owner", "unresolved_import"),
                finding("", "budget_exhausted")
            ]),
            "budget_exhausted"
        );
    }

    #[test]
    fn an_unknown_reason_word_is_a_broken_build() {
        let caught = std::panic::catch_unwind(|| state("no_such_word_in_the_list"));
        assert!(caught.is_err());
    }
}
