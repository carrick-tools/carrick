use crate::analyzer::ApiAnalysisResult;
use crate::findings::{Finding, PrDelta, Topology, tier};
use std::collections::{BTreeMap, BTreeSet};

// Display helpers for the wire [`Topology`]. Defined here (not in
// `findings`) because they are presentation policy: the cloud renderer
// applies its own equivalents when it rebuilds the PR comment.
impl Topology {
    fn is_monorepo(&self) -> bool {
        self.local_service_count > 1
    }

    fn has_peers(&self) -> bool {
        self.peer_repo_count > 0
    }

    /// Cross-service matching is only conclusive when there is more than one
    /// service to match across: peer repos, or multiple local services in a
    /// monorepo. A lone single-service repo has no baseline, so its
    /// connectivity findings are framed as informational rather than headline
    /// issues (an endpoint looks "orphaned" only because nothing else is
    /// indexed yet).
    fn has_baseline(&self) -> bool {
        self.has_peers() || self.is_monorepo()
    }

    /// Subtitle appended to the comment header to name the topology.
    fn header_suffix(&self) -> String {
        if self.is_monorepo() {
            format!(" · monorepo ({} services)", self.local_service_count)
        } else if !self.has_peers() {
            format!(" · {}", self.repo_name)
        } else {
            String::new()
        }
    }

    /// Scope noun phrase rendered after "across" in the verdict, so it must
    /// not itself contain "across". A monorepo's service count is already shown
    /// in the header suffix, so once peers exist the scope reports repos.
    fn scope_phrase(&self) -> String {
        if self.has_peers() {
            format!("{} repos", self.peer_repo_count + 1)
        } else if self.is_monorepo() {
            format!("{} services", self.local_service_count)
        } else {
            self.repo_name.clone()
        }
    }
}

pub struct FormattedOutput {
    pub content: String,
}

impl FormattedOutput {
    pub fn new(result: ApiAnalysisResult, topology: Topology, pr_delta: Option<PrDelta>) -> Self {
        let content = format_analysis_results(result, &topology, pr_delta.as_ref());
        Self { content }
    }

    pub fn print(&self) {
        println!("{}", self.content);
    }
}

pub fn format_analysis_results(
    result: ApiAnalysisResult,
    topology: &Topology,
    pr_delta: Option<&PrDelta>,
) -> String {
    if result.findings.is_empty() {
        return format_no_issues(&result, topology, pr_delta);
    }

    let categorized = categorize_findings(&result.findings);
    let has_baseline = topology.has_baseline();
    // Headline rule (mirrored by the cloud renderer): risks always count;
    // missing endpoints count only with a baseline (a lone single-service repo
    // has nothing to match against, so they are listed as informational);
    // major dependency conflicts count; advisory findings (env-var
    // suggestions, unparseable version pins) never do.
    //
    // Orphaned endpoints never count, with or without a baseline. "No indexed
    // service calls this" is an inventory fact, not a defect: browsers, agents
    // and other external clients are not in the index, so on a system that
    // serves them orphans are most of its endpoints. And an orphan cannot
    // break anything — nothing calls it. They get their own section instead.
    let connectivity_in_headline = if has_baseline {
        categorized.missing.len()
    } else {
        0
    };
    let total_issues = categorized.risks.len()
        + connectivity_in_headline
        + categorized.major_dependencies.len()
        + categorized.degraded_types.len();

    let mut output = String::new();

    // Machine-readable markers consumed by the GitHub Action. The issue count
    // must stay parseable.
    output.push_str("<!-- CARRICK_OUTPUT_START -->\n");
    output.push_str(&format!("<!-- CARRICK_ISSUE_COUNT:{} -->\n", total_issues));

    output.push_str(&format!("## 🪢 Carrick{}\n\n", topology.header_suffix()));

    // Verdict callout. GitHub alert blocks carry severity colour natively, so
    // the comment conveys state without leaning on emoji.
    output.push_str(&format_verdict(
        &categorized,
        total_issues,
        has_baseline,
        topology,
    ));
    output.push_str("\n\n");

    output.push_str(&format_pr_delta(
        pr_delta,
        &categorized.missing_keys(),
        &result.sdk_edges,
    ));

    output.push_str(&format!(
        "Indexed **{} endpoints** and **{} cross-service calls**.\n\n",
        result.endpoints.len(),
        result.calls.len(),
    ));

    let banner = format_graphql_banner(
        &result.detected_graphql_libraries,
        result.graphql_operations_indexed,
    );
    output.push_str(&banner);

    // Sections, ordered by actionability. Verified runs last as a collapsed
    // positive signal.
    if !categorized.degraded_types.is_empty() {
        output.push_str(&format_degraded_types_section(&categorized.degraded_types));
        output.push_str("\n\n");
    }
    if !categorized.risks.is_empty() {
        output.push_str(&format_critical_section(&categorized.risks));
        output.push_str("\n\n");
    }
    if !categorized.missing.is_empty() {
        output.push_str(&format_connectivity_section(
            &categorized.missing,
            has_baseline,
        ));
        output.push_str("\n\n");
    }
    if !categorized.dependencies_is_empty() {
        output.push_str(&format_dependency_section(
            &categorized.major_dependencies,
            &categorized.unparseable_dependencies,
        ));
        output.push_str("\n\n");
    }
    if !categorized.configuration.is_empty() {
        output.push_str(&format_configuration_section(&categorized.configuration));
        output.push_str("\n\n");
    }
    if !categorized.shared_contracts.is_empty() {
        output.push_str(&format_shared_contract_section(
            &categorized.shared_contracts,
        ));
        output.push_str("\n\n");
    }
    if !result.sdk_edges.is_empty() || !result.sdk_unresolved.is_empty() {
        output.push_str(&format_sdk_section(
            &result.sdk_edges,
            &result.sdk_unresolved,
        ));
        output.push_str("\n\n");
    }
    if !result.verified_endpoints.is_empty() {
        output.push_str(&format_verified_section(&result.verified_endpoints));
        output.push_str("\n\n");
    }
    // Dead last: informational, and usually the longest list on a system that
    // serves browsers or agents.
    if !categorized.orphaned.is_empty() {
        output.push_str(&format_unconsumed_section(
            &categorized.orphaned,
            has_baseline,
        ));
        output.push_str("\n\n");
    }

    output.push_str("<!-- CARRICK_OUTPUT_END -->\n");
    output
}

/// The "In this PR" block: endpoints this change added or removed relative to
/// the repo's last-indexed state. Empty (renders nothing) outside a PR run,
/// when there is no prior index to diff against, or when nothing changed.
/// A removed endpoint whose (method, path) still shows up as a missing
/// endpoint is flagged — the PR deletes a producer something still calls.
fn format_pr_delta(
    pr_delta: Option<&PrDelta>,
    still_missing: &[(String, String)],
    sdk_edges: &[crate::cloud_storage::SdkEdge],
) -> String {
    let Some(delta) = pr_delta else {
        return String::new();
    };
    if delta.is_empty() {
        return String::new();
    }
    let mut output = String::from("**In this PR**\n\n");
    for ep in &delta.new_endpoints {
        output.push_str(&format!(
            "- New endpoint `{} {}`{}\n",
            code_span(&ep.method),
            code_span(&ep.path),
            service_suffix(ep.service.as_deref())
        ));
    }
    for ep in &delta.removed_endpoints {
        let consumed = if still_missing
            .iter()
            .any(|(method, path)| endpoints_overlap(&ep.method, &ep.path, method, path))
        {
            " — ⚠ still consumed".to_string()
        } else {
            // A consumer that reaches this route through a published client
            // writes no URL, so it can never appear as a missing endpoint.
            // The SDK edge is the only evidence the route still has a caller.
            sdk_consumers_of(&ep.method, &ep.path, sdk_edges)
        };
        output.push_str(&format!(
            "- Removed `{} {}`{}{}\n",
            code_span(&ep.method),
            code_span(&ep.path),
            service_suffix(ep.service.as_deref()),
            consumed
        ));
    }
    output.push('\n');
    output
}

/// Whether a removed endpoint and a missing-endpoint finding name the same
/// route: methods equal case-insensitively, paths segment-wise with a param
/// placeholder on EITHER side matching anything. Literal equality would
/// never fire on parameterized routes — the removed side carries declared
/// param names (`/orders/:id`) while the missing side is normalized
/// (`/orders/:param`) or concrete (`/orders/123`). Placeholder syntax and
/// the trailing-`?` optional marker defer to `MountGraph::is_param_segment`
/// so this flag and the matcher agree on what a param is (`{id}`, `<id>`,
/// `[id]` — not just `:id`).
fn endpoints_overlap(a_method: &str, a_path: &str, b_method: &str, b_path: &str) -> bool {
    if !a_method.eq_ignore_ascii_case(b_method) {
        return false;
    }
    let is_param = |seg: &str| {
        crate::mount_graph::MountGraph::is_param_segment(seg.strip_suffix('?').unwrap_or(seg))
    };
    let a_segments: Vec<&str> = a_path.split('/').collect();
    let b_segments: Vec<&str> = b_path.split('/').collect();
    a_segments.len() == b_segments.len()
        && a_segments
            .iter()
            .zip(&b_segments)
            .all(|(a_seg, b_seg)| is_param(a_seg) || is_param(b_seg) || a_seg == b_seg)
}

/// ``(`service`)`` suffix for a delta line; empty when the service is absent
/// or sanitizes away.
fn service_suffix(service: Option<&str>) -> String {
    service
        .map(code_span)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" (`{}`)", s))
        .unwrap_or_default()
}

