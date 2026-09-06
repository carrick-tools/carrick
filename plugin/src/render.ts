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
  const out: string[] = [
    `${who} at ${shortHash(boundary.commit_hash)}: ${boundary.files_attempted ?? 0} file(s) sent to the analyzer`,
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
  if ((boundary.candidates_not_classified ?? 0) > 0) {
    out.push(
      `  ${boundary.candidates_not_classified} candidate(s) not classified locally: no model runs on this machine`,
    );
  }
  if ((boundary.unemitted_literal_candidates ?? 0) > 0) {
    out.push(
      `  ${boundary.unemitted_literal_candidates} bare route-literal call site(s) left unclassified`,
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
  if (boundary.types_degraded) {
    out.push(
      `  types degraded at ${boundary.types_degraded.stage ?? "an unnamed stage"}: ${boundary.types_degraded.detail ?? "no detail"}`,
    );
  }
  if (boundary.bare_checkout) {
    out.push("  types captured on a bare checkout: anything through a dependency is `any`");
  }
  return out;
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

function verdictText(item: CheckItem): string {
  if (!item.verdict) return "";
  const { state, result, detail } = item.verdict;
  const head = result === "compatible" && state !== "resolved" ? `${result} (${state})` : result;
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

function staleLine(result: CheckResult): string | null {
  if (!result.stale) return null;
  const changed = result.changed_since_index;
  const suffix =
    typeof changed === "number"
      ? ` ${changed} file(s) in the workspace have changed since ${shortHash(result.index_commit)}.`
      : "";
  return `This file has changed since the index, so these verdicts describe the indexed version.${suffix}`;
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
  const boundary = boundaryLines(result.boundary, result.service);
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
  if (boundary.length) {
    lines.push(`Boundary: ${boundary[0]}`);
    for (const line of boundary.slice(1)) lines.push(line);
  }
  return lines.join("\n");
}

/**
 * The SessionStart line: what the index holds and how far the tree has moved
 * from it. Printed once, at exit 0, so Claude Code adds it to the session.
 */
export function renderSessionStart(result: CheckResult): string {
  if (result.error === "not_indexed") {
    return "Carrick has no index for this workspace, so nothing in this session is checked against the other services. `carrick index` builds one.";
  }
  if (result.error) {
    return `Carrick could not read its index for this workspace (${result.error}).`;
  }
  const who = result.service ?? "this workspace";
  const changed = result.changed_since_index;
  const lines: string[] = [
    typeof changed === "number"
      ? `Carrick indexed ${who} at ${shortHash(result.index_commit)}. ${changed} file(s) have changed since then, and their routes and calls are whatever the index last saw.`
      : `Carrick indexed ${who} at ${shortHash(result.index_commit)}.`,
  ];
  const items = reportableItems(result);
  for (const item of items.slice(0, MAX_ITEM_LINES)) lines.push(itemLine(item, result.file));
  const boundary = boundaryLines(result.boundary, result.service);
  if (boundary.length) {
    lines.push(`Boundary: ${boundary[0]}`);
    for (const line of boundary.slice(1)) lines.push(line);
  }
  return lines.join("\n");
}
