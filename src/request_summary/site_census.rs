//! The census of decided sites and their records (carrick#2433,
//! carrick#2435): a measurement on real repos, offline and model-free.
//!
//! Per repo it counts discovery's records, the distinct sites where a name
//! is decided, the leads by reason, the records by bucket (every slot
//! forced, some asked, lead), and the record bytes at p50, p90 and max. It
//! compares every decided HTTP site with the request summaries' rows for the
//! same request: a row that is not on a decided site is listed. And it says
//! how many records one line inserted at the top of a file would change
//! (each record names its positions as `file:line`): for the file with the
//! most records, and on average over the files that hold any.
//!
//! Each repo needs a flag-on scan's rows (`<store>/positions`), discovery's
//! census for it (carrick#2431, `{ "sites": [...] }`), and the store's
//! `/library-claims` answer for its packages:
//!
//! `CARRICK_SITE_CENSUS='<label>=<repo>=<positions dir>=<sites.json>=<library-claims.json>[=<blobs dir>][;…]'
//! [CARRICK_SITE_CENSUS_OUT=<file>] cargo test --lib site_census -- --ignored --nocapture`
//!
//! With a scan's blobs dir, a `routes` section reads the route sites against
//! that scan's endpoint rows (carrick#2474 R1): by what holds the receiver
//! and how discovery reached them (a sites file from `discovery_census` with
//! `CARRICK_RECEIVER_TRACE=1` names both), recall, extras, and the
//! mount-shaped sites apart. It also counts the route sites by where their
//! instance walks end, a labelled maker's result or a hole by its reason,
//! and by how many ends each has, one per caller that hands an instance in
//! (carrick#2474 R2a).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::decided_sites::{
    DiscoveryFile, InstanceAt, InstanceEnd, MakerLabel, ScopeLabel, SiteLabel,
    decided_sites_traced, relative,
};
use super::site_record::{Bucket, site_records};
use crate::config::Config;
use crate::position_rows::PositionIndex;

#[derive(Debug, Default, Serialize)]
struct RepoCensus {
    discovered: usize,
    labelled: usize,
    decided_sites: usize,
    leads: BTreeMap<String, usize>,
    unplaced: usize,
    walked: usize,
    buckets: BTreeMap<String, usize>,
    lead_reasons: BTreeMap<String, usize>,
    state_bytes_p50: usize,
    state_bytes_p90: usize,
    state_bytes_max: usize,
    /// Summary rows whose request this pass decides.
    summary_rows_compared: usize,
    /// Of those, rows not on a decided site, as `file:line`.
    summary_rows_off_site: Vec<String>,
    /// Records naming a line of the file with the most records.
    edit_reasks_busiest: usize,
    busiest_file: String,
    /// Records naming a line of a file, on average over files holding any.
    edit_reasks_mean: f64,
    /// The scope claims the trace's own verify request verified, by claim
    /// and receiver, over every service (carrick#2474 R2b).
    scopes_verified: usize,
    /// How long deciding the sites with the receiver trace took, over
    /// every service (instance walks and composed paths included).
    traced_ms: u128,
    /// The route sites against the scan's endpoint rows (carrick#2474 R1),
    /// when the census is handed the scan's blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    routes: Option<RoutesCensus>,
}