/// Build the GitHub alert block that opens the comment. The alert kind sets
/// the colour (CAUTION red, WARNING amber, NOTE blue), so severity reads at a
/// glance without emoji.
fn format_verdict(
    categorized: &CategorizedFindings,
    total_issues: usize,
    has_baseline: bool,
    topology: &Topology,
) -> String {
    let kind = if !categorized.risks.is_empty() || !categorized.degraded_types.is_empty() {
        "CAUTION"
    } else if total_issues > 0 {
        "WARNING"
    } else {
        "NOTE"
    };

    let mut parts: Vec<String> = Vec::new();
    // First in the summary line: it qualifies everything after it.
    if !categorized.degraded_types.is_empty() {
        parts.push(format!(
            "**types not extracted for {} service{}**",
            categorized.degraded_types.len(),
            plural(categorized.degraded_types.len())
        ));
    }
    if !categorized.risks.is_empty() {
        parts.push(format!(
            "**{} contract risk{}**",
            categorized.risks.len(),
            plural(categorized.risks.len())
        ));
    }
    if !categorized.missing.is_empty() {
        let noun = if has_baseline {
            "connectivity gap"
        } else {
            "connectivity observation"
        };
        parts.push(format!(
            "{} {}{}",
            categorized.missing.len(),
            noun,
            plural(categorized.missing.len())
        ));
    }
    // Named separately from the counted items: an endpoint nothing in the
    // index calls is inventory, not a gap.
    if !categorized.orphaned.is_empty() {
        parts.push(format!(
            "{} endpoint{} without an indexed consumer",
            categorized.orphaned.len(),
            plural(categorized.orphaned.len())
        ));
    }
    if !categorized.dependencies_is_empty() {
        let n = categorized.major_dependencies.len() + categorized.unparseable_dependencies.len();
        parts.push(format!("{} dependency conflict{}", n, plural(n)));
    }
    if !categorized.configuration.is_empty() {
        parts.push(format!(
            "{} configuration suggestion{}",
            categorized.configuration.len(),
            plural(categorized.configuration.len())
        ));
    }
    if !categorized.shared_contracts.is_empty() {
        parts.push(format!(
            "{} shared external contract{}",
            categorized.shared_contracts.len(),
            plural(categorized.shared_contracts.len())
        ));
    }

    let summary = if parts.is_empty() {
        "Nothing to flag".to_string()
    } else {
        join_human(&parts)
    };

    // Only explain the informational framing when there are connectivity
    // findings to frame; otherwise the note would reference connectivity that
    // isn't present (e.g. a lone repo with only configuration suggestions).
    let baseline_note = if !has_baseline && !categorized.connectivity_is_empty() {
        " First repo indexed, so connectivity findings are informational.".to_string()
    } else {
        String::new()
    };

    format!(
        "> [!{}]\n> {} across {}.{}",
        kind,
        summary,
        topology.scope_phrase(),
        baseline_note
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Join phrases as "a", "a and b", or "a, b and c".
fn join_human(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => {
            let (last, head) = parts.split_last().unwrap();
            format!("{} and {}", head.join(", "), last)
        }
    }
}

fn format_no_issues(
    result: &ApiAnalysisResult,
    topology: &Topology,
    pr_delta: Option<&PrDelta>,
) -> String {
    let mut output = String::new();
    output.push_str("<!-- CARRICK_OUTPUT_START -->\n<!-- CARRICK_ISSUE_COUNT:0 -->\n");
    output.push_str(&format!("## 🪢 Carrick{}\n\n", topology.header_suffix()));
    output.push_str(&format!(
        "> [!TIP]\n> All cross-service calls match the indexed contracts across {}.\n\n",
        topology.scope_phrase()
    ));
    output.push_str(&format_pr_delta(pr_delta, &[], &result.sdk_edges));
    output.push_str(&format!(
        "Indexed **{} endpoints** and **{} cross-service calls**.\n\n",
        result.endpoints.len(),
        result.calls.len(),
    ));
    output.push_str(&format_graphql_banner(
        &result.detected_graphql_libraries,
        result.graphql_operations_indexed,
    ));
    if !result.sdk_edges.is_empty() || !result.sdk_unresolved.is_empty() {
        output.push_str(&format_sdk_section(
            &result.sdk_edges,
            &result.sdk_unresolved,
        ));
        output.push_str("\n\n");
    }
    if !result.verified_endpoints.is_empty() {
        output.push_str(&format_verified_section(&result.verified_endpoints));
        output.push_str("\n\n");
    }
    output.push_str("<!-- CARRICK_OUTPUT_END -->\n");
    output
}

/// The " still consumed via `<package>` by `<consumer>`" marker for a removed
/// endpoint that an SDK edge still points at, or an empty string.
///
/// Route comparison defers to [`endpoints_overlap`] so this flag and the
/// direct one agree on what a param segment is: the removed side carries
/// declared param names (`/orders/:id`) while an SDK edge's `producer_key`
/// carries the producer's own resolved path, and literal equality would miss
/// every parameterized route.
fn sdk_consumers_of(
    method: &str,
    path: &str,
    sdk_edges: &[crate::cloud_storage::SdkEdge],
) -> String {
    let mut packages: BTreeSet<&str> = BTreeSet::new();
    let mut consumers: BTreeSet<&str> = BTreeSet::new();
    for edge in sdk_edges {
        let Some((edge_method, edge_path)) = http_route(&edge.producer_key) else {
            continue;
        };
        if endpoints_overlap(method, path, edge_method, edge_path) {
            packages.insert(&edge.package);
            consumers.insert(&edge.consumer_repo);
        }
    }
    if packages.is_empty() {
        return String::new();
    }
    let render = |names: BTreeSet<&str>| {
        join_human(
            &names
                .into_iter()
                .map(|name| format!("`{}`", code_span(name)))
                .collect::<Vec<_>>(),
        )
    };
    format!(
        " — ⚠ still consumed via {} by {}",
        render(packages),
        render(consumers)
    )
}

/// Split an `OperationKey::canonical()` into `(METHOD, path)`, for HTTP keys
/// only — a GraphQL or socket key names no route.
fn http_route(producer_key: &str) -> Option<(&str, &str)> {
    let mut parts = producer_key.splitn(3, '|');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("http"), Some(method), Some(path)) => Some((method, path)),
        _ => None,
    }
}

/// The collapsed "Consumers via SDK" block: who reaches this project's
/// endpoints through a published npm client rather than over a URL.
///
/// These pairs are invisible to the direct matcher by construction — the
/// consumer's source contains no route at all — so the section exists to make
/// the relationship visible, not to flag anything. Unresolved calls are
/// reported as one honest line rather than being left out: "this package is
/// called and nothing is known about where those calls land" is a different
/// statement from "there are no such calls".
fn format_sdk_section(
    edges: &[crate::cloud_storage::SdkEdge],
    unresolved: &[crate::cloud_storage::SdkUnresolved],
) -> String {
    let mut output = format!(
        "<details>\n<summary><strong>Consumers via SDK ({})</strong></summary>\n\n",
        edges.len()
    );
    output.push_str(
        "> Calls that reach an indexed endpoint through a published client rather than a \
         URL the caller writes.\n\n",
    );

    if edges.is_empty() {
        output.push_str("No SDK call resolved to an indexed endpoint.\n\n");
    } else {
        output.push_str(
            "| Consumer | Call site | Through | Endpoint | Types |\n\
             | :--- | :--- | :--- | :--- | :--- |\n",
        );
        for edge in edges {
            let endpoint = match http_route(&edge.producer_key) {
                Some((method, path)) => format!("`{} {}`", code_span(method), code_span(path)),
                None => format!("`{}`", code_span(&edge.producer_key)),
            };
            let verdict = match edge.type_compatible {
                Some(true) => "compatible".to_string(),
                Some(false) => format!(
                    "**INCOMPATIBLE**: {}",
                    code_span(
                        edge.mismatch_reason
                            .as_deref()
                            .unwrap_or("no reason recorded")
                    )
                ),
                None => "unverified".to_string(),
            };
            output.push_str(&format!(
                "| `{}` | `{}` | `{}` → `{}` | {} (`{}`) | {} |\n",
                code_span(&edge.consumer_repo),
                code_span(&edge.consumer_location),
                code_span(&edge.callee),
                code_span(&edge.package),
                endpoint,
                code_span(&edge.producer_repo),
                verdict
            ));
        }
        output.push('\n');

        // "unverified" is a statement about where the verdict would have come
        // from, not about the types (#525). The verdict for an SDK hop is the
        // one the SDK repo's own scan stored for its call to the producer, so
        // an edge without one says the SDK repo has not stored a verdict for
        // that pair — silence would read as "compared and fine".
        let unverified = edges
            .iter()
            .filter(|edge| edge.type_compatible.is_none())
            .count();
        if unverified > 0 {
            let sdk_repos: BTreeSet<&str> = edges
                .iter()
                .filter(|edge| edge.type_compatible.is_none())
                .map(|edge| edge.sdk_repo.as_str())
                .collect();
            output.push_str(&format!(
                "{} edge{} unverified: no type verdict is stored in {}'s own scan for that call. \
                 Re-scan it to produce one.\n\n",
                unverified,
                plural(unverified),
                join_human(
                    &sdk_repos
                        .into_iter()
                        .map(|name| format!("`{}`", code_span(name)))
                        .collect::<Vec<_>>()
                )
            ));
        }
    }

    if !unresolved.is_empty() {
        let total: usize = unresolved.iter().map(|entry| entry.count).sum();
        let reasons: Vec<String> = unresolved
            .iter()
            .map(|entry| {
                format!(
                    "`{}` ×{} {}",
                    code_span(&entry.package),
                    entry.count,
                    code_span(&entry.reason)
                )
            })
            .collect();
        output.push_str(&format!(
            "{} SDK call{} could not be resolved to an endpoint: {}.\n\n",
            total,
            plural(total),
            join_human(&reasons)
        ));
    }

    output.push_str("</details>");
    output
}

/// Render one verified-endpoint table row, marking mock/test-handler
/// producers so the match is trusted accordingly (#380).
fn format_verified_row(entry: &crate::analyzer::VerifiedEndpointEntry) -> String {
    let marker = if entry.provenance.is_mock() {
        " (mock handler)"
    } else {
        ""
    };
    format!("| `{}` | `{}`{} |\n", entry.method, entry.path, marker)
}

/// Render a verdict subsection (heading + honest caption + table) when it has
/// rows; empty buckets render nothing.
fn format_verified_subsection(
    heading: &str,
    caption: &str,
    rows: &[&crate::analyzer::VerifiedEndpointEntry],
) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = format!("**{} ({})**\n\n> {}\n\n", heading, rows.len(), caption);
    out.push_str("| Method | Path |\n| :--- | :--- |\n");
    for entry in rows {
        out.push_str(&format_verified_row(entry));
    }
    out.push('\n');
    out
}

