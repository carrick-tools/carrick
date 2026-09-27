//! Which of a PR run's findings were already on main (carrick-cloud#1369).
//!
//! A PR check fails only on the risks the PR introduced. The cloud cannot tell
//! which those are: the PR payload carries display labels and no pairing, and
//! main's method mismatches are stored nowhere (carrick-cloud#1370). The
//! scanner can. On a PR run it holds main's copy of this repo (its last
//! uploaded index) beside the peers, so it matches that copy against the same
//! peers and marks each PR finding with whether main yields it too.
//!
//! "The same finding" is the same kind on the same pairing
//! ([`Finding::pair`]): producer service and operation, consumer service and
//! file, and the direction. The line is left out, because a PR that edits the
//! consumer file above a call moves every line below the edit.
//!
//! Leaving the line out would let a second broken call slip through as "on
//! main" when it sits in the same file and hits the same pairing as an
//! existing one. So the comparison counts call sites per file: when the PR
//! has more sites for a pairing in one file than main has, every finding with
//! sites in that group reads as introduced.
//!
//! `on_main: false` is a claim that main does not have the finding, and the
//! cloud fails the check on it, so it is only made when main's side was judged
//! the way the PR's was (carrick-cloud#1408). Main's side is recomputed from
//! its stored copy, which carries no sources, so a type verdict the PR run
//! reached by retyping the consumer's own code (carrick#1491) can never be
//! reached again there. Main's stored verdicts are main's own answer to those,
//! from the scan that had main's sources, and they count on main's side. When
//! neither source judged a pairing, or main's copy is not main as this PR's
//! base has it, the finding says the run could not tell
//! ([`OnMainUnknown`]) instead of guessing.
//!
//! A copy another scanner version wrote is judged by how that scanner read the
//! files the PR left alone ([`untouched_reading`], carrick#1530): read as this
//! run reads them, it counts as main's.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;

use crate::analyzer::ApiEndpointDetails;
use crate::cloud_storage::{CloudRepoData, DirectionVerdict, ManifestTypeKind};
use crate::findings::{Finding, OnMainUnknown};
use crate::operation::TypeVerdict;

/// A comparison bucket: kind, pairing, and the consumer file of one site.
type SiteGroup = (&'static str, String, String);

const TYPE_MISMATCH: &str = "type_mismatch";

/// The file half of a `file:line` (or `file:line:col`, or bare `file`) site.
fn site_file(site: &str) -> String {
    crate::type_manifest::parse_file_location(site).0
}

/// One call site as `file:line`, whatever form it was written in, so a site
/// two sources report is recognised as one.
fn site_id(site: &str) -> String {
    let (file, line) = crate::type_manifest::parse_file_location(site);
    format!("{file}:{line}")
}

/// The buckets one finding counts in, with the site it counts, one entry per
/// call site. A finding with no pairing counts in none, and a finding with a
/// pairing but no sites counts once, under an empty file.
fn groups(finding: &Finding) -> Vec<(SiteGroup, String)> {
    let Some(pair) = finding.pair() else {
        return Vec::new();
    };
    let sites: &[String] = match finding {
        Finding::TypeMismatch { call_sites, .. } | Finding::MethodMismatch { call_sites, .. } => {
            call_sites
        }
        _ => return Vec::new(),
    };
    if sites.is_empty() {
        return vec![(
            (finding.kind(), pair.to_string(), String::new()),
            String::new(),
        )];
    }
    sites
        .iter()
        .map(|site| {
            (
                (finding.kind(), pair.to_string(), site_file(site)),
                site_id(site),
            )
        })
        .collect()
}

/// The distinct sites each group holds. Distinct, because main's side reads
/// one site from two sources (its recomputed findings and its stored
/// verdicts), and a site both report is still one broken call.
fn sites_by_group(
    entries: impl Iterator<Item = (SiteGroup, String)>,
) -> HashMap<SiteGroup, HashSet<String>> {
    let mut sites: HashMap<SiteGroup, HashSet<String>> = HashMap::new();
    for (group, site) in entries {
        sites.entry(group).or_default().insert(site);
    }
    sites
}

/// Main's answer at one type-checked call site, keyed the way a type
/// mismatch is paired ([`Finding::pair`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainTypeSite {
    /// The pairing, as `type_pair_key` in the analyzer builds it.
    pub pair: String,
    /// The call site, `file:line`, repo-relative.
    pub site: String,
    /// Main has a type mismatch at this site.
    pub incompatible: bool,
    /// The answer compares two known types, so it settles the site either
    /// way. An unresolved answer says main could not judge it.
    pub resolved: bool,
}

impl MainTypeSite {
    fn group(&self) -> SiteGroup {
        (TYPE_MISMATCH, self.pair.clone(), site_file(&self.site))
    }
}

/// Main's side of the comparison.
#[derive(Clone, Debug, Default)]
pub struct MainSide {
    /// Main's findings, recomputed this run from its stored copy against the
    /// same peers.
    pub findings: Vec<Finding>,
    /// The answer main's recomputed type check reached at each call site. It
    /// is judged against the peers as they are now.
    pub recomputed: Vec<MainTypeSite>,
    /// The answer main's own scan stored at each call site. It was judged
    /// with main's sources on disk, against the peers as they were then.
    pub stored: Vec<MainTypeSite>,
    /// Main's recomputed type check ran.
    pub types_judged: bool,
}

/// Whether main's copy is main as this PR's base has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainCopy {
    /// Nothing shows it to be anything else.
    Current,
    /// It is provably not: see [`OnMainUnknown::MainIndexStale`].
    Stale,
    /// Another scanner version wrote it: see
    /// [`OnMainUnknown::MainIndexOtherScanner`].
    OtherScanner,
}

