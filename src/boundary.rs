//! What one service's scan could not classify (carrick#705).
//!
//! Every answer the index gives should end with what it is silent about. The
//! scanner already counts most of that while it works — candidates it declined
//! to emit, members a join could not follow, files the analyzer never answered
//! for — and until now it threw the counts away at upload, so a reader of the
//! index could see the rows and never the gap beside them.
//!
//! This module collects those numbers into one struct per service and puts it
//! on the blob. Nothing here re-derives a fact: each field is either a counter
//! the scan already kept ([`ProcessingStats`]) or a read of the rows this blob
//! already carries. A count that would need a new judgement is not here.
//!
//! Two rules the shape enforces:
//!
//! * A number is never mistaken for the whole list. Every count carries its
//!   reasons capped at [`MAX_REASONS`] entries, with the exact total beside
//!   them, so a reader can tell "here are all 12" from "here are 200 of 4,610".
//! * Absence is not zero. The whole block is optional: a blob written by a
//!   scanner that predates it carries no boundary at all, which reads as "this
//!   scan did not state its boundary", not as "this scan had none".

use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};

use crate::agents::file_analyzer_agent::{EmissionStyle, FileAnalysisResult};
use crate::agents::file_orchestrator::ProcessingStats;
use crate::cloud_storage::{CloudRepoData, ManifestRole, ManifestTypeKind, TypeDegradation};

/// How many reasons a count carries before the list is truncated. The total is
/// kept exactly either way.
pub const MAX_REASONS: usize = 200;