/// Render a "Verified" details block listing every endpoint that at least one
/// consumer call matched. Surfaced so the report communicates what's *working*,
/// not just what's broken — without it, a clean cross-repo run produces no
/// positive signal at all.
///
/// "Verified" is the STRUCTURAL claim (method + path matched). The type-level
/// claim is made per bucket from each entry's `type_verdict` (#260, #456), NOT
/// blanket-asserted: only pairs the TypeScript compiler pass actually compared
/// get the compiler caption; pairs it reached but could not verify get their
/// own honest bucket; and rows without a verdict (not type-checked, non-HTTP)
/// get no type claim at all. The old single caption labelled every row as
/// compiler-compared, which overstated coverage exactly where the check was
/// being deliberately honest. Mirrors the cloud `render_pr_comment.js`
/// `verifiedSection` buckets so the terminal/Action log and the PR comment
/// agree.
fn format_verified_section(verified: &[crate::analyzer::VerifiedEndpointEntry]) -> String {
    use crate::operation::TypeVerdict;

    let type_checked: Vec<&_> = verified
        .iter()
        .filter(|e| e.type_verdict == Some(TypeVerdict::Compatible))
        .collect();
    let unverifiable: Vec<&_> = verified
        .iter()
        .filter(|e| e.type_verdict == Some(TypeVerdict::Unverifiable))
        .collect();
    // Everything else: no verdict (not type-checked / non-HTTP) OR incompatible
    // (already reported as a loud finding above; none of the three verified
    // captions fit it, and "not compared" carries the least-wrong advice).
    let matched_only: Vec<&_> = verified
        .iter()
        .filter(|e| {
            e.type_verdict != Some(TypeVerdict::Compatible)
                && e.type_verdict != Some(TypeVerdict::Unverifiable)
        })
        .collect();

    let mut output = format!(
        "<details>\n<summary><strong>Verified ({})</strong></summary>\n\n",
        verified.len()
    );
    output.push_str("> Endpoints with at least one matching consumer call.\n\n");

    // No per-endpoint type verdicts anywhere: one table, structural claim only
    // — no compiler sentence for pairs nobody proved.
    if type_checked.is_empty() && unverifiable.is_empty() {
        output.push_str("| Method | Path |\n| :--- | :--- |\n");
        for entry in verified {
            output.push_str(&format_verified_row(entry));
        }
        output.push_str("\n</details>");
        return output;
    }

    output.push_str(&format_verified_subsection(
        "Type-checked",
        "Request/response types were resolved and compared by the TypeScript compiler pass.",
        &type_checked,
    ));
    output.push_str(&format_verified_subsection(
        "Types not verifiable",
        "The compiler pass reached these pairs but could not compare their types (for example the producer's published types decay to `any` outside its own workspace). Treat the types as unreviewed, not as agreeing.",
        &unverifiable,
    ));
    output.push_str(&format_verified_subsection(
        "Types not compared",
        "Matched by method and path. No stored verdict marks these types compatible, so treat them as unverified; any pairs found incompatible are reported as risks above.",
        &matched_only,
    ));
    output.push_str("</details>");
    output
}

/// Render a banner when GraphQL libraries are detected but no operations
/// could be extracted. GraphQL extraction is parse-based (SDL files,
/// `gql` template literals); code-first schemas and Relay compiled
/// artifacts produce nothing statically, so the banner suggests committing
/// an emitted schema instead of staying silent about the coverage gap.
fn format_graphql_banner(graphql_libraries: &[String], operations_indexed: bool) -> String {
    if graphql_libraries.is_empty() || operations_indexed {
        return String::new();
    }
    let mut libs: Vec<&String> = graphql_libraries.iter().collect();
    libs.sort();
    libs.dedup();
    let lib_list = libs
        .iter()
        .map(|s| format!("`{}`", s))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "> [!NOTE]\n> **GraphQL detected** ({}), but no schema or operation documents were found. Carrick extracts GraphQL contracts from SDL (`.graphql`/`.gql` files, `gql` template literals). If your schema is code-first (Pothos, TypeGraphQL, Nexus), commit the emitted `schema.graphql` to index it. Relay compiled artifacts and persisted queries are out of scope.\n\n",
        lib_list,
    )
}

#[derive(Debug, Clone)]
struct EnvVarSuggestionGroup {
    method: String,
    env_var: String,
    path: String,
    locations: Vec<String>,
}

struct CategorizedFindings<'a> {
    /// Contract risks: type mismatches and method mismatches.
    risks: Vec<&'a Finding>,
    /// Consumer calls with no producer.
    missing: Vec<&'a Finding>,
    /// Producers with no consumer.
    orphaned: Vec<&'a Finding>,
    /// Env-var calls regrouped for the configuration section.
    configuration: Vec<EnvVarSuggestionGroup>,
    /// Dependency conflicts split by tier: `major` counts toward the headline,
    /// `unparseable` is advisory.
    major_dependencies: Vec<&'a Finding>,
    unparseable_dependencies: Vec<&'a Finding>,
    /// Shared external contracts (#379): groups of repos whose call sites
    /// encode the same externally-served API. Advisory; carries no
    /// producer/consumer roles.
    shared_contracts: Vec<&'a Finding>,
    /// Services whose type extraction failed (carrick#535). Counts toward the
    /// headline: it is the reason the type verdicts below are missing.
    degraded_types: Vec<&'a Finding>,
}

impl CategorizedFindings<'_> {
    /// Whether the connectivity *family* is empty — missing and orphaned
    /// together, counted or not. Deliberately not missing-only: its one caller
    /// decides whether the first-repo framing note applies, and that note
    /// frames both kinds. Everything that counts uses `missing` directly.
    fn connectivity_is_empty(&self) -> bool {
        self.missing.is_empty() && self.orphaned.is_empty()
    }

    fn dependencies_is_empty(&self) -> bool {
        self.major_dependencies.is_empty() && self.unparseable_dependencies.is_empty()
    }

    /// (method, path) of every missing-endpoint finding — the join keys for
    /// the delta section's "still consumed" flag on removed endpoints
    /// (matched param-aware by `endpoints_overlap`, not literally).
    fn missing_keys(&self) -> Vec<(String, String)> {
        self.missing
            .iter()
            .filter_map(|finding| match finding {
                Finding::MissingEndpoint { method, path, .. } => {
                    Some((method.clone(), path.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

fn categorize_findings(findings: &[Finding]) -> CategorizedFindings<'_> {
    let mut risks = Vec::new();
    let mut missing = Vec::new();
    let mut orphaned = Vec::new();
    let mut env_var_calls = Vec::new();
    let mut major_dependencies = Vec::new();
    let mut unparseable_dependencies = Vec::new();
    let mut shared_contracts = Vec::new();
    let mut degraded_types = Vec::new();

    for finding in findings {
        match finding {
            Finding::TypeMismatch { .. } | Finding::MethodMismatch { .. } => risks.push(finding),
            Finding::MissingEndpoint { .. } => missing.push(finding),
            Finding::OrphanedEndpoint { .. } => orphaned.push(finding),
            Finding::EnvVarCall { .. } => env_var_calls.push(finding),
            Finding::DependencyConflict { tier: t, .. } => {
                if t == tier::MAJOR {
                    major_dependencies.push(finding);
                } else {
                    unparseable_dependencies.push(finding);
                }
            }
            Finding::SharedExternalContract { .. } => shared_contracts.push(finding),
            Finding::DegradedTypes { .. } => degraded_types.push(finding),
        }
    }

    CategorizedFindings {
        risks,
        missing,
        orphaned,
        configuration: group_env_var_suggestions(&env_var_calls),
        major_dependencies,
        unparseable_dependencies,
        shared_contracts,
        degraded_types,
    }
}

/// The degraded-types banner: which services lost their types, at which
/// stage, and what the failure was. Rendered above every other section
/// because it changes how the rest of the report should be read — no type
/// verdict below it means "not checked", not "checked and fine".
fn format_degraded_types_section(degraded: &[&Finding]) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "<details open>\n<summary><strong>Types not extracted ({})</strong></summary>\n\n",
        degraded.len()
    ));
    output.push_str(
        "> Type extraction failed for the services below, so no contract in them was \
         type-checked this run. Endpoints and calls are still indexed; their types are not.\n\n",
    );
    output.push_str("| Service | Stage | Failure |\n| :--- | :--- | :--- |\n");
    for finding in degraded {
        let Finding::DegradedTypes {
            service,
            stage,
            detail,
        } = finding
        else {
            continue;
        };
        output.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            code_cell(service),
            code_cell(stage),
            cell(detail)
        ));
    }
    output.push_str("\n</details>");
    output
}

fn format_critical_section(risks: &[&Finding]) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "<details>\n<summary><strong>Contract risks ({})</strong></summary>\n\n",
        risks.len()
    ));
    output.push_str(
        "> A consumer call conflicts with the producer it targets in the index. These break the consumer at runtime.\n\n",
    );
    output.push_str("| Endpoint | Issue |\n| :--- | :--- |\n");
    for finding in risks {
        let (endpoint, detail) = match finding {
            Finding::TypeMismatch {
                method,
                path,
                producer_type,
                consumer_type,
                detail,
                producer_provenance,
                ..
            } => (
                format!("{} {}", method, path),
                format!(
                    "producer `{}`{} vs consumer `{}`: {}",
                    producer_type,
                    // The producer shape comes from a mock/test handler —
                    // often still the canonical contract, but say so (#380).
                    if producer_provenance.is_mock() {
                        " (mock handler)"
                    } else {
                        ""
                    },
                    consumer_type,
                    detail
                ),
            ),
            Finding::MethodMismatch {
                method,
                path,
                expected_method,
                ..
            } => (
                format!("{} {}", method, path),
                format!(
                    "call uses `{}` but the producer expects `{}`",
                    method, expected_method
                ),
            ),
            // categorize_findings only routes the two risk kinds here.
            _ => continue,
        };
        output.push_str(&format!("| `{}` | {} |\n", cell(&endpoint), cell(&detail)));
    }
    output.push_str("\n</details>");
    output
}

