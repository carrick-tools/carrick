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

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

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
                site.clone(),
            )
        })
        .collect()
}

/// The distinct sites each group holds. Distinct, because main's side reads
/// one site from two sources (its recomputed findings and its stored
/// verdicts), and a site both report is still one broken call.
fn sites_by_group<'a>(
    entries: impl Iterator<Item = (SiteGroup, String)> + 'a,
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
    /// Main's answer at each type-checked call site: every outcome of the
    /// recomputed check, and every site of main's stored verdicts.
    pub type_sites: Vec<MainTypeSite>,
    /// Main has a type answer to compare with: its recomputed check ran, or
    /// its stored copy carries verdicts.
    pub types_judged: bool,
}

/// Whether main's copy is main as this PR's base has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainCopy {
    /// Nothing shows it to be anything else.
    Current,
    /// It is provably not: see [`OnMainUnknown::MainIndexStale`].
    Stale,
}

/// Mark each pairing-carrying finding in `pr` with whether main has it.
///
/// `true` when, for every site group the finding counts in, main has at least
/// as many sites as the PR. Otherwise `false` only when main's copy is current
/// and main's side judged at least one group the PR has more sites in; else
/// the run could not tell, and the finding carries why. A finding with no
/// pairing is left as it is.
pub fn mark_on_main(pr: &mut [Finding], main: &MainSide, copy: MainCopy) {
    let theirs = sites_by_group(
        main.findings.iter().flat_map(groups).chain(
            main.type_sites
                .iter()
                .filter(|site| site.incompatible)
                .map(|site| (site.group(), site.site.clone())),
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
/// A type mismatch is judged when main has a resolved answer in the group,
/// from either source, or has no answer in it at all while it has type
/// answers elsewhere: then the PR added the pairing. An answer that exists
/// and is unresolved is main being unable to judge, typically a response the
/// PR run judged by retyping the consumer's own code and main's stored copy
/// carries no verdict for.
fn judged(main: &MainSide, group: &SiteGroup) -> bool {
    if group.0 != TYPE_MISMATCH {
        return true;
    }
    let mut answers = main
        .type_sites
        .iter()
        .filter(|site| site.group() == *group)
        .peekable();
    if answers.peek().is_none() {
        return main.types_judged;
    }
    answers.any(|site| site.resolved)
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
/// uncommitted changes, by another scanner version (whose extraction and
/// judge may differ from this run's, so a difference would not be the PR's),
/// or at a commit `differs` says has changes against the base. A copy at
/// another commit this clone cannot compare, such as the head of a
/// squash-merged branch that a laptop index scanned, is not shown to differ
/// and counts as current: its verdicts are still main's latest answer.
pub fn main_copy(
    main_self: &[CloudRepoData],
    base: Option<&str>,
    differs: impl Fn(&str, &str) -> Option<bool>,
) -> MainCopy {
    let this_version = env!("CARGO_PKG_VERSION");
    let stale = main_self.iter().any(|repo| {
        repo.dirty == Some(true)
            || repo.scanner_version.as_deref() != Some(this_version)
            || base.is_some_and(|base| {
                repo.commit_hash != base && differs(&repo.commit_hash, base) == Some(true)
            })
    });
    if stale {
        MainCopy::Stale
    } else {
        MainCopy::Current
    }
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
    // The upload strips the parsed type nodes (`strip_ast_nodes`), so main's
    // stored copy never has them and this run's always does.
    for row in data.endpoints.iter_mut().chain(data.calls.iter_mut()) {
        row.request_type = None;
        row.response_type = None;
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
            type_sites: Vec::new(),
            types_judged: true,
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
            type_sites: Vec::new(),
            types_judged: false,
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
        let mut type_sites = vec![recomputed_unresolved(site)];
        type_sites.extend(stored_type_sites(std::slice::from_ref(copy)));
        MainSide {
            findings: Vec::new(),
            type_sites,
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
            findings: Vec::new(),
            type_sites: vec![recomputed_unresolved(site)],
            types_judged: true,
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

    /// One site both sources report is one broken call: a second broken call
    /// the PR adds in the same file still reads as introduced.
    #[test]
    fn a_site_both_sources_report_counts_once() {
        let site = "src/screens/rules.tsx:184";
        let mut main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        main.type_sites[0].incompatible = true;
        main.type_sites[0].resolved = true;
        let mut pr = vec![retyped(site), retyped("src/screens/rules.tsx:300")];
        mark_on_main(&mut pr, &main, MainCopy::Current);
        assert_eq!(on_main(&pr[0]), Some(false));
        assert_eq!(on_main(&pr[1]), Some(false));
    }

    /// A stale copy never yields `false`: what main's copy lacks may have
    /// reached main after it. What it has is still on main.
    #[test]
    fn a_stale_copy_never_calls_a_finding_introduced() {
        let site = "src/screens/rules.tsx:184";
        let main = main_with_stored(&main_copy_with(site, "incompatible", true), site);
        let other = "rules-api|GET|/rules/:param/holidays~rules-web|src/screens/other.tsx|response";
        let mut pr = vec![
            retyped(site),
            retyped("src/screens/other.tsx:9").with_pair(Some(other.to_string())),
        ];
        mark_on_main(&mut pr, &main, MainCopy::Stale);
        assert_eq!(on_main(&pr[0]), Some(true));
        assert_eq!(unknown(&pr[1]), Some(OnMainUnknown::MainIndexStale));
    }

    /// A copy at another commit this clone cannot compare is main's latest
    /// index and is compared with: the laptop index a new project starts
    /// from scans a branch head that squash-merges to another commit. A copy
    /// at a commit this clone shows to differ from the base, a dirty copy, and
    /// a copy another scanner version wrote are stale.
    #[test]
    fn which_copies_are_stale() {
        let copy = main_copy_with("src/screens/rules.tsx:1", "compatible", true);
        let cannot_compare = |_: &str, _: &str| None;
        let differs = |_: &str, _: &str| Some(true);
        let same_tree = |_: &str, _: &str| Some(false);
        let one = std::slice::from_ref(&copy);
        assert_eq!(
            main_copy(one, Some("bbbb2222"), cannot_compare),
            MainCopy::Current
        );
        assert_eq!(
            main_copy(one, Some("bbbb2222"), same_tree),
            MainCopy::Current
        );
        assert_eq!(main_copy(one, Some("aaaa1111"), differs), MainCopy::Current);
        assert_eq!(main_copy(one, None, differs), MainCopy::Current);
        assert_eq!(main_copy(one, Some("bbbb2222"), differs), MainCopy::Stale);

        let mut dirty = copy.clone();
        dirty.dirty = Some(true);
        assert_eq!(
            main_copy(std::slice::from_ref(&dirty), None, cannot_compare),
            MainCopy::Stale
        );
        let mut other_version = copy.clone();
        other_version.scanner_version = Some("0.0.1".to_string());
        assert_eq!(
            main_copy(std::slice::from_ref(&other_version), None, cannot_compare),
            MainCopy::Stale
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
