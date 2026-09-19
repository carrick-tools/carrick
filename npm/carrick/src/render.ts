// Turning a check payload into lines a model reads.
//
// Shape, from the design (§4.4) and carried by both hooks: locations first,
// boundary last, one line per item. The boundary wording is a port of
// `ServiceBoundary::lines` in src/boundary.rs, so the CLI, the PR comment and
// this hook say the same sentence about the same count.

import {
  connectedItems,
  problemItems,
  type Boundary,
  type CheckItem,
  type CheckResult,
  type Counted,
  type Counterpart,
  type RunningScan,
  type StatusRepo,
  type StatusResult,
  type StatusService,
} from "./contract.ts";

/** How many item lines one hook message carries before it says how many are left. */
export const MAX_ITEM_LINES = 12;
/** How many counterpart sites one item line names before the same. */
export const MAX_COUNTERPARTS = 4;

export function shortHash(commit: string | undefined): string {
  if (!commit) return "an unknown commit";
  return commit.slice(0, 7);
}

function firstReason(count: Counted): string {
  const first = count.reasons?.[0];
  if (!first) return "";
  return count.total > 1 ? ` (e.g. ${first})` : ` (${first})`;
}

/**
 * The boundary as the CLI prints it: one line per thing the scan could not
 * classify, and nothing for the ones it classified all of.
 */
export function boundaryLines(boundary: Boundary | undefined, service: string | undefined): string[] {
  if (!boundary) return [];
  const who = service ?? "this workspace";
  // Local mode dispatches nothing to the analyzer, so the count is 0 on every
  // laptop index and the clause would be noise on every edit. The commit is
  // the part of the header that always says something.
  const attempted = boundary.files_attempted ?? 0;
  const out: string[] = [
    attempted > 0
      ? `${who} at ${shortHash(boundary.commit_hash)}: ${attempted} file(s) sent to the analyzer`
      : `${who} at ${shortHash(boundary.commit_hash)}`,
  ];
  const push = (count: Counted | undefined, what: string): void => {
    if (!count || count.total === 0) return;
    out.push(`  ${count.total} ${what}${firstReason(count)}`);
  };
  push(boundary.files_lost, "file(s) the analyzer never answered for");
  push(
    boundary.consumers_not_resolved,
    "call site(s) that named a client member and did not resolve to it",
  );
  push(boundary.sdk_unresolved, "SDK call(s) that produced no edge");
  push(boundary.unknown_call_paths, "indexed call(s) whose path no producer may claim");
  push(boundary.routes_without_response_type, "route(s) with no resolved response type");
  push(boundary.calls_without_expected_type, "call(s) with no resolved expected type");
  if ((boundary.unemitted_literal_candidates ?? 0) > 0) {
    const sites = boundary.unemitted_literal_sites ?? [];
    const where = sites.length
      ? ` (${(boundary.unemitted_literal_candidates ?? 0) > 1 ? "e.g. " : ""}${sites[0]})`
      : "";
    out.push(
      `  ${boundary.unemitted_literal_candidates} bare route-literal call site(s) left unclassified${where}`,
    );
  }
  if ((boundary.model_only_rows ?? 0) > 0) {
    out.push(
      `  ${boundary.model_only_rows} row(s) the model alone states (${boundary.model_rows_joined ?? 0} joined a deterministic row)`,
    );
  }
  if ((boundary.model_endpoints_discarded_in_claimed_modules ?? 0) > 0) {
    out.push(
      `  ${boundary.model_endpoints_discarded_in_claimed_modules} model endpoint(s) dropped in modules a routing convention claims`,
    );
  }
  // One event, one line: the stage when the scan recorded one, otherwise the
  // sentence it wrote about the same event (carrick#997 item 3).
  if (boundary.types_degraded) {
    out.push(
      `  types degraded at ${boundary.types_degraded.stage ?? "an unnamed stage"}: ${boundary.types_degraded.detail ?? "no detail"}`,
    );
  } else if (boundary.type_extraction_status) {
    out.push(`  warning: ${boundary.type_extraction_status}`);
  }
  if (boundary.bare_checkout) {
    out.push("  types captured on a bare checkout: anything through a dependency is `any`");
  }
  return out;
}