/// A count and, up to [`MAX_REASONS`] of them, what it is a count OF.
///
/// `total` is always the real number. `reasons` is a sample whenever
/// `total > MAX_REASONS`, and `truncated` says which of the two the reader is
/// looking at without having to compare lengths against a constant.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Counted {
    pub total: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl Counted {
    /// Build a count from the reasons themselves: the total is how many there
    /// were, the list is the first [`MAX_REASONS`] of them.
    pub fn from_reasons(reasons: Vec<String>) -> Self {
        let total = reasons.len();
        Self::new(total, reasons)
    }

    /// Build a count whose total is known independently of the reasons — a
    /// counter the scan kept, with whatever it can name beside it.
    pub fn new(total: usize, mut reasons: Vec<String>) -> Self {
        let truncated = reasons.len() > MAX_REASONS;
        reasons.truncate(MAX_REASONS);
        Self {
            total,
            reasons,
            truncated,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.total == 0
    }
}

/// What this service's scan could not classify, stated beside what it did.
///
/// One per service, emitted into the index blob and printed as the last lines
/// of the CLI output. Every field is a counter the scan already kept or a read
/// of this blob's own rows.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceBoundary {
    /// Files whose hosted model answers were withheld because their working-tree bytes changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_withheld_changed_files: Option<usize>,
    /// The commit this service's rows were read at. Already on the blob;
    /// repeated here so the whole boundary is one struct: a count means
    /// nothing without the tree it was counted on.
    pub commit_hash: String,
    /// Files this scan sent to the analyzer. The rest of the service's files
    /// either replayed a cached answer or raised no candidate at all.
    pub files_attempted: usize,
    /// Files the analyzer was asked about and never answered for. Their rows
    /// are whatever the deterministic layer stated and nothing more.
    pub files_lost: Counted,
    /// Candidates carrying a route-shaped literal and a readable verb that
    /// neither layer produced a row for. A bare `x.verb("/lit", arg)` states no
    /// producer/consumer role, so the scanner counts the gap rather than
    /// guessing at it (the 2026-09-05 ruling).
    pub unemitted_literal_candidates: usize,
    /// Where those sites are, `file:line`, up to [`MAX_REASONS`] of them. The
    /// count above stays a count and keeps its type — the cloud's MCP server
    /// reads it as a number — and this says which sites it counted, so the
    /// line reconciles against the file instead of hanging in the air
    /// (carrick#997 item 7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unemitted_literal_sites: Vec<String>,
    /// Candidates in files no model was asked about, with no deterministic row
    /// at their span: what a paid scan would classify and this one did not.
    /// Zero on any scan that ran the model. `None` on a blob from a scanner
    /// that did not count it — absent is not the same as counting none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_awaiting_model: Option<usize>,
    /// Why this service's types are degraded, in the sentence the scan
    /// recorded. Carried here so the boundary renderer prints it: it was on
    /// every blob and on no screen (carrick#997 item 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_extraction_status: Option<String>,
    /// Call sites that named a client member and did not resolve to it
    /// (carrick#656), so a consumer listing for those operations is incomplete.
    pub consumers_not_resolved: Counted,
    /// SDK calls that produced no edge, per package and reason. Folded in after
    /// the cross-repo join, which is where the SDK surface of the peers is
    /// known; zero before that runs.
    pub sdk_unresolved: Counted,
    /// Indexed calls whose path carries no literal segment, so no producer may
    /// claim them (`carrick_match::MatchVerdict::UnknownCallPath`).
    pub unknown_call_paths: Counted,
    /// Model rows kept as rows of their own: no deterministic source states
    /// them, so they are the model's reading alone.
    pub model_only_rows: usize,
    /// Model rows that folded into a deterministic row at their span.
    pub model_rows_joined: usize,
    /// Model methods, targets and paths discarded because the source states
    /// something else at the same span.
    pub model_contradictions_discarded: usize,
    /// Model endpoints dropped in modules a routing convention already claims
    /// (carrick#704): in such a module the route set is the exported handlers,
    /// so a row the model states there has no registration witness at all.
    /// `None` on a blob from a scanner that did not count it — absent is not
    /// the same as counting none.
    ///
    /// Not disjoint from `model_contradictions_discarded`, and the two must not
    /// be added: one model row can raise both. A claimed route module that also
    /// holds a route-shaped literal (a `fetch("/api/x")` in its loader is
    /// enough) gets that row's path re-anchored to the literal first — one
    /// discarded STATEMENT — and the re-anchored row then matches no derived
    /// route and is dropped whole — one discarded ROW. Each count is true of a
    /// different thing about the same row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_endpoints_discarded_in_claimed_modules: Option<usize>,
    /// Indexed routes with no resolved response type, so nothing on the
    /// producer side of a compatibility check. Routes counted in
    /// `routes_without_body` are not in this count.
    pub routes_without_response_type: Counted,
    /// Indexed routes the analysis states send no response body at all (a
    /// redirect, a 204, a stream handed to the transport, a throw), and which
    /// have no resolved response type (carrick#1159). They have no body
    /// contract to resolve, so they are not a shortfall in
    /// `routes_without_response_type`, and are counted here instead. `None`
    /// on a blob from a scanner that did not separate them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes_without_body: Option<Counted>,
    /// Indexed calls with no resolved expected type, the consumer-side mirror
    /// of the line above.
    pub calls_without_expected_type: Counted,
    /// Set when type extraction failed outright for this service, in which case
    /// the two counts above are the whole index, not a shortfall in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types_degraded: Option<TypeDegradation>,
    /// Whether the tree had no `node_modules` when types were captured. Every
    /// type that resolves through a dependency is `any` on a bare checkout.
    pub bare_checkout: bool,
}