/// Mark each pairing-carrying finding in `pr` with whether main has it.
///
/// `true` when, for every site group the finding counts in, main has at least
/// as many sites as the PR. Otherwise `false` only when main's copy is current
/// and main's side judged at least one group the PR has more sites in; else
/// the run could not tell, and the finding carries why. A finding with no
/// pairing is left as it is.
///
/// Main's sites are its recomputed findings, plus each stored mismatch at a
/// site the recomputation reached no resolved answer for. Where the
/// recomputation did resolve the site, its answer is judged against the peers
/// as they are now and replaces the stored one: a producer that fixed the
/// contract since main's last scan makes main's stored mismatch history, and a
/// PR that breaks the call again introduces the break.
pub fn mark_on_main(pr: &mut [Finding], main: &MainSide, copy: MainCopy) {
    let fresh: HashSet<(&str, String)> = main
        .recomputed
        .iter()
        .filter(|site| site.resolved)
        .map(|site| (site.pair.as_str(), site_id(&site.site)))
        .collect();
    let theirs = sites_by_group(
        main.findings.iter().flat_map(groups).chain(
            main.stored
                .iter()
                .filter(|site| site.incompatible)
                .filter(|site| !fresh.contains(&(site.pair.as_str(), site_id(&site.site))))
                .map(|site| (site.group(), site_id(&site.site))),
        ),
    );
    let ours = sites_by_group(pr.iter().flat_map(groups));
    let count = |sites: &HashMap<SiteGroup, HashSet<String>>, group: &SiteGroup| {
        sites.get(group).map_or(0, HashSet::len)
    };
    for finding in pr.iter_mut() {
        let groups: Vec<SiteGroup> = groups(finding).into_iter().map(|(g, _)| g).collect();
        if groups.is_empty() {
            continue;
        }
        let short: Vec<&SiteGroup> = groups
            .iter()
            .filter(|group| count(&ours, group) > count(&theirs, group))
            .collect();
        if short.is_empty() {
            finding.set_on_main(Some(true));
        } else if copy == MainCopy::Stale {
            finding.set_on_main_unknown(OnMainUnknown::MainIndexStale);
        } else if copy == MainCopy::OtherScanner {
            finding.set_on_main_unknown(OnMainUnknown::MainIndexOtherScanner);
        } else if short.iter().any(|group| judged(main, group)) {
            finding.set_on_main(Some(false));
        } else {
            finding.set_on_main_unknown(OnMainUnknown::MainTypesUnjudged);
        }
    }
}

/// Whether main's side judged a group the PR has more sites in, so that main
/// provably lacks at least one of them.
///
/// A method mismatch is recomputed from main's copy exactly as the PR's was.
/// A type mismatch group is judged when every distinct site main has in it
/// carries at least one resolved answer, from either source, or when main has
/// no site in it at all and its recomputed type check ran: then the PR added
/// the pairing. One unresolved site is enough to leave the group unjudged,
/// because the break the PR shows there may be one main has too; typically a
/// response the PR run judged by retyping the consumer's own code, which main's
/// recomputation cannot do.
fn judged(main: &MainSide, group: &SiteGroup) -> bool {
    if group.0 != TYPE_MISMATCH {
        return true;
    }
    let mut resolved_at: HashMap<String, bool> = HashMap::new();
    for site in main
        .recomputed
        .iter()
        .chain(main.stored.iter())
        .filter(|site| site.group() == *group)
    {
        *resolved_at.entry(site_id(&site.site)).or_default() |= site.resolved;
    }
    if resolved_at.is_empty() {
        return main.types_judged;
    }
    resolved_at.values().all(|resolved| *resolved)
}

/// Mark every pairing-carrying finding as not comparable, and why.
pub fn mark_all_unknown(pr: &mut [Finding], reason: OnMainUnknown) {
    for finding in pr.iter_mut() {
        if finding.pair().is_some() {
            finding.set_on_main_unknown(reason);
        }
    }
}

/// Mark every pairing-carrying finding as on main, for a PR whose scanned
/// surface is main's: the same inputs against the same peers yield the same
/// findings, so there is nothing to run.
pub fn mark_all_on_main(pr: &mut [Finding]) {
    for finding in pr.iter_mut() {
        if finding.pair().is_some() {
            finding.set_on_main(Some(true));
        }
    }
}

/// Whether any finding carries a pairing to compare. Without one, the
/// comparison could mark nothing and is not run.
pub fn has_anything_to_mark(pr: &[Finding]) -> bool {
    pr.iter().any(|finding| finding.pair().is_some())
}

/// Main's stored verdicts, one [`MainTypeSite`] per call site and direction.
///
/// Main's own scan wrote these with main's sources on disk, so they hold the
/// answers a recomputation from the stored copy cannot reach: a response the
/// scan judged by retyping the consumer's call (carrick#1491). A row without
/// per-site answers (a scanner older than carrick#1385) places nothing.
pub fn stored_type_sites(main_self: &[CloudRepoData]) -> Vec<MainTypeSite> {
    let mut sites = Vec::new();
    for verdict in main_self
        .iter()
        .flat_map(|repo| repo.compat_verdicts.iter().flatten())
    {
        for site in &verdict.sites {
            let location = crate::analyzer::repo_relative_location(&site.consumer_location);
            let file = site_file(&location);
            for (kind, answer) in [
                (ManifestTypeKind::Request, &site.request),
                (ManifestTypeKind::Response, &site.response),
            ] {
                let Some(answer) = answer else { continue };
                let Some(pair) = crate::analyzer::stored_type_pair_key(
                    &verdict.producer_repo,
                    &verdict.producer_key,
                    &verdict.consumer_repo,
                    &file,
                    kind,
                ) else {
                    continue;
                };
                sites.push(stored_site(pair, location.clone(), answer));
            }
        }
    }
    sites
}

fn stored_site(pair: String, site: String, answer: &DirectionVerdict) -> MainTypeSite {
    MainTypeSite {
        pair,
        site,
        incompatible: answer.verdict == TypeVerdict::Incompatible,
        resolved: answer.resolved,
    }
}

/// Whether main's copy is provably not main as this PR's base has it.
///
/// Stale when a stored service was scanned from a working tree with
/// uncommitted changes, or at a commit `differs` says has changes against the
/// base. A copy at another commit this clone cannot compare, such as the head
/// of a squash-merged branch that a laptop index scanned, is not shown to
/// differ and counts as current: its verdicts are still main's latest answer.
///
/// Otherwise, a copy another scanner version wrote counts as current only when
/// `read_alike` says that scanner read the files this PR left alone as this
/// run did ([`untouched_reading`], carrick#1530). Its extraction and type check
/// may differ from this run's, and the files both scanners read unchanged are
/// the evidence of whether they do. It is asked only for such a copy.
pub fn main_copy(
    main_self: &[CloudRepoData],
    base: Option<&str>,
    differs: impl Fn(&str, &str) -> Option<bool>,
    read_alike: impl FnOnce() -> bool,
) -> MainCopy {
    let stale = main_self.iter().any(|repo| {
        repo.dirty == Some(true)
            || base.is_some_and(|base| {
                repo.commit_hash != base && differs(&repo.commit_hash, base) == Some(true)
            })
    });
    let this_version = env!("CARGO_PKG_VERSION");
    let other_scanner = main_self
        .iter()
        .any(|repo| repo.scanner_version.as_deref() != Some(this_version));
    if stale {
        MainCopy::Stale
    } else if other_scanner && !read_alike() {
        MainCopy::OtherScanner
    } else {
        MainCopy::Current
    }
}

