//! Which manifest aliases a pull request's change can reach, per service
//! (carrick#3102).
//!
//! A pull request scan reads main's type-reach record for each service
//! (carrick#3101) and decides, before its type stage, whether the service is
//! re-typed whole or only in part: [`ReachDecision::Whole`] with the reason,
//! or [`ReachDecision::Partial`] with the aliases the change reaches and the
//! ones it cannot. Every reason to re-type whole fails closed: a missed type
//! is a missed break.
//!
//! The decision is logged, dumped (`CARRICK_TYPE_REACH_DUMP`) and kept for
//! the run (`type_compat_v2::record_type_reach`). Nothing acts on it yet:
//! the type stage still types every alias (carrick#3103 is the first reader).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::change_scope::{Reason, Scope};
use crate::cloud_storage::{
    CloudRepoData, ScannerBuild, TYPE_REACH_RECORD_VERSION, TypeManifestEntry, TypeReachRecord,
};
use crate::services::type_sidecar::{ReachAnswer, ReachChange, Reached};

/// `off` re-types every service whole: the A/B control for carrick#3102 to
/// carrick#3105. Read per service. Deleted once carrick#3103's compare replay
/// has passed on a release.
pub(crate) const TYPE_REACH_ENV: &str = "CARRICK_TYPE_REACH";

/// A directory: each service's decision is written there as
/// `<service>.json`, for the truth replay to read.
pub(crate) const TYPE_REACH_DUMP_ENV: &str = "CARRICK_TYPE_REACH_DUMP";

/// Whether this run's environment turns the decision off.
pub(crate) fn turned_off() -> bool {
    std::env::var(TYPE_REACH_ENV).is_ok_and(|value| value.trim() == "off")
}

/// What a pull request scan re-types of one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub(crate) enum ReachDecision {
    /// Every alias, for `reason`.
    Whole { reason: String },
    /// The aliases of this run's manifest the change can reach, and the
    /// ones it cannot.
    Partial {
        reached: BTreeSet<String>,
        unreached: BTreeSet<String>,
    },
}

impl ReachDecision {
    /// Whether no alias of the service is reached and at least one is
    /// unreached: the case where main's types stand in for the service's
    /// type stage (carrick#3103). An unreached alias has a twin in main's
    /// manifest with the same row key.
    pub fn keeps_main_types(&self) -> bool {
        matches!(
            self,
            ReachDecision::Partial { reached, unreached }
                if reached.is_empty() && !unreached.is_empty()
        )
    }

    /// The aliases a [`Self::keeps_main_types`] decision keeps.
    pub fn kept_aliases(&self) -> usize {
        match self {
            ReachDecision::Partial { unreached, .. } => unreached.len(),
            ReachDecision::Whole { .. } => 0,
        }
    }

    fn whole(reason: impl Into<String>) -> Self {
        ReachDecision::Whole {
            reason: reason.into(),
        }
    }
}

/// One alias of this run's manifest, as the decision read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AliasReach {
    pub alias: String,
    /// The protocol and operation of its first manifest row, for whoever
    /// reads the dump.
    pub protocol: crate::operation::Protocol,
    pub operation: String,
    /// Main's manifest holds the alias with the same row key.
    pub on_main: bool,
    pub key_files: BTreeSet<String>,
    pub reached: bool,
}

/// A service's decision with what it was read from: the dump's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Decided {
    #[serde(flatten)]
    pub decision: ReachDecision,
    /// Every changed path, relative to the scan root.
    pub changed: Vec<String>,
    /// R: the walk's reached files and every changed path.
    pub reached_files: BTreeSet<String>,
    pub aliases: Vec<AliasReach>,
}

impl Decided {
    fn whole(reason: impl Into<String>) -> Self {
        Decided {
            decision: ReachDecision::whole(reason),
            changed: Vec::new(),
            reached_files: BTreeSet::new(),
            aliases: Vec::new(),
        }
    }

    /// The one info line a service's decision gets.
    pub fn line(&self, service: &str) -> String {
        match &self.decision {
            ReachDecision::Whole { reason } => format!("type_reach: {service} whole ({reason})"),
            ReachDecision::Partial { reached, unreached } => format!(
                "type_reach: {service} {} of {} aliases reached ({} files reached, {} changed)",
                reached.len(),
                reached.len() + unreached.len(),
                self.reached_files.len(),
                self.changed.len()
            ),
        }
    }
}

/// What is known before any change is read. Every field is this run's, or
/// main's stored generation of the service.
pub(crate) struct Gate<'a> {
    pub off: bool,
    pub stored: Option<&'a CloudRepoData>,
    /// This binary's build ([`ScannerBuild::current`]).
    pub this_build: Option<ScannerBuild>,
    pub cache_version: u32,
    /// This run's carrick.json entry for the service, serialised as a blob
    /// stores it.
    pub config_json: Option<&'a str>,
    /// Main's type-reach record of the service.
    pub record: Option<&'a TypeReachRecord>,
    /// The service's `--changed-only` scope, when the run decided scopes.
    pub scope: Option<&'a Scope>,
}

/// The first whole-service reason that holds before the change is read,
/// in the order carrick#3102 lists them.
pub(crate) fn gate_reason(gate: &Gate<'_>) -> Option<String> {
    if gate.off {
        return Some("off".to_string());
    }
    let Some(stored) = gate.stored else {
        return Some("no_stored".to_string());
    };
    let build_commit = |build: Option<&ScannerBuild>| build.map(|build| build.commit.clone());
    if build_commit(stored.scanner_build.as_ref()) != build_commit(gate.this_build.as_ref())
        || stored.cache_version != Some(gate.cache_version)
    {
        return Some("other_build".to_string());
    }
    if stored.dirty == Some(true) {
        return Some("stored_dirty".to_string());
    }
    if stored.config_json.as_deref() != gate.config_json {
        return Some("config_changed".to_string());
    }
    if stored.types_degraded.is_some() || stored.capture_stub.is_none() {
        return Some("types_degraded".to_string());
    }
    let record_usable = gate.record.is_some_and(|record| {
        record.version == TYPE_REACH_RECORD_VERSION
            && build_commit(record.scanner_build.as_ref()) == build_commit(gate.this_build.as_ref())
    });
    if !record_usable {
        return Some("no_record".to_string());
    }
    None
}

