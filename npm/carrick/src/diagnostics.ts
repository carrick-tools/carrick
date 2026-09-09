// Check verdicts as LSP diagnostics, inside a noise budget.
//
// The budget is carrick#879 and the design record of 2026-09-09 ("The noise
// budget for the whole surface"). A finding a user turns off is worth less than
// no finding at all, because turning it off takes every later finding with it,
// so what this file publishes is bounded before it is interesting.
//
// R1: a finding on a fact row is an error; the same finding on a candidate row
// is a warning that says it is the model's reading. `verdict.state` is the type
// layer's word and not a confidence in the row: `method_mismatch` and
// `producer_removed` are routing facts and carry `not_checked` because no type
// verdict bears on them, so keying severity off the state alone would demote
// every routing finding. A state of `unresolved` is the exception: nothing was
// claimed there, so nothing is asserted here either.
//
// SEVERITY POLICY, stated once and only here (severityOf). Error is for a fact
// row whose verdict claims something and is not compatible. Warning is for
// candidates, for `unresolved`, and for a routing finding with no counterpart
// this machine can open: an error nobody can navigate to asserts more than the
// payload supports. Information is not used per file at all, except as the
// boundary fallback below. Hint is never used anywhere: it renders as a faint
// underline to a human and as nothing at all to an agent, which buys presence
// without buying attention.
//
// E18: Claude Code drops `relatedInformation` from the attachment it gives the
// model, and editors render it as clickable locations. So every counterpart
// site goes in BOTH: in the message text for the agent, and in
// `relatedInformation` for the human.
//
// THE BOUNDARY IS NEVER DROPPED, and after carrick#879 it is not always in the
// Problems list. The local index holds no bare-receiver route and no `fetch`
// call, because both need a model, so a file with no findings is not a file
// with no contracts. It belongs to the workspace rather than to a file, so a
// client that has somewhere workspace-shaped to put it says so with
// `boundarySurface` (the VS Code status bar item does) and gets no per-file
// row; a client that says nothing keeps the file-level Information diagnostic,
// which `carrick.boundary` turns off. The hook channel and `carrick check` are
// untouched by any of this: render.ts states the boundary there whatever a
// client said, which is what the agent reads.
//
// A counterpart's `file` is relative to its OWN repo, and the payload names
// that repo absolutely, so `repo` + `file` is the path and nothing is guessed.
// `repo` is null when the index no longer holds the counterpart's repo: then
// there is no path to give, the site stays in the message text with no URI, and
// no diagnostic is mirrored onto a file this machine cannot point at.

import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import {
  isCandidate,
  problemItems,
  type CheckItem,
  type CheckResult,
  type Counterpart,
} from "./contract.ts";
import { boundaryFor, stateWord } from "./render.ts";
import { DEFAULT_SURFACES, type Surfaces } from "./surfaces.ts";

export const SOURCE = "carrick";

export const SEVERITY = { error: 1, warning: 2, information: 3, hint: 4 } as const;

/**
 * Findings one file publishes before it says how many are left.
 *
 * The hook's `MAX_ITEM_LINES` is the same idea on the other channel and the two
 * messages are written to read alike. The boundary fallback rides outside this
 * cap: it is a statement about the workspace and not a finding, so a file with
 * ten findings and a boundary publishes eleven rows and only one of them is the
 * overflow row's business.
 */
export const MAX_PER_FILE = 10;
/** Findings one check publishes across every file it touches. */
export const MAX_PER_CHECK = 30;

export type Position = { line: number; character: number };
export type Range = { start: Position; end: Position };

export type Diagnostic = {
  range: Range;
  severity: number;
  code?: string;
  source: string;
  message: string;
  relatedInformation?: Array<{
    location: { uri: string; range: Range };
    message: string;
  }>;
};

function rangeAt(line: number | undefined, col: number | undefined): Range {
  const zeroLine = Math.max(0, (line ?? 1) - 1);
  const zeroCol = Math.max(0, (col ?? 1) - 1);
  return {
    start: { line: zeroLine, character: zeroCol },
    end: { line: zeroLine, character: zeroCol + 1 },
  };
}