/**
 * The boundary for one payload: the CLI's own lines when it sent them,
 * otherwise this port of `ServiceBoundary::lines`.
 *
 * `boundary_lines` is printed exactly as it arrives, so the hook, the
 * diagnostic and `carrick check` in a terminal say the same sentence about the
 * same count. Nothing here reformats or re-orders it.
 */
export function boundaryFor(result: CheckResult): string[] {
  if (result.boundary_lines?.length) return result.boundary_lines;
  const counts = boundaryLines(result.boundary, result.service);
  // `boundary_note` is the sentence about what a local index cannot hold at
  // all, and it leads the CLI's own lines, so it leads these too.
  if (result.boundary_note) return [result.boundary_note, ...counts];
  return counts;
}

/** True when the lines came from the CLI rather than from the port above. */
export function boundaryIsPreRendered(result: CheckResult): boolean {
  return Boolean(result.boundary_lines?.length);
}

function counterpartText(counterparts: Counterpart[]): string {
  const shown = counterparts.slice(0, MAX_COUNTERPARTS);
  const rendered = shown
    .map((counterpart) => {
      const where = counterpart.file
        ? `${counterpart.file}${counterpart.line ? `:${counterpart.line}` : ""}`
        : "an unnamed file";
      return counterpart.service ? `${counterpart.service} ${where}` : where;
    })
    .join(", ");
  const rest = counterparts.length - shown.length;
  return rest > 0 ? `${rendered}, and ${rest} more` : rendered;
}

/**
 * What to call the other side. A `peer` is a shared external contract, so it
 * gets no producer/consumer word: the locations are the whole answer.
 */
function roleLabel(counterparts: Counterpart[]): string {
  const roles = new Set(counterparts.map((counterpart) => counterpart.role));
  if (roles.size === 1 && roles.has("consumer")) return "Consumers";
  if (roles.size === 1 && roles.has("producer")) return "Producers";
  if (roles.size === 1 && roles.has("peer")) return "Same contract in";
  return "Counterparts";
}

/** `[fact]` or `[candidate, model]`: R1's label, plus what produced the row. */
function sourceLabel(item: CheckItem): string {
  const parts: string[] = [];
  if (item.source) parts.push(item.source);
  if (item.resolution_source) parts.push(item.resolution_source);
  return parts.length ? ` [${parts.join(", ")}]` : "";
}

/**
 * The verdict state in words. It is the type layer's word and only that: it
 * says nothing about freshness, which `stale` and `changed_since_index` carry.
 */
export function stateWord(state: string | undefined): string {
  if (state === "not_checked") return "no type verdict";
  if (state === "unresolved") return "no usable type on one side, so nothing is claimed";
  if (state === "resolved") return "compiler-compared";
  return state ?? "";
}

/**
 * The two shapes a mismatch is about, in one sentence, or null where the
 * payload does not hold them (carrick#1033).
 *
 * Both surfaces print this: the LSP diagnostic and the post-edit hook line.
 * The compiler compared two shapes during the scan, and a line that states
 * only the outcome makes the reader open the counterpart file to learn what it
 * expected.
 *
 * The sentence is built from the direction and nothing else. The direction's
 * payload IS the actual type — the producer's response, or the consumer's
 * request body — and the side that reads that payload is the one named with
 * the type it declares. So the roles swap with the direction, and the row's
 * own side decides whether the counterpart is the reader (named by service and
 * location) or the sender (named the same way, with the row itself as the
 * reader).
 *
 * A counterpart is NAMED only when exactly one of them carries the role in
 * question. A route with two consumers takes its verdict from the first
 * finding that names the operation, so naming one of them would state that
 * THAT consumer reads the type — which the payload does not say. The
 * unchanged Counterparts line lists them all.
 */