/// Whether this run's detection, guidance or extraction config differs from
/// the ones main stored, as serialised JSON. Both blobs carry them in the
/// `cached_*` fields the setup stamps.
pub(crate) fn setup_differs(stored: &CloudRepoData, this_run: &CloudRepoData) -> bool {
    json(&stored.cached_detection) != json(&this_run.cached_detection)
        || json(&stored.cached_guidance) != json(&this_run.cached_guidance)
        || json(&stored.cached_extraction_config) != json(&this_run.cached_extraction_config)
}

/// A value as serialised JSON, `Null` when it does not serialise.
fn json<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// The files of a graph `reach_graph` wrote, or `None` when it cannot be
/// read.
pub(crate) fn graph_nodes(graph: &serde_json::Value) -> Option<BTreeSet<String>> {
    let strings = graph.get("strings")?.as_array()?;
    graph
        .get("files")?
        .as_array()?
        .iter()
        .map(|file| {
            let at = usize::try_from(file.get("p")?.as_u64()?).ok()?;
            Some(strings.get(at)?.as_str()?.to_string())
        })
        .collect()
}

/// One path changed since main's stored commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    /// git's status letter: `A`, `D`, `M` or `T`.
    pub status: char,
    /// Relative to the repository's top level, as git names it.
    pub top: String,
    /// Relative to the scan root, as the graph and the manifest name it.
    pub path: String,
}

/// Everything the decision reads after the gate.
pub(crate) struct Inputs<'a> {
    pub gate: Gate<'a>,
    /// [`setup_differs`].
    pub setup_differs: bool,
    /// `Err` when git could not compare the tree with main's commit.
    pub changed: Result<Vec<Change>, String>,
    /// This run's manifest.
    pub manifest: &'a [TypeManifestEntry],
    /// The files of every request this run built for each alias, as the
    /// requests name them.
    pub request_files: &'a BTreeMap<String, BTreeSet<String>>,
    /// The scan root, to read manifest and request paths against.
    pub repo_path: &'a str,
}

/// How the decision reads the two sides of a change and walks it.
pub(crate) trait Walker {
    /// The text `top` (relative to the top level) had at main's commit, or
    /// `None` when this clone does not hold it.
    fn before(&self, top: &str) -> Option<String>;
    /// The text of `path` (relative to the scan root) in the working tree.
    fn after(&self, path: &str) -> String;
    /// The sidecar's `reach` over main's graph.
    fn walk(
        &self,
        graph: &serde_json::Value,
        changes: &[ReachChange],
    ) -> Result<ReachAnswer, String>;
}

/// Whether a change to `path` is a change to a TypeScript or JavaScript
/// source the walk reads.
fn is_source(path: &str) -> bool {
    crate::file_finder::has_source_extension(std::path::Path::new(path))
}

/// A tsconfig or jsconfig file: `.json` or `.jsonc` whose name starts with
/// `tsconfig` or `jsconfig`.
fn is_config_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    (name.starts_with("tsconfig") || name.starts_with("jsconfig"))
        && (name.ends_with(".json") || name.ends_with(".jsonc"))
}

/// The alias a derived capture alias goes with: `<alias>_Unwidened`,
/// `<alias>_Mode<16 hex>` and `<alias>_ModeOther` are captured beside their
/// base (`type_compat_v2::unwidened_alias`, `type_compat_v2::mode_alias`).
pub(crate) fn base_alias(alias: &str) -> &str {
    if let Some(base) = alias.strip_suffix("_Unwidened") {
        return base;
    }
    if let Some(base) = alias.strip_suffix("_ModeOther") {
        return base;
    }
    if let Some((base, hash)) = alias.rsplit_once("_Mode")
        && hash.len() == 16
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return base;
    }
    alias
}

/// A manifest entry's row key: protocol, method and path (or the other
/// protocols' identity), role, type kind, evidence file and line.
fn row_key(entry: &TypeManifestEntry, repo_path: &str) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        json(&entry.key),
        json(&entry.role),
        json(&entry.type_kind),
        relative(&entry.evidence.file_path, repo_path),
        entry.evidence.line_number
    )
}

fn relative(file: &str, repo_path: &str) -> String {
    super::repo_relative(file, repo_path)
}