/**
 * A routing finding: a result with no type verdict behind it.
 *
 * `method_mismatch` and `producer_removed` are read off two deterministic rows
 * and carry `not_checked` because no type verdict bears on them. They are the
 * findings whose whole claim is about the other side of a call.
 */
function isRoutingFinding(item: CheckItem): boolean {
  const verdict = item.verdict;
  return Boolean(verdict && verdict.result != null && verdict.state === "not_checked");
}

/**
 * The severity policy, in one place.
 *
 * Error only for a finding on a fact row that claims something (R1c) and whose
 * claim a reader can go and check. `resolvable` is whether any counterpart of
 * this row is on this disk: a routing finding is entirely a statement about the
 * other side, so when the other side cannot be opened it is a warning. Nothing
 * here ever returns Hint.
 */
export function severityOf(item: CheckItem, resolvable = true): number {
  if (isCandidate(item)) return SEVERITY.warning;
  if (item.source !== "fact") return SEVERITY.warning;
  if (item.verdict?.state === "unresolved") return SEVERITY.warning;
  if (isRoutingFinding(item) && !resolvable) return SEVERITY.warning;
  return SEVERITY.error;
}

/** `consumer in admin-ui`, and for a peer the role word is left out. */
function counterpartPhrase(counterpart: Counterpart): string {
  const service = counterpart.service ?? "an unnamed service";
  return counterpart.role === "peer" ? `the same contract in ${service}` : `${counterpart.role} in ${service}`;
}

function counterpartWhere(counterpart: Counterpart): string {
  return `${counterpart.file ?? "an unnamed file"}${counterpart.line ? `:${counterpart.line}` : ""}`;
}

export function messageOf(item: CheckItem): string {
  const operation = [item.method, item.path].filter(Boolean).join(" ");
  const verdict = item.verdict;
  // `result` is null wherever the state is the whole statement, and a result
  // whose state is not `resolved` has no compiler verdict behind it.
  const verdictWords = verdict
    ? verdict.result == null
      ? stateWord(verdict.state)
      : verdict.state === "resolved"
        ? verdict.result
        : `${verdict.result} (${stateWord(verdict.state)})`
    : "";
  const head = [operation, verdictWords].filter(Boolean).join(" ");
  const parts: string[] = [verdict?.detail ? `${head}: ${verdict.detail}` : head];
  if (isCandidate(item)) {
    const from = item.resolution_source ? ` (${item.resolution_source})` : "";
    parts.push(`Candidate row${from}, so this is a reading of the code and not a fact about it.`);
  }
  if (item.evidence) parts.push(`Read off ${item.evidence}.`);
  const counterparts = item.counterparts ?? [];
  if (counterparts.length) {
    parts.push(
      `Counterparts: ${counterparts
        .map((counterpart) => `${counterpartPhrase(counterpart)}, ${counterpartWhere(counterpart)}`)
        .join("; ")}`,
    );
  }
  return parts.join("\n");
}

function defaultExists(target: string): boolean {
  try {
    return fs.existsSync(target);
  } catch {
    return false;
  }
}

/**
 * Where a counterpart lands on this disk, or null when it cannot be pointed at.
 *
 * `repo` + `file`, and nothing else: the payload states the repo absolutely, so
 * a reader that also tried the workspace root or a directory named after the
 * service would be guessing at paths the producer already knows.
 */
export function resolveCounterpart(
  counterpart: Counterpart,
  exists: (target: string) => boolean = defaultExists,
): string | null {
  if (!counterpart.file) return null;
  if (path.isAbsolute(counterpart.file)) return exists(counterpart.file) ? counterpart.file : null;
  if (!counterpart.repo) return null;
  const target = path.resolve(counterpart.repo, counterpart.file);
  return exists(target) ? target : null;
}

export type DiagnosticOptions = {
  /** Injectable for tests. */
  exists?: (target: string) => boolean;
  /** The switches the client stated. Every surface is on when it stated none. */
  surfaces?: Surfaces;
};

/** Every resolvable counterpart of one item, one entry per file it lands in. */
type CounterpartGroup = {
  /** Absolute path on this disk. */
  target: string;
  /** The counterparts in that file, in payload order. */
  sites: Counterpart[];
};