/// What the files a PR left alone say about a copy of main that another
/// scanner version wrote (carrick#1530).
///
/// Carrick releases most days and main's copy keeps the version of its last
/// scan, so a version test alone leaves every PR uncompared from each release
/// until main is scanned again. The PR run has its own reading of every file
/// and main's copy has the other scanner's. Where the PR changed nothing, both
/// read the same bytes, so a difference between the two readings is a
/// difference between the scanners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UntouchedReading {
    /// Both scanners read every row in those files alike.
    Alike {
        /// Operation and type rows compared.
        rows: usize,
        /// Files those rows sit in.
        files: usize,
    },
    /// The first place the two readings part.
    Differ {
        /// The service, when the repo has more than one.
        service: Option<String>,
        /// The file, or empty for a fact about the whole service.
        file: String,
        /// What differs there.
        part: &'static str,
    },
    /// No row either side holds sits in a file the PR left alone, so nothing
    /// vouches for main's copy.
    NothingToCompare,
    /// Which files the PR left alone is not known, and why.
    Unknown(String),
}

impl UntouchedReading {
    pub fn alike(&self) -> bool {
        matches!(self, Self::Alike { .. })
    }
}

/// One part of a service's reading: the file it describes (empty for a fact
/// about the whole service) and what it holds.
type Part = (String, &'static str);

/// A service's reading of the files a PR left alone, each part as the sorted
/// JSON of its rows, so two scans that emit the same rows in another order
/// read alike.
#[derive(Default, PartialEq)]
struct Reading {
    parts: BTreeMap<Part, Vec<String>>,
    /// Endpoint, call and type manifest rows.
    rows: usize,
}

impl Reading {
    fn add(&mut self, file: String, part: &'static str, row: impl Serialize) {
        let row = serde_json::to_value(row)
            .map(|value| value.to_string())
            .unwrap_or_default();
        self.parts.entry((file, part)).or_default().push(row);
    }

    fn sorted(mut self) -> Self {
        for rows in self.parts.values_mut() {
            rows.sort();
        }
        self
    }

    /// The first part the two readings hold differently.
    fn first_difference<'a>(&'a self, other: &'a Self) -> Option<&'a Part> {
        self.parts
            .keys()
            .chain(other.parts.keys())
            .filter(|part| self.parts.get(*part) != other.parts.get(*part))
            .min()
    }

    fn files(&self) -> impl Iterator<Item = &str> {
        self.parts
            .keys()
            .map(|(file, _)| file.as_str())
            .filter(|file| !file.is_empty())
    }
}

/// One service's reading of the files in `untouched`: what main's side of the
/// comparison computes findings from.
///
/// Endpoints, calls, the type manifest and the mount graph, row by row, and
/// for each manifest alias its declaration and record in the capture stub,
/// which is what the type check judges. A mount edge belongs to no file and is
/// read when neither router it joins sits in a file the PR changed. Left out:
/// intents, signatures and function definitions, which change with prompts and
/// feed no finding, and the package and config files, which are what a scanner
/// reads rather than how it reads them.
fn untouched_reading_of(data: &CloudRepoData, untouched: &HashSet<String>) -> Reading {
    let mut reading = Reading::default();
    let unchanged = |location: &str| -> Option<String> {
        let file = site_file(location);
        untouched.contains(&file).then_some(file)
    };

    for (part, ops) in [("endpoints", &data.endpoints), ("calls", &data.calls)] {
        for op in ops {
            let Some(file) = unchanged(&op.file_path.to_string_lossy()) else {
                continue;
            };
            let mut op = op.clone();
            strip_parsed_types(&mut op);
            reading.add(file, part, op);
            reading.rows += 1;
        }
    }

    let mut aliases: Vec<(String, &str)> = Vec::new();
    for entry in data.type_manifest.iter().flatten() {
        let Some(file) = unchanged(&entry.file_path) else {
            continue;
        };
        aliases.push((file.clone(), &entry.type_alias));
        reading.add(file, "type manifest", entry);
        reading.rows += 1;
    }

    if let Some(graph) = &data.mount_graph {
        for (name, node) in &graph.nodes {
            if let Some(file) = unchanged(&node.file_location) {
                reading.add(file, "mount graph nodes", (name, node));
            }
        }
        for endpoint in &graph.endpoints {
            if let Some(file) = unchanged(&endpoint.file_location) {
                reading.add(file, "mount graph endpoints", endpoint);
            }
        }
        for call in &graph.data_calls {
            if let Some(file) = unchanged(&call.file_location) {
                reading.add(file, "mount graph calls", call);
            }
        }
        let changed = |router: &str| {
            graph
                .nodes
                .get(router)
                .is_some_and(|node| unchanged(&node.file_location).is_none())
        };
        for edge in &graph.mounts {
            if !changed(&edge.parent) && !changed(&edge.child) {
                reading.add(String::new(), "mounts", edge);
            }
        }
        for mount in &data.mounts {
            let name = |owner: &crate::visitor::OwnerType| match owner {
                crate::visitor::OwnerType::App(name) | crate::visitor::OwnerType::Router(name) => {
                    name.clone()
                }
            };
            if !changed(&name(&mount.parent)) && !changed(&name(&mount.child)) {
                reading.add(String::new(), "mounts", mount);
            }
        }
    } else {
        for mount in &data.mounts {
            reading.add(String::new(), "mounts", mount);
        }
    }

    if let Some(stub) = &data.capture_stub {
        let stub = StubReading::of(stub);
        for (file, alias) in aliases {
            reading.add(
                file.clone(),
                "type declarations",
                (alias, stub.declarations.get(alias)),
            );
            reading.add(file, "capture records", (alias, stub.records.get(alias)));
        }
        for (name, text) in stub.unread {
            reading.add(String::new(), "capture stub", (name, text));
        }
    }
    reading.add(
        String::new(),
        "type extraction",
        (
            &data.types_degraded,
            &data.type_extraction_status,
            data.capture_stub.as_ref().map(|stub| stub.artifact_version),
        ),
    );
    reading.sorted()
}

/// The capture stub, alias by alias: each alias's declaration text and its
/// record in `carrick-manifest.json`. A file that does not parse is kept whole
/// in `unread`, so it is compared in full rather than skipped.
struct StubReading<'a> {
    declarations: HashMap<String, Vec<String>>,
    records: HashMap<String, serde_json::Value>,
    unread: Vec<(&'a str, &'a str)>,
}

