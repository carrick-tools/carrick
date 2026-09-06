// Check verdicts as LSP diagnostics.
//
// Four rules decide what a diagnostic looks like here.
//
// R1: a finding on a fact row is an error; the same finding on a candidate row
// is a warning that says it is the model's reading. `verdict.state` is the type
// layer's word and not a confidence in the row: `method_mismatch` and
// `producer_removed` are routing facts and carry `not_checked` because no type
// verdict bears on them, so keying severity off the state alone would demote
// every routing finding. A state of `unresolved` is the exception: nothing was
// claimed there, so nothing is asserted here either.
//
// E18: Claude Code drops `relatedInformation` from the attachment it gives the
// model, and editors render it as clickable locations. So every counterpart
// site goes in BOTH: in the message text for the agent, and in
// `relatedInformation` for the human.
//
// The boundary is never dropped. The local index holds no bare-receiver route
// and no `fetch` call, because both need a model, so a file with no findings is
// not a file with no contracts. One information diagnostic carries the boundary
// on every checked file that has one.
//
// Counterpart paths are relative to the counterpart's OWN repo, and the payload
// does not name that repo's directory. So a counterpart location is resolved
// against the workspace root and against `<root>/<service>`, and when neither
// exists the site stays in the message text and gets no URI: a wrong URI is
// worse than a location the reader has to open themselves.

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

export const SOURCE = "carrick";

export const SEVERITY = { error: 1, warning: 2, information: 3, hint: 4 } as const;

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

/** An error only for a finding on a fact row that claims something (R1c). */
export function severityOf(item: CheckItem): number {
  if (isCandidate(item)) return SEVERITY.warning;
  if (item.source !== "fact") return SEVERITY.warning;
  if (item.verdict?.state === "unresolved") return SEVERITY.warning;
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
 * Where a counterpart's repo-relative path lands on this disk, or null.
 *
 * `<root>/<file>` covers a workspace whose repo directory is already in the
 * path; `<root>/<service>/<file>` covers the common layout where the service
 * name is its directory.
 */
export function resolveCounterpart(
  root: string,
  counterpart: Counterpart,
  exists: (target: string) => boolean = defaultExists,
  ownRepo?: string,
): string | null {
  if (!counterpart.file) return null;
  if (path.isAbsolute(counterpart.file)) return exists(counterpart.file) ? counterpart.file : null;
  const candidates = [path.resolve(root, counterpart.file)];
  if (counterpart.service) {
    candidates.push(path.resolve(root, counterpart.service, counterpart.file));
  }
  // A counterpart in the queried file's own repo resolves against `repo`.
  if (ownRepo) candidates.push(path.resolve(ownRepo, counterpart.file));
  return candidates.find((candidate) => exists(candidate)) ?? null;
}

export type DiagnosticOptions = {
  /** Injectable for tests. */
  exists?: (target: string) => boolean;
};

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

  for (const item of problemItems(result)) {
    const counterparts = item.counterparts ?? [];
    const related = counterparts
      .map((counterpart) => ({ counterpart, resolved: resolveCounterpart(root, counterpart, exists, result.repo) }))
      .filter((entry) => entry.resolved !== null)
      .map((entry) => ({
        location: {
          uri: pathToFileURL(entry.resolved as string).toString(),
          range: rangeAt(entry.counterpart.line, 1),
        },
        message: counterpartPhrase(entry.counterpart),
      }));
    const diagnostic: Diagnostic = {
      range: rangeAt(item.line, item.col),
      severity: severityOf(item),
      source: SOURCE,
      message: messageOf(item),
    };
    if (item.verdict?.result) diagnostic.code = item.verdict.result;
    if (related.length) diagnostic.relatedInformation = related;
    put(checkedAbs, diagnostic);

    for (const counterpart of counterparts) {
      const resolved = resolveCounterpart(root, counterpart, exists, result.repo);
      if (!resolved) continue;
      const mirrored: Diagnostic = {
        range: rangeAt(counterpart.line, 1),
        severity: severityOf(item),
        source: SOURCE,
        message: `${counterpartPhrase(counterpart)} of ${result.file ?? checkedFile}. ${messageOf(item)}`,
      };
      if (item.verdict?.result) mirrored.code = item.verdict.result;
      put(resolved, mirrored);
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

  const boundary = boundaryFor(result);
  if (boundary.length) {
    put(checkedAbs, {
      range: rangeAt(1, 1),
      severity: SEVERITY.information,
      code: "boundary",
      source: SOURCE,
      message: `What this service's scan could not classify:\n${boundary.join("\n")}`,
    });
  }
  return byFile;
}