/// The decision for one service.
pub(crate) fn decide(inputs: &Inputs<'_>, walker: &dyn Walker) -> Decided {
    if let Some(reason) = gate_reason(&inputs.gate) {
        return Decided::whole(reason);
    }
    // The gate let neither through without them; a graph that cannot be
    // read is no record.
    let (Some(stored), Some(record)) = (inputs.gate.stored, inputs.gate.record) else {
        return Decided::whole("no_record");
    };
    let Some(nodes) = graph_nodes(&record.graph) else {
        return Decided::whole("no_record");
    };
    if inputs.setup_differs {
        return Decided::whole("setup_differs");
    }
    if let Some(Scope::Analysed { reason, .. }) = inputs.gate.scope
        && *reason != Reason::InputsChanged
    {
        return Decided::whole(format!("scope_{}", reason.as_str()));
    }
    let changed = match &inputs.changed {
        Ok(changed) => changed,
        Err(_) => return Decided::whole("changes_unread"),
    };
    if changed
        .iter()
        .any(|change| crate::change_scope::is_repo_input(&change.top))
    {
        return Decided::whole("repo_input");
    }
    if changed.iter().any(|change| {
        is_config_file(&change.path) || (nodes.contains(&change.path) && !is_source(&change.path))
    }) {
        return Decided::whole("config_file");
    }

    let mut walk_changes = Vec::new();
    for change in changed.iter().filter(|change| is_source(&change.path)) {
        let before = if change.status == 'A' {
            String::new()
        } else {
            match walker.before(&change.top) {
                Some(text) => text,
                None => return Decided::whole("before_text_missing"),
            }
        };
        let after = if change.status == 'D' {
            String::new()
        } else {
            walker.after(&change.path)
        };
        walk_changes.push(ReachChange {
            file: change.path.clone(),
            before,
            after,
        });
    }
    let mut reached_files: BTreeSet<String> =
        changed.iter().map(|change| change.path.clone()).collect();
    if !walk_changes.is_empty() {
        let answer = match walker.walk(&record.graph, &walk_changes) {
            Ok(answer) => answer,
            Err(_) => return Decided::whole("sidecar"),
        };
        let files = match answer.reached {
            Reached::All => return Decided::whole("walk_all"),
            Reached::Files(files) => files,
        };
        // A file main's graph does not hold may still be in the service's
        // program (an added script or augmentation the tsconfig includes),
        // so an added one widens as a node does.
        let added: BTreeSet<&str> = changed
            .iter()
            .filter(|change| change.status == 'A')
            .map(|change| change.path.as_str())
            .collect();
        if answer
            .augmented
            .iter()
            .any(|file| nodes.contains(file) || added.contains(file.as_str()))
        {
            return Decided::whole("augmentation");
        }
        reached_files.extend(files);
    }

    let repo_path = inputs.repo_path;
    let mut main_keys: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for entry in stored.type_manifest.as_deref().unwrap_or_default() {
        main_keys
            .entry(entry.type_alias.as_str())
            .or_default()
            .insert(row_key(entry, repo_path));
    }
    let mut this_keys: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut key_files: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut operations: BTreeMap<&str, &crate::operation::OperationKey> = BTreeMap::new();
    for entry in inputs.manifest {
        let alias = entry.type_alias.as_str();
        operations.entry(alias).or_insert(&entry.key);
        this_keys
            .entry(alias)
            .or_default()
            .insert(row_key(entry, repo_path));
        let files = key_files.entry(alias).or_default();
        files.insert(relative(&entry.file_path, repo_path));
        files.insert(relative(&entry.evidence.file_path, repo_path));
    }
    // This run's requests and main's record: a derived capture alias goes
    // with its base.
    let requested = inputs
        .request_files
        .iter()
        .map(|(alias, files)| (alias, files.iter().collect::<Vec<_>>()))
        .chain(
            record
                .aliases
                .iter()
                .map(|(alias, files)| (alias, files.iter().collect::<Vec<_>>())),
        );
    for (alias, files) in requested {
        if let Some(own) = key_files.get_mut(base_alias(alias)) {
            own.extend(files.into_iter().map(|file| relative(file, repo_path)));
        }
    }

    let mut aliases = Vec::new();
    let mut reached = BTreeSet::new();
    let mut unreached = BTreeSet::new();
    for (alias, keys) in &this_keys {
        let on_main = main_keys
            .get(alias)
            .is_some_and(|main| keys.is_subset(main));
        let files = key_files.remove(alias).unwrap_or_default();
        let is_reached = !on_main || files.iter().any(|file| reached_files.contains(file));
        if is_reached {
            reached.insert(alias.to_string());
        } else {
            unreached.insert(alias.to_string());
        }
        let operation = operations[alias];
        aliases.push(AliasReach {
            alias: alias.to_string(),
            protocol: operation.protocol(),
            operation: operation.to_string(),
            on_main,
            key_files: files,
            reached: is_reached,
        });
    }
    Decided {
        decision: ReachDecision::Partial { reached, unreached },
        changed: changed.iter().map(|change| change.path.clone()).collect(),
        reached_files,
        aliases,
    }
}

// ===========================================================================
// The run's side: main's records, each service's scope, the changed paths
// ===========================================================================

/// What a pull request run reads once and hands each service's analysis.
pub(crate) struct ReachRun {
    /// Main's type-reach records, by service id
    /// (the blob's `service_name`, else its `repo_name`).
    pub records: std::collections::HashMap<String, TypeReachRecord>,
    /// Each analysed service's `--changed-only` scope, by its analysed
    /// index; empty when the run decided none.
    pub scopes: Vec<Option<Scope>>,
    pub changes: std::sync::Arc<crate::change_scope::ChangedSince>,
    /// The scan root relative to the repository's top level (`""` when they
    /// are one), or `None` when git named no top level.
    pub scan_prefix: Option<String>,
}

impl ReachRun {
    pub fn new(
        repo_path: &str,
        records: std::collections::HashMap<String, TypeReachRecord>,
        scopes: Vec<Option<Scope>>,
        changes: std::sync::Arc<crate::change_scope::ChangedSince>,
    ) -> Self {
        let scan_prefix = crate::git_state::toplevel(repo_path).ok().and_then(|top| {
            let top = top.canonicalize().unwrap_or(top);
            let root = std::path::Path::new(repo_path)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(repo_path));
            let prefix = root.strip_prefix(&top).ok()?;
            Some(prefix.to_string_lossy().replace('\\', "/"))
        });
        Self {
            records,
            scopes,
            changes,
            scan_prefix,
        }
    }

    /// The paths changed since `commit`, each named both ways.
    fn changed(&self, commit: &str) -> Result<Vec<Change>, String> {
        let prefix = self
            .scan_prefix
            .as_deref()
            .ok_or("git named no top level for the scan root")?;
        Ok(self
            .changes
            .since(commit)?
            .into_iter()
            .map(|change| Change {
                status: change.status,
                path: scan_relative(&change.path, prefix),
                top: change.path,
            })
            .collect())
    }
}

/// `top` (relative to the repository's top level) relative to a scan root
/// at `prefix` below it: stripped of the prefix when under it, else reached
/// with `..` segments, as the graph names a file outside the root.
pub(crate) fn scan_relative(top: &str, prefix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return top.to_string();
    }
    if let Some(rest) = top
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        return rest.to_string();
    }
    let up = prefix.split('/').count();
    format!("{}{top}", "../".repeat(up))
}

/// One service's share of a [`ReachRun`].
#[derive(Clone, Copy)]
pub(crate) struct ReachContext<'a> {
    pub run: &'a ReachRun,
    pub scope: Option<&'a Scope>,
}

/// The sidecar process that will type the service, and git, as a
/// [`Walker`].
struct SidecarWalker<'a> {
    sidecar: Option<&'a crate::services::type_sidecar::TypeSidecar>,
    repo_path: &'a str,
    commit: &'a str,
}