impl ServiceBoundary {
    /// Collect the boundary for one service from its finished blob and the
    /// stats of the scan that produced it.
    ///
    /// Call it once the blob is complete and its paths are repo-relative: the
    /// reason lists quote file locations, and an absolute path in the index is
    /// the leak carrick#599 exists to prevent. `repo_path` is that scan root —
    /// the counters are kept while the scan still speaks in absolute paths, so
    /// the reasons are relativised here rather than trusted.
    ///
    /// `file_results` is the merged per-file analysis the scan built its rows
    /// from (model and deterministic rows both), keyed absolute or
    /// repo-relative. It is read only for which routes state no response body.
    pub fn collect(
        data: &CloudRepoData,
        stats: &ProcessingStats,
        file_results: &HashMap<String, FileAnalysisResult>,
        repo_path: &str,
    ) -> Self {
        let prefix = format!("{}/", repo_path.trim_end_matches('/'));
        let no_body = no_body_sites(file_results, &prefix);
        let (routes_without_response_type, routes_without_body) =
            routes_without_a_type(data, &no_body);
        let lost = stats
            .errors
            .iter()
            .map(|reason| reason.replace(&prefix, ""))
            .collect();
        let mut sites: Vec<String> = stats
            .unemitted_literal_sites
            .iter()
            .map(|site| site.replace(&prefix, ""))
            .collect();
        sites.truncate(MAX_REASONS);
        Self {
            candidates_withheld_changed_files: None,
            commit_hash: data.commit_hash.clone(),
            files_attempted: stats.files_model_dispatched,
            files_lost: Counted::new(stats.files_analysis_failed, lost),
            unemitted_literal_candidates: stats.unemitted_literal_candidates,
            unemitted_literal_sites: sites,
            candidates_awaiting_model: Some(stats.candidates_awaiting_model),
            type_extraction_status: data.type_extraction_status.clone(),
            consumers_not_resolved: unfollowed_members(data),
            // Known only after the cross-repo SDK join; see `fold_sdk_unresolved`.
            sdk_unresolved: Counted::default(),
            unknown_call_paths: unknown_call_paths(data),
            model_only_rows: stats.model_only_rows,
            model_rows_joined: stats.model_rows_joined,
            model_contradictions_discarded: stats.model_contradictions_discarded,
            model_endpoints_discarded_in_claimed_modules: Some(
                stats.model_endpoints_discarded_in_claimed_modules,
            ),
            routes_without_response_type,
            routes_without_body: Some(routes_without_body),
            calls_without_expected_type: operations_without_a_type(data, ManifestRole::Consumer),
            types_degraded: data.types_degraded.clone(),
            bare_checkout: data
                .capture_stub
                .as_ref()
                .is_some_and(|stub| stub.bare_checkout),
        }
    }

    /// Fold in what the cross-repo SDK join could not resolve for this service.
    ///
    /// Separate from [`Self::collect`] because the join needs the peers' SDK
    /// surfaces, which a single service's scan does not have.
    pub fn fold_sdk_unresolved(&mut self, unresolved: &[crate::cloud_storage::SdkUnresolved]) {
        let total = unresolved.iter().map(|entry| entry.count).sum();
        let reasons = unresolved
            .iter()
            .map(|entry| format!("{} ×{}: {}", entry.package, entry.count, entry.reason))
            .collect();
        self.sdk_unresolved = Counted::new(total, reasons);
    }

    /// The boundary as the CLI prints it: one line per thing this scan could
    /// not classify, and nothing for the ones it classified all of.
    ///
    /// The commit line is unconditional. A boundary with nothing under it is
    /// still an answer, and "at this commit, nothing unclassified" is the one
    /// worth reading twice.
    pub fn lines(&self, service: &str) -> Vec<String> {
        let mut out = vec![format!(
            "{service} at {}: {} file(s) sent to the analyzer",
            short_hash(&self.commit_hash),
            self.files_attempted
        )];
        let mut push = |count: &Counted, what: &str| {
            if !count.is_zero() {
                out.push(format!("  {} {what}{}", count.total, first_reason(count)));
            }
        };
        push(&self.files_lost, "file(s) the analyzer never answered for");
        push(
            &self.consumers_not_resolved,
            "call site(s) that named a client member and did not resolve to it",
        );
        push(&self.sdk_unresolved, "SDK call(s) that produced no edge");
        push(
            &self.unknown_call_paths,
            "indexed call(s) whose path no producer may claim",
        );
        push(
            &self.routes_without_response_type,
            "route(s) with no resolved response type",
        );
        if let Some(no_body) = &self.routes_without_body {
            push(
                no_body,
                "route(s) that send no response body (not counted above)",
            );
        }
        push(
            &self.calls_without_expected_type,
            "call(s) with no resolved expected type",
        );
        if self.unemitted_literal_candidates > 0 {
            out.push(format!(
                "  {} bare route-literal call site(s) left unclassified{}",
                self.unemitted_literal_candidates,
                first_site(
                    &self.unemitted_literal_sites,
                    self.unemitted_literal_candidates
                )
            ));
        }
        if self.model_only_rows > 0 {
            out.push(format!(
                "  {} row(s) the model alone states ({} joined a deterministic row)",
                self.model_only_rows, self.model_rows_joined
            ));
        }
        if let Some(discarded) = self.model_endpoints_discarded_in_claimed_modules
            && discarded > 0
        {
            out.push(format!(
                "  {discarded} model endpoint(s) dropped in modules a routing convention claims"
            ));
        }
        // One reason, once. `types_degraded` names the stage and the error;
        // `type_extraction_status` is the sentence a scan wrote about the same
        // event, and on the one path that sets only the second (the extraction
        // config, which is the model's and absent whenever no model ran) it is
        // the only record there is (carrick#997 item 3).
        if let Some(degraded) = &self.types_degraded {
            out.push(format!(
                "  types degraded at {}: {}",
                degraded.stage, degraded.detail
            ));
        } else if let Some(status) = &self.type_extraction_status {
            out.push(format!("  warning: {status}"));
        }
        if self.bare_checkout {
            out.push(
                "  types captured on a bare checkout: anything through a dependency is `any`"
                    .to_string(),
            );
        }
        out
    }
}