impl<'a> StubReading<'a> {
    fn of(stub: &'a crate::cloud_storage::CaptureStubArtifact) -> Self {
        let mut reading = Self {
            declarations: HashMap::new(),
            records: HashMap::new(),
            unread: Vec::new(),
        };
        for (name, text) in &stub.files {
            if name.ends_with(".d.ts") {
                match declarations_by_name(text) {
                    Some(found) => {
                        for (alias, declaration) in found {
                            reading
                                .declarations
                                .entry(alias)
                                .or_default()
                                .push(declaration);
                        }
                    }
                    None => reading.unread.push((name, text)),
                }
            } else if name == "carrick-manifest.json" {
                let records = serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|manifest| manifest.get("aliases")?.as_array().cloned());
                match records {
                    Some(records) => {
                        for record in records {
                            if let Some(alias) = record.get("alias").and_then(|a| a.as_str()) {
                                reading.records.insert(alias.to_string(), record.clone());
                            }
                        }
                    }
                    None => reading.unread.push((name, text)),
                }
            }
        }
        for declarations in reading.declarations.values_mut() {
            declarations.sort();
        }
        reading
    }
}

/// Every top-level type alias and interface a declaration file declares, by
/// name, with its source text. `None` when the file does not parse.
fn declarations_by_name(text: &str) -> Option<Vec<(String, String)>> {
    use swc_common::{FileName, SourceMap, SourceMapper, Spanned, sync::Lrc};
    use swc_ecma_ast::{Decl, ModuleDecl, ModuleItem, Stmt};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

    let source_map: Lrc<SourceMap> = Default::default();
    let file = source_map.new_source_file(
        Lrc::new(FileName::Custom("capture-stub.d.ts".into())),
        text.to_string(),
    );
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax {
            dts: true,
            ..Default::default()
        }),
        Default::default(),
        StringInput::from(&*file),
        None,
    );
    let module = Parser::new_from(lexer).parse_module().ok()?;
    let mut found = Vec::new();
    for item in &module.body {
        let decl = match item {
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            _ => continue,
        };
        let name = match decl {
            Decl::TsTypeAlias(alias) => alias.id.sym.to_string(),
            Decl::TsInterface(interface) => interface.id.sym.to_string(),
            _ => continue,
        };
        found.push((name, source_map.span_to_snippet(item.span()).ok()?));
    }
    Some(found)
}

/// Whether the other scanner that wrote `main` read the files in `untouched`
/// (the files this PR left alone, repo-relative) as this run did, service for
/// service. `current` is this run's services in the form they upload in.
pub fn untouched_reading(
    current: &[CloudRepoData],
    main: &[CloudRepoData],
    untouched: &HashSet<String>,
) -> UntouchedReading {
    let services = |repos: &[CloudRepoData]| {
        let mut names: Vec<Option<String>> =
            repos.iter().map(|repo| repo.service_name.clone()).collect();
        names.sort();
        names
    };
    if services(current) != services(main) {
        return UntouchedReading::Differ {
            service: None,
            file: String::new(),
            part: "services",
        };
    }
    let mut rows = 0;
    let mut files: HashSet<String> = HashSet::new();
    for ours in current {
        let Some(theirs) = main
            .iter()
            .find(|repo| repo.service_name == ours.service_name)
        else {
            continue;
        };
        let ours_read = untouched_reading_of(ours, untouched);
        let theirs_read = untouched_reading_of(theirs, untouched);
        if let Some((file, part)) = ours_read.first_difference(&theirs_read) {
            return UntouchedReading::Differ {
                service: (current.len() > 1)
                    .then(|| ours.service_name.clone())
                    .flatten(),
                file: file.clone(),
                part,
            };
        }
        rows += ours_read.rows;
        files.extend(ours_read.files().map(str::to_string));
    }
    if rows == 0 {
        UntouchedReading::NothingToCompare
    } else {
        UntouchedReading::Alike {
            rows,
            files: files.len(),
        }
    }
}

/// The upload strips the parsed type nodes (`strip_ast_nodes`), so main's
/// stored copy never has them and this run's always does.
fn strip_parsed_types(row: &mut ApiEndpointDetails) {
    row.request_type = None;
    row.response_type = None;
}

/// The fields of a stored service that describe the run rather than the code:
/// when it ran, at which commit, what it cached, and the cross-repo facts it
/// attached on upload. Everything else is an input to the findings. The list
/// errs toward keeping a field: an unlisted run fact only makes two surfaces
/// differ, and the comparison then runs as it would have.
fn surface(data: &CloudRepoData) -> serde_json::Value {
    let mut data = data.clone();
    data.last_updated = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;
    data.commit_hash = String::new();
    data.dirty = None;
    data.file_results = None;
    data.cached_detection = None;
    data.cached_guidance = None;
    data.cached_extraction_config = None;
    data.package_json_hash = None;
    data.cache_version = None;
    data.compat_verdicts = None;
    data.sdk_edges = None;
    data.sdk_unresolved = None;
    data.boundary = None;
    for row in data.endpoints.iter_mut().chain(data.calls.iter_mut()) {
        strip_parsed_types(row);
    }
    // Intents describe functions for search; no finding reads them.
    for definition in data.function_definitions.values_mut() {
        definition.intent = None;
        definition.body_source = None;
    }
    serde_json::to_value(&data).unwrap_or(serde_json::Value::Null)
}

