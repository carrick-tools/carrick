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
  type ChangedResult,
  type CheckItem,
  type CheckResult,
  type Counted,
  type Counterpart,
  type RunningScan,
  type StatusRepo,
  type StatusResult,
  type StatusService,
} from "./contract.ts";
import type { SyncRecord, SyncedRepo } from "./hosted-snapshot.ts";

/** How many item lines one hook message carries before it says how many are left. */
export const MAX_ITEM_LINES = 12;
/** How many counterpart sites one item line names before the same. */
export const MAX_COUNTERPARTS = 4;
/** How many uses lines one hook message carries before it says how many are left. */
export const MAX_USE_LINES = 8;

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
  const unparsed = boundary.files_not_parsed;
  if (unparsed && unparsed.total > 0) {
    const paths = (unparsed.reasons ?? []).map((reason) => reason.replace(/: does_not_parse$/, ""));
    const named = paths.slice(0, 2).join(", ");
    const rest = unparsed.total - Math.min(2, paths.length);
    const where = named ? ` (${named}${rest > 0 ? ` and ${rest} more` : ""})` : "";
    out.push(
      `  ${unparsed.total} file(s) did not parse${where}: the index states nothing from ${unparsed.total === 1 ? "it" : "them"}, so treat a route or call missing there as a gap in the index, not a finding.`,
    );
  }
  push(
    boundary.consumers_not_resolved,
    "call site(s) that named a client member and did not resolve to it",
  );
  push(boundary.sdk_unresolved, "SDK call(s) that produced no edge");
  push(boundary.unknown_call_paths, "indexed call(s) whose path no producer may claim");
  push(boundary.routes_without_response_type, "route(s) with no resolved response type");
  push(boundary.calls_without_expected_type, "call(s) with no resolved expected type");
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
  push(
    boundary.model_endpoints_dropped_unanchored,
    "route(s) found by analysis but left out of the index: their registration site could not be located",
  );
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
export function itemLine(
  item: CheckItem,
  file: string | undefined,
  options: { locatorsOnly?: boolean } = {},
): string {
  const where = `${file ?? "this file"}:${item.line ?? 1}:${item.col ?? 1}`;
  const operation = [item.method, item.path].filter(Boolean).join(" ");
  const counterparts = item.counterparts ?? [];
  // Locators only (carrick#2069): where the row is, what it is, and who is on
  // the other side. The Read hook has no edit to judge, so no verdict words.
  if (options.locatorsOnly) {
    const head = `- ${where} ${item.kind}${operation ? ` ${operation}` : ""}`;
    return counterparts.length
      ? `${head}. ${roleLabel(counterparts)}: ${counterpartText(counterparts)}`
      : head;
  }
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

/** Problem lines one edit's message carries before it counts the rest. */
export const MAX_EDIT_PROBLEMS = 3;

/** Rows outside the edited file that one edit may name (`recheck.moved`). */
export const MAX_MOVED_ROWS = 3;

/**
 * The rows outside the edited file that this edit moved (`recheck.moved`):
 * one line each, naming the row and, for a pair, its counterpart. A row the
 * index never judged is not called a break.
 */
export function movedLines(result: CheckResult): string[] {
  const moved = result.recheck?.moved ?? [];
  const lines = moved.slice(0, MAX_MOVED_ROWS).map((row) => {
    const at = `${row.file}${row.line ? `:${row.line}` : ""}`;
    const what = `${row.role} ${row.method} ${row.path}`;
    if (row.after === "no_route") {
      return cutLine(`- ${at}: this ${what} matched a route in the index and this edit left it with none.`);
    }
    const other = row.counterparts.length > 0 ? ` against ${row.counterparts.join(", ")}` : "";
    const verdict = row.other_build
      ? `reads ${row.after} now; the index was built by another carrick version, so this edit may not be the cause`
      : row.not_judged_before
        ? `reads ${row.after} now; the index never judged it, so this edit may not be the cause`
        : `agreed in the index and now reads ${row.after}`;
    return cutLine(`- ${at}: this ${what}${other} ${verdict}.`);
  });
  if (moved.length > MAX_MOVED_ROWS) {
    lines.push(`- and ${moved.length - MAX_MOVED_ROWS} more rows outside this file moved: \`carrick check --changed\` lists them.`);
  }
  return lines;
}
/** How long one problem line of an edit's message may run before it is cut. */
export const MAX_EDIT_LINE_CHARS = 400;

function cutLine(text: string): string {
  return text.length <= MAX_EDIT_LINE_CHARS ? text : `${text.slice(0, MAX_EDIT_LINE_CHARS - 3)}...`;
}

/**
 * Whether the items describe the file as it is on disk now: the file has not
 * moved past the index, a re-check answered from the working tree, or the
 * file is gone (its rows then say what the deletion leaves behind).
 */
export function answersTheEdit(result: CheckResult): boolean {
  if (!result.stale || result.deleted) return true;
  const ran = result.recheck?.ran;
  return ran !== undefined && ran !== "none";
}

/** Services a stale re-check line names before it counts the rest. */
const MAX_STALE_SERVICES = 3;

/** "N services that read <subject> were judged at their indexed version (a, b, and K more)". */
export function staleClause(stale: readonly string[], subject: string): string {
  const names = stale.slice(0, MAX_STALE_SERVICES).join(", ");
  const more = stale.length > MAX_STALE_SERVICES ? `, and ${stale.length - MAX_STALE_SERVICES} more` : "";
  return stale.length === 1
    ? `1 service that reads ${subject} was judged at its indexed version (${names})`
    : `${stale.length} services that read ${subject} were judged at their indexed version (${names}${more})`;
}

/** Operations a Stop line names before it counts the rest. */
const MAX_NAMED_OPERATIONS = 3;

/** One checked operation, as a clause: "GET /users/:id at api/users.ts:12". */
function operationClause(item: CheckItem, where: string): string {
  const op = `${item.method ?? ""} ${item.path ?? ""}`.trim();
  const at = `${where}${item.line ? `:${item.line}` : ""}`;
  return item.kind === "call" ? `a call to ${op} at ${at}` : `${op} at ${at}`;
}

/** Up to three clauses, then "and N more". */
function namedOperations(clauses: readonly string[]): string {
  const shown = clauses.slice(0, MAX_NAMED_OPERATIONS).join("; ");
  const more = clauses.length - MAX_NAMED_OPERATIONS;
  return more > 0 ? `${shown}; and ${more} more` : shown;
}

/**
 * The Stop hook's words for a change whose every edited file a per-file
 * re-check answered after its last edit (carrick#3466). It speaks only with
 * something to say:
 * - problems: a header and the problem and moved lines of `fresh` (the
 *   answers this Stop delivers; the others were printed when they landed);
 * - judged operations with no problem: one line naming them;
 * - operations whose types were not checked: one line naming them and why;
 * - no route or call in any edited file: nothing.
 * `key` is the sorted list of the operations a clean or not-checked line
 * names, so the caller prints the same set once; null when problems print.
 */
export function sessionCheckedLines(
  covered: ReadonlyArray<{ result: CheckResult; where: string }>,
  fresh: ReadonlyArray<{ result: CheckResult; where: string }>,
): { text: string; key: string | null } {
  const results = covered.map((entry) => entry.result);
  const files = results.length;
  const services = [...new Set(results.map((result) => result.service).filter((s): s is string => Boolean(s)))];
  const where = services.length > 0 ? ` (${services.join(", ")})` : "";
  const problems = results.reduce((total, result) => total + problemItems(result).length, 0);

  const lines: string[] = [];
  for (const { result, where: file } of fresh) {
    if (result.deleted) lines.push(deletedLine(result, file));
    const found = problemItems(result).sort((a, b) => (a.line ?? 0) - (b.line ?? 0));
    for (const item of found.slice(0, MAX_EDIT_PROBLEMS)) lines.push(cutLine(itemLine(item, file)));
    const hidden = found.length - Math.min(found.length, MAX_EDIT_PROBLEMS);
    if (hidden > 0) lines.push(`- and ${hidden} more: \`carrick check ${file}\` lists them.`);
    lines.push(...movedLines(result));
  }
  if (problems > 0 || lines.length > 0) {
    const tail = problems > 0 ? `${problems} contract ${problems === 1 ? "problem" : "problems"}` : "this change affects other code";
    return {
      text: [`Carrick checked the ${files} ${files === 1 ? "file" : "files"} this session edited${where}: ${tail}.`, ...lines].join("\n"),
      key: null,
    };
  }

  type Named = { clause: string; id: string };
  const judged: Named[] = [];
  const unjudged: Named[] = [];
  let unjudgedResult: CheckResult | undefined;
  for (const { result, where: file } of covered) {
    const named = (item: CheckItem): Named => ({
      clause: operationClause(item, file),
      id: `${file}|${item.kind}|${item.method ?? ""}|${item.path ?? ""}`,
    });
    const items = result.items ?? [];
    if (judgedAnyPair(result)) {
      judged.push(...items.filter((item) => item.verdict?.state === "resolved" && item.verdict.result != null).map(named));
    } else if (items.length > 0) {
      unjudged.push(...items.map(named));
      unjudgedResult ??= result;
    }
  }
  const out: string[] = [];
  const noun = (n: number) => `${n} ${n === 1 ? "operation" : "operations"}`;
  if (judged.length > 0) {
    out.push(
      `Carrick checked the ${noun(judged.length)} this session changed (${namedOperations(judged.map((n) => n.clause))}): both sides agree.`,
    );
  }
  if (unjudged.length > 0) {
    out.push(
      `Carrick found the ${noun(unjudged.length)} this session changed (${namedOperations(unjudged.map((n) => n.clause))}) but did not check their types (${notJudgedReason(unjudgedResult!)}), so there is no contract verdict.`,
    );
  }
  if (out.length === 0) return { text: "", key: null };
  const key = [...judged, ...unjudged].map((n) => n.id).sort().join("\n");
  return { text: out.join("\n"), key };
}

/**
 * The Stop hook's line for a change it could not state from the per-file
 * re-checks: each file and why, and that the whole-change check is queued.
 */
export function notCoveredLine(gaps: ReadonlyArray<{ file: string; reason: string }>): string {
  const named = gaps.map((gap) => `${gap.file}: ${gap.reason.trim().replace(/\.+$/, "")}`).join("; ");
  return `Carrick did not check the whole change: ${named}. A background \`carrick check --changed\` is queued; its answer comes with the next Carrick line.`;
}

/**
 * The Stop hook's line when the whole-change check it waited for did not
 * answer inside the wait: the files not checked, and the command that does.
 */
export function notFinishedLine(gaps: ReadonlyArray<{ file: string; reason: string }>): string {
  const named = gaps.map((gap) => `${gap.file}: ${gap.reason.trim().replace(/\.+$/, "")}`).join("; ");
  return `Carrick could not finish checking this session's change in time: ${named}. Run \`carrick check --changed\` to see the answer.`;
}

/** What a background `check --changed` found: its problem lines, or "" when it found none. */
export function changedLines(changed: ChangedResult): string {
  const head = "Carrick ran `carrick check --changed` for this session's change";
  const files = `${changed.files} changed ${changed.files === 1 ? "file" : "files"}`;
  if (changed.exit === 1) {
    const failing = changed.problems.filter((problem) => problem.fails);
    const lines = [
      `${head}: ${changed.failing} contract ${changed.failing === 1 ? "problem" : "problems"} in ${files}.`,
    ];
    for (const problem of failing.slice(0, MAX_EDIT_PROBLEMS)) {
      const at = `${problem.file}${problem.line ? `:${problem.line}` : ""}`;
      const other = problem.other.length > 0 ? ` (against ${problem.other.join(", ")})` : "";
      lines.push(cutLine(`- ${at} ${problem.role} ${problem.method} ${problem.path}: ${problem.reason.replace(/\.+$/, "")}${other}.`));
    }
    if (failing.length > MAX_EDIT_PROBLEMS) {
      lines.push(`- and ${failing.length - MAX_EDIT_PROBLEMS} more: \`carrick check --changed\` lists them.`);
    }
    return lines.join("\n");
  }
  // Exit 0 (clean), exit 2 (could not check) and a run that gave no answer say
  // nothing: only a problem is worth the agent's attention (hook-wording audit T3).
  return "";
}

function deletedLine(result: CheckResult, where: string): string {
  const consumers = (result.items ?? []).reduce(
    (total, item) => total + (item.counterparts ?? []).length,
    0,
  );
  return `Carrick: ${where} is gone from disk and the index still holds ${(result.items ?? []).length} row(s) for it, with ${consumers} counterpart(s) still on the other side.`;
}

/**
 * Whether the answer says no re-check ran: the file matches the index, or a
 * warranted re-check did not finish. The CLI sends `recheck` on every
 * `--recheck` read, so a `none` there is its statement that nothing ran.
 */
function recheckDidNotRun(result: CheckResult): boolean {
  return result.recheck?.ran === "none";
}

/**
 * Whether the answer holds a type verdict: a pair the compiler compared, whose
 * result is `compatible` or a mismatch. `not_checked` and `unresolved` rows
 * are pairs nobody judged, so a file with only those has no type verdict.
 */
function judgedAnyPair(result: CheckResult): boolean {
  return (result.items ?? []).some(
    (item) => item.verdict?.state === "resolved" && item.verdict.result != null,
  );
}

/** Why no pair was judged, in the words the index recorded, else a plain statement. */
function notJudgedReason(result: CheckResult): string {
  // The build's own statement first: a type check that did not run is the
  // cause of every unjudged pair, whatever the boundary says (carrick#3204).
  const stated = result.type_check;
  if (stated && !stated.ran && stated.reason) return stated.reason.replace(/\.+$/, "");
  const boundary = result.boundary;
  const degraded = boundary?.types_degraded;
  if (degraded?.detail) return degraded.detail.replace(/\.+$/, "");
  if (boundary?.type_extraction_status) return boundary.type_extraction_status.replace(/\.+$/, "");
  // Pairs the index compared whose sides did not resolve to a usable type.
  const unresolved = (result.items ?? []).filter((item) => item.verdict?.state === "unresolved").length;
  if (unresolved > 0) {
    return unresolved === 1
      ? "1 pair was compared but did not resolve"
      : `${unresolved} pairs were compared but none resolved`;
  }
  return "no pair in this file was compared";
}

/**
 * The first line of an edit's message, or `null` when the edit has no finding
 * to head: a clean file, a file that matches the index and a file no
 * re-check answered for are all silent (hook-wording audit T4, 568 firings and
 * no finding). A header exists only to introduce a problem or a moved row.
 */
function headline(result: CheckResult, where: string, problems: number, late: boolean): string | null {
  const moved = movedLines(result).length > 0;
  if (problems === 0) return moved ? `Carrick re-checked ${where} after ${late ? "your earlier edit to it" : "this edit"}; rows elsewhere moved:` : null;
  const count = `${problems} contract ${problems === 1 ? "problem" : "problems"}`;
  if (recheckDidNotRun(result)) return `Carrick: ${where}: ${count}.`;
  const stale = result.recheck?.scope?.stale ?? [];
  // A service that reads this edit was judged at a version that predates it,
  // so the problems are stated for what was re-read; the Stop hook queues
  // `check --changed` for it (carrick#3466).
  if (stale.length > 0) return `Carrick re-checked ${where}: ${count} in what it re-read; ${staleClause(stale, "this edit")}.`;
  return `Carrick re-checked ${where} after ${late ? "your earlier edit to it" : "this edit"}: ${count}.`;
}

/**
 * The PostToolUse context, or `null` for an error payload.
 *
 * Only what this edit changed or what needs action, a few lines at most: on a
 * large repo the index's stored verdicts for one file ran to 8.7 KB per edit,
 * every one of them about rows the edit did not touch. So:
 *
 * - A file the re-check did not answer for, or one that matches the index:
 *   nothing, and no stored verdict, because those describe the file before
 *   the edit. The Stop hook names a gap once.
 * - A file answered from the working tree: its problems (a result other than
 *   `compatible`), at most three, and nothing about the rows that are fine.
 * - Then, unless `options.uses` is `false`, one line counting who uses this
 *   file from elsewhere (carrick#2067); the hook shows it once per file per
 *   session, and the uses themselves are the Read hook's to list.
 *
 * A clean file prints nothing. Silence is the answer: 568 clean firings
 * carried no information and cost the reader attention.
 */
export function renderPostToolUse(
  result: CheckResult,
  displayFile?: string,
  options: { uses?: boolean; late?: boolean } = {},
): string | null {
  if (result.error) return null;
  // `result.file` is relative to the repo that owns it, so the caller's own
  // workspace-relative path is the one a reader can open; it wins when given.
  const where = displayFile ?? result.file ?? "this file";
  const lines: string[] = [];
  if (answersTheEdit(result)) {
    const problems = problemItems(result).sort((a, b) => (a.line ?? 0) - (b.line ?? 0));
    if (result.deleted) {
      lines.push(deletedLine(result, where));
    } else {
      const head = headline(result, where, problems.length, options.late === true);
      if (head !== null) lines.push(head);
    }
    for (const item of problems.slice(0, MAX_EDIT_PROBLEMS)) lines.push(cutLine(itemLine(item, where)));
    const hidden = problems.length - Math.min(problems.length, MAX_EDIT_PROBLEMS);
    if (hidden > 0) lines.push(`- and ${hidden} more: \`carrick check ${where}\` lists them.`);
    lines.push(...movedLines(result));
  }
  const uses = options.uses === false ? 0 : (result.uses_lines ?? []).length;
  if (uses > 0) {
    const them = uses === 1 ? "the 1 use; open it" : `the ${uses} uses; open them`;
    lines.push(
      `Other files use what this file declares: \`carrick check ${where}\` lists ${them} if this edit changed a signature or a type.`,
    );
  }
  return lines.length > 0 ? lines.join("\n") : null;
}

/**
 * The PostToolUse context for a file the agent has just read (carrick#2069), or
 * `null` when there is nothing to say: no header on an empty answer, because a
 * header on every Read would be noise.
 *
 * Locations only, never verdicts: nothing has been edited, so there is nothing
 * to judge. The CLI's `uses_lines` first, then one line for each route or call
 * that names a counterpart in another service.
 */
export function renderPostRead(result: CheckResult, displayFile?: string): string | null {
  if (result.error) return null;
  const where = displayFile ?? result.file ?? "this file";
  const uses = result.uses_lines ?? [];
  const items = connectedItems(result).sort((a, b) => (a.line ?? 0) - (b.line ?? 0));
  if (uses.length === 0 && items.length === 0) return null;

  const lines: string[] = [
    `Carrick: other code depends on ${where} (indexed at ${shortHash(result.index_commit)}). Open these before you change what they use:`,
  ];
  lines.push(...uses.slice(0, MAX_USE_LINES));
  for (const item of items.slice(0, MAX_ITEM_LINES)) {
    lines.push(itemLine(item, where, { locatorsOnly: true }));
  }
  if (uses.length > MAX_USE_LINES || items.length > MAX_ITEM_LINES) {
    lines.push(`- more are listed by \`carrick check ${where}\`.`);
  }
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

/**
 * Candidates only `carrick index` resolves, or 0 when the hosted index is the
 * index answering for this service: an index read in from the hosted rows
 * (`hosted_state` "enriched") was built with the model, so the local
 * pre-model count describes nothing the agent can act on.
 */
export function candidatesWaiting(service: StatusService): number {
  if (service.hosted_state === "enriched") return 0;
  return service.boundary?.candidates_awaiting_model ?? 0;
}

/**
 * One line per service: what it holds, at which commit, how far the part of
 * the tree it reads has moved since, and what is waiting for a paid scan.
 *
 * Services of one repo share a commit and no longer share a changed-file
 * count: each is told how many files its own scan reads have changed, and the
 * repo's own line carries what belongs to no service (carrick#997 item 4).
 * Never the files themselves: `carrick status` lists them (carrick#3477).
 */
export function serviceLine(service: StatusService): string {
  const head = `- ${service.service} at ${shortHash(service.index_commit)}: ${service.routes} route(s), ${service.calls} call(s)`;
  const waiting = candidatesWaiting(service);
  const awaiting = waiting ? `, ${waiting} candidate(s) waiting for Carrick's cloud scan` : "";
  return `${head}, changed since index: ${service.changed_since_index}${awaiting}`;
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
  return `- ${repo.name}: ${repo.outside_every_service} file(s) changed outside every service`;
}

/** `2 h ago`, `8 days ago`: how long before `now` the time `iso` was. */
export function ago(iso: string, now: number): string | null {
  const then = Date.parse(iso);
  if (!Number.isFinite(then)) return null;
  const minutes = Math.max(0, Math.round((now - then) / 60_000));
  if (minutes < 60) return `${Math.max(1, minutes)} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} days ago`;
}

/** The repo's own name, as `example/api` is `api`. */
function repoName(repo: SyncedRepo): string {
  return repo.repo.split("/").pop() ?? repo.repo;
}

/** `api at abc1234 (scanned 2 h ago)`. */
function indexAt(repo: SyncedRepo, commit: string, scannedAt: string | null, now: number): string {
  const age = scannedAt ? ago(scannedAt, now) : null;
  return `${repoName(repo)} at ${commit.slice(0, 7)}${age ? ` (scanned ${age})` : ""}`;
}

/** Up to this many repos a hosted line names before it counts the rest. */
const NAMED_REPOS = 3;

function named(parts: string[]): string {
  const shown = parts.slice(0, NAMED_REPOS);
  const rest = parts.length - shown.length;
  return rest > 0 ? `${shown.join(", ")}, +${rest} more` : shown.join(", ");
}

/** A record older than this says when it was taken: the cloud may have moved since. */
const STALE_RECORD_MS = 60 * 60_000;

/**
 * The first session-start line where the hosted copy has a record
 * (carrick#3432): which hosted index the MCP tools answer from and how old it
 * is, and, where this machine's copy is older, that copy apart. The status
 * line's own date is this machine's scan, which an agent read as the age of
 * every answer.
 */
export function hostedLine(sync: SyncRecord, now: number): string | null {
  if (!sync.repos.length) return null;
  const hosted = sync.repos.map((repo) => indexAt(repo, repo.hosted_commit, repo.hosted_scanned_at, now));
  let line = `Carrick's MCP tools answer from the hosted index: ${named(hosted)}.`;
  const older = sync.repos.filter((repo) => repo.copy_commit !== repo.hosted_commit);
  if (older.length) {
    const copies = older.map((repo) => indexAt(repo, repo.copy_commit, repo.copy_scanned_at, now));
    line += ` The hooks and \`carrick check\` compare against this machine's older copy: ${named(copies)}.`;
  }
  const checked = Date.parse(sync.at);
  if (Number.isFinite(checked) && now - checked > STALE_RECORD_MS) {
    line += ` Last checked ${ago(sync.at, now)}.`;
  }
  return line;
}

/**
 * The line for a copy HEAD keeps back (carrick#3433): the hosted commit is
 * not in HEAD's history, so `refresh --hosted-only` cannot move the copy to
 * it. Staying on the branch's base is correct; the line makes it visible and
 * names the move. Null when no repo is held.
 */
export function heldLine(sync: SyncRecord): string | null {
  const held = sync.repos.find(
    (repo) => repo.copy_commit !== repo.hosted_commit && !repo.head_holds_hosted,
  );
  if (!held) return null;
  const target = `the hosted index of ${repoName(held)} (${held.hosted_commit.slice(0, 7)})`;
  const stuck = "so this machine's copy cannot move forward.";
  const pull = "`git pull` (or a rebase onto the default branch)";
  if (!held.fetched) {
    return `This clone has not fetched ${target}, ${stuck} \`git fetch\`, then ${pull}, moves it.`;
  }
  const behind =
    held.behind !== null
      ? `This checkout is ${held.behind} commit${held.behind === 1 ? "" : "s"} behind ${target}`
      : `This checkout's HEAD does not hold ${target}`;
  if (held.head_detached) {
    return `${behind}, ${stuck} HEAD is detached. Check out the default branch and \`git pull\` to move it.`;
  }
  return `${behind}, ${stuck} ${pull} moves it.`;
}

/**
 * The SessionStart line, from `carrick status --json`.
 *
 * `check` and `touch` each take exactly one file, so this is the only read that
 * answers for a workspace. It carries no verdict: `status` states what is
 * indexed and what each service could not classify, and nothing about whether a
 * contract holds.
 */
export function renderSessionStart(
  status: StatusResult,
  options: { afterHostedLine?: boolean } = {},
): string {
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
    // Carrick builds the index in the cloud; what this machine can do is read
    // it in (carrick#3361).
    return "Carrick has no index for this workspace, so nothing in this session is checked against the other services. `carrick refresh --workspace <dir>` downloads the index.";
  }
  if (status.error) {
    // The CLI's own sentence, which names the move: a format mismatch says
    // which scanner wrote the index and that `carrick refresh` rebuilds it.
    // The wire code alone told a reader nothing they could act on
    // (carrick#1009).
    return `Carrick could not read its index for this workspace: ${status.message ?? status.error}`;
  }
  if (!status.services.length) {
    return `Carrick has an index at ${status.workspace ?? "this workspace"} and it holds no services.`;
  }

  const version = status.scanner_version ? `, scanner ${status.scanner_version}` : "";
  const where = status.workspace ? ` in ${status.workspace}` : "";
  // Below a hosted line this date is the one beside the MCP's, so it says
  // whose it is (carrick#3432).
  const machine = options.afterHostedLine ? " on this machine" : "";
  const when = status.indexed_at ? `${machine} at ${status.indexed_at}` : "";

  // Every service, with its boundary, is what this printed: on a workspace of
  // fifteen that was about ninety lines in every session's context, most of
  // them counts of things nobody acts on when a session opens — SDK calls that
  // produced no edge, examples of types that did not resolve, rows the model
  // alone states (carrick#1365). A small workspace keeps all of it, because
  // there it IS the orientation; a large one gets the services with something
  // to do about them, and one line naming the command that prints the rest.
  // The totals go on the first line only where the services below it are the
  // exceptions; where every service is listed, each line carries its own.
  const detailed = status.services.length <= DETAILED_SERVICES;
  const lines: string[] = [
    `Carrick indexed ${status.services.length} service(s)${where}${when}${version}${detailed ? "" : totals(status)}.`,
    ...scans,
  ];
  // Said once for the whole index, ahead of the services: every verdict below
  // is missing, and an agent that reads "no contract problem" later would
  // otherwise take that as an answer (carrick#3204).
  if (status.type_check && !status.type_check.ran) {
    const reason = status.type_check.reason?.replace(/\.+$/, "") ?? "no reason recorded";
    lines.push(
      `Types were not checked for this index (${reason}), so no pair has a type verdict.`,
    );
  }
  if (detailed) {
    for (const service of status.services) lines.push(serviceLine(service));
  } else {
    // One count for the services that moved, never their files: an edit to a
    // shared file moved every service that reads it, and the same paths on
    // each line were not something to act on at session start (carrick#3477).
    const moved = status.services.filter((service) => service.changed_since_index > 0).length;
    if (moved > 0) {
      lines.push(
        `${moved} service${moved === 1 ? " has" : "s have"} local changes since the index; \`carrick status\` lists them.`,
      );
    }
    const actionable = status.services.filter(hasSomethingToDo);
    for (const service of actionable.slice(0, LISTED_SERVICES)) lines.push(serviceLine(service));
    if (actionable.length > LISTED_SERVICES) {
      lines.push(`- +${actionable.length - LISTED_SERVICES} more service(s) with something to act on`);
    }
  }
  for (const repo of status.repos ?? []) {
    const line = repoLine(repo);
    if (line) lines.push(line);
  }
  if (detailed) {
    for (const service of status.services) {
      for (const line of serviceBoundary(service)) lines.push(line);
    }
  } else {
    lines.push(BOUNDARY_POINTER);
  }
  return lines.join("\n");
}

/**
 * The one instruction a session start gives about finishing work. The
 * AGENTS.md section alone did not get the check run (proof run: 1 of 6).
 */
export const CHECK_BEFORE_DONE =
  "When your change touches a route, a call to another service, an event or a type another service reads, run `carrick check --changed` before you call it done. Exit 1 is a contract problem; exit 2 means nothing could be checked.";

/** Up to this many services, a session start states every one of them. */
export const DETAILED_SERVICES = 3;

/** Above that, at most this many of the ones with something to do. */
export const LISTED_SERVICES = 8;

/** Where the per-service boundary report lives, for the session that wants it. */
export const BOUNDARY_POINTER =
  "Per-service detail, including what each scan could not classify: `carrick status`.";

/**
 * Whether a service is worth a line at the start of a session.
 *
 * Two things a reader can act on now: the service is holding candidates that
 * only `carrick index` resolves, or the hosted rows are not the ones being
 * answered from. Local changes are a count at the session start, not a reason
 * to name the service (carrick#3477). A service that is neither is in the
 * total on the first line and nowhere else.
 */
export function hasSomethingToDo(service: StatusService): boolean {
  if (candidatesWaiting(service) > 0) return true;
  return service.hosted_state !== undefined && service.hosted_state !== "enriched";
}

/** What the whole index holds, so the services below it can be the exceptions. */
function totals(status: StatusResult): string {
  const sum = (pick: (service: StatusService) => number): number =>
    status.services.reduce((total, service) => total + pick(service), 0);
  return `: ${sum((service) => service.routes)} route(s), ${sum((service) => service.calls)} call(s)`;
}