export function typedMismatchClause(item: CheckItem): string | null {
  const direction = item.direction;
  const actual = item.actual_type;
  const expected = item.expected_type;
  if (!actual || !expected) return null;
  if (direction !== "request" && direction !== "response") return null;
  // The clause replaces the result and state words, so it is used only where
  // those words are exactly "the compiler compared these two and they differ".
  if (item.verdict?.state !== "resolved" || item.verdict?.result !== "type_mismatch") return null;

  const verb = direction === "response" ? "reads" : "expects";
  const sender = direction === "response" ? "producer" : "consumer";
  const reader = direction === "response" ? "consumer" : "producer";
  const rowSide = item.kind === "route" ? "producer" : item.kind === "call" ? "consumer" : null;
  const payload = `${direction} is ${actual}`;

  const site = (role: string): string | null => {
    const named = (item.counterparts ?? []).filter((counterpart) => counterpart.role === role);
    if (named.length !== 1) return null;
    const only = named[0];
    if (!only?.file) return null;
    const where = `${only.file}${only.line ? `:${only.line}` : ""}`;
    return only.service ? `${only.service} ${where}` : where;
  };

  if (rowSide === reader) {
    const from = site(sender);
    const lead = from ? `${payload} from ${sender} at ${from}` : payload;
    return `${lead}, this ${item.kind} ${verb} ${expected}`;
  }
  const at = site(reader);
  return at
    ? `${payload}, ${reader} at ${at} ${verb} ${expected}`
    : `${payload}, a ${reader} ${verb} ${expected}`;
}

/**
 * What is known about the contract at this row.
 *
 * `result` is null wherever the state is the whole statement, so there is no
 * result word to print and the state carries the line. Where both are present
 * the state qualifies the result, because `method_mismatch` with no type
 * verdict behind it is a different claim from a compiler-compared mismatch.
 */
function verdictText(item: CheckItem): string {
  if (!item.verdict) return "";
  const typed = typedMismatchClause(item);
  // The typed sentence stands in for the result word, and the compiler's own
  // reason follows it as its own segment rather than being lost.
  if (typed) return ` ${typed}`;
  const { state, result, detail } = item.verdict;
  const head =
    result == null
      ? stateWord(state)
      : state === "resolved"
        ? result
        : `${result} (${stateWord(state)})`;
  if (!head) return detail ? ` ${detail}` : "";
  return detail ? ` ${head}: ${detail}` : ` ${head}`;
}

/** One line for one route or call: where it is, what it is, what is known about it. */
export function itemLine(item: CheckItem, file: string | undefined): string {
  const where = `${file ?? "this file"}:${item.line ?? 1}:${item.col ?? 1}`;
  const operation = [item.method, item.path].filter(Boolean).join(" ");
  const counterparts = item.counterparts ?? [];
  const segments = [
    `- ${where} ${item.kind}${operation ? ` ${operation}` : ""}${sourceLabel(item)}${verdictText(item)}`,
  ];
  // The typed sentence replaced the result word and the detail with it, so the
  // compiler's reason follows as its own segment (carrick#1033).
  if (typedMismatchClause(item) && item.verdict?.detail) segments.push(item.verdict.detail);
  if (item.evidence) segments.push(`Read off ${item.evidence}`);
  if (counterparts.length) {
    segments.push(`${roleLabel(counterparts)}: ${counterpartText(counterparts)}`);
  }
  return segments.map((segment) => segment.replace(/\.+$/, "")).join(". ");
}

/** Problems first, then everything that names another service, both by line. */
export function reportableItems(result: CheckResult): CheckItem[] {
  const byLine = (a: CheckItem, b: CheckItem): number => (a.line ?? 0) - (b.line ?? 0);
  const problems = problemItems(result).sort(byLine);
  const connected = connectedItems(result)
    .filter((item) => !problems.includes(item))
    .sort(byLine);
  return [...problems, ...connected];
}

function deletedLine(result: CheckResult): string | null {
  if (!result.deleted) return null;
  const consumers = (result.items ?? []).reduce(
    (total, item) => total + (item.counterparts ?? []).length,
    0,
  );
  return `This file is gone from disk and the index still holds ${(result.items ?? []).length} row(s) for it, with ${consumers} counterpart(s) still on the other side.`;
}

/**
 * Append the boundary to a rendered message.
 *
 * The port's first line is labelled, because on its own it reads as a bare
 * count. The CLI's own lines are appended untouched: a label glued to the front
 * of them would no longer be the bytes the CLI printed.
 */
function pushBoundary(lines: string[], result: CheckResult, boundary: string[]): void {
  if (!boundary.length) return;
  if (boundaryIsPreRendered(result)) {
    for (const line of boundary) lines.push(line);
    return;
  }
  lines.push(`Boundary: ${boundary[0]}`);
  for (const line of boundary.slice(1)) lines.push(line);
}