/// A repo's route sites (carrick#2474 R1): the records whose label states
/// an HTTP route (kind `http`, role `handles`), read against the endpoint
/// rows of the scan's blobs, each keyed `(method, path as written,
/// file:line)`.
#[derive(Debug, Default, Serialize)]
struct RoutesCensus {
    route_sites: usize,
    /// By what holds the receiver (`none`: the site names none).
    by_receiver: BTreeMap<String, usize>,
    /// By how discovery reached the labelled call: `name`, `instance`,
    /// `declaration` (`none`: discovery ran without the trace).
    by_reached: BTreeMap<String, usize>,
    /// Route sites whose record forces no method (the model is asked it).
    method_unstated: usize,
    endpoint_rows: usize,
    /// Endpoint rows a route site states: the same `file:line`, the same
    /// method where the site forces one, and the row's path ends with the
    /// path the site writes (the row's carries its mount prefix).
    recalled: usize,
    /// Endpoint rows with such a route site at the same `file:line` whose
    /// path the row's does not end with.
    other_path: Vec<String>,
    /// Endpoint rows no route site states.
    missed: Vec<String>,
    /// Route sites (mount-shaped ones aside) that state no endpoint row.
    extras: Vec<String>,
    /// Route sites whose label states no method at all (no method, no
    /// method slot, no default): a member that registers under every verb
    /// or none, the shape of a mount, that the store labels `receive`
    /// (carrick-tools/carrick-cloud#2426). Counted apart.
    mount_shaped: Vec<String>,
    /// Every route site: `file:line METHOD path receiver-kind reached`.
    sites: Vec<String>,
    /// Route sites by where their instance walks end (carrick#2474 R2a):
    /// `root`, a hole's reason, `root+<reason>` for a site with both, or
    /// `none` for a site that names no instance.
    by_instance_end: BTreeMap<String, usize>,
    /// Every instance end, one per caller that hands one in, by `root` or
    /// the hole's reason.
    instance_ends: BTreeMap<String, usize>,
    /// Route sites by how many instance ends they have: the per-caller
    /// fan-out.
    fan_out: BTreeMap<usize, usize>,
    /// Every instance end: `file:line <- at root file:line` or `<- at
    /// <reason> [text]`.
    instances: Vec<String>,
    /// Endpoint rows a route site at the same `file:line` (and method,
    /// where the site forces one) composes the row's full path for
    /// (carrick#2474 R2b), both read without a trailing `/`.
    composed_recalled: usize,
    /// Endpoint rows with a route site at the same place that composes
    /// another path or none: `row <- each composed path, or (why none)`.
    composed_differs: Vec<String>,
    /// Composed paths by where their walk up ends: `top`, a hole's reason,
    /// `cycle`.
    composed_ends: BTreeMap<String, usize>,
    /// Route sites whose composed paths stopped at the cap.
    composed_trimmed: usize,
    /// Every composed path: `file:line METHOD path <- edges`.
    composed: Vec<String>,
    /// Where route chains leave their service, and how they resolve
    /// across the repo's services (carrick#2474 R5a).
    boundaries: BoundariesCensus,
}

/// The services' boundary facts (carrick#2474 R5a), resolved across them
/// under the Q5 switch ([`super::decided_sites::boundaries::FAN_OUT`]).
#[derive(Debug, Default, Serialize)]
struct BoundariesCensus {
    /// Route segments by boundary kind (`entry_param`, `callback_option`).
    segments: BTreeMap<String, usize>,
    /// Route segments by the boundary they end at, `kind file:line`.
    segments_at: BTreeMap<String, usize>,
    /// Call-ins by the function they call, `file:line pN`.
    call_ins: BTreeMap<String, usize>,
    /// Hand-outs by the function, `file:line pN.key`.
    hand_outs: BTreeMap<String, usize>,
    /// Segments by outcome: `resolved`, or why not.
    resolved: BTreeMap<String, usize>,
    /// Hosts whose path is not read, over every segment.
    unread_hosts: usize,
    /// Every segment: `service file:line METHOD -> paths (unread hosts)` or
    /// `-> (reason)`.
    lines: Vec<String>,
}

/// The census of `facts`, each service's name and boundary facts.
fn census_boundaries(
    facts: &[(String, super::decided_sites::RouteBoundaries)],
) -> BoundariesCensus {
    use super::decided_sites::boundaries::{Boundary, FAN_OUT, resolve};
    let mut census = BoundariesCensus::default();
    let fn_at =
        |at: &super::decided_sites::boundaries::FnAt| format!("{}:{}", at.decl_file, at.decl_line);
    for (_, service) in facts {
        for segment in &service.routes {
            let (kind, at) = match &segment.at {
                Boundary::EntryParam { function, param } => {
                    ("entry_param", format!("{} p{param}", fn_at(function)))
                }
                Boundary::CallbackOption { host_at, key, .. } => {
                    ("callback_option", format!("{host_at} {key}"))
                }
            };
            *census.segments.entry(kind.to_string()).or_default() += 1;
            *census
                .segments_at
                .entry(format!("{kind} {at}"))
                .or_default() += 1;
        }
        for call_in in &service.call_ins {
            *census
                .call_ins
                .entry(format!("{} p{}", fn_at(&call_in.function), call_in.param))
                .or_default() += 1;
        }
        for hand_out in &service.hand_outs {
            *census
                .hand_outs
                .entry(format!(
                    "{} p{}.{}",
                    fn_at(&hand_out.function),
                    hand_out.param,
                    hand_out.key
                ))
                .or_default() += 1;
        }
    }
    let services: Vec<(&str, &super::decided_sites::RouteBoundaries)> = facts
        .iter()
        .map(|(name, facts)| (name.as_str(), facts))
        .collect();
    for resolved in resolve(&services, FAN_OUT) {
        census.unread_hosts += resolved.unread_hosts;
        let outcome = resolved
            .untraced
            .clone()
            .unwrap_or_else(|| "resolved".to_string());
        *census.resolved.entry(outcome).or_default() += 1;
        census.lines.push(format!(
            "{} {}:{} {} -> {}",
            resolved.service,
            resolved.file,
            resolved.line,
            resolved.method,
            match &resolved.untraced {
                Some(reason) => format!("({reason})"),
                None => format!(
                    "{} ({} unread hosts)",
                    resolved.paths.join("|"),
                    resolved.unread_hosts
                ),
            }
        ));
    }
    census.lines.sort();
    census
}