/// Whether this run scanned exactly what main's stored copy describes, service
/// for service. Then main's findings are this run's, and the comparison has
/// nothing to add.
pub fn same_surface(current: &[CloudRepoData], main: &[CloudRepoData]) -> bool {
    if current.len() != main.len() {
        return false;
    }
    let keyed = |repos: &[CloudRepoData]| {
        let mut rows: Vec<(Option<String>, serde_json::Value)> = repos
            .iter()
            .map(|repo| (repo.service_name.clone(), surface(repo)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    };
    let ours = keyed(current);
    ours.iter().all(|(_, value)| !value.is_null()) && ours == keyed(main)
}

/// How many times this process ran main's side of the comparison. Read by the
/// tests that prove a skip.
static MAIN_SIDE_RUNS: AtomicUsize = AtomicUsize::new(0);

pub fn record_main_side_run() {
    MAIN_SIDE_RUNS.fetch_add(1, Ordering::SeqCst);
}

#[allow(dead_code)] // Called by tests/ through the library, never by the binary.
pub fn main_side_runs() -> usize {
    MAIN_SIDE_RUNS.load(Ordering::SeqCst)
}

/// A way for main's side of the comparison to fail, injected by a test.
#[allow(dead_code)] // Constructed by tests/ through the library, never by the binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainSideFault {
    /// The analysis returns an error.
    Error,
    /// The analysis panics.
    Panic,
    /// The analysis never finishes.
    Hang,
}

static FAULT: Mutex<Option<MainSideFault>> = Mutex::new(None);

/// Make the next run of main's side of the comparison fail as `fault`.
/// Honoured only under `CARRICK_MOCK_ALL`, like
/// [`crate::agent_service::inject_mock_failure`].
#[allow(dead_code)] // Called by tests/ through the library, never by the binary.
pub fn inject_mock_main_side_fault(fault: MainSideFault) {
    *FAULT.lock().unwrap_or_else(|p| p.into_inner()) = Some(fault);
}

/// The injected fault, taken once. Always `None` outside `CARRICK_MOCK_ALL`.
pub fn take_mock_main_side_fault() -> Option<MainSideFault> {
    if std::env::var("CARRICK_MOCK_ALL").is_err() {
        return None;
    }
    FAULT.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// A caught panic's message, for the one line that reports it.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::ManifestTypeKind;
    use serde_json::json;

    fn type_mismatch(pair: &str, site: &str) -> Finding {
        Finding::type_mismatch(
            "POST",
            "/orders",
            None,
            vec![site.to_string()],
            "Order",
            "OrderInput",
            "Property 'total' is missing",
        )
        .with_direction(Some(ManifestTypeKind::Request))
        .with_pair(Some(pair.to_string()))
    }

    fn method_mismatch(pair: &str, sites: &[&str]) -> Finding {
        Finding::method_mismatch(
            "PUT",
            "/orders/:id",
            None,
            sites.iter().map(|s| s.to_string()).collect(),
            "POST",
        )
        .with_pair(Some(pair.to_string()))
    }

    fn on_main(finding: &Finding) -> Option<bool> {
        match finding {
            Finding::TypeMismatch { on_main, .. } | Finding::MethodMismatch { on_main, .. } => {
                *on_main
            }
            _ => None,
        }
    }

    fn unknown(finding: &Finding) -> Option<OnMainUnknown> {
        match finding {
            Finding::TypeMismatch {
                on_main_unknown, ..
            }
            | Finding::MethodMismatch {
                on_main_unknown, ..
            } => *on_main_unknown,
            _ => None,
        }
    }

    /// Main's side as the recomputation alone yields it, its type check run.
    fn main_side(findings: Vec<Finding>) -> MainSide {
        MainSide {
            findings,
            types_judged: true,
            ..MainSide::default()
        }
    }

    const ORDERS: &str = "orders-api|POST|/orders~web|src/checkout.ts|request";

    /// The smoke-test PR's shape: it edited the consumer file above an
    /// existing broken call, so the line moved. The pairing did not.
    #[test]
    fn a_finding_whose_line_moved_is_on_main() {
        let main = main_side(vec![type_mismatch(ORDERS, "src/checkout.ts:40")]);
        let mut pr = vec![type_mismatch(ORDERS, "src/checkout.ts:52")];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(true));
    }

    #[test]
    fn a_pairing_main_does_not_have_is_introduced() {
        let main = main_side(vec![type_mismatch(ORDERS, "src/checkout.ts:40")]);
        let other = "orders-api|POST|/orders~web|src/cart.ts|request";
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            type_mismatch(other, "src/cart.ts:9"),
        ];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(true));
        assert_eq!(on_main(&pr[1]), Some(false));
    }

    /// The same kind is part of the identity: a method mismatch on main does
    /// not cover a type mismatch that happens to carry the same key.
    #[test]
    fn the_kind_is_part_of_the_identity() {
        let main = main_side(vec![method_mismatch(ORDERS, &["src/checkout.ts:40"])]);
        let mut pr = vec![type_mismatch(ORDERS, "src/checkout.ts:40")];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
    }

    /// A second broken call on the same pairing in the same file cannot be
    /// told apart from the first once lines are dropped, so both read as
    /// introduced rather than letting the new one pass.
    #[test]
    fn a_second_broken_call_in_the_same_file_fails_the_whole_group() {
        let main = main_side(vec![type_mismatch(ORDERS, "src/checkout.ts:40")]);
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            type_mismatch(ORDERS, "src/checkout.ts:88"),
        ];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
        assert_eq!(on_main(&pr[1]), Some(false));
    }

    /// A method mismatch aggregates every consumer site of one wrong verb, so
    /// a new file sending it makes the finding introduced.
    #[test]
    fn a_method_mismatch_that_gained_a_file_is_introduced() {
        let pair = "POST /orders/:id~PUT";
        let main = main_side(vec![method_mismatch(pair, &["src/a.ts:3"])]);
        let mut same = vec![method_mismatch(pair, &["src/a.ts:7"])];
        mark_on_main(&mut same, &main, MainCopy::Current);
        assert_eq!(on_main(&same[0]), Some(true));

        let mut grew = vec![method_mismatch(pair, &["src/a.ts:7", "src/b.ts:1"])];
        mark_on_main(&mut grew, &main, MainCopy::Current);
        assert_eq!(on_main(&grew[0]), Some(false));
    }

    #[test]
    fn a_finding_without_a_pairing_is_left_unmarked() {
        let main = main_side(vec![type_mismatch(ORDERS, "src/checkout.ts:40")]);
        let mut pr = vec![
            Finding::type_mismatch("POST", "/orders", None, vec![], "A", "B", "x"),
            Finding::missing_endpoint("GET", "/gone", None, vec!["src/x.ts:1".into()]),
        ];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), None);
        assert_eq!(unknown(&pr[0]), None);
        let wire = serde_json::to_value(&pr[1]).unwrap();
        assert!(!wire.as_object().unwrap().contains_key("on_main"));
        assert!(!wire.as_object().unwrap().contains_key("on_main_unknown"));
    }

    /// Main has no type answer at all (its recomputed check did not run and
    /// its copy stores no verdicts), so a PR type mismatch main lacks cannot
    /// be called introduced. Method mismatches need no type check and are
    /// still compared.
    #[test]
    fn a_type_mismatch_main_has_no_type_answer_for_is_not_called_introduced() {
        let pair = "POST /orders/:id~PUT";
        let main = MainSide {
            findings: vec![method_mismatch(pair, &["src/a.ts:3"])],
            types_judged: false,
            ..MainSide::default()
        };
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            method_mismatch(pair, &["src/a.ts:3"]),
        ];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), None);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::MainTypesUnjudged));
        assert_eq!(on_main(&pr[1]), Some(true));
    }

    // --- carrick-cloud#1408: main's stored verdicts, and a stale copy ---

    /// A response pairing, with a parameterised route whose name differs
    /// between the producer's manifest and the edge's producer key.
    const RULES: &str =
        "rules-api|GET|/rules/:param/holidays~rules-web|src/screens/rules.tsx|response";

    fn retyped(site: &str) -> Finding {
        Finding::type_mismatch(
            "GET",
            "/rules/:ruleId/holidays",
            None,
            vec![site.to_string()],
            "Holiday[]",
            "GET /rules/:ruleId/holidays → Response",
            "the consumer uses what the producer's response does not provide",
        )
        .with_direction(Some(ManifestTypeKind::Response))
        .with_pair(Some(RULES.to_string()))
    }

    /// Main's stored copy of the consumer, as main's own scan wrote it: one
    /// verdict row for the pairing, answered at `site` for the response.
    fn main_copy_with(site: &str, verdict: &str, resolved: bool) -> CloudRepoData {
        let answer = json!({ "verdict": verdict, "resolved": resolved });
        serde_json::from_value(json!({
            "repo_name": "rules-web",
            "service_name": "rules-web",
            "endpoints": [], "calls": [], "mounts": [], "apps": {},
            "imported_handlers": [], "function_definitions": {},
            "last_updated": "2026-09-25T14:23:32Z",
            "commit_hash": "aaaa1111",
            "scanner_version": env!("CARGO_PKG_VERSION"),
            "compat_verdicts": [{
                "producer_repo": "rules-api",
                "producer_key": "http|GET|/rules/:calendarId/holidays",
                "consumer_repo": "rules-web",
                "consumer_key": "http|GET|/rules/:calendarId/holidays",
                "response": answer,
                "sites": [{ "consumer_location": site, "response": answer }],
                "scanner_version": env!("CARGO_PKG_VERSION")
            }]
        }))
        .expect("a stored blob deserializes")
    }

    /// The recomputation's answer at the site: the stored copy carries no
    /// sources, so the retype cannot run and the consumer's side is unknown.
    fn recomputed_unresolved(site: &str) -> MainTypeSite {
        MainTypeSite {
            pair: RULES.to_string(),
            site: site.to_string(),
            incompatible: false,
            resolved: false,
        }
    }

    fn main_with_stored(copy: &CloudRepoData, site: &str) -> MainSide {
        MainSide {
            findings: Vec::new(),
            recomputed: vec![recomputed_unresolved(site)],
            stored: stored_type_sites(std::slice::from_ref(copy)),
            types_judged: true,
        }
    }

    /// The smoke test's misattributed rows (carrick-cloud#1408): the PR run
    /// found the mismatch by retyping the consumer's own code, which the
    /// recomputation from main's stored copy cannot do. Main's own scan did,
    /// and stored it. That stored answer is main's, so the finding is on main.
    #[test]
    fn a_retyped_mismatch_main_stored_is_on_main() {
        let site = "src/screens/rules.tsx:184";
        let main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(true));

        // Without main's stored answer the recomputation alone cannot judge
        // the pairing, and the run says so rather than blame the PR.
        let recomputed_only = MainSide {
            recomputed: vec![recomputed_unresolved(site)],
            types_judged: true,
            ..MainSide::default()
        };
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &recomputed_only, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), None);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::MainTypesUnjudged));
    }

    /// The smoke test's one real row: main's scan retyped the call and found
    /// it compatible; the PR broke it. Introduced.
    #[test]
    fn a_retyped_mismatch_main_stored_as_compatible_is_introduced() {
        let site = "src/screens/rules.tsx:259";
        let main = main_with_stored(&main_copy_with(site, "compatible", true), site);
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
    }

    /// Main's scan reached the pair and could not verify it either. Whether
    /// the PR's edit made it verifiable or main's scan fell short, the run
    /// cannot tell.
    #[test]
    fn a_mismatch_main_stored_as_unverifiable_is_not_compared() {
        let site = "src/screens/rules.tsx:75";
        let main = main_with_stored(&main_copy_with(site, "unverifiable", false), site);
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::MainTypesUnjudged));
    }

    /// Review of carrick#1525, F1: main's stored mismatch was judged against
    /// the producer as it was then. Where main's recomputation resolves the
    /// same site against the producer as it is now, that answer wins: the
    /// producer fixed the contract, and a PR that breaks the call again
    /// introduces the break.
    #[test]
    fn a_resolved_recomputation_overrules_a_stored_mismatch() {
        let site = "src/screens/rules.tsx:184";
        let mut main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        main.recomputed[0].resolved = true;
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
    }

    /// Review of carrick#1525, F2: a group is judged only when every site main
    /// has in it carries a resolved answer. One site main's recomputation
    /// could not resolve and nothing stored may be broken on main too.
    #[test]
    fn one_unjudged_site_leaves_the_group_unjudged() {
        let judged = "src/screens/rules.tsx:184";
        let unjudged = "src/screens/rules.tsx:300";
        let mut main = main_with_stored(&main_copy_with(judged, "compatible", true), judged);
        main.recomputed.push(recomputed_unresolved(unjudged));
        let mut pr = vec![retyped(judged), retyped(unjudged)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::MainTypesUnjudged));
        assert_eq!(unknown(&pr[1]), Some(OnMainUnknown::MainTypesUnjudged));

        // Main's type check did not run and it has no site in the group: the
        // run cannot tell either, stored rows elsewhere notwithstanding.
        let elsewhere = main_copy_with("src/screens/other.tsx:1", "compatible", true);
        let main = MainSide {
            stored: stored_type_sites(std::slice::from_ref(&elsewhere)),
            types_judged: false,
            ..MainSide::default()
        };
        let mut pr = vec![retyped(judged)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::MainTypesUnjudged));
    }

    /// One site both sources report is one broken call: a second broken call
    /// the PR adds in the same file still reads as introduced.
    #[test]
    fn a_site_both_sources_report_counts_once() {
        let site = "src/screens/rules.tsx:184";
        let added = "src/screens/rules.tsx:300";
        let mut main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        // The recomputation found it too, over a type it could not resolve all
        // the way down, and judged the site of the PR's new call compatible.
        main.findings = vec![retyped(site)];
        main.recomputed[0].incompatible = true;
        main.recomputed.push(MainTypeSite {
            pair: RULES.to_string(),
            site: added.to_string(),
            incompatible: false,
            resolved: true,
        });
        // Counted twice, main would have two sites to the PR's two.
        let mut pr = vec![retyped(site), retyped(added)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
        assert_eq!(on_main(&pr[1]), Some(false));

        // Without the added call, the one site counts once on each side.
        let mut pr = vec![retyped(site)];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(true));
    }

    /// A stale copy, or one another scanner version wrote, never yields
    /// `false`: what main's copy lacks may have reached main after it, or may
    /// be a difference between two scanners. What it has is still on main.
    #[test]
    fn a_stale_copy_never_calls_a_finding_introduced() {
        let site = "src/screens/rules.tsx:184";
        let main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        let other = "rules-api|GET|/rules/:param/holidays~rules-web|src/screens/other.tsx|response";
        for (copy, reason) in [
            (MainCopy::Stale, OnMainUnknown::MainIndexStale),
            (MainCopy::OtherScanner, OnMainUnknown::MainIndexOtherScanner),
        ] {
            let mut pr = vec![
                retyped(site),
                retyped("src/screens/other.tsx:9").with_pair(Some(other.to_string())),
            ];
            mark_on_main(&mut pr, &main, copy);
            assert_eq!(on_main(&pr[0]), Some(true));
            assert_eq!(unknown(&pr[1]), Some(reason));
        }
    }

    /// A copy at another commit this clone cannot compare is main's latest
    /// index and is compared with: the laptop index a new project starts
    /// from scans a branch head that squash-merges to another commit. A copy
    /// at a commit this clone shows to differ from the base and a dirty copy
    /// are stale. A copy another scanner version wrote is told apart unless
    /// that scanner read the files the PR left alone as this run did
    /// (carrick#1530), and that is asked only of such a copy.
    #[test]
    fn which_copies_are_stale() {
        let copy = main_copy_with("src/screens/rules.tsx:1", "compatible", true);
        let cannot_compare = |_: &str, _: &str| None;
        let differs = |_: &str, _: &str| Some(true);
        let same_tree = |_: &str, _: &str| Some(false);
        let never_asked = || panic!("a copy this scanner version wrote needs no evidence");
        let one = std::slice::from_ref(&copy);
        assert_eq!(
            main_copy(one, Some("bbbb2222"), cannot_compare, never_asked),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(one, Some("bbbb2222"), same_tree, never_asked),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(one, Some("aaaa1111"), differs, never_asked),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(one, None, differs, never_asked),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(one, Some("bbbb2222"), differs, never_asked),
            MainCopy::Stale
        );

        let mut dirty = copy.clone();
        dirty.dirty = Some(true);
        assert_eq!(
            main_copy(
                std::slice::from_ref(&dirty),
                None,
                cannot_compare,
                never_asked
            ),
            MainCopy::Stale
        );
        let mut other_version = copy.clone();
        other_version.scanner_version = Some("0.0.1".to_string());
        let other = std::slice::from_ref(&other_version);
        assert_eq!(
            main_copy(other, None, cannot_compare, || false),
            MainCopy::OtherScanner
        );
        assert_eq!(
            main_copy(other, None, cannot_compare, || true),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(other, Some("bbbb2222"), differs, || true),
            MainCopy::Stale
        );
    }

    // --- carrick#1530: another scanner's copy, judged on the untouched files ---

    /// One call row, as a stored copy or this run's upload holds it.
    fn call_row(file: &str, line: u32, method: &str, path: &str) -> serde_json::Value {
        json!({
            "owner": null,
            "key": { "protocol": "http", "method": method, "path": path },
            "params": [], "request_body": null, "response_body": null,
            "handler_name": null, "request_type": null, "response_type": null,
            "file_path": format!("{file}:{line}")
        })
    }

    /// One consumer manifest row for the call at `file:line`.
    fn manifest_row(file: &str, line: u32, alias: &str) -> serde_json::Value {
        json!({
            "protocol": "http", "method": "GET", "path": "/orders",
            "role": "consumer", "type_kind": "response", "type_alias": alias,
            "file_path": file, "line_number": line,
            "is_explicit": false, "type_state": "implicit",
            "evidence": {
                "file_path": file, "line_number": line, "infer_kind": "response_body",
                "is_explicit": false, "type_state": "implicit"
            }
        })
    }

    /// A consumer service: a call and its manifest row in each of `files`,
    /// each alias declared in the capture stub as `declared` says.
    fn service(scanner: &str, files: &[(&str, &str)]) -> CloudRepoData {
        let alias = |file: &str| format!("Endpoint_orders_Response_Call{}", file.len());
        let surface: String = files
            .iter()
            .map(|(file, declared)| format!("export type {} = {declared};\n", alias(file)))
            .collect();
        let records: Vec<serde_json::Value> = files
            .iter()
            .map(|(file, _)| json!({ "alias": alias(file), "source_file": file, "self_check": "ok" }))
            .collect();
        serde_json::from_value(json!({
            "repo_name": "web",
            "service_name": "web",
            "endpoints": [],
            "calls": files.iter().map(|(file, _)| call_row(file, 3, "GET", "/orders")).collect::<Vec<_>>(),
            "mounts": [], "apps": {}, "imported_handlers": [],
            "function_definitions": {},
            "last_updated": "2026-09-27T00:00:00Z",
            "commit_hash": "aaaa1111",
            "scanner_version": scanner,
            "type_manifest": files.iter().map(|(file, _)| manifest_row(file, 3, &alias(file))).collect::<Vec<_>>(),
            "capture_stub": {
                "artifact_version": 2, "package_name": "@carrick/web", "ts_version": "5.9.3",
                "bare_checkout": false,
                "files": {
                    "types/surface.d.ts": surface,
                    "carrick-manifest.json": json!({ "aliases": records }).to_string()
                }
            }
        }))
        .expect("a stored service deserializes")
    }

    fn untouched(files: &[&str]) -> HashSet<String> {
        files.iter().map(|file| file.to_string()).collect()
    }

    const CART: &str = "src/cart.ts";
    const CHECKOUT: &str = "src/checkout.ts";
    const ORDER: &str = "{ id: string; total: number }";

    /// The PR changed the checkout file and left the cart file alone. The
    /// other scanner read the cart file as this run did, so its copy counts,
    /// whatever either says about the checkout file.
    #[test]
    fn another_scanner_that_reads_the_untouched_files_alike_is_compared_with() {
        let main = service("0.0.1", &[(CART, ORDER), (CHECKOUT, "unknown")]);
        let pr = service(
            env!("CARGO_PKG_VERSION"),
            &[(CART, ORDER), (CHECKOUT, "{ id: string }")],
        );
        let reading = untouched_reading(
            std::slice::from_ref(&pr),
            std::slice::from_ref(&main),
            &untouched(&[CART]),
        );
        assert_eq!(reading, UntouchedReading::Alike { rows: 2, files: 1 });

        // The same rows emitted in another order still read alike.
        let mut reordered = pr.clone();
        reordered.calls.reverse();
        reordered.type_manifest.as_mut().unwrap().reverse();
        let reading = untouched_reading(
            std::slice::from_ref(&reordered),
            std::slice::from_ref(&main),
            &untouched(&[CART]),
        );
        assert!(reading.alike(), "{reading:?}");
    }

    /// A row the other scanner read differently in a file the PR left alone
    /// is a difference between the scanners, and names where it is.
    #[test]
    fn another_scanner_that_reads_an_untouched_row_differently_is_not() {
        let main = service("0.0.1", &[(CART, ORDER), (CHECKOUT, ORDER)]);
        let pr = service(
            env!("CARGO_PKG_VERSION"),
            &[(CART, ORDER), (CHECKOUT, ORDER)],
        );
        let untouched = untouched(&[CART]);

        let mut verb = main.clone();
        verb.calls[0] = serde_json::from_value(call_row(CART, 3, "POST", "/orders")).unwrap();
        assert_eq!(
            untouched_reading(
                std::slice::from_ref(&pr),
                std::slice::from_ref(&verb),
                &untouched
            ),
            UntouchedReading::Differ {
                service: None,
                file: CART.to_string(),
                part: "calls",
            }
        );

        // A call the other scanner never saw.
        let mut missed = main.clone();
        missed
            .calls
            .retain(|call| site_file(&call.file_path.to_string_lossy()) != CART);
        assert_eq!(missed.calls.len(), 1, "the cart call is gone");
        assert!(
            !untouched_reading(
                std::slice::from_ref(&pr),
                std::slice::from_ref(&missed),
                &untouched
            )
            .alike()
        );

        // Only the capture stub differs: the type check judges the alias as
        // the declaration says, so a sidecar that declares it differently
        // reads the file differently, though every row is the same.
        let declared = service("0.0.1", &[(CART, "unknown"), (CHECKOUT, ORDER)]);
        assert_eq!(
            untouched_reading(
                std::slice::from_ref(&pr),
                std::slice::from_ref(&declared),
                &untouched
            ),
            UntouchedReading::Differ {
                service: None,
                file: CART.to_string(),
                part: "type declarations",
            }
        );
    }

    /// A PR that changed every file with rows leaves nothing to compare, and
    /// a copy whose services are not this run's is not read at all.
    #[test]
    fn nothing_untouched_to_compare_is_not_evidence() {
        let main = service("0.0.1", &[(CART, ORDER), (CHECKOUT, ORDER)]);
        let pr = service(
            env!("CARGO_PKG_VERSION"),
            &[(CART, ORDER), (CHECKOUT, ORDER)],
        );
        assert_eq!(
            untouched_reading(
                std::slice::from_ref(&pr),
                std::slice::from_ref(&main),
                &untouched(&["README.md"])
            ),
            UntouchedReading::NothingToCompare
        );

        let mut renamed = main.clone();
        renamed.service_name = Some("storefront".to_string());
        assert_eq!(
            untouched_reading(
                std::slice::from_ref(&pr),
                std::slice::from_ref(&renamed),
                &untouched(&[CART])
            ),
            UntouchedReading::Differ {
                service: None,
                file: String::new(),
                part: "services",
            }
        );
    }

    /// A stub declaration file that does not parse is compared whole, so a
    /// difference in it is never skipped.
    #[test]
    fn a_stub_file_that_does_not_parse_is_compared_whole() {
        let main = service("0.0.1", &[(CART, ORDER), (CHECKOUT, ORDER)]);
        let pr = service(
            env!("CARGO_PKG_VERSION"),
            &[(CART, ORDER), (CHECKOUT, ORDER)],
        );
        let broken = |data: &CloudRepoData, tail: &str| {
            let mut data = data.clone();
            let files = &mut data.capture_stub.as_mut().unwrap().files;
            let text = files.get_mut("types/surface.d.ts").unwrap();
            text.push_str(tail);
            data
        };
        let reading = untouched_reading(
            std::slice::from_ref(&broken(&pr, "export type = {")),
            std::slice::from_ref(&broken(&main, "export type = { x")),
            &untouched(&[CART]),
        );
        assert_eq!(
            reading,
            UntouchedReading::Differ {
                service: None,
                file: String::new(),
                part: "capture stub",
            }
        );
    }

    /// A stored row with no per-site answers (a scanner older than
    /// carrick#1385) places nothing, rather than counting under no file.
    #[test]
    fn a_stored_row_without_sites_places_nothing() {
        let mut copy = main_copy_with("src/screens/rules.tsx:184", "incompatible", true);
        copy.compat_verdicts.as_mut().unwrap()[0].sites.clear();
        assert!(stored_type_sites(std::slice::from_ref(&copy)).is_empty());
    }

    #[test]
    fn mark_all_unknown_says_why_on_every_pairing() {
        let mut pr = vec![
            type_mismatch(ORDERS, "src/checkout.ts:40"),
            Finding::type_mismatch("POST", "/orders", None, vec![], "A", "B", "x"),
        ];
        mark_all_unknown(&mut pr, OnMainUnknown::NoMainIndex);
        assert_eq!(unknown(&pr[0]), Some(OnMainUnknown::NoMainIndex));
        assert_eq!(unknown(&pr[1]), None);
    }
}