impl ServiceBoundary {
    /// What a scan left for a model to classify, as a table cell — empty when
    /// there is nothing waiting, or when this blob predates the count.
    ///
    /// A pass that ran no model reads `0 route(s) 0 call(s)` both when a
    /// service holds no API at all and when every candidate in it is waiting
    /// for the paid scan. This is the number that tells those two apart
    /// (carrick#997 item 8), and the summary tables print it beside the route
    /// and call counts. It names the command that resolves them, which since
    /// carrick#1008 is `carrick index` itself rather than a flag on it.
    pub fn awaiting_model(&self) -> Option<String> {
        match self.candidates_awaiting_model {
            Some(count) if count > 0 => {
                Some(format!("{count} candidate(s) waiting for `carrick index`"))
            }
            _ => None,
        }
    }
}

/// ` (e.g. src/a.ts:4)` — one of the sites a count counted, so the number can
/// be reconciled against the file. Empty when the scan recorded none.
fn first_site(sites: &[String], total: usize) -> String {
    match sites.first() {
        None => String::new(),
        Some(first) if total > 1 => format!(" (e.g. {first})"),
        Some(first) => format!(" ({first})"),
    }
}

/// `12 (first reason, …)` — the first reason inline, and whether there are
/// more, so a CLI line is never mistaken for the whole list.
fn first_reason(count: &Counted) -> String {
    match count.reasons.first() {
        None => String::new(),
        Some(first) if count.total > 1 => format!(" (e.g. {first})"),
        Some(first) => format!(" ({first})"),
    }
}

fn short_hash(commit: &str) -> &str {
    let end = commit.len().min(7);
    if commit.is_empty() {
        "an unknown commit"
    } else {
        &commit[..end]
    }
}

/// Client members the join could not follow, one entry per member name. The
/// same member is stamped on every row the join produced for it, so the rows
/// are deduped by name before the counts are added.
fn unfollowed_members(data: &CloudRepoData) -> Counted {
    let Some(graph) = data.mount_graph.as_ref() else {
        return Counted::default();
    };
    let mut by_member: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for call in graph.get_data_calls() {
        if let Some(unfollowed) = call.consumers_not_resolved.as_ref() {
            let entry = by_member.entry(unfollowed.member.as_str()).or_default();
            *entry = (*entry).max(unfollowed.count);
        }
    }
    let total = by_member.values().map(|count| *count as usize).sum();
    let reasons = by_member
        .iter()
        .map(|(member, count)| format!("{member} ×{count}"))
        .collect();
    Counted::new(total, reasons)
}

/// Indexed calls whose path carries no literal segment. The matcher refuses to
/// pair those with any producer, so they are in the index and unclaimable.
fn unknown_call_paths(data: &CloudRepoData) -> Counted {
    let reasons = data
        .calls
        .iter()
        .filter_map(|call| Some((call, call.key.as_http()?)))
        .filter(|(_, (_, path))| carrick_match::is_unknown_call_path(path))
        .map(|(call, (method, path))| format!("{method} {path} ({})", call.file_path.display()))
        .collect();
    Counted::from_reasons(reasons)
}