/**
 * A finding's counterparts grouped by the file they land in.
 *
 * Two call sites in one consumer file are one finding in one file, so they are
 * one mirrored row naming both lines rather than two rows saying the same
 * sentence. Order follows the payload, so the row lands on the first site.
 */
function groupCounterparts(
  counterparts: Counterpart[],
  exists: (target: string) => boolean,
): CounterpartGroup[] {
  const groups: CounterpartGroup[] = [];
  const byTarget = new Map<string, CounterpartGroup>();
  for (const counterpart of counterparts) {
    const target = resolveCounterpart(counterpart, exists);
    if (!target) continue;
    const existing = byTarget.get(target);
    if (existing) {
      existing.sites.push(counterpart);
      continue;
    }
    const group: CounterpartGroup = { target, sites: [counterpart] };
    byTarget.set(target, group);
    groups.push(group);
  }
  return groups;
}

/** The eleventh row: what was not shown, and the command that shows it. */
function overflowRow(hidden: number, where: string, checkedFile: string): Diagnostic {
  return {
    range: rangeAt(1, 1),
    severity: SEVERITY.warning,
    code: "capped",
    source: SOURCE,
    message: `and ${hidden} more finding(s) ${where}, from \`carrick check ${checkedFile}\`.`,
  };
}

const isBoundaryRow = (diagnostic: Diagnostic): boolean => diagnostic.code === "boundary";

/**
 * Findings first and then by line, which is the order the cap keeps.
 *
 * "Problems first" is severity order: when ten of forty rows survive, the ten
 * that survive are the ones that claim the most.
 */
function orderRows(rows: Diagnostic[]): Diagnostic[] {
  return [...rows].sort((a, b) => a.severity - b.severity || a.range.start.line - b.range.start.line);
}

/**
 * The cap, applied to a whole check.
 *
 * Ten findings per file with an eleventh row naming the remainder, thirty rows
 * across the check with one more naming what the check as a whole did not show,
 * and a file the budget drops publishes an empty list so its previous rows are
 * cleared rather than left standing. The boundary fallback rides outside both
 * counts: it states something about the workspace and is not a finding.
 */
export function capDiagnostics(
  byFile: Map<string, Diagnostic[]>,
  checkedAbs: string,
  checkedFile: string,
): Map<string, Diagnostic[]> {
  const order = [
    ...(byFile.has(checkedAbs) ? [checkedAbs] : []),
    ...[...byFile.keys()].filter((file) => file !== checkedAbs).sort(),
  ];
  const staged = order.map((file) => {
    const rows = byFile.get(file) ?? [];
    const findings = orderRows(rows.filter((row) => !isBoundaryRow(row)));
    return {
      file,
      shown: findings.slice(0, MAX_PER_FILE),
      hidden: Math.max(0, findings.length - MAX_PER_FILE),
      boundary: rows.filter(isBoundaryRow),
    };
  });

  // Whether the check as a whole overflows is decided before the walk, so the
  // row that says so has a slot reserved and the thirty is never exceeded.
  const wanted = staged.reduce((total, file) => total + file.shown.length + (file.hidden ? 1 : 0), 0);
  const overflows = wanted > MAX_PER_CHECK;
  let budget = overflows ? MAX_PER_CHECK - 1 : MAX_PER_CHECK;

  const capped = new Map<string, Diagnostic[]>();
  let unstated = 0;
  for (const file of staged) {
    const take = Math.max(0, Math.min(file.shown.length, budget));
    const rows: Diagnostic[] = file.shown.slice(0, take);
    budget -= take;
    unstated += file.shown.length - take;
    if (file.hidden > 0) {
      if (budget > 0 && take === file.shown.length) {
        rows.push(overflowRow(file.hidden, "in this file", checkedFile));
        budget -= 1;
      } else {
        unstated += file.hidden;
      }
    }
    rows.push(...file.boundary);
    capped.set(file.file, rows);
  }
  if (unstated > 0) {
    const rows = capped.get(checkedAbs) ?? [];
    rows.push(overflowRow(unstated, "elsewhere in this check", checkedFile));
    capped.set(checkedAbs, rows);
  }
  return capped;
}