/// An instance end's kind: `root`, or the hole's reason.
fn end_kind(end: &InstanceEnd) -> String {
    if matches!(end.end, InstanceAt::Scope { .. }) {
        return "scope".to_string();
    }
    match end.hole() {
        None => "root".to_string(),
        Some(reason) => serde_json::to_value(reason)
            .ok()
            .and_then(|reason| reason.as_str().map(str::to_string))
            .unwrap_or_default(),
    }
}

/// A composed path as the census lists it: the path (`?` where it is not
/// text), with where its walk ends after it unless that is the top.
fn composed_text(composed: &super::decided_sites::Composed) -> String {
    let path = composed.path.as_deref().unwrap_or("?");
    match composed.end.as_str() {
        "top" => path.to_string(),
        end => format!("{path} ({end})"),
    }
}

/// One endpoint row of the scan: method, path, `file:line`.
fn endpoint_rows(blobs: &Path) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(blobs).expect("the blobs dir reads") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(blob) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&path).unwrap())
        else {
            continue;
        };
        for endpoint in blob["endpoints"].as_array().into_iter().flatten() {
            let key = &endpoint["key"];
            if key["protocol"] != "http" {
                continue;
            }
            let (Some(method), Some(at), Some(file)) = (
                key["method"].as_str(),
                key["path"].as_str(),
                endpoint["file_path"].as_str(),
            ) else {
                continue;
            };
            rows.push((method.to_uppercase(), at.to_string(), file.to_string()));
        }
    }
    rows.sort();
    rows.dedup();
    rows
}

/// One record with what the census reads beside it: how discovery reached
/// its labelled call, the path its decided site's code writes
/// ([`super::decided_sites::DecidedSite::name_text`]), whether its
/// label states no method at all (the mount shape), and the instance ends
/// its decided site names (carrick#2474 R2a).
struct Routed {
    record: super::site_record::SiteRecord,
    reached: Option<String>,
    written: Option<String>,
    no_method: bool,
    instances: Vec<InstanceEnd>,
    composed: Vec<super::decided_sites::Composed>,
    composed_trimmed: bool,
}