/// Operations with no resolved type on the side that matters for them: the
/// response a route sends, and the response a call expects.
///
/// An operation counts as typed when the manifest carries an entry for its key
/// at its own site with a resolved definition. No manifest at all — a service
/// whose type extraction failed — means every operation counts, which is the
/// honest reading: the index has no type for any of them.
fn operations_without_a_type(data: &CloudRepoData, role: ManifestRole) -> Counted {
    Counted::from_reasons(
        untyped_operations(data, role)
            .map(|(reason, _)| reason)
            .collect(),
    )
}

/// The producer half of [`operations_without_a_type`], split in two: routes
/// the analysis states send no body (carrick#1159), and every other untyped
/// route. A redirect or a 204 has no response contract to resolve, so counting
/// it as a missing type overstates the shortfall.
fn routes_without_a_type(
    data: &CloudRepoData,
    no_body: &HashSet<(String, u32)>,
) -> (Counted, Counted) {
    let (bodiless, rest): (Vec<_>, Vec<_>) = untyped_operations(data, ManifestRole::Producer)
        .partition(|(_, site)| site.as_ref().is_some_and(|site| no_body.contains(site)));
    (
        Counted::from_reasons(rest.into_iter().map(|(reason, _)| reason).collect()),
        Counted::from_reasons(bodiless.into_iter().map(|(reason, _)| reason).collect()),
    )
}

/// Every operation of `role` with no resolved response type, as its reason
/// line and its `(file, line)` site when the row states one.
fn untyped_operations(
    data: &CloudRepoData,
    role: ManifestRole,
) -> impl Iterator<Item = (String, Option<(String, u32)>)> + '_ {
    let operations = match role {
        ManifestRole::Producer => &data.endpoints,
        ManifestRole::Consumer => &data.calls,
    };
    let manifest = data.type_manifest.as_deref().unwrap_or_default();
    let typed: HashSet<(String, String)> = manifest
        .iter()
        .filter(|entry| entry.role == role)
        .filter(|entry| matches!(entry.type_kind, ManifestTypeKind::Response))
        .filter(|entry| entry.resolved_definition.is_some())
        .map(|entry| (entry.key.canonical(), entry.file_path.clone()))
        .collect();
    operations.iter().filter_map(move |operation| {
        let location = operation.file_path.to_string_lossy();
        let (file, line) = match location.rsplit_once(':') {
            Some((file, line)) => (file, line.parse::<u32>().ok()),
            None => (location.as_ref(), None),
        };
        if typed.contains(&(operation.key.canonical(), file.to_string())) {
            return None;
        }
        let reason = format!(
            "{} ({})",
            operation.key.canonical(),
            operation.file_path.display()
        );
        Some((reason, line.map(|line| (file.to_string(), line))))
    })
}