impl Walker for SidecarWalker<'_> {
    fn before(&self, top: &str) -> Option<String> {
        crate::git_state::file_at_commit(self.repo_path, self.commit, top)
    }

    fn after(&self, path: &str) -> String {
        // An unreadable file is walked as deleted, which reaches at least
        // what any edit of it could.
        std::fs::read_to_string(std::path::Path::new(self.repo_path).join(path)).unwrap_or_default()
    }

    fn walk(
        &self,
        graph: &serde_json::Value,
        changes: &[ReachChange],
    ) -> Result<ReachAnswer, String> {
        let sidecar = self.sidecar.ok_or("no type sidecar for the service")?;
        // The type stage waits for the same process next; a wait already
        // over returns at once.
        sidecar
            .wait_ready(crate::services::type_sidecar::ready_budget())
            .map_err(|e| e.to_string())?;
        sidecar.reach(graph, changes).map_err(|e| e.to_string())
    }
}

/// One service's facts for [`decide_service`].
pub(crate) struct ServiceFacts<'a> {
    /// The id the record and the registry key it by.
    pub id: &'a str,
    /// The name its log lines carry.
    pub label: &'a str,
    pub config: &'a crate::config::Config,
    pub stored: Option<&'a CloudRepoData>,
    pub repo_path: &'a str,
}

fn gate_of<'a>(
    context: ReachContext<'a>,
    facts: &ServiceFacts<'a>,
    config_json: Option<&'a str>,
) -> Gate<'a> {
    Gate {
        off: turned_off(),
        stored: facts.stored,
        this_build: ScannerBuild::current(),
        cache_version: crate::engine::CACHE_VERSION,
        config_json,
        record: context.run.records.get(facts.id),
        scope: context.scope,
    }
}

/// Decide, log, dump and keep one service's decision, just before its type
/// stage. `this_run` is its payload so far: the setup it stamped and the
/// manifest it built. `request_files` is every request the type stage will
/// send, by alias. `started` is when the run began collecting those
/// requests: the line and the dump carry the time the decision took.
/// Returns the decision, which the caller acts on (carrick#3103).
pub(crate) fn decide_service(
    started: std::time::Instant,
    context: ReachContext<'_>,
    facts: &ServiceFacts<'_>,
    this_run: &CloudRepoData,
    request_files: &BTreeMap<String, BTreeSet<String>>,
    sidecar: Option<&crate::services::type_sidecar::TypeSidecar>,
) -> ReachDecision {
    let config_json = serde_json::to_string(facts.config).ok();
    let commit = facts
        .stored
        .map(|stored| stored.commit_hash.as_str())
        .unwrap_or_default();
    let inputs = Inputs {
        gate: gate_of(context, facts, config_json.as_deref()),
        setup_differs: facts
            .stored
            .is_some_and(|stored| setup_differs(stored, this_run)),
        changed: context.run.changed(commit),
        manifest: this_run.type_manifest.as_deref().unwrap_or_default(),
        request_files,
        repo_path: facts.repo_path,
    };
    let walker = SidecarWalker {
        sidecar,
        repo_path: facts.repo_path,
        commit,
    };
    let decided = decide(&inputs, &walker);
    report(facts, decided, Some(started.elapsed()))
}

/// The decision of a service whose analysis does not reach the point
/// [`decide_service`] runs at: no stored generation, another cache format,
/// or a tree git could not compare. Always whole.
pub(crate) fn decide_service_whole(context: ReachContext<'_>, facts: &ServiceFacts<'_>) {
    let config_json = serde_json::to_string(facts.config).ok();
    let reason = gate_reason(&gate_of(context, facts, config_json.as_deref()))
        .unwrap_or_else(|| "changes_unread".to_string());
    report(facts, Decided::whole(reason), None);
}

/// Put main's stored types in for a service no change reaches
/// (carrick#3103): its bundle, capture stub and extraction status as
/// stored, and each manifest row replaced by main's row of the same key,
/// every field. Called before the payload's paths are made repo-relative:
/// row keys read both sides through `repo_relative`, and main's rows are
/// stored repo-relative, so that sweep leaves them as they are. Returns how
/// many rows had no twin in
/// main's manifest and kept this run's row (the decision guarantees none).
pub(crate) fn keep_main_types(
    cloud_data: &mut CloudRepoData,
    stored: &CloudRepoData,
    repo_path: &str,
) -> usize {
    cloud_data.bundled_types = stored.bundled_types.clone();
    cloud_data.capture_stub = stored.capture_stub.clone();
    cloud_data.type_extraction_status = stored.type_extraction_status.clone();
    cloud_data.types_degraded = None;
    let main_rows: BTreeMap<String, &TypeManifestEntry> = stored
        .type_manifest
        .iter()
        .flatten()
        .map(|entry| (row_key(entry, repo_path), entry))
        .collect();
    let mut without_twin = 0;
    for entry in cloud_data.type_manifest.iter_mut().flatten() {
        match main_rows.get(&row_key(entry, repo_path)) {
            Some(twin) => *entry = (*twin).clone(),
            None => without_twin += 1,
        }
    }
    without_twin
}

/// Say that a service took main's types for `aliases` unreached aliases:
/// one info line, and `kept_main_types` in the service's dump when the run
/// dumps. The dump is written by [`report`] before the type stage, so this
/// one reads what the stage actually did, not what was decided.
pub(crate) fn note_kept(id: &str, label: &str, aliases: usize) {
    tracing::info!("type_reach: {label} kept main's types (0 of {aliases} aliases reached)");
    let Some(dir) = std::env::var_os(TYPE_REACH_DUMP_ENV) else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{}.json", id.replace(['/', '\\'], "_")));
    let updated = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|mut body| {
            body["kept_main_types"] = serde_json::json!(aliases);
            serde_json::to_vec_pretty(&body).ok()
        })
        .map(|bytes| std::fs::write(&path, bytes));
    if !matches!(updated, Some(Ok(()))) {
        tracing::warn!(
            "Could not add the kept-types count to the type-reach dump of service {label} at {}",
            path.display()
        );
    }
}