/// The routes section: `records` against the scan's endpoint rows in
/// `blobs`. A route's path is the one the record forces, else the one its
/// decided site's code writes; its method the one the record forces, and a
/// site that states none matches a row of any method (counted).
fn census_routes(records: &[Routed], blobs: &Path) -> RoutesCensus {
    use crate::site_questions::Slot;
    let mut census = RoutesCensus::default();
    let mut stated: Vec<(String, String, String)> = Vec::new();
    type Site = (String, String, String);
    let mut composed_at: Vec<(Site, Vec<super::decided_sites::Composed>)> = Vec::new();
    for routed in records {
        let record = &routed.record;
        let forced = |slot| record.state.forced.get(&slot).cloned();
        if forced(Slot::Kind).as_deref() != Some("http")
            || forced(Slot::Role).as_deref() != Some("handles")
        {
            continue;
        }
        census.route_sites += 1;
        let kind = record.receiver.as_ref().map_or("none".to_string(), |at| {
            serde_json::to_value(at.kind)
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_string))
                .unwrap_or_default()
        });
        *census.by_receiver.entry(kind.clone()).or_default() += 1;
        let reached = routed.reached.clone().unwrap_or_else(|| "none".to_string());
        *census.by_reached.entry(reached.clone()).or_default() += 1;
        let at = format!("{}:{}", record.anchors.call.file, record.anchors.call.line);
        let method = forced(Slot::Method).unwrap_or_default().to_uppercase();
        if method.is_empty() {
            census.method_unstated += 1;
        }
        let name = forced(Slot::Name)
            .or_else(|| routed.written.clone())
            .unwrap_or_default();
        census
            .sites
            .push(format!("{at} {method} {name} {kind} {reached}"));
        let instances = &routed.instances;
        let kinds: std::collections::BTreeSet<String> = instances.iter().map(end_kind).collect();
        let site_kind = match (kinds.contains("root"), kinds.len()) {
            (_, 0) => "none".to_string(),
            (true, 1) => "root".to_string(),
            (true, _) => format!(
                "root+{}",
                kinds
                    .iter()
                    .filter(|kind| *kind != "root")
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("+")
            ),
            (false, _) => kinds.iter().cloned().collect::<Vec<_>>().join("+"),
        };
        *census.by_instance_end.entry(site_kind).or_default() += 1;
        *census.fan_out.entry(instances.len()).or_default() += 1;
        for end in instances {
            *census.instance_ends.entry(end_kind(end)).or_default() += 1;
            let what = match &end.end {
                InstanceAt::Root { file, line, .. } => format!("root {file}:{line}"),
                InstanceAt::Scope { file, line, .. } => format!("scope {file}:{line}"),
                InstanceAt::Hole { text, .. } => match text {
                    Some(text) => format!("{} {text}", end_kind(end)),
                    None => end_kind(end),
                },
            };
            census
                .instances
                .push(format!("{at} {method} {name} <- {} {what}", end.at));
        }
        if routed.no_method {
            census.mount_shaped.push(format!("{at} {method} {name}"));
            continue;
        }
        census.composed_trimmed += usize::from(routed.composed_trimmed);
        for composed in &routed.composed {
            *census
                .composed_ends
                .entry(composed.end.clone())
                .or_default() += 1;
            census.composed.push(format!(
                "{at} {method} {} <- {}",
                composed_text(composed),
                composed.edges.join(" > ")
            ));
        }
        let site = (method, name, at);
        composed_at.push((site.clone(), routed.composed.clone()));
        stated.push(site);
    }
    let rows = endpoint_rows(blobs);
    census.endpoint_rows = rows.len();
    let same_place = |row: &(String, String, String), site: &(String, String, String)| {
        (site.0.is_empty() || row.0 == site.0) && row.2 == site.2
    };
    for row in &rows {
        if stated
            .iter()
            .any(|site| same_place(row, site) && !site.1.is_empty() && row.1.ends_with(&site.1))
        {
            census.recalled += 1;
        } else if stated.iter().any(|site| same_place(row, site)) {
            census
                .other_path
                .push(format!("{} {} {}", row.2, row.0, row.1));
        } else {
            census.missed.push(format!("{} {} {}", row.2, row.0, row.1));
        }
    }
    for row in &rows {
        let here: Vec<&Vec<super::decided_sites::Composed>> = composed_at
            .iter()
            .filter(|(site, _)| same_place(row, site))
            .map(|(_, composed)| composed)
            .collect();
        if here.is_empty() {
            continue;
        }
        // Both read without a trailing `/` (a route `/` under a prefix).
        let bare = |path: &str| match path.trim_end_matches('/') {
            "" => "/".to_string(),
            path => path.to_string(),
        };
        let path = bare(&row.1);
        if here
            .iter()
            .flat_map(|composed| composed.iter())
            .any(|composed| composed.path.as_deref().map(bare).as_deref() == Some(path.as_str()))
        {
            census.composed_recalled += 1;
        } else {
            let got: Vec<String> = here
                .iter()
                .flat_map(|composed| composed.iter())
                .map(composed_text)
                .collect();
            census.composed_differs.push(format!(
                "{} {} {} <- {}",
                row.2,
                row.0,
                row.1,
                if got.is_empty() {
                    "(no instance)".to_string()
                } else {
                    got.join(" | ")
                }
            ));
        }
    }
    for site in &stated {
        if !rows.iter().any(|row| same_place(row, site)) {
            census
                .extras
                .push(format!("{} {} {}", site.2, site.0, site.1));
        }
    }
    for list in [
        &mut census.other_path,
        &mut census.missed,
        &mut census.extras,
        &mut census.mount_shaped,
        &mut census.sites,
        &mut census.instances,
        &mut census.composed_differs,
        &mut census.composed,
    ] {
        list.sort();
        list.dedup();
    }
    census
}

fn percentile(sorted: &[usize], pct: usize) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * pct / 100]
}