/// Escape a value for a Markdown table cell: no pipes, and no line breaks
/// (CRLF, lone CR, or LF) that would otherwise split the row.
fn cell(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

/// Sanitize a value that will be wrapped in inline-code backticks inside a
/// table cell. Beyond `cell()`'s pipe/newline escaping, drop backticks: an
/// HTTP method, route, or service name should never contain one, and a stray
/// backtick (e.g. from a hand-written carrick.json `serviceName`) would break
/// out of the code span and could inject Markdown.
fn code_cell(value: &str) -> String {
    cell(value).replace('`', "")
}

/// Sanitize a value wrapped in inline-code backticks in prose (not a table):
/// collapse line breaks and drop backticks so the value can't break out of the
/// span. Unlike `code_cell` it does not escape pipes, which are literal in a
/// code span outside a table (escaping them would show a stray backslash).
fn code_span(value: &str) -> String {
    value
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .replace('`', "")
        .trim()
        .to_string()
}

fn format_connectivity_section(missing: &[&Finding], has_baseline: bool) -> String {
    let mut output = String::new();

    let heading = if has_baseline {
        "Connectivity"
    } else {
        "Connectivity Observations"
    };
    output.push_str(&format!(
        "<details>\n<summary><strong>{} ({})</strong></summary>\n\n",
        heading,
        missing.len()
    ));

    output.push_str("> Missing endpoints have a consumer call but no producer.\n\n");

    if !missing.is_empty() {
        output.push_str(&format!("**Missing ({})**\n\n", missing.len()));
        output.push_str("| Method | Path | Called from |\n| :--- | :--- | :--- |\n");
        for finding in missing {
            let Finding::MissingEndpoint {
                method,
                path,
                call_sites,
                ..
            } = finding
            else {
                continue;
            };
            let called_from = if call_sites.is_empty() {
                "-".to_string()
            } else {
                format!("`{}`", code_cell(&call_sites.join(", ")))
            };
            output.push_str(&format!(
                "| `{}` | `{}` | {} |\n",
                code_cell(method),
                code_cell(path),
                called_from
            ));
        }
        output.push('\n');
    }

    output.push_str("\n</details>");
    output
}

/// Orphaned endpoints, outside the headline and last in the report. The index
/// only sees service-to-service calls, so routes served to browsers, agents and
/// other external clients land here by design. Listed so a genuinely dead route
/// can be found, never counted as a problem.
fn format_unconsumed_section(orphaned: &[&Finding], has_baseline: bool) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "<details>\n<summary><strong>Endpoints without an indexed consumer ({})</strong></summary>\n\n",
        orphaned.len()
    ));

    output.push_str("> No indexed service calls these. Routes served to browsers, agents or other external clients belong here; nothing to fix unless you expected a service to call one of them.\n\n");

    if !has_baseline {
        output.push_str("> **First repo indexed for this project.** With nothing else to match against, every endpoint without a same-repo consumer is listed below; most resolve once you connect the repos that call them.\n\n");
    }

    type OrphanRow<'a> = (&'a String, &'a String, &'a Option<String>, &'static str);
    fn orphan_row(finding: &Finding) -> Option<OrphanRow<'_>> {
        match finding {
            Finding::OrphanedEndpoint {
                method,
                path,
                service,
                provenance,
                // The local report lists view modules like any other
                // unconsumed route for now; excluding them is the renderers'
                // change (R1b, cloud#599 and carrick#721).
                ..
            } => {
                // Mark mock/test-handler producers: an orphaned mock is
                // expected (its consumers are usually not scanned), so the
                // row should not read like a dead product route (#380).
                let marker = if provenance.is_mock() {
                    " (mock handler)"
                } else {
                    ""
                };
                Some((method, path, service, marker))
            }
            _ => None,
        }
    }
    // Show the owning-service column only when at least one orphan is
    // attributed (single-repo runs have none, so the column is dropped).
    if orphaned
        .iter()
        .filter_map(|f| orphan_row(f))
        .any(|(_, _, service, _)| service.is_some())
    {
        output.push_str("| Method | Path | Service |\n| :--- | :--- | :--- |\n");
        for (method, path, service, marker) in orphaned.iter().filter_map(|f| orphan_row(f)) {
            // A row can be unattributed even when the column is shown (e.g.
            // a GraphQL orphan alongside an attributed HTTP one); use a dash
            // rather than an empty cell.
            let service = service
                .as_deref()
                .map(|s| format!("`{}`", code_cell(s)))
                .unwrap_or_else(|| "-".to_string());
            output.push_str(&format!(
                "| `{}` | `{}`{} | {} |\n",
                code_cell(method),
                code_cell(path),
                marker,
                service
            ));
        }
    } else {
        output.push_str("| Method | Path |\n| :--- | :--- |\n");
        for (method, path, _, marker) in orphaned.iter().filter_map(|f| orphan_row(f)) {
            output.push_str(&format!(
                "| `{}` | `{}`{} |\n",
                code_cell(method),
                code_cell(path),
                marker
            ));
        }
    }

    output.push_str("\n</details>");
    output
}

fn format_configuration_section(issues: &[EnvVarSuggestionGroup]) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "<details>\n<summary><strong>Configuration suggestions ({})</strong></summary>\n\n",
        issues.len()
    ));

    output.push_str("> These calls use environment variables to construct the URL. Add them to `internalEnvVars` (to validate routes) or `externalEnvVars` (to ignore) in your `carrick.json`.\n\n");

    for issue in issues {
        let count = issue.locations.len().max(1);
        output.push_str(&format!(
            "  - `{} {}` using **[{}]** ({} call site{})\n",
            issue.method,
            issue.path,
            issue.env_var,
            count,
            if count == 1 { "" } else { "s" }
        ));

        for loc in issue.locations.iter().take(3) {
            output.push_str(&format!("    - `{}`\n", loc));
        }
        if issue.locations.len() > 3 {
            output.push_str(&format!("    - … +{} more\n", issue.locations.len() - 3));
        }
    }

    output.push_str("\n</details>");
    output
}

/// Render the shared-external-contract groups (#379): repos whose call sites
/// all target the same route that no indexed service defines. Deliberately
/// role-free wording — none of the repos is a producer, and no contract
/// verdict is implied.
fn format_shared_contract_section(shared: &[&Finding]) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "<details>\n<summary><strong>Shared external contracts ({})</strong></summary>\n\n",
        shared.len()
    ));
    output.push_str(
        "> These repos call the same API, but no indexed service defines it — they encode the same external contract. Useful as a change-impact signal; no producer exists in the index, so no contract verdict is implied.\n\n",
    );
    output.push_str("| Method | Path | Encoded by | Call sites |\n| :--- | :--- | :--- | :--- |\n");
    for finding in shared {
        let Finding::SharedExternalContract {
            method,
            path,
            repos,
            call_sites,
        } = finding
        else {
            // categorize_findings only routes SharedExternalContract here.
            continue;
        };
        let repo_list = repos
            .iter()
            .map(|r| format!("`{}`", code_cell(r)))
            .collect::<Vec<_>>()
            .join(", ");
        let sites = if call_sites.is_empty() {
            "-".to_string()
        } else {
            format!("`{}`", code_cell(&call_sites.join(", ")))
        };
        output.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            code_cell(method),
            code_cell(path),
            repo_list,
            sites
        ));
    }
    output.push_str("\n</details>");
    output
}

fn format_dependency_section(major: &[&Finding], unparseable: &[&Finding]) -> String {
    let mut output = String::new();

    output.push_str(&format!(
        "<details>\n<summary><strong>Dependency conflicts ({})</strong></summary>\n\n",
        major.len() + unparseable.len()
    ));

    output.push_str("> Packages with different versions across services in the index.\n\n");

    let render_group = |output: &mut String, heading: &str, group: &[&Finding]| {
        if group.is_empty() {
            return;
        }
        output.push_str(&format!("### {} ({})\n\n", heading, group.len()));
        for finding in group {
            let Finding::DependencyConflict {
                package_name,
                versions,
                ..
            } = finding
            else {
                continue;
            };
            output.push_str(&format!("#### {}\n\n", package_name));
            output.push_str("| Repository | Version | Source |\n| :--- | :--- | :--- |\n");
            for v in versions {
                output.push_str(&format!(
                    "| `{}` | `{}` | `{}` |\n",
                    code_cell(&v.repo),
                    code_cell(&v.version),
                    code_cell(&v.source)
                ));
            }
            output.push('\n');
        }
    };

    render_group(&mut output, "Major version conflicts", major);
    render_group(
        &mut output,
        "Unparseable version pins (advisory)",
        unparseable,
    );

    output.push_str("</details>");
    output
}