function staleLine(result: CheckResult): string | null {
  if (!result.stale) return null;
  const changed = result.changed_since_index;
  const suffix =
    typeof changed === "number"
      ? ` ${changed} file(s) in the workspace have changed since ${shortHash(result.index_commit)}.`
      : "";
  // A re-check that ran has already answered from the working tree, so the
  // sentence that sends a reader to re-index would be false (carrick#1036).
  const ran = result.recheck?.ran;
  if (ran && ran !== "none") {
    return `These verdicts were re-computed from your working tree in ${result.recheck?.elapsed_ms} ms.${suffix}`;
  }
  const since = result.recheck?.stale_since;
  const budget = since
    ? ` The re-check did not finish inside its budget, so this is the answer computed at ${since}.`
    : "";
  return `This file has changed since the index, so these verdicts describe the indexed version.${suffix}${budget}`;
}

/**
 * The PostToolUse context, or `null` when the index has nothing to say at all.
 *
 * A file with no indexed route or call still gets the boundary, because that is
 * the difference between "nothing crosses a service here" and "nothing here was
 * classified". Only an error payload and a payload with neither items nor a
 * boundary are silent.
 */
export function renderPostToolUse(result: CheckResult, displayFile?: string): string | null {
  if (result.error) return null;
  const items = reportableItems(result);
  const boundary = boundaryFor(result);
  // The boundary is never dropped: the local index holds no bare-receiver route
  // and no `fetch` call, because both need a model, so an empty answer without
  // the boundary beside it reads as "there is nothing here" when it means
  // "nothing here was classified".
  if (items.length === 0 && boundary.length === 0) return null;

  // `result.file` is relative to the repo that owns it, so the caller's own
  // workspace-relative path is the one a reader can open; it wins when given.
  const where = displayFile ?? result.file;
  const service = result.service ? `${result.service}, ` : "";
  const lines: string[] = [
    `Carrick checked ${where ?? "this file"} against the workspace index (${service}indexed at ${shortHash(result.index_commit)}).`,
  ];
  for (const item of items.slice(0, MAX_ITEM_LINES)) lines.push(itemLine(item, where));
  const hidden = items.length - Math.min(items.length, MAX_ITEM_LINES);
  if (hidden > 0) {
    lines.push(`- and ${hidden} more route(s) or call(s) in this file, from \`carrick check\`.`);
  }
  const deleted = deletedLine(result);
  if (deleted) lines.push(deleted);
  const stale = staleLine(result);
  if (stale) lines.push(stale);
  pushBoundary(lines, result, boundary);
  return lines.join("\n");
}

/**
 * The boundary for one service in a status answer, same preference as a check.
 */
export function serviceBoundary(service: StatusService): string[] {
  if (service.boundary_lines?.length) return service.boundary_lines;
  const counts = boundaryLines(service.boundary, service.service);
  if (service.boundary_note) return [service.boundary_note, ...counts];
  return counts;
}

/** How many stale files one service line names before it says how many are left. */
export const MAX_STALE_FILES = 5;

function staleText(service: StatusService): string {
  const total = service.stale_files_total ?? service.changed_since_index;
  const shown = (service.stale_files ?? []).slice(0, MAX_STALE_FILES);
  if (!shown.length) return "";
  const rest = total - shown.length;
  const listed = rest > 0 ? `${shown.join(", ")}, +${rest} more` : shown.join(", ");
  return ` (${listed})`;
}

/**
 * One line per service: what it holds, at which commit, how far the part of
 * the tree it reads has moved since, and what is waiting for a paid scan.
 *
 * Services of one repo share a commit and no longer share a changed-file
 * count: each is told about the files its own scan reads, and the repo's own
 * line carries what belongs to no service (carrick#997 item 4).
 */
export function serviceLine(service: StatusService): string {
  const head = `- ${service.service} at ${shortHash(service.index_commit)}: ${service.routes} route(s), ${service.calls} call(s)`;
  const waiting = service.boundary?.candidates_awaiting_model;
  const awaiting = waiting ? `, ${waiting} candidate(s) waiting for \`carrick index\`` : "";
  return `${head}, changed since index: ${service.changed_since_index}${staleText(service)}${awaiting}`;
}