fn census_repo(
    root: &Path,
    positions: &Path,
    sites: &Path,
    claims: &Path,
    blobs: Option<&Path>,
) -> RepoCensus {
    let root = std::fs::canonicalize(root).expect("the repo exists");
    let config = root.join("carrick.json");
    let services = if config.exists() {
        Config::load_services(vec![config]).expect("carrick.json loads")
    } else {
        vec![Config::default()]
    };
    let discovered = serde_json::from_str::<DiscoveryFile>(
        &std::fs::read_to_string(sites).expect("the sites file reads"),
    )
    .expect("the sites file parses")
    .sites;
    // Read as a scan reads the store's answer: the store writes a method
    // as text, which the raw reader's label shape does not parse.
    let exports = crate::library_store::parse_answer(
        &std::fs::read_to_string(claims).expect("the claims read"),
    )
    .exports;
    let labels = SiteLabel::from_exports(&exports);
    let makers = MakerLabel::from_exports(&exports);
    // The scope claims count as a scan counts them: those the trace's own
    // verify request verifies, per service (carrick#2474 R2b).
    let script = super::fixture_walk::sidecar().expect("the built sidecar");
    let mut by_service: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in std::fs::read_dir(positions).expect("the positions dir reads") {
        let path = entry.expect("dir entry").path();
        if path.to_string_lossy().ends_with(".jsonl.gz")
            && let Ok(rows) = crate::position_rows::read_rows_file(&path)
        {
            by_service.insert(rows.header.service.clone(), path);
        }
    }
    let mut census = RepoCensus {
        discovered: discovered.len(),
        labelled: discovered
            .iter()
            .filter(|site| {
                site.end
                    .as_ref()
                    .is_some_and(|end| SiteLabel::find(&labels, end).is_some())
            })
            .count(),
        ..RepoCensus::default()
    };
    let mut bytes: Vec<usize> = Vec::new();
    let mut per_file: HashMap<String, usize> = HashMap::new();
    let mut routed: Vec<Routed> = Vec::new();
    let mut boundaries: Vec<(String, super::decided_sites::RouteBoundaries)> = Vec::new();
    for service in &services {
        let label = service
            .service_name
            .clone()
            .unwrap_or_else(|| "(root)".to_string());
        let index = match by_service.get(&label) {
            Some(path) => PositionIndex::of(
                &root,
                crate::position_rows::read_rows_file(path).expect("rows read"),
            ),
            None => PositionIndex::missing(&root),
        };
        let inputs = crate::engine::discover_service_inputs(&root, service, Some(index));
        let files: HashSet<String> = inputs
            .files
            .keys()
            .map(|file| relative(&root, file))
            .collect();
        let mine: Vec<_> = discovered
            .iter()
            .filter(|site| files.contains(&site.file))
            .cloned()
            .collect();
        let declarations =
            super::fixture_walk::census_declarations(&script, &root, service, &inputs, &exports);
        let scopes = ScopeLabel::from_exports(&exports, &declarations.verified_scopes);
        census.scopes_verified += declarations.verified_scopes.len();
        // Traced: each site also names its instance (carrick#2474 R2a),
        // which only the routes section reads.
        let started = std::time::Instant::now();
        let procedures = super::decided_sites::ProcedureLabels::from_exports(
            &exports,
            &declarations.verified_scopes,
        );
        let decided = decided_sites_traced(
            &inputs,
            &root,
            &mine,
            &labels,
            &makers,
            &scopes,
            &procedures,
        );
        census.traced_ms += started.elapsed().as_millis();
        boundaries.push((label.clone(), decided.boundaries.clone()));
        census.decided_sites += decided.sites.len();
        census.unplaced += decided.unplaced.len();
        census.walked += decided.walked.len();
        for lead in &decided.leads {
            let reason = serde_json::to_value(lead.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            *census.leads.entry(reason).or_default() += 1;
        }
        let records = site_records(&inputs, &root, &decided, &labels);
        // How discovery reached each record's labelled call, and the
        // instance it is made on, as the decided site at the record's call
        // says.
        for record in &records {
            let call = &record.anchors.call;
            let site = decided.sites.iter().find(|site| {
                site.file == call.file
                    && site.span_start == call.span_start
                    && site.span_end == call.span_end
            });
            let reached = site
                .and_then(|site| serde_json::to_value(site.reached?).ok())
                .and_then(|by| by.as_str().map(str::to_string));
            routed.push(Routed {
                record: record.clone(),
                reached,
                written: site.and_then(|site| site.name_text()),
                no_method: site
                    .and_then(|site| SiteLabel::find(&labels, &site.end))
                    .is_some_and(|label| {
                        label.method.as_ref().is_none_or(|method| {
                            method.literal.is_none()
                                && method.from.is_none()
                                && method.default.is_none()
                        })
                    }),
                instances: site.map(|site| site.instances.clone()).unwrap_or_default(),
                composed: site.map(|site| site.composed.clone()).unwrap_or_default(),
                composed_trimmed: site.is_some_and(|site| site.composed_trimmed),
            });
        }
        for record in &records {
            let bucket = match record.bucket {
                Bucket::Forced => "forced",
                Bucket::Asked => "asked",
                Bucket::Lead => "lead",
            };
            *census.buckets.entry(bucket.to_string()).or_default() += 1;
            if let Some(reason) = record.lead {
                *census.lead_reasons.entry(reason.to_string()).or_default() += 1;
            }
            bytes.push(record.state_bytes());
            let state = serde_json::to_string(&record.state).unwrap_or_default();
            for file in &files {
                if state.contains(&format!("{file}:")) {
                    *per_file.entry(file.clone()).or_default() += 1;
                }
            }
        }
        // The summaries' rows against the decided sites of the same request.
        let summaries = crate::request_summary::summarize(
            &inputs,
            &crate::client_semantics::LibrarySemantics::default(),
        );
        let mut sites_of: HashMap<(String, u32), HashSet<(String, u32)>> = HashMap::new();
        for site in decided
            .sites
            .iter()
            .filter(|site| site.end.role == "http_client")
        {
            sites_of
                .entry((site.end.file.clone(), site.end.span_start))
                .or_default()
                .insert((site.file.clone(), site.line));
        }
        for file in inputs.files.keys() {
            for rows in summaries.rows(file).into_iter().flat_map(|at| at.values()) {
                for row in rows {
                    let request = (relative(&root, &row.request_at.0), row.request_at.1);
                    let Some(decided) = sites_of.get(&request) else {
                        continue;
                    };
                    census.summary_rows_compared += 1;
                    let at = (relative(&root, file), row.line);
                    if !decided.contains(&at) {
                        census
                            .summary_rows_off_site
                            .push(format!("{}:{}", at.0, at.1));
                    }
                }
            }
        }
    }
    bytes.sort_unstable();
    census.state_bytes_p50 = percentile(&bytes, 50);
    census.state_bytes_p90 = percentile(&bytes, 90);
    census.state_bytes_max = bytes.last().copied().unwrap_or_default();
    if let Some((file, count)) = per_file
        .iter()
        .max_by_key(|(file, count)| (**count, file.as_str()))
    {
        census.busiest_file = file.clone();
        census.edit_reasks_busiest = *count;
    }
    if !per_file.is_empty() {
        census.edit_reasks_mean = per_file.values().sum::<usize>() as f64 / per_file.len() as f64;
    }
    census.summary_rows_off_site.sort();
    census.summary_rows_off_site.dedup();
    census.routes = blobs.map(|blobs| {
        let mut routes = census_routes(&routed, blobs);
        routes.boundaries = census_boundaries(&boundaries);
        routes
    });
    census
}

#[test]
#[ignore = "a census over real repos: set CARRICK_SITE_CENSUS"]
fn site_census() {
    let Ok(spec) = std::env::var("CARRICK_SITE_CENSUS") else {
        crate::errln!("CARRICK_SITE_CENSUS is not set");
        return;
    };
    let mut out: BTreeMap<String, RepoCensus> = BTreeMap::new();
    for repo in spec.split(';').filter(|part| !part.is_empty()) {
        let parts: Vec<&str> = repo.split('=').collect();
        let (label, root, positions, sites, claims, blobs) = match parts.as_slice() {
            [label, root, positions, sites, claims] => {
                (label, root, positions, sites, claims, None)
            }
            [label, root, positions, sites, claims, blobs] => (
                label,
                root,
                positions,
                sites,
                claims,
                Some(Path::new(*blobs)),
            ),
            _ => {
                panic!("expected <label>=<repo>=<positions>=<sites>=<claims>[=<blobs>], got {repo}")
            }
        };
        out.insert(
            label.to_string(),
            census_repo(
                Path::new(root),
                Path::new(positions),
                Path::new(sites),
                Path::new(claims),
                blobs,
            ),
        );
    }
    let text = serde_json::to_string_pretty(&out).expect("the census serialises");
    match std::env::var("CARRICK_SITE_CENSUS_OUT") {
        Ok(path) => std::fs::write(path, &text).expect("the census writes"),
        Err(_) => crate::outln!("{text}"),
    }
}