/// Regroup typed env-var findings by `(env_var, method, path)` for the
/// configuration section. The analyzer already groups per matcher pass, so
/// this mostly re-sorts; the BTreeMap keeps the section order deterministic.
fn group_env_var_suggestions(findings: &[&Finding]) -> Vec<EnvVarSuggestionGroup> {
    let mut grouped: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();

    for finding in findings {
        let Finding::EnvVarCall {
            method,
            path,
            env_var,
            call_sites,
        } = finding
        else {
            continue;
        };
        grouped
            .entry((env_var.clone(), method.clone(), path.clone()))
            .or_default()
            .extend(call_sites.iter().cloned());
    }

    grouped
        .into_iter()
        .map(
            |((env_var, method, path), locations)| EnvVarSuggestionGroup {
                method,
                env_var,
                path,
                locations: locations.into_iter().collect(),
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{EndpointRef, Finding, PackageVersionRef, tier};
    use crate::operation::EndpointProvenance;

    /// A poly-repo topology with peers, so connectivity findings are
    /// conclusive (has_baseline == true).
    fn topology_baseline() -> Topology {
        Topology {
            repo_name: "api-server".to_string(),
            local_service_count: 1,
            peer_repo_count: 2,
        }
    }

    /// A lone single-service repo with no peers (has_baseline == false): the
    /// first-repo-indexed framing.
    fn topology_first_repo() -> Topology {
        Topology {
            repo_name: "api-server".to_string(),
            local_service_count: 1,
            peer_repo_count: 0,
        }
    }

    fn sdk_edge(producer_key: &str) -> crate::cloud_storage::SdkEdge {
        crate::cloud_storage::SdkEdge {
            consumer_repo: "checkout".to_string(),
            consumer_location: "src/checkout.ts:42".to_string(),
            package: "@fixture/ledger-sdk".to_string(),
            import_symbol: Some("default".to_string()),
            callee: "ledger.payments.create".to_string(),
            sdk_repo: "ledger-sdk".to_string(),
            sdk_member: "payments.create".to_string(),
            sdk_location: "src/resources/payments.ts:28".to_string(),
            producer_repo: "payments-api".to_string(),
            producer_key: producer_key.to_string(),
            type_compatible: Some(true),
            mismatch_reason: None,
            scanner_version: "0.0.0-test".to_string(),
        }
    }

    fn result_with_sdk(
        edges: Vec<crate::cloud_storage::SdkEdge>,
        unresolved: Vec<crate::cloud_storage::SdkUnresolved>,
    ) -> ApiAnalysisResult {
        let mut result = result_with(vec![]);
        result.sdk_edges = edges;
        result.sdk_unresolved = unresolved;
        result
    }

    /// A run that lost its types must say so above everything else and count
    /// toward the headline. Before this, a dead sidecar produced one warn
    /// line and a report that read as clean (carrick#535).
    #[test]
    fn degraded_types_section_leads_the_report_and_counts() {
        let output = format_analysis_results(
            result_with(vec![Finding::degraded_types(
                "api",
                "capture",
                "Sidecar process died unexpectedly",
            )]),
            &topology_baseline(),
            None,
        );

        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
        assert!(output.contains("[!CAUTION]"));
        assert!(output.contains("**types not extracted for 1 service**"));
        assert!(output.contains("<summary><strong>Types not extracted (1)</strong></summary>"));
        assert!(output.contains("| `api` | `capture` | Sidecar process died unexpectedly |"));
        // It leads: nothing else may be rendered above it.
        let degraded_at = output.find("Types not extracted").expect("section");
        let indexed_at = output.find("Indexed **").expect("stats line");
        assert!(degraded_at > indexed_at);
    }

    /// The section states the whole relationship on one row: who calls, from
    /// where, through what, onto which endpoint, and what the types say.
    #[test]
    fn sdk_section_lists_each_edge_end_to_end() {
        let output = format_analysis_results(
            result_with_sdk(vec![sdk_edge("http|POST|/v1/payments")], vec![]),
            &topology_baseline(),
            None,
        );

        assert!(output.contains("<summary><strong>Consumers via SDK (1)</strong></summary>"));
        assert!(output.contains("| `checkout` | `src/checkout.ts:42` | `ledger.payments.create` \u{2192} `@fixture/ledger-sdk` | `POST /v1/payments` (`payments-api`) | compatible |"));
    }

    /// A pair with no stored verdict must not read as compatible, and the
    /// section says where the missing verdict would have come from (#525).
    #[test]
    fn sdk_section_reports_an_unevaluated_pair_as_unverified() {
        let mut edge = sdk_edge("http|POST|/v1/payments");
        edge.type_compatible = None;
        let output = format_analysis_results(
            result_with_sdk(vec![edge], vec![]),
            &topology_baseline(),
            None,
        );
        assert!(output.contains("| unverified |"));
        assert!(!output.contains("| compatible |"));
        assert!(output.contains(
            "1 edge unverified: no type verdict is stored in `ledger-sdk`'s own scan for that \
             call. Re-scan it to produce one."
        ));
    }

    /// An incompatible edge names the break in its row.
    #[test]
    fn sdk_section_reports_an_incompatible_pair_with_its_reason() {
        let mut edge = sdk_edge("http|POST|/v1/payments");
        edge.type_compatible = Some(false);
        edge.mismatch_reason = Some("Property 'amountCents' is missing".to_string());
        let output = format_analysis_results(
            result_with_sdk(vec![edge], vec![]),
            &topology_baseline(),
            None,
        );
        assert!(output.contains("| **INCOMPATIBLE**: Property 'amountCents' is missing |"));
        // Every edge carries a verdict, so the unverified footnote stays away.
        assert!(!output.contains("unverified"));
    }

    /// The point of #525: an SDK-mediated break weighs the same as a direct
    /// one. The finding the join emits is a contract risk, so it counts in the
    /// headline and turns the callout red.
    #[test]
    fn an_incompatible_sdk_edge_counts_toward_the_pr_verdict() {
        let mut edge = sdk_edge("http|POST|/v1/payments");
        edge.type_compatible = Some(false);
        edge.mismatch_reason = Some("Property 'amountCents' is missing".to_string());

        let mut result = result_with_sdk(vec![edge.clone()], vec![]);
        // The engine appends exactly this before rendering.
        result.findings = crate::sdk_edges::type_mismatch_findings(&[edge]);

        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
        assert!(output.contains("[!CAUTION]"));
        assert!(output.contains("**1 contract risk**"));
        // The risk row cites the consumer's own call site and the hop.
        assert!(output.contains("<summary><strong>Contract risks (1)</strong></summary>"));
        assert!(output.contains("reached through `@fixture/ledger-sdk`"));
    }

    /// "There are no such calls" and "this package is called and nothing is
    /// known about where those calls land" are different statements.
    #[test]
    fn sdk_section_reports_unresolved_calls_with_their_reasons() {
        let output = format_analysis_results(
            result_with_sdk(
                vec![],
                vec![
                    crate::cloud_storage::SdkUnresolved {
                        package: "@fixture/ledger-sdk".to_string(),
                        count: 3,
                        reason: "member_not_found".to_string(),
                    },
                    crate::cloud_storage::SdkUnresolved {
                        package: "@vendor/mailer".to_string(),
                        count: 1,
                        reason: "no_sdk_repo_in_project".to_string(),
                    },
                ],
            ),
            &topology_baseline(),
            None,
        );

        assert!(output.contains("<summary><strong>Consumers via SDK (0)</strong></summary>"));
        assert!(output.contains("No SDK call resolved to an indexed endpoint."));
        assert!(output.contains(
            "4 SDK calls could not be resolved to an endpoint: `@fixture/ledger-sdk` \u{00d7}3 \
             member_not_found and `@vendor/mailer` \u{00d7}1 no_sdk_repo_in_project."
        ));
    }

    /// A consumer reaching a route through a published client writes no URL,
    /// so it can never appear as a missing endpoint — the SDK edge is the only
    /// evidence the removed route still has a caller.
    #[test]
    fn test_pr_delta_flags_a_removed_endpoint_still_consumed_via_an_sdk() {
        let delta = PrDelta {
            new_endpoints: vec![],
            removed_endpoints: vec![EndpointRef {
                method: "POST".to_string(),
                path: "/v1/payments".to_string(),
                service: None,
            }],
        };
        let output = format_analysis_results(
            result_with_sdk(vec![sdk_edge("http|POST|/v1/payments")], vec![]),
            &topology_baseline(),
            Some(&delta),
        );

        assert!(output.contains(
            "- Removed `POST /v1/payments` \u{2014} \u{26a0} still consumed via \
             `@fixture/ledger-sdk` by `checkout`"
        ));
    }

    /// The SDK marker reuses the param-aware route join, so a producer key
    /// carrying `:paymentId` still flags a removal declared as `:id`.
    #[test]
    fn sdk_still_consumed_marker_is_param_aware() {
        let delta = PrDelta {
            new_endpoints: vec![],
            removed_endpoints: vec![EndpointRef {
                method: "POST".to_string(),
                path: "/v1/payments/:id/refunds".to_string(),
                service: None,
            }],
        };
        let output = format_analysis_results(
            result_with_sdk(
                vec![sdk_edge("http|POST|/v1/payments/:paymentId/refunds")],
                vec![],
            ),
            &topology_baseline(),
            Some(&delta),
        );

        assert!(output.contains("still consumed via `@fixture/ledger-sdk` by `checkout`"));
    }

    /// A non-HTTP producer key names no route, so it can never flag a removed
    /// HTTP endpoint.
    #[test]
    fn sdk_marker_ignores_non_http_producer_keys() {
        let delta = PrDelta {
            new_endpoints: vec![],
            removed_endpoints: vec![EndpointRef {
                method: "POST".to_string(),
                path: "/v1/payments".to_string(),
                service: None,
            }],
        };
        let output = format_analysis_results(
            result_with_sdk(vec![sdk_edge("graphql|query|payments")], vec![]),
            &topology_baseline(),
            Some(&delta),
        );

        assert!(output.contains("- Removed `POST /v1/payments`\n"));
        assert!(!output.contains("still consumed"));
    }

    fn result_with(findings: Vec<Finding>) -> ApiAnalysisResult {
        ApiAnalysisResult {
            endpoints: vec![],
            calls: vec![],
            findings,
            dependency_conflicts: vec![],
            verified_endpoints: vec![],
            sdk_edges: Vec::new(),
            sdk_unresolved: Vec::new(),
            detected_graphql_libraries: vec![],
            graphql_operations_indexed: false,
            cross_repo_matches: vec![],
        }
    }

    /// #379: a shared-external-contract group renders as its own role-free
    /// advisory section — named repos, no producer/consumer labels, no
    /// headline issue counted, and no connectivity framing.
    #[test]
    fn shared_external_contract_renders_role_free_group() {
        let result = result_with(vec![Finding::shared_external_contract(
            "POST",
            "/v2/widgets",
            vec!["repo-alpha".to_string(), "repo-beta".to_string()],
            vec!["src/widgets-client.ts:33".to_string()],
        )]);
        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(
            output.contains("Shared external contracts (1)"),
            "missing section: {output}"
        );
        assert!(output.contains("`repo-alpha`") && output.contains("`repo-beta`"));
        assert!(output.contains("`POST`") && output.contains("`/v2/widgets`"));
        // Advisory: never a headline issue.
        assert!(
            output.contains("<!-- CARRICK_ISSUE_COUNT:0 -->"),
            "shared contracts must not count as issues: {output}"
        );
        // Role-free table: repos are "Encoded by", never a Producer/Consumer
        // column.
        assert!(output.contains("| Method | Path | Encoded by | Call sites |"));
        assert!(!output.contains("| Producer") && !output.contains("| Consumer"));
        // The verdict mentions the group.
        assert!(output.contains("1 shared external contract"));
    }

    fn type_mismatch_finding() -> Finding {
        Finding::type_mismatch(
            "GET",
            "/api/users",
            None,
            vec!["web/src/client.ts:12".to_string()],
            "UserResponse",
            "User[]",
            "Property 'role' is missing",
        )
    }

    fn major_conflict_finding() -> Finding {
        Finding::dependency_conflict(
            "zod",
            tier::MAJOR,
            vec![
                PackageVersionRef {
                    repo: "billing".to_string(),
                    version: "3.22.0".to_string(),
                    source: "billing/package.json".to_string(),
                },
                PackageVersionRef {
                    repo: "web".to_string(),
                    version: "4.0.0".to_string(),
                    source: "web/package.json".to_string(),
                },
            ],
        )
    }

    #[test]
    fn test_type_mismatch_renders_as_contract_risk() {
        let output = format_analysis_results(
            result_with(vec![type_mismatch_finding()]),
            &topology_baseline(),
            None,
        );

        // The endpoint, both type names, and the compiler error surface as a
        // contract-risk table row, and the risk headlines the verdict.
        assert!(output.contains("Contract risks (1)"));
        assert!(output.contains("GET /api/users"));
        assert!(output.contains("UserResponse"));
        assert!(output.contains("User[]"));
        assert!(output.contains("Property 'role' is missing"));
        assert!(output.contains("> [!CAUTION]"));
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
    }

    #[test]
    fn test_method_mismatch_renders_as_contract_risk() {
        let finding = Finding::method_mismatch(
            "GET",
            "/api/orders",
            None,
            vec!["web/src/orders.ts:4".to_string()],
            "POST",
        );
        let output =
            format_analysis_results(result_with(vec![finding]), &topology_baseline(), None);

        assert!(output.contains("Contract risks (1)"));
        assert!(output.contains("GET /api/orders"));
        assert!(output.contains("call uses `GET` but the producer expects `POST`"));
        assert!(output.contains("> [!CAUTION]"));
        // A method mismatch is a risk even without a connectivity baseline.
        let no_baseline = format_analysis_results(
            result_with(vec![Finding::method_mismatch(
                "GET",
                "/api/orders",
                None,
                vec![],
                "POST",
            )]),
            &topology_first_repo(),
            None,
        );
        assert!(no_baseline.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
    }

    #[test]
    fn test_graphql_banner_renders_when_libraries_detected() {
        let mut result = result_with(vec![]);
        result.detected_graphql_libraries =
            vec!["graphql-request".to_string(), "@apollo/client".to_string()];
        let output = format_analysis_results(result, &topology_baseline(), None);
        assert!(output.contains("GraphQL detected"));
        assert!(output.contains("graphql-request"));
        assert!(output.contains("@apollo/client"));
        assert!(output.contains("no schema or operation documents were found"));
    }

    #[test]
    fn test_graphql_banner_absent_when_operations_indexed() {
        let mut result = result_with(vec![]);
        result.detected_graphql_libraries = vec!["@apollo/client".to_string()];
        result.graphql_operations_indexed = true;
        let output = format_analysis_results(result, &topology_baseline(), None);
        assert!(!output.contains("GraphQL detected"));
    }

    #[test]
    fn test_graphql_banner_absent_when_no_libraries() {
        let output = format_analysis_results(result_with(vec![]), &topology_baseline(), None);
        assert!(!output.contains("GraphQL detected"));
    }

    #[test]
    fn test_no_issues_output() {
        let output = format_analysis_results(result_with(vec![]), &topology_baseline(), None);

        assert!(output.contains("All cross-service calls match the indexed contracts"));
        assert!(output.contains("CARRICK_ISSUE_COUNT:0"));
    }

    /// The machine markers are the Action's contract: the terminal output must
    /// carry the delimiters and a parseable issue count. (The old
    /// `pr_comment_body` stripping is gone — the cloud renders its own
    /// comment from the typed payload.)
    #[test]
    fn test_terminal_output_carries_machine_markers() {
        for result in [
            result_with(vec![]),
            result_with(vec![type_mismatch_finding()]),
        ] {
            let formatted = FormattedOutput::new(result, topology_baseline(), None);
            assert!(formatted.content.contains("<!-- CARRICK_OUTPUT_START -->"));
            assert!(formatted.content.contains("<!-- CARRICK_OUTPUT_END -->"));
            assert!(formatted.content.contains("CARRICK_ISSUE_COUNT:"));
        }
    }

    #[test]
    fn test_verified_section_renders_when_matches_present() {
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![
            crate::analyzer::VerifiedEndpointEntry {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: EndpointProvenance::Route,
                type_verdict: None,
            },
            crate::analyzer::VerifiedEndpointEntry {
                method: "POST".to_string(),
                path: "/api/orders".to_string(),
                provenance: EndpointProvenance::Route,
                type_verdict: None,
            },
        ];

        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("Verified (2)"));
        assert!(output.contains("`GET` | `/api/users`"));
        assert!(output.contains("`POST` | `/api/orders`"));
    }

    #[test]
    fn test_verified_section_marks_mock_producers() {
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![
            crate::analyzer::VerifiedEndpointEntry {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                provenance: EndpointProvenance::Route,
                type_verdict: None,
            },
            crate::analyzer::VerifiedEndpointEntry {
                method: "GET".to_string(),
                path: "/api/widgets".to_string(),
                provenance: EndpointProvenance::Mock,
                type_verdict: None,
            },
        ];

        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("`GET` | `/api/users` |"));
        assert!(
            output.contains("`GET` | `/api/widgets` (mock handler) |"),
            "mock producer must be marked in the verified table: {output}"
        );
    }

    #[test]
    fn test_orphaned_and_mismatch_rows_mark_mock_producers() {
        use crate::operation::EndpointProvenance;

        let findings = vec![
            Finding::type_mismatch(
                "GET",
                "/api/widgets",
                None,
                vec!["client.ts:3".into()],
                "Widget",
                "Gadget",
                "boom",
            )
            .with_producer_provenance(EndpointProvenance::Mock),
            Finding::orphaned_endpoint("POST", "/api/widgets", None)
                .with_producer_provenance(EndpointProvenance::Mock),
        ];
        let result = result_with(findings);
        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(
            output.contains("producer `Widget` (mock handler) vs consumer `Gadget`"),
            "type-mismatch row must mark a mock producer: {output}"
        );
        assert!(
            output.contains("`/api/widgets` (mock handler)"),
            "orphaned row must mark a mock producer: {output}"
        );
    }

    #[test]
    fn test_verified_section_singular_label() {
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![crate::analyzer::VerifiedEndpointEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            provenance: EndpointProvenance::Route,
            type_verdict: None,
        }];
        let output = format_analysis_results(result, &topology_baseline(), None);
        assert!(output.contains("Verified (1)"));
    }

    #[test]
    fn test_verified_section_renders_on_clean_run() {
        // Even when there are zero issues, a clean run with verified
        // matches must surface them — that's the whole point of the
        // section. Pre-fix, `format_no_issues` ignored verified entirely.
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![crate::analyzer::VerifiedEndpointEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            provenance: EndpointProvenance::Route,
            type_verdict: None,
        }];
        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("All cross-service calls match the indexed contracts"));
        assert!(output.contains("Verified (1)"));
    }

    /// #456: with NO per-endpoint verdicts, the verified section makes only
    /// the structural claim — it must NOT blanket-assert the TypeScript
    /// compiler compared anything (the overstatement this issue kills).
    #[test]
    fn test_verified_section_no_verdicts_makes_no_compiler_claim() {
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![crate::analyzer::VerifiedEndpointEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            provenance: EndpointProvenance::Route,
            type_verdict: None,
        }];
        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("Verified (1)"));
        assert!(
            output.contains("Endpoints with at least one matching consumer call."),
            "structural claim is always present: {output}"
        );
        assert!(
            !output.contains("compared by the TypeScript compiler pass"),
            "a row with no verdict must not claim compiler comparison: {output}"
        );
        assert!(
            !output.contains("Type-checked"),
            "no Type-checked bucket without a compatible verdict: {output}"
        );
    }

    /// #456: mixed verdicts split into the three honest buckets, each with its
    /// own caption — the compiler claim applies ONLY to the compatible row.
    #[test]
    fn test_verified_section_buckets_by_type_verdict() {
        use crate::operation::TypeVerdict;
        let ve = |method: &str, path: &str, verdict: Option<TypeVerdict>| {
            crate::analyzer::VerifiedEndpointEntry {
                method: method.to_string(),
                path: path.to_string(),
                provenance: EndpointProvenance::Route,
                type_verdict: verdict,
            }
        };
        let mut result = result_with(vec![]);
        result.verified_endpoints = vec![
            ve("GET", "/api/users", Some(TypeVerdict::Compatible)),
            ve("POST", "/api/refunds", Some(TypeVerdict::Unverifiable)),
            ve("PUT", "/api/widgets", None),
        ];
        let output = format_analysis_results(result, &topology_baseline(), None);

        assert!(output.contains("Verified (3)"));
        assert!(output.contains("**Type-checked (1)**"), "output: {output}");
        assert!(
            output.contains("**Types not verifiable (1)**"),
            "output: {output}"
        );
        assert!(
            output.contains("**Types not compared (1)**"),
            "output: {output}"
        );
        assert!(output.contains("compared by the TypeScript compiler pass"));
        // The compatible endpoint sits under Type-checked; the unverifiable and
        // no-verdict endpoints must NOT be under a compiler-compared claim.
        let type_checked_idx = output.find("**Type-checked").unwrap();
        let users_idx = output.find("/api/users").unwrap();
        let refunds_idx = output.find("/api/refunds").unwrap();
        assert!(
            users_idx > type_checked_idx && refunds_idx > users_idx,
            "compatible row precedes the unverifiable row across buckets: {output}"
        );
    }

    #[test]
    fn test_env_var_findings_group_into_configuration_section() {
        // Two findings for the same (env_var, method, path) — e.g. one per
        // matcher pass — regroup into a single suggestion with merged,
        // deduplicated call sites.
        let findings = vec![
            Finding::env_var_call(
                "GET",
                "/orders",
                "ORDER_SERVICE_URL",
                vec!["src/orders.ts:3".to_string(), "src/retry.ts:9".to_string()],
            ),
            Finding::env_var_call(
                "GET",
                "/orders",
                "ORDER_SERVICE_URL",
                vec!["src/orders.ts:3".to_string()],
            ),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        assert!(output.contains("Configuration suggestions (1)"));
        assert!(output.contains("`GET /orders` using **[ORDER_SERVICE_URL]** (2 call sites)"));
        assert!(output.contains("`src/orders.ts:3`"));
        assert!(output.contains("`src/retry.ts:9`"));
        // Advisory findings never gate CI.
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:0 -->"));
    }

    #[test]
    fn test_missing_endpoint_lists_call_sites() {
        let finding = Finding::missing_endpoint(
            "POST",
            "/api/orders",
            None,
            vec!["src/client.ts:8".to_string()],
        );
        let output =
            format_analysis_results(result_with(vec![finding]), &topology_baseline(), None);

        assert!(output.contains("**Missing (1)**"));
        assert!(output.contains("| `POST` | `/api/orders` | `src/client.ts:8` |"));
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
    }

    #[test]
    fn test_no_baseline_excludes_connectivity_from_headline_count() {
        // First repo indexed (has_baseline = false): connectivity findings are
        // inconclusive (no peers to match against) so they must be kept OUT of
        // the headline CARRICK_ISSUE_COUNT, yet the sections must still render —
        // the missing one framed as informational "Observations", the orphans
        // in their own always-informational section.
        let findings = vec![
            Finding::missing_endpoint("GET", "/api/invoices", None, vec![]),
            Finding::orphaned_endpoint("GET", "/api/users", None),
            Finding::orphaned_endpoint("PUT", "/api/sessions", None),
        ];
        let output = format_analysis_results(result_with(findings), &topology_first_repo(), None);

        // Headline count excludes the connectivity findings → zero issues.
        assert!(
            output.contains("<!-- CARRICK_ISSUE_COUNT:0 -->"),
            "first-run connectivity findings must not inflate the headline count, got:\n{}",
            output
        );
        // But the connectivity section still renders, framed as observations.
        assert!(
            output.contains("Connectivity Observations"),
            "connectivity section must still render as informational observations, got:\n{}",
            output
        );
        // The orphans are listed apart from it, never inside its count.
        assert!(
            output.contains(
                "<summary><strong>Endpoints without an indexed consumer (2)</strong></summary>"
            ),
            "orphans must render in their own section, got:\n{}",
            output
        );
        assert!(
            output.contains("Connectivity Observations (1)"),
            "the connectivity heading counts missing only, got:\n{}",
            output
        );
        assert!(
            output.contains("First repo indexed"),
            "first-run observations must carry the informational framing, got:\n{}",
            output
        );
        // The orphaned endpoints are rendered as Method/Path table rows.
        assert!(output.contains("`/api/users`"));
        assert!(output.contains("`/api/sessions`"));
        // Headline phrasing reflects the informational framing, not "gaps",
        // and the orphans get their own clause rather than swelling that one.
        assert!(output.contains("1 connectivity observation"));
        assert!(output.contains("2 endpoints without an indexed consumer"));
    }

    /// With a baseline, a missing endpoint counts. An orphan alongside it does
    /// not: it is named in its own verdict clause and its own section, and
    /// stays out of both the headline count and the connectivity heading.
    #[test]
    fn test_baseline_counts_connectivity_gaps() {
        let findings = vec![
            Finding::missing_endpoint("GET", "/a", None, vec![]),
            Finding::orphaned_endpoint("POST", "/b", None),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
        assert!(output.contains("1 connectivity gap"));
        assert!(output.contains("1 endpoint without an indexed consumer"));
        assert!(output.contains("> [!WARNING]"));
        // The connectivity heading counts the missing endpoint only.
        assert!(output.contains("<summary><strong>Connectivity (1)</strong></summary>"));
        assert!(!output.contains("Orphaned ("));
    }

    #[test]
    fn test_monorepo_header_and_scope() {
        let topology = Topology {
            repo_name: "platform".to_string(),
            local_service_count: 3,
            peer_repo_count: 0,
        };
        // A monorepo has a cross-service baseline even with no peers, so a
        // type mismatch headlines as a contract risk.
        let output =
            format_analysis_results(result_with(vec![type_mismatch_finding()]), &topology, None);

        assert!(output.contains("## 🪢 Carrick · monorepo (3 services)"));
        assert!(output.contains("> [!CAUTION]"));
        assert!(output.contains("contract risk"));
        assert!(output.contains("3 services"));
    }

    #[test]
    fn test_single_repo_clean_header() {
        let topology = Topology {
            repo_name: "api-server".to_string(),
            local_service_count: 1,
            peer_repo_count: 0,
        };
        let output = format_analysis_results(result_with(vec![]), &topology, None);

        assert!(output.contains("## 🪢 Carrick · api-server"));
        assert!(output.contains("> [!TIP]"));
        assert!(output.contains("across api-server"));
    }

    #[test]
    fn test_monorepo_with_peers_scope_has_no_double_across() {
        // A monorepo that also has peer repos is the one topology where the
        // scope phrase and the verdict's "across" could collide. The service
        // count lives in the header suffix; the scope reports repos.
        let topology = Topology {
            repo_name: "platform".to_string(),
            local_service_count: 3,
            peer_repo_count: 2,
        };
        let output =
            format_analysis_results(result_with(vec![major_conflict_finding()]), &topology, None);

        assert!(output.contains("## 🪢 Carrick · monorepo (3 services)"));
        assert!(output.contains("across 3 repos"));
        assert!(
            !output.contains("services across"),
            "the verdict must not double 'across', got:\n{}",
            output
        );
    }

    #[test]
    fn test_verdict_warning_on_major_dependency_conflict() {
        let output = format_analysis_results(
            result_with(vec![major_conflict_finding()]),
            &topology_baseline(),
            None,
        );

        // No contract risks, but a counted issue → amber, not red, and the
        // major tier counts toward the headline.
        assert!(output.contains("> [!WARNING]"));
        assert!(!output.contains("> [!CAUTION]"));
        assert!(output.contains("dependency conflict"));
        assert!(output.contains("Major version conflicts (1)"));
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
    }

    #[test]
    fn test_unparseable_dependency_conflict_is_advisory() {
        let finding = Finding::dependency_conflict(
            "internal-lib",
            tier::UNPARSEABLE,
            vec![PackageVersionRef {
                repo: "billing".to_string(),
                version: "workspace:*".to_string(),
                source: "billing/package.json".to_string(),
            }],
        );
        let output =
            format_analysis_results(result_with(vec![finding]), &topology_baseline(), None);

        // Listed, but never counted and never escalating past NOTE.
        assert!(output.contains("Unparseable version pins"));
        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:0 -->"));
        assert!(output.contains("> [!NOTE]"));
    }

    #[test]
    fn test_no_baseline_without_connectivity_omits_baseline_note() {
        // A lone repo with only a configuration suggestion (no connectivity
        // findings) must not claim "connectivity findings are informational".
        let findings = vec![Finding::env_var_call(
            "GET",
            "/orders",
            "ORDER_SERVICE_URL",
            vec!["src/orders.ts".to_string()],
        )];
        let output = format_analysis_results(result_with(findings), &topology_first_repo(), None);

        assert!(output.contains("configuration suggestion"));
        assert!(
            !output.contains("First repo indexed"),
            "no connectivity findings means no baseline note, got:\n{}",
            output
        );
    }

    #[test]
    fn test_no_decorative_emoji_in_output() {
        // Severity is carried by GitHub alert blocks, not emoji. Only the 🪢
        // brand mark in the header is allowed.
        let findings = vec![
            type_mismatch_finding(),
            Finding::orphaned_endpoint("GET", "/legacy/ping", Some("billing".to_string())),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        for banned in ["✅", "ℹ️", "⚠️", "❌", "🔁"] {
            assert!(
                !output.contains(banned),
                "decorative emoji {} must not appear, got:\n{}",
                banned,
                output
            );
        }
        // The brand mark is intentionally retained.
        assert!(output.contains("🪢"));
    }

    #[test]
    fn test_orphaned_endpoints_show_service_when_attributed() {
        // When orphans carry an owning service, the unconsumed-endpoints table
        // gains a Service column naming where each lives.
        let findings = vec![
            Finding::orphaned_endpoint("GET", "/users", Some("auth".to_string())),
            Finding::orphaned_endpoint("POST", "/charges", Some("billing".to_string())),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        assert!(output.contains("| Method | Path | Service |"));
        assert!(output.contains("| `GET` | `/users` | `auth` |"));
        assert!(output.contains("| `POST` | `/charges` | `billing` |"));
    }

    #[test]
    fn test_orphaned_endpoints_omit_service_column_when_unattributed() {
        // A lone repo has no service attribution, so the column is dropped
        // rather than rendering an empty cell per row.
        let findings = vec![Finding::orphaned_endpoint("GET", "/legacy/ping", None)];
        let output = format_analysis_results(result_with(findings), &topology_first_repo(), None);

        assert!(!output.contains("Service |"));
        assert!(output.contains("| `GET` | `/legacy/ping` |"));
    }

    #[test]
    fn test_cell_escapes_pipes_and_collapses_line_breaks() {
        assert_eq!(cell("a|b"), "a\\|b");
        assert_eq!(cell("a\r\nb"), "a b");
        assert_eq!(cell("a\rb"), "a b");
        assert_eq!(cell("a\nb"), "a b");
        // code_cell additionally drops backticks for inline-code cells.
        assert_eq!(code_cell("x`y|z"), "xy\\|z");
    }

    #[test]
    fn test_orphaned_mixed_attribution_uses_dash_and_escapes_cells() {
        // When the Service column is shown, an unattributed row gets a dash, and
        // any pipe in a value is escaped so it can't break the table.
        let findings = vec![
            // Backtick must be stripped so it can't break the code span.
            Finding::orphaned_endpoint("GET", "/users", Some("au`th".to_string())),
            Finding::orphaned_endpoint("QUERY", "weird|field", None),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        assert!(output.contains("| `GET` | `/users` | `auth` |"));
        // Unattributed row → dash, and the pipe in the path is escaped.
        assert!(output.contains("| `QUERY` | `weird\\|field` | - |"));
    }

    /// Orphans alone are inventory, not issues: even with a full baseline the
    /// headline stays at zero, and they render in their own section rather
    /// than under "Connectivity".
    #[test]
    fn test_orphans_alone_are_informational_and_never_counted() {
        let findings = vec![
            Finding::orphaned_endpoint("GET", "/users", Some("auth".to_string())),
            Finding::orphaned_endpoint("POST", "/charges", Some("billing".to_string())),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        assert!(
            output.contains("<!-- CARRICK_ISSUE_COUNT:0 -->"),
            "orphans must never reach the headline count, got:\n{}",
            output
        );
        assert!(output.contains(
            "<summary><strong>Endpoints without an indexed consumer (2)</strong></summary>"
        ));
        assert!(output.contains(
            "> No indexed service calls these. Routes served to browsers, agents or other \
             external clients belong here; nothing to fix unless you expected a service to call \
             one of them."
        ));
        // The old sub-block under "Connectivity" is gone entirely.
        assert!(
            !output.contains("Orphaned ("),
            "orphans must not render as a Connectivity sub-block, got:\n{}",
            output
        );
        assert!(
            !output.contains("<summary><strong>Connectivity"),
            "no missing endpoints means no Connectivity section at all, got:\n{}",
            output
        );
        // Zero counted issues, but findings exist — so this is the NOTE path,
        // not the clean-run TIP.
        assert!(output.contains("> [!NOTE]"));
        assert!(!output.contains("All cross-service calls match the indexed contracts"));
    }

    /// Only one of the two connectivity kinds counts. The verdict must say
    /// both, in separate clauses, so "1 gap" is never read as "2 problems".
    #[test]
    fn test_verdict_names_missing_and_orphans_in_separate_clauses() {
        let findings = vec![
            Finding::missing_endpoint("GET", "/a", None, vec!["src/a.ts:1".to_string()]),
            Finding::orphaned_endpoint("POST", "/b", None),
        ];
        let output = format_analysis_results(result_with(findings), &topology_baseline(), None);

        assert!(output.contains("<!-- CARRICK_ISSUE_COUNT:1 -->"));
        assert!(output.contains("1 connectivity gap"));
        assert!(output.contains("1 endpoint without an indexed consumer"));
        // Singular clause, so the plural form must not leak in.
        assert!(!output.contains("1 connectivity gaps"));
        assert!(!output.contains("1 endpoints without an indexed consumer"));
    }

    /// The unconsumed section is dead last: informational, and usually the
    /// longest list on a system that serves browsers or agents.
    #[test]
    fn test_unconsumed_section_renders_after_every_other_section() {
        let findings = vec![
            major_conflict_finding(),
            Finding::env_var_call(
                "GET",
                "/orders",
                "ORDER_SERVICE_URL",
                vec!["src/orders.ts:3".to_string()],
            ),
            Finding::orphaned_endpoint("GET", "/users", Some("auth".to_string())),
        ];
        let mut result = result_with(findings);
        result.verified_endpoints = vec![crate::analyzer::VerifiedEndpointEntry {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            provenance: EndpointProvenance::Route,
            type_verdict: None,
        }];
        let output = format_analysis_results(result, &topology_baseline(), None);

        let unconsumed_at = output
            .find("Endpoints without an indexed consumer")
            .expect("unconsumed section");
        for earlier in [
            "Major version conflicts",
            "Configuration suggestions",
            "Verified (",
        ] {
            let at = output
                .find(earlier)
                .unwrap_or_else(|| panic!("missing section {earlier} in:\n{output}"));
            assert!(
                at < unconsumed_at,
                "{earlier} must render before the unconsumed section, got:\n{output}"
            );
        }
        // And still inside the machine-readable envelope.
        let end_at = output
            .find("<!-- CARRICK_OUTPUT_END -->")
            .expect("end marker");
        assert!(unconsumed_at < end_at);
    }

    #[test]
    fn test_pr_delta_strip_lists_new_endpoints() {
        let delta = PrDelta {
            new_endpoints: vec![
                EndpointRef {
                    method: "POST".to_string(),
                    path: "/v2/invoices".to_string(),
                    service: Some("billing".to_string()),
                },
                EndpointRef {
                    method: "GET".to_string(),
                    path: "/charges".to_string(),
                    service: None,
                },
            ],
            removed_endpoints: vec![],
        };
        let output = format_analysis_results(
            result_with(vec![major_conflict_finding()]),
            &topology_baseline(),
            Some(&delta),
        );

        assert!(output.contains("**In this PR**"));
        assert!(output.contains("- New endpoint `POST /v2/invoices` (`billing`)"));
        assert!(output.contains("- New endpoint `GET /charges`"));
        // The strip sits above the findings sections.
        let strip_at = output.find("**In this PR**").unwrap();
        let deps_at = output.find("Dependency conflicts").unwrap();
        assert!(strip_at < deps_at, "the strip must precede the sections");
    }

    #[test]
    fn test_pr_delta_absent_renders_no_strip() {
        let output = format_analysis_results(result_with(vec![]), &topology_baseline(), None);
        assert!(!output.contains("In this PR"));
    }

    #[test]
    fn test_pr_delta_strip_renders_on_clean_run() {
        // A PR can add an endpoint without introducing any issue, so the strip
        // must also appear on the clean (TIP) path.
        let delta = PrDelta {
            new_endpoints: vec![EndpointRef {
                method: "GET".to_string(),
                path: "/health".to_string(),
                service: None,
            }],
            removed_endpoints: vec![],
        };
        let output =
            format_analysis_results(result_with(vec![]), &topology_baseline(), Some(&delta));

        assert!(output.contains("> [!TIP]"));
        assert!(output.contains("**In this PR**"));
        assert!(output.contains("- New endpoint `GET /health`"));
    }

    #[test]
    fn test_pr_delta_lists_removed_endpoints_and_flags_still_consumed() {
        // A removed endpoint whose (method, path) still shows up as a missing
        // endpoint is a producer this PR deletes out from under a consumer.
        let delta = PrDelta {
            new_endpoints: vec![],
            removed_endpoints: vec![
                EndpointRef {
                    method: "DELETE".to_string(),
                    path: "/api/sessions".to_string(),
                    service: None,
                },
                EndpointRef {
                    method: "GET".to_string(),
                    path: "/api/unused".to_string(),
                    service: Some("auth".to_string()),
                },
            ],
        };
        let findings = vec![Finding::missing_endpoint(
            "DELETE",
            "/api/sessions",
            None,
            vec!["web/src/auth.ts:9".to_string()],
        )];
        let output =
            format_analysis_results(result_with(findings), &topology_baseline(), Some(&delta));

        assert!(output.contains("- Removed `DELETE /api/sessions` — ⚠ still consumed"));
        assert!(output.contains("- Removed `GET /api/unused` (`auth`)\n"));
        assert!(!output.contains("`GET /api/unused` (`auth`) — ⚠"));
    }

    /// The "still consumed" join must be param-aware: the removed side
    /// carries declared param names (`/orders/:id`) while the missing side is
    /// normalized (`/orders/:param`) or concrete — literal equality would
    /// never flag a parameterized route.
    #[test]
    fn test_pr_delta_still_consumed_join_is_param_aware() {
        let delta = PrDelta {
            new_endpoints: vec![],
            removed_endpoints: vec![
                // Param name differs from the missing finding's `:param`.
                EndpointRef {
                    method: "GET".to_string(),
                    path: "/orders/:id".to_string(),
                    service: None,
                },
                // Same prefix, different segment count — must NOT be flagged.
                EndpointRef {
                    method: "GET".to_string(),
                    path: "/orders".to_string(),
                    service: None,
                },
                // Param matches a concrete missing segment, but the method
                // differs — must NOT be flagged.
                EndpointRef {
                    method: "DELETE".to_string(),
                    path: "/users/:id".to_string(),
                    service: None,
                },
            ],
        };
        let findings = vec![
            Finding::missing_endpoint(
                "GET",
                "/orders/:param",
                None,
                vec!["web/src/orders.ts:3".to_string()],
            ),
            Finding::missing_endpoint("GET", "/users/42", None, vec![]),
        ];
        let output =
            format_analysis_results(result_with(findings), &topology_baseline(), Some(&delta));

        assert!(output.contains("- Removed `GET /orders/:id` — ⚠ still consumed"));
        assert!(output.contains("- Removed `GET /orders`\n"));
        assert!(output.contains("- Removed `DELETE /users/:id`\n"));
    }

    #[test]
    fn test_still_consumed_join_matches_all_param_syntaxes() {
        // The overlap check must agree with MountGraph::is_param_segment:
        // `{id}` (OpenAPI/Fastify), `<id>` (Flask), `[id]` (Next.js), and a
        // trailing `?` optional marker are placeholders too — not just `:id`.
        assert!(endpoints_overlap("GET", "/o/{id}", "GET", "/o/:param"));
        assert!(endpoints_overlap("GET", "/o/<id>", "GET", "/o/123"));
        assert!(endpoints_overlap("GET", "/o/[id]", "GET", "/o/:param"));
        assert!(endpoints_overlap("GET", "/o/:id?", "GET", "/o/456"));
        // Methods still gate, and concrete mismatched segments still fail.
        assert!(!endpoints_overlap("POST", "/o/{id}", "GET", "/o/:param"));
        assert!(!endpoints_overlap("GET", "/o/{id}/x", "GET", "/o/:param/y"));
    }

    #[test]
    fn test_pr_delta_strip_inline_code_is_prose_safe() {
        // In a prose code span a pipe is literal (must not be backslash-escaped
        // the way a table cell would), and a backtick in the service is dropped.
        let delta = PrDelta {
            new_endpoints: vec![EndpointRef {
                method: "GET".to_string(),
                path: "/a|b".to_string(),
                service: Some("sv`c".to_string()),
            }],
            removed_endpoints: vec![],
        };
        let output =
            format_analysis_results(result_with(vec![]), &topology_baseline(), Some(&delta));

        assert!(output.contains("- New endpoint `GET /a|b` (`svc`)"));
        assert!(
            !output.contains("\\|"),
            "prose code span must not escape pipes"
        );
    }

    #[test]
    fn test_pr_delta_strip_omits_empty_service_suffix() {
        // A service name that sanitizes to empty (e.g. only backticks) must not
        // render an empty " (``)" suffix.
        let delta = PrDelta {
            new_endpoints: vec![EndpointRef {
                method: "GET".to_string(),
                path: "/x".to_string(),
                service: Some("``".to_string()),
            }],
            removed_endpoints: vec![],
        };
        let output =
            format_analysis_results(result_with(vec![]), &topology_baseline(), Some(&delta));

        assert!(output.contains("- New endpoint `GET /x`\n"));
        assert!(!output.contains("(`"), "empty service must omit the suffix");
    }

    /// The badge/dashboard placeholder protocol is gone: URLs are built
    /// cloud-side from the verified identity, so the scanner must not emit
    /// `{{CARRICK_*}}` markers anymore.
    #[test]
    fn test_no_placeholder_protocol_in_output() {
        for result in [
            result_with(vec![]),
            result_with(vec![type_mismatch_finding()]),
        ] {
            let output = format_analysis_results(result, &topology_baseline(), None);
            assert!(!output.contains("{{CARRICK_BADGE"));
            assert!(!output.contains("{{CARRICK_LINK"));
            assert!(!output.contains("Carrick dashboard"));
        }
    }
}