/**
 * One line for a scan happening right now, which is the only thing in a status
 * answer that is not about the past (carrick#992). A session that starts while
 * the first index is still being built should be told that, rather than told
 * there is no index and left to start a second one.
 */
export function runningScanLine(scan: RunningScan): string {
  const counts = scan.progress?.total
    ? `, ${scan.progress.done ?? 0} of ${scan.progress.total} ${scan.progress.phase ?? "files"}`
    : "";
  const where = scan.phase ? `: ${scan.phase}` : "";
  if (scan.status === "failed") {
    // The error's first line is the reason; the lines after it are an excerpt
    // of the scan's own log, which is for the log's reader, not a session
    // start (carrick#1103).
    const reason = scan.error?.split("\n").find((line) => line.trim())?.trim();
    return `- scan ${scan.scan_id} failed${reason ? `: ${reason}` : ""}`;
  }
  if (scan.status === "finished") {
    return `- scan ${scan.scan_id} finished. The index is written.`;
  }
  // Neither running nor finished: the prompts went to Carrick Cloud and the
  // index arrives when something collects them (carrick#1229). Without this
  // case the line below claims a scan is still going and points at a log that
  // ended.
  if (scan.status === "dispatched") {
    return `- scan ${scan.scan_id} handed this workspace to Carrick Cloud to analyse. \`carrick resume\` builds the index when it is done.`;
  }
  const slow = scan.notice ? ` (${scan.notice})` : "";
  return `- scan ${scan.scan_id} is running${where}${counts}${slow}. Its output is in .carrick/scan-${scan.scan_id}.log`;
}

/**
 * One line per repo, for the files no service in it reads: a workflow, a
 * lockfile, an editor's settings. Empty when there are none, because a repo
 * whose every change is inside a service has nothing to add.
 */
export function repoLine(repo: StatusRepo): string | null {
  if (!repo.outside_every_service) return null;
  const shown = (repo.stale_files ?? []).slice(0, MAX_STALE_FILES);
  const rest = repo.outside_every_service - shown.length;
  const listed = shown.length
    ? ` (${rest > 0 ? `${shown.join(", ")}, +${rest} more` : shown.join(", ")})`
    : "";
  return `- ${repo.name}: ${repo.outside_every_service} file(s) changed outside every service${listed}`;
}

/**
 * The SessionStart line, from `carrick status --json`.
 *
 * `check` and `touch` each take exactly one file, so this is the only read that
 * answers for a workspace. It carries no verdict: `status` states what is
 * indexed and what each service could not classify, and nothing about whether a
 * contract holds.
 */
export function renderSessionStart(status: StatusResult): string {
  const scans = [...(status.running_scans ?? []).map(runningScanLine), ...(status.analysing ?? [])];
  if (status.error === "not_indexed") {
    // A first index being built right now is the answer, not "there is none":
    // told the latter, an agent starts a second scan (carrick#992). The same
    // holds for one being built in the cloud (carrick#1229).
    if (scans.length) {
      return [
        "Carrick has no index for this workspace yet, and a scan is building one.",
        ...scans,
      ].join("\n");
    }
    return "Carrick has no index for this workspace, so nothing in this session is checked against the other services. `carrick index --workspace <dir>` builds one.";
  }
  if (status.error) {
    // The CLI's own sentence, which names the move: a format mismatch says
    // which scanner wrote the index and that `carrick index` rebuilds it. The
    // wire code alone told a reader nothing they could act on (carrick#1009).
    return `Carrick could not read its index for this workspace: ${status.message ?? status.error}`;
  }
  if (!status.services.length) {
    return `Carrick has an index at ${status.workspace ?? "this workspace"} and it holds no services.`;
  }

  const version = status.scanner_version ? `, scanner ${status.scanner_version}` : "";
  const where = status.workspace ? ` in ${status.workspace}` : "";
  const when = status.indexed_at ? ` at ${status.indexed_at}` : "";
  const lines: string[] = [
    `Carrick indexed ${status.services.length} service(s)${where}${when}${version}.`,
    ...scans,
  ];

  for (const service of status.services) {
    lines.push(serviceLine(service));
  }
  for (const repo of status.repos ?? []) {
    const line = repoLine(repo);
    if (line) lines.push(line);
  }
  for (const service of status.services) {
    for (const line of serviceBoundary(service)) lines.push(line);
  }
  return lines.join("\n");
}