/// The `(repo-relative file, line)` of every route the merged analysis states
/// sends no response body.
fn no_body_sites(
    file_results: &HashMap<String, FileAnalysisResult>,
    prefix: &str,
) -> HashSet<(String, u32)> {
    file_results
        .iter()
        .flat_map(|(file, result)| {
            let file = file.strip_prefix(prefix).unwrap_or(file);
            let file = file.strip_prefix("./").unwrap_or(file);
            result
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.emission_style == Some(EmissionStyle::NoPayload))
                .map(move |endpoint| (file.to_string(), endpoint.line_number.max(1) as u32))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::SdkUnresolved;

    #[test]
    fn a_count_keeps_the_exact_total_when_the_reasons_are_capped() {
        let reasons: Vec<String> = (0..MAX_REASONS + 11).map(|n| n.to_string()).collect();
        let counted = Counted::from_reasons(reasons);
        assert_eq!(counted.total, MAX_REASONS + 11);
        assert_eq!(counted.reasons.len(), MAX_REASONS);
        assert!(counted.truncated, "a capped list says so");

        let short = Counted::from_reasons(vec!["one".to_string()]);
        assert_eq!(short.total, 1);
        assert!(!short.truncated);
    }

    #[test]
    fn the_sdk_fold_sums_the_counts_and_names_the_packages() {
        let mut boundary = ServiceBoundary::default();
        boundary.fold_sdk_unresolved(&[
            SdkUnresolved {
                package: "@org/ledger".to_string(),
                count: 3,
                reason: "member not found".to_string(),
            },
            SdkUnresolved {
                package: "@org/billing".to_string(),
                count: 1,
                reason: "peer has no surface".to_string(),
            },
        ]);
        assert_eq!(boundary.sdk_unresolved.total, 4);
        assert_eq!(boundary.sdk_unresolved.reasons.len(), 2);
        assert!(boundary.sdk_unresolved.reasons[0].contains("@org/ledger ×3"));
    }

    /// The scan counts its losses while it still speaks in absolute paths, so
    /// the reasons are relativised on the way into the blob. An absolute runner
    /// path in the index is the leak carrick#599 exists to prevent, and the
    /// projection's own guard does not cover this block.
    #[test]
    fn a_lost_file_is_named_by_its_repo_relative_path() {
        use crate::cloud_storage::CloudRepoData;

        let repo_path = "/home/runner/work/acme-app/acme-app";
        let blob = serde_json::json!({
            "repo_name": "acme/app",
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": {},
            "config_json": null,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-01-01T00:00:00Z",
            "commit_hash": "abc1234"
        });
        let data: CloudRepoData = serde_json::from_value(blob).expect("the blob reads");
        let stats = ProcessingStats {
            files_analysis_failed: 1,
            errors: vec![format!(
                "Failed to analyze {repo_path}/src/dead.ts: gateway timeout"
            )],
            ..Default::default()
        };

        let boundary = ServiceBoundary::collect(&data, &stats, &HashMap::new(), repo_path);
        assert_eq!(
            boundary.files_lost.reasons,
            vec!["Failed to analyze src/dead.ts: gateway timeout".to_string()]
        );
    }

    /// A route the analysis states sends no body (a redirect, a 204) has no
    /// response contract to resolve. It is counted in `routes_without_body`,
    /// not as a missing response type (carrick#1159). The file_results key is
    /// absolute, as the full scan passes it; the operation row is relative.
    #[test]
    fn routes_that_send_no_body_are_counted_apart_from_missing_types() {
        use crate::agents::file_analyzer_agent::FileAnalysisResult;
        use crate::analyzer::ApiEndpointDetails;
        use crate::operation::OperationKey;

        let repo_path = "/work/acme";
        let endpoint = |method: &str, path: &str, site: &str| ApiEndpointDetails {
            view_module: false,
            owner: None,
            key: OperationKey::http(method, path.to_string()),
            params: vec![],
            request_body: None,
            response_body: None,
            handler_name: None,
            request_type: None,
            response_type: None,
            file_path: std::path::PathBuf::from(site),
            repo_name: None,
            service_name: None,
            provenance: Default::default(),
            resolution_source: None,
            dispatch: None,
            schema_binding: None,
        };
        let data = CloudRepoData {
            endpoints: vec![
                endpoint("GET", "/login", "src/routes.ts:4"),
                endpoint("GET", "/users", "src/routes.ts:9"),
            ],
            ..serde_json::from_value(serde_json::json!({
                "repo_name": "acme/app", "endpoints": [], "calls": [], "mounts": [],
                "apps": {}, "imported_handlers": [], "function_definitions": {},
                "last_updated": "2026-01-01T00:00:00Z", "commit_hash": "abc1234"
            }))
            .expect("the blob reads")
        };
        let row = |line: u32, style: &str| {
            serde_json::json!({
                "candidate_id": format!("span:{line}"), "line_number": line,
                "owner_node": "app", "method": "GET", "path": "/x",
                "handler_name": "anonymous", "pattern_matched": ".get(",
                "payload_expression_text": null, "payload_expression_line": null,
                "response_expression_text": null, "response_expression_line": null,
                "primary_type_symbol": null, "type_import_source": null,
                "emission_style": style
            })
        };
        let result: FileAnalysisResult = serde_json::from_value(serde_json::json!({
            "mounts": [], "data_calls": [],
            "endpoints": [row(4, "no-payload"), row(9, "imperative-send")]
        }))
        .expect("the analysis reads");
        let file_results = HashMap::from([(format!("{repo_path}/src/routes.ts"), result)]);

        let boundary =
            ServiceBoundary::collect(&data, &ProcessingStats::default(), &file_results, repo_path);
        assert_eq!(
            boundary.routes_without_response_type.reasons,
            vec!["http|GET|/users (src/routes.ts:9)".to_string()]
        );
        assert_eq!(
            boundary.routes_without_body,
            Some(Counted::from_reasons(vec![
                "http|GET|/login (src/routes.ts:4)".to_string()
            ]))
        );
        assert!(
            boundary
                .lines("orders")
                .iter()
                .any(|line| line.contains("1 route(s) that send no response body")),
        );
    }

    #[test]
    fn the_printed_block_states_the_commit_and_only_the_non_zero_counts() {
        let mut boundary = ServiceBoundary {
            commit_hash: "0123456789abcdef".to_string(),
            files_attempted: 12,
            ..Default::default()
        };
        let clean = boundary.lines("orders");
        assert_eq!(clean.len(), 1, "nothing unclassified, one line: {clean:?}");
        assert!(clean[0].contains("orders at 0123456"));

        boundary.unknown_call_paths = Counted::from_reasons(vec![
            "GET /${base} (src/a.ts:4)".to_string(),
            "GET /${base} (src/b.ts:9)".to_string(),
        ]);
        boundary.bare_checkout = true;
        let stated = boundary.lines("orders");
        assert!(stated.iter().any(|line| line.contains("2 indexed call(s)")));
        assert!(
            stated
                .iter()
                .any(|line| line.contains("e.g. GET /${base} (src/a.ts:4)")),
            "a count names one of the things it counts: {stated:?}"
        );
        assert!(stated.iter().any(|line| line.contains("bare checkout")));
    }

    /// A count of something nobody can point at is a number to argue with. The
    /// line names one of the sites it counted (carrick#997 item 7).
    #[test]
    fn the_unclassified_line_names_a_site() {
        let boundary = ServiceBoundary {
            commit_hash: "0123456".to_string(),
            unemitted_literal_candidates: 2,
            unemitted_literal_sites: vec![
                "apps/gateway/main.ts:11".to_string(),
                "apps/gateway/main.ts:40".to_string(),
            ],
            ..Default::default()
        };
        let lines = boundary.lines("gateway");
        assert!(
            lines.iter().any(|line| line
                .contains("2 bare route-literal call site(s) left unclassified")
                && line.contains("e.g. apps/gateway/main.ts:11")),
            "{lines:?}"
        );
    }

    /// The reason a service's types are thin is printed, once. A scan that
    /// recorded a stage keeps the line that names the stage; a scan that
    /// recorded only the sentence prints the sentence (carrick#997 item 3).
    #[test]
    fn a_degraded_type_pass_states_its_reason_once() {
        let mut boundary = ServiceBoundary {
            commit_hash: "0123456".to_string(),
            type_extraction_status: Some("machinery unwrapping disabled this run".to_string()),
            ..Default::default()
        };
        let lines = boundary.lines("gateway");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("machinery unwrapping"))
                .count(),
            1,
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("warning:")),
            "{lines:?}"
        );

        boundary.types_degraded = Some(TypeDegradation {
            stage: "spawn".to_string(),
            detail: "sidecar unavailable".to_string(),
        });
        let lines = boundary.lines("gateway");
        assert!(
            lines
                .iter()
                .any(|line| line.contains("types degraded at spawn")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("warning:")),
            "one event, one line: {lines:?}"
        );
    }

    /// `0 route(s) 0 call(s)` reads the same for a service with no API and for
    /// one whose every candidate is waiting for the paid scan. A blob that
    /// predates the count says nothing rather than zero (carrick#997 item 8).
    #[test]
    fn what_is_waiting_for_the_model_is_a_sentence_or_nothing() {
        let mut boundary = ServiceBoundary::default();
        assert_eq!(boundary.awaiting_model(), None, "never counted");
        boundary.candidates_awaiting_model = Some(0);
        assert_eq!(boundary.awaiting_model(), None, "counted none");
        boundary.candidates_awaiting_model = Some(8);
        assert_eq!(
            boundary.awaiting_model().as_deref(),
            Some("8 candidate(s) waiting for `carrick index`")
        );
    }
}