fn report(
    facts: &ServiceFacts<'_>,
    decided: Decided,
    took: Option<std::time::Duration>,
) -> ReachDecision {
    let took_ms = took.map(|took| took.as_millis());
    match took_ms {
        Some(ms) => tracing::info!("{} in {ms} ms", decided.line(facts.label)),
        None => tracing::info!("{}", decided.line(facts.label)),
    }
    if let Some(dir) = std::env::var_os(TYPE_REACH_DUMP_ENV) {
        let path =
            std::path::Path::new(&dir).join(format!("{}.json", facts.id.replace(['/', '\\'], "_")));
        let body =
            serde_json::json!({ "service": facts.id, "decided": &decided, "took_ms": took_ms });
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            std::fs::write(&path, serde_json::to_vec_pretty(&body).unwrap_or_default())
        });
        if let Err(e) = written {
            tracing::warn!(
                "Could not write the type-reach dump of service {} to {} ({e})",
                facts.label,
                path.display()
            );
        }
    }
    super::type_compat_v2::record_type_reach(facts.id, decided.decision.clone());
    decided.decision
}

/// The run's one total line over every decision kept, or `None` when the
/// run decided none.
pub(crate) fn total_line(
    decisions: &std::collections::HashMap<String, ReachDecision>,
) -> Option<String> {
    if decisions.is_empty() {
        return None;
    }
    let (mut whole, mut reached, mut aliases) = (0, 0, 0);
    for decision in decisions.values() {
        match decision {
            ReachDecision::Whole { .. } => whole += 1,
            ReachDecision::Partial {
                reached: r,
                unreached,
            } => {
                reached += r.len();
                aliases += r.len() + unreached.len();
            }
        }
    }
    Some(format!(
        "type_reach: {} service(s), {whole} whole, {} partial with {reached} of {aliases} aliases reached",
        decisions.len(),
        decisions.len() - whole
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::{
        CaptureStubArtifact, ManifestRole, ManifestTypeKind, ManifestTypeState, TypeEvidence,
    };
    use crate::operation::OperationKey;
    use crate::services::type_sidecar::InferKind;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const CACHE: u32 = 9;
    const CONFIG: &str = "{\"serviceName\":\"api\"}";

    fn entry(alias: &str, file: &str, line: u32) -> TypeManifestEntry {
        TypeManifestEntry {
            key: OperationKey::http("GET", format!("/{alias}")),
            role: ManifestRole::Producer,
            type_kind: ManifestTypeKind::Response,
            type_alias: alias.to_string(),
            file_path: file.to_string(),
            line_number: line,
            is_explicit: false,
            type_state: ManifestTypeState::Implicit,
            evidence: TypeEvidence {
                file_path: file.to_string(),
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                line_number: line,
                infer_kind: InferKind::ResponseBody,
                is_explicit: false,
                type_state: ManifestTypeState::Implicit,
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
            v1_state_before_demotion: None,
        }
    }

    /// Main's manifest: `A` served from `src/a.ts`, `B` from `src/b.ts`.
    fn manifest() -> Vec<TypeManifestEntry> {
        vec![entry("A", "src/a.ts", 3), entry("B", "src/b.ts", 5)]
    }

    fn stored() -> CloudRepoData {
        let mut blob: CloudRepoData = serde_json::from_value(serde_json::json!({
            "repo_name": "mono",
            "service_name": "api",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": CONFIG,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-10-01T00:00:00Z",
            "commit_hash": "abc123",
        }))
        .unwrap();
        blob.cache_version = Some(CACHE);
        blob.capture_stub = Some(CaptureStubArtifact {
            artifact_version: 1,
            package_name: "@carrick/api".to_string(),
            ts_version: "5.9.0".to_string(),
            bare_checkout: false,
            files: Default::default(),
        });
        blob.type_manifest = Some(manifest());
        blob
    }

    /// Main's record: its graph holds four files, one of them JSON, and
    /// `B`'s unwidened reading was asked in `src/c.ts`.
    fn record() -> TypeReachRecord {
        TypeReachRecord {
            version: TYPE_REACH_RECORD_VERSION,
            scanner_build: None,
            graph: serde_json::json!({
                "version": 1,
                "strings": ["src/a.ts", "src/b.ts", "src/types.ts", "src/data.json"],
                "files": [{ "p": 0 }, { "p": 1 }, { "p": 2 }, { "p": 3 }],
            }),
            aliases: [
                ("A".to_string(), vec!["src/a.ts".to_string()]),
                ("B".to_string(), vec!["src/b.ts".to_string()]),
                ("B_Unwidened".to_string(), vec!["src/c.ts".to_string()]),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn change(status: char, path: &str) -> Change {
        Change {
            status,
            top: path.to_string(),
            path: path.to_string(),
        }
    }

    /// git and the sidecar, answered from the test.
    struct Fake {
        missing_before: Vec<&'static str>,
        answer: Result<ReachAnswer, String>,
        walked: RefCell<Vec<Vec<ReachChange>>>,
    }

    impl Fake {
        fn reaching(files: &[&str]) -> Self {
            Fake {
                missing_before: Vec::new(),
                answer: Ok(ReachAnswer {
                    reached: Reached::Files(files.iter().map(|f| f.to_string()).collect()),
                    augmented: Vec::new(),
                    exports_moved: false,
                }),
                walked: RefCell::new(Vec::new()),
            }
        }
    }

    impl Walker for Fake {
        fn before(&self, top: &str) -> Option<String> {
            (!self.missing_before.contains(&top)).then(|| format!("before {top}"))
        }
        fn after(&self, path: &str) -> String {
            format!("after {path}")
        }
        fn walk(
            &self,
            _graph: &serde_json::Value,
            changes: &[ReachChange],
        ) -> Result<ReachAnswer, String> {
            self.walked.borrow_mut().push(changes.to_vec());
            self.answer.clone()
        }
    }

    struct Case {
        stored: Option<CloudRepoData>,
        record: Option<TypeReachRecord>,
        this_build: Option<ScannerBuild>,
        off: bool,
        config_json: Option<&'static str>,
        scope: Option<Scope>,
        setup_differs: bool,
        changed: Result<Vec<Change>, String>,
        manifest: Vec<TypeManifestEntry>,
        request_files: BTreeMap<String, BTreeSet<String>>,
    }

    impl Case {
        /// Nothing differs from main and nothing changed.
        fn clean() -> Self {
            Case {
                stored: Some(stored()),
                record: Some(record()),
                this_build: None,
                off: false,
                config_json: Some(CONFIG),
                scope: None,
                setup_differs: false,
                changed: Ok(Vec::new()),
                manifest: manifest(),
                request_files: BTreeMap::new(),
            }
        }

        fn changing(changes: Vec<Change>) -> Self {
            Case {
                changed: Ok(changes),
                ..Case::clean()
            }
        }

        fn decide(&self, walker: &Fake) -> Decided {
            decide(
                &Inputs {
                    gate: Gate {
                        off: self.off,
                        stored: self.stored.as_ref(),
                        this_build: self.this_build.clone(),
                        cache_version: CACHE,
                        config_json: self.config_json,
                        record: self.record.as_ref(),
                        scope: self.scope.as_ref(),
                    },
                    setup_differs: self.setup_differs,
                    changed: self.changed.clone(),
                    manifest: &self.manifest,
                    request_files: &self.request_files,
                    repo_path: "/repo",
                },
                walker,
            )
        }

        fn reason(&self, walker: &Fake) -> String {
            match self.decide(walker).decision {
                ReachDecision::Whole { reason } => reason,
                partial => panic!("expected whole, got {partial:?}"),
            }
        }
    }

    fn reached_of(decided: &Decided) -> Vec<&str> {
        match &decided.decision {
            ReachDecision::Partial { reached, .. } => reached.iter().map(String::as_str).collect(),
            whole => panic!("expected partial, got {whole:?}"),
        }
    }

    // ---- Whole, one test per reason -----------------------------------

    #[test]
    fn a_clean_tree_is_partial_with_nothing_reached() {
        let walker = Fake::reaching(&[]);
        let decided = Case::clean().decide(&walker);
        assert_eq!(reached_of(&decided), Vec::<&str>::new());
        assert!(
            walker.walked.borrow().is_empty(),
            "no source changed, no walk"
        );
    }

    #[test]
    fn off_is_whole() {
        let case = Case {
            off: true,
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "off");
    }

    #[test]
    fn no_stored_generation_is_whole() {
        let case = Case {
            stored: None,
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "no_stored");
    }

    #[test]
    fn another_scanner_build_is_whole() {
        let case = Case {
            this_build: Some(ScannerBuild {
                commit: "b".repeat(40),
                dirty: false,
            }),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "other_build");
    }

    #[test]
    fn another_cache_version_is_whole() {
        let mut blob = stored();
        blob.cache_version = Some(CACHE - 1);
        let case = Case {
            stored: Some(blob),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "other_build");
    }

    #[test]
    fn a_dirty_stored_generation_is_whole() {
        let mut blob = stored();
        blob.dirty = Some(true);
        let case = Case {
            stored: Some(blob),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "stored_dirty");
    }

    #[test]
    fn a_changed_service_config_is_whole() {
        let case = Case {
            config_json: Some("{\"serviceName\":\"api\",\"tsconfig\":\"x.json\"}"),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "config_changed");
    }

    #[test]
    fn a_stored_generation_without_its_capture_is_whole() {
        let mut blob = stored();
        blob.capture_stub = None;
        let case = Case {
            stored: Some(blob),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "types_degraded");
    }

    #[test]
    fn a_degraded_stored_generation_is_whole() {
        let mut blob = stored();
        blob.types_degraded = Some(crate::cloud_storage::TypeDegradation {
            stage: "capture".to_string(),
            detail: "out of memory".to_string(),
        });
        let case = Case {
            stored: Some(blob),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "types_degraded");
    }

    #[test]
    fn no_record_is_whole() {
        let case = Case {
            record: None,
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "no_record");
    }

    #[test]
    fn a_record_of_another_version_is_whole() {
        let mut stale = record();
        stale.version = TYPE_REACH_RECORD_VERSION + 1;
        let case = Case {
            record: Some(stale),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "no_record");
    }

    #[test]
    fn a_record_of_another_build_is_whole() {
        let mut other = record();
        other.scanner_build = Some(ScannerBuild {
            commit: "c".repeat(40),
            dirty: false,
        });
        let case = Case {
            record: Some(other),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "no_record");
    }

    #[test]
    fn a_record_whose_graph_cannot_be_read_is_whole() {
        let mut broken = record();
        broken.graph = serde_json::json!({ "version": 1, "files": [{ "p": 7 }] });
        let case = Case {
            record: Some(broken),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "no_record");
    }

    #[test]
    fn a_setup_unlike_mains_is_whole() {
        let case = Case {
            setup_differs: true,
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "setup_differs");
    }

    #[test]
    fn setup_differs_compares_detection_guidance_and_extraction_config() {
        let main = stored();
        assert!(!setup_differs(&main, &stored()));
        let mut detected = stored();
        detected.cached_detection = Some(Default::default());
        assert!(setup_differs(&main, &detected));
        let mut guided = stored();
        guided.cached_guidance = Some(crate::local_mode::offline_guidance());
        assert!(setup_differs(&main, &guided));
        let mut configured = stored();
        configured.cached_extraction_config = Some(Default::default());
        assert!(setup_differs(&main, &configured));
    }

    #[test]
    fn a_scope_analysed_for_another_reason_than_its_inputs_is_whole() {
        let case = Case {
            scope: Some(Scope::Analysed {
                reason: Reason::UnresolvedImport,
                detail: "x".to_string(),
            }),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "scope_unresolved_import");
        let inputs_changed = Case {
            scope: Some(Scope::Analysed {
                reason: Reason::InputsChanged,
                detail: "x".to_string(),
            }),
            ..Case::clean()
        };
        reached_of(&inputs_changed.decide(&Fake::reaching(&[])));
    }

    #[test]
    fn changes_git_could_not_list_are_whole() {
        let case = Case {
            changed: Err("no such commit".to_string()),
            ..Case::clean()
        };
        assert_eq!(case.reason(&Fake::reaching(&[])), "changes_unread");
    }

    #[test]
    fn a_changed_repo_input_is_whole() {
        for path in [
            "package.json",
            "pnpm-lock.yaml",
            "src/schema.graphql",
            ".gitignore",
        ] {
            let case = Case::changing(vec![change('M', path)]);
            assert_eq!(case.reason(&Fake::reaching(&[])), "repo_input", "{path}");
        }
    }

    #[test]
    fn a_changed_tsconfig_is_whole() {
        for path in [
            "tsconfig.json",
            "packages/api/tsconfig.build.json",
            "jsconfig.jsonc",
        ] {
            let case = Case::changing(vec![change('M', path)]);
            assert_eq!(case.reason(&Fake::reaching(&[])), "config_file", "{path}");
        }
    }

    #[test]
    fn a_changed_graph_node_that_is_not_source_is_whole() {
        let case = Case::changing(vec![change('M', "src/data.json")]);
        assert_eq!(case.reason(&Fake::reaching(&[])), "config_file");
        // The same file outside the graph is just a changed path.
        let elsewhere = Case::changing(vec![change('M', "docs/data.json")]);
        reached_of(&elsewhere.decide(&Fake::reaching(&[])));
    }

    #[test]
    fn a_change_whose_before_text_is_missing_is_whole() {
        let mut walker = Fake::reaching(&["src/a.ts"]);
        walker.missing_before = vec!["src/a.ts"];
        let case = Case::changing(vec![change('M', "src/a.ts")]);
        assert_eq!(case.reason(&walker), "before_text_missing");
        // An added file has no before text to miss.
        let added = Case::changing(vec![change('A', "src/a.ts")]);
        reached_of(&added.decide(&walker));
    }

    #[test]
    fn a_walk_that_reaches_everything_is_whole() {
        let mut walker = Fake::reaching(&[]);
        walker.answer = Ok(ReachAnswer {
            reached: Reached::All,
            augmented: Vec::new(),
            exports_moved: false,
        });
        let case = Case::changing(vec![change('M', "src/a.ts")]);
        assert_eq!(case.reason(&walker), "walk_all");
    }

    #[test]
    fn an_augmentation_in_the_services_graph_is_whole() {
        let mut walker = Fake::reaching(&["src/types.ts"]);
        walker.answer = Ok(ReachAnswer {
            reached: Reached::Files(vec!["src/types.ts".to_string()]),
            augmented: vec!["src/types.ts".to_string()],
            exports_moved: false,
        });
        let case = Case::changing(vec![change('M', "src/types.ts")]);
        assert_eq!(case.reason(&walker), "augmentation");
    }

    #[test]
    fn an_added_augmentation_is_whole() {
        let mut walker = Fake::reaching(&["src/new.d.ts"]);
        walker.answer = Ok(ReachAnswer {
            reached: Reached::Files(vec!["src/new.d.ts".to_string()]),
            augmented: vec!["src/new.d.ts".to_string()],
            exports_moved: false,
        });
        let case = Case::changing(vec![change('A', "src/new.d.ts")]);
        assert_eq!(case.reason(&walker), "augmentation");
    }

    #[test]
    fn an_augmentation_in_another_services_files_is_not_whole() {
        let mut walker = Fake::reaching(&["web/src/aug.ts"]);
        walker.answer = Ok(ReachAnswer {
            reached: Reached::Files(vec!["web/src/aug.ts".to_string()]),
            augmented: vec!["web/src/aug.ts".to_string()],
            exports_moved: false,
        });
        let case = Case::changing(vec![change('M', "web/src/aug.ts")]);
        reached_of(&case.decide(&walker));
    }

    #[test]
    fn a_failed_walk_is_whole() {
        let mut walker = Fake::reaching(&[]);
        walker.answer = Err("the sidecar died".to_string());
        let case = Case::changing(vec![change('M', "src/a.ts")]);
        assert_eq!(case.reason(&walker), "sidecar");
    }

    // ---- Partial ------------------------------------------------------

    #[test]
    fn an_alias_in_a_changed_file_is_reached() {
        let walker = Fake::reaching(&["src/a.ts"]);
        let decided = Case::changing(vec![change('M', "src/a.ts")]).decide(&walker);
        assert_eq!(reached_of(&decided), vec!["A"]);
        assert_eq!(
            decided.line("api"),
            "type_reach: api 1 of 2 aliases reached (1 files reached, 1 changed)"
        );
        // Both sides of the change went to the walk.
        assert_eq!(
            *walker.walked.borrow(),
            vec![vec![ReachChange {
                file: "src/a.ts".to_string(),
                before: "before src/a.ts".to_string(),
                after: "after src/a.ts".to_string(),
            }]]
        );
    }

    #[test]
    fn an_alias_whose_request_file_is_reached_is_reached() {
        let case = Case {
            request_files: [(
                "B".to_string(),
                ["/repo/src/types.ts".to_string()].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            ..Case::changing(vec![change('M', "src/model.ts")])
        };
        // The walk reaches the request's file, not the manifest's.
        let decided = case.decide(&Fake::reaching(&["src/model.ts", "src/types.ts"]));
        assert_eq!(reached_of(&decided), vec!["B"]);
    }

    #[test]
    fn an_alias_only_mains_record_ties_to_a_changed_file_is_reached() {
        let mut tied = record();
        tied.aliases.insert(
            "A".to_string(),
            vec!["src/a.ts".to_string(), "src/helper.ts".to_string()],
        );
        let case = Case {
            record: Some(tied),
            ..Case::changing(vec![change('M', "src/helper.ts")])
        };
        let decided = case.decide(&Fake::reaching(&["src/helper.ts"]));
        assert_eq!(reached_of(&decided), vec!["A"]);
    }

    #[test]
    fn a_rekeyed_alias_is_reached_whatever_changed() {
        let case = Case {
            manifest: vec![entry("A", "src/a.ts", 4), entry("B", "src/b.ts", 5)],
            ..Case::clean()
        };
        let decided = case.decide(&Fake::reaching(&[]));
        assert_eq!(reached_of(&decided), vec!["A"]);
        let new_alias = Case {
            manifest: vec![entry("C", "src/c.ts", 1), entry("B", "src/b.ts", 5)],
            ..Case::clean()
        };
        assert_eq!(
            reached_of(&new_alias.decide(&Fake::reaching(&[]))),
            vec!["C"]
        );
    }

    #[test]
    fn a_derived_alias_follows_its_base() {
        // Main's record asked `B_Unwidened` in `src/c.ts`.
        let decided =
            Case::changing(vec![change('M', "src/c.ts")]).decide(&Fake::reaching(&["src/c.ts"]));
        assert_eq!(reached_of(&decided), vec!["B"]);
        // And this run's own derived requests.
        let case = Case {
            request_files: [(
                "A_Mode0123456789abcdef".to_string(),
                ["/repo/src/mode.ts".to_string()].into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            ..Case::changing(vec![change('M', "src/mode.ts")])
        };
        assert_eq!(
            reached_of(&case.decide(&Fake::reaching(&["src/mode.ts"]))),
            vec!["A"]
        );
    }

    #[test]
    fn base_alias_strips_only_derived_suffixes() {
        assert_eq!(base_alias("X_Unwidened"), "X");
        assert_eq!(base_alias("X_ModeOther"), "X");
        assert_eq!(base_alias("X_Mode0123456789abcdef"), "X");
        assert_eq!(base_alias("X_ModeShort"), "X_ModeShort");
        assert_eq!(base_alias("X"), "X");
    }

    #[test]
    fn a_whole_decision_reads_as_one_line() {
        assert_eq!(
            Decided::whole("repo_input").line("api"),
            "type_reach: api whole (repo_input)"
        );
    }

    #[test]
    fn the_total_line_counts_whole_and_partial_services() {
        let decisions: HashMap<String, ReachDecision> = [
            ("a".to_string(), ReachDecision::whole("off")),
            (
                "b".to_string(),
                ReachDecision::Partial {
                    reached: ["X".to_string()].into_iter().collect(),
                    unreached: ["Y".to_string(), "Z".to_string()].into_iter().collect(),
                },
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            total_line(&decisions).unwrap(),
            "type_reach: 2 service(s), 1 whole, 1 partial with 1 of 3 aliases reached"
        );
        assert_eq!(total_line(&HashMap::new()), None);
    }

    // ---- Main's types stand in (carrick#3103) -------------------------

    #[test]
    fn a_service_no_change_reaches_keeps_mains_types() {
        let decided = Case::clean().decide(&Fake::reaching(&[]));
        assert!(decided.decision.keeps_main_types());
        assert_eq!(decided.decision.kept_aliases(), 2);
    }

    #[test]
    fn a_service_with_one_reached_alias_is_retyped() {
        let case = Case::changing(vec![change('M', "src/a.ts")]);
        let decided = case.decide(&Fake::reaching(&["src/a.ts"]));
        assert_eq!(reached_of(&decided), vec!["A"]);
        assert!(!decided.decision.keeps_main_types());
    }

    #[test]
    fn a_whole_decision_keeps_nothing() {
        assert!(!ReachDecision::whole("off").keeps_main_types());
    }

    #[test]
    fn a_service_with_no_alias_keeps_nothing() {
        let case = Case {
            manifest: Vec::new(),
            ..Case::clean()
        };
        let decided = case.decide(&Fake::reaching(&[]));
        assert!(!decided.decision.keeps_main_types());
    }

    #[test]
    fn a_degraded_stored_generation_with_nothing_reached_is_retyped_whole() {
        let mut blob = stored();
        blob.types_degraded = Some(crate::cloud_storage::TypeDegradation {
            stage: "capture".to_string(),
            detail: "out of memory".to_string(),
        });
        let case = Case {
            stored: Some(blob),
            ..Case::clean()
        };
        let decided = case.decide(&Fake::reaching(&[]));
        assert!(!decided.decision.keeps_main_types());
        assert_eq!(case.reason(&Fake::reaching(&[])), "types_degraded");
    }

    #[test]
    fn keeping_mains_types_takes_its_rows_bundle_and_stub_whole() {
        let mut main = stored();
        main.bundled_types = Some("declare type A = string;".to_string());
        main.type_extraction_status = Some("main's status".to_string());
        for row in main.type_manifest.as_mut().unwrap() {
            row.resolved_definition = Some(format!("main's {}", row.type_alias));
        }
        // This run's rows: the same keys, typed differently (here, not at
        // all), with paths still under the scan root.
        let mut this_run = stored();
        this_run.capture_stub = None;
        this_run.types_degraded = Some(crate::cloud_storage::TypeDegradation {
            stage: "capture".to_string(),
            detail: "this run's".to_string(),
        });
        for row in this_run.type_manifest.as_mut().unwrap() {
            row.file_path = format!("/repo/{}", row.file_path);
            row.evidence.file_path = row.file_path.clone();
        }
        let without_twin = keep_main_types(&mut this_run, &main, "/repo");
        assert_eq!(without_twin, 0);
        assert_eq!(json(&this_run.type_manifest), json(&main.type_manifest));
        assert_eq!(this_run.bundled_types, main.bundled_types);
        assert_eq!(json(&this_run.capture_stub), json(&main.capture_stub));
        assert_eq!(this_run.type_extraction_status, main.type_extraction_status);
        assert!(this_run.types_degraded.is_none());
    }

    #[test]
    fn a_row_main_lacks_keeps_this_runs_row_and_is_counted() {
        let main = stored();
        let mut this_run = stored();
        this_run
            .type_manifest
            .as_mut()
            .unwrap()
            .push(entry("C", "src/c.ts", 9));
        assert_eq!(keep_main_types(&mut this_run, &main, "/repo"), 1);
        assert_eq!(this_run.type_manifest.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn paths_from_the_top_level_read_from_the_scan_root() {
        assert_eq!(scan_relative("src/a.ts", ""), "src/a.ts");
        assert_eq!(scan_relative("apps/api/src/a.ts", "apps/api"), "src/a.ts");
        assert_eq!(scan_relative("libs/x.ts", "apps/api"), "../../libs/x.ts");
    }
}