/**
 * Diagnostics for one check payload, keyed by the absolute file they belong to.
 *
 * The checked file gets one per problem item, one for the boundary, and one
 * more when the index holds a file that is no longer on disk. Each counterpart
 * site whose path resolves gets the same finding at its own line, so an agent
 * that opens the consumer reads it there too.
 */
export function toDiagnostics(
  result: CheckResult,
  root: string,
  checkedFile: string,
  options: DiagnosticOptions = {},
): Map<string, Diagnostic[]> {
  const exists = options.exists ?? defaultExists;
  const surfaces = options.surfaces ?? DEFAULT_SURFACES;
  const byFile = new Map<string, Diagnostic[]>();
  if (result.error) return byFile;

  // The path the caller asked about, always: that is the document the editor
  // has open, and a diagnostic published to any other URI is invisible. The
  // payload's `repo` + `file` names the same file by another route and is used
  // for counterparts, where the caller has no path of its own.
  const checkedAbs = path.resolve(root, checkedFile);
  const put = (file: string, diagnostic: Diagnostic): void => {
    const existing = byFile.get(file);
    if (existing) existing.push(diagnostic);
    else byFile.set(file, [diagnostic]);
  };

  if (surfaces.diagnostics) {
    for (const item of problemItems(result)) {
      const groups = groupCounterparts(item.counterparts ?? [], exists);
      // A routing finding whose other side is not on this disk cannot be gone
      // and looked at, so it is stated as a warning rather than asserted.
      const severity = severityOf(item, groups.length > 0);
      const related = groups.flatMap((group) =>
        group.sites.map((counterpart) => ({
          location: {
            uri: pathToFileURL(group.target).toString(),
            range: rangeAt(counterpart.line, 1),
          },
          message: counterpartPhrase(counterpart),
        })),
      );
      const diagnostic: Diagnostic = {
        range: rangeAt(item.line, item.col),
        severity,
        source: SOURCE,
        message: messageOf(item),
      };
      if (item.verdict?.result) diagnostic.code = item.verdict.result;
      if (related.length) diagnostic.relatedInformation = related;
      put(checkedAbs, diagnostic);

      // 0b: a finding is mirrored onto each counterpart FILE, once, and never
      // below Warning, because a row a reader did not ask for has to be worth
      // the interruption in the file it lands in.
      if (severity > SEVERITY.warning) continue;
      for (const group of groups) {
        const [first, ...rest] = group.sites;
        if (!first) continue;
        const alsoAt = rest
          .map((counterpart) => counterpart.line)
          .filter((line): line is number => typeof line === "number");
        const also = alsoAt.length ? ` Also at line ${alsoAt.join(", ")} in this file.` : "";
        const mirrored: Diagnostic = {
          range: rangeAt(first.line, 1),
          severity,
          source: SOURCE,
          message: `${counterpartPhrase(first)} of ${result.file ?? checkedFile}.${also} ${messageOf(item)}`,
        };
        if (item.verdict?.result) mirrored.code = item.verdict.result;
        put(group.target, mirrored);
      }
    }

    if (result.deleted) {
      const consumers = (result.items ?? []).reduce(
        (total, item) => total + (item.counterparts ?? []).length,
        0,
      );
      put(checkedAbs, {
        range: rangeAt(1, 1),
        severity: SEVERITY.warning,
        code: "producer_removed",
        source: SOURCE,
        message: `producer_removed: the index holds ${(result.items ?? []).length} row(s) for this file and it is no longer on disk. ${consumers} counterpart(s) still name it.`,
      });
    }
  }

  // The fallback, and only the fallback: a client that told us it has a
  // workspace surface for the boundary (the VS Code status bar) is not sent a
  // per-file row for it. Nothing here decides whether the boundary is stated,
  // only whether it is stated HERE; render.ts states it on the agent channel
  // either way.
  const boundary = boundaryFor(result);
  if (boundary.length && surfaces.boundary && !surfaces.boundarySurface) {
    put(checkedAbs, {
      range: rangeAt(1, 1),
      severity: SEVERITY.information,
      code: "boundary",
      source: SOURCE,
      message: `What this service's scan could not classify:\n${boundary.join("\n")}`,
    });
  }
  return capDiagnostics(byFile, checkedAbs, checkedFile);
}
