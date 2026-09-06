// The `carrick check|touch <file> --json` output, schema `carrick.check/0`.
//
// The CLI owns this shape: docs/local-mode-output.md and
// docs/schemas/carrick-check-0.json (carrick#708). It may ADD fields; it never
// renames or removes one. Everything here is read-only: the
// plugin parses what it is given, keeps the fields it renders, and drops the
// rest. An unknown `kind`, `result` or `state` value is carried through as a
// string rather than rejected, so a CLI that learns a new verdict result still
// renders a line here instead of nothing.

/** A count with up to 200 of its reasons, mirroring `Counted` in src/boundary.rs. */
export type Counted = {
  total: number;
  reasons?: string[];
  truncated?: boolean;
};

/** `ServiceBoundary` from src/boundary.rs, as it rides the check output. */
export type Boundary = {
  commit_hash?: string;
  files_attempted?: number;
  files_lost?: Counted;
  unemitted_literal_candidates?: number;
  consumers_not_resolved?: Counted;
  sdk_unresolved?: Counted;
  unknown_call_paths?: Counted;
  model_only_rows?: number;
  model_rows_joined?: number;
  model_contradictions_discarded?: number;
  model_endpoints_discarded_in_claimed_modules?: number;
  routes_without_response_type?: Counted;
  calls_without_expected_type?: Counted;
  /** Local mode runs no model, so nothing it indexed was classified by one. */
  candidates_not_classified?: number;
  types_degraded?: { stage?: string; detail?: string };
  bare_checkout?: boolean;
};

export type Counterpart = {
  /**
   * `peer` is a shared external contract: both sides call the same third
   * party and neither serves the other, so it gets no producer/consumer word.
   */
  role: "producer" | "consumer" | "peer" | string;
  service?: string;
  file?: string;
  line?: number;
};

export type Verdict = {
  state: "resolved" | "unresolved" | "not_checked" | string;
  /** Null while `state` is `not_checked`: matched, never compared. */
  result:
    | null
    | "compatible"
    | "type_mismatch"
    | "method_mismatch"
    | "producer_removed"
    | string;
  detail?: string;
};

export type CheckItem = {
  kind: "route" | "call" | string;
  method?: string;
  path?: string;
  line?: number;
  col?: number;
  /** R1: a row whose only source is the model is a candidate, never a fact. */
  source?: "fact" | "candidate" | string;
  resolution_source?: string | null;
  /** One line naming what the row was read off. */
  evidence?: string | null;
  counterparts?: Counterpart[];
  verdict?: Verdict | null;
};

export type CheckResult = {
  schema: string;
  /** Set instead of the payload, e.g. `not_indexed`. */
  error?: string;
  file?: string;
  service?: string;
  index_commit?: string;
  /** RFC 3339 time the index or the last refresh of this service ran. */
  indexed_at?: string;
  /** The scanner release that wrote the index. */
  scanner_version?: string;
  changed_since_index?: number;
  stale?: boolean;
  /** The index holds this file and it is no longer on disk. */
  deleted?: boolean;
  items?: CheckItem[];
  boundary?: Boundary;
};

export const SCHEMA = "carrick.check/0";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Parse CLI stdout into a `CheckResult`, or `null` when it is not one.
 *
 * Only the schema tag is required. A payload whose `items` is missing reads as
 * "no locations", which is what an unindexed file returns, and is not an error.
 */
export function parseCheckResult(stdout: string): CheckResult | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) return null;
  if (typeof parsed["schema"] !== "string") return null;
  if (parsed["schema"] !== SCHEMA) return null;
  const items = Array.isArray(parsed["items"]) ? (parsed["items"] as CheckItem[]) : [];
  const result: CheckResult = {
    schema: parsed["schema"],
    items,
  };
  if (typeof parsed["error"] === "string") result.error = parsed["error"];
  if (typeof parsed["file"] === "string") result.file = parsed["file"];
  if (typeof parsed["service"] === "string") result.service = parsed["service"];
  if (typeof parsed["index_commit"] === "string") result.index_commit = parsed["index_commit"];
  if (typeof parsed["indexed_at"] === "string") result.indexed_at = parsed["indexed_at"];
  if (typeof parsed["scanner_version"] === "string") {
    result.scanner_version = parsed["scanner_version"];
  }
  if (typeof parsed["deleted"] === "boolean") result.deleted = parsed["deleted"];
  if (typeof parsed["changed_since_index"] === "number") {
    result.changed_since_index = parsed["changed_since_index"];
  }
  if (typeof parsed["stale"] === "boolean") result.stale = parsed["stale"];
  if (isRecord(parsed["boundary"])) result.boundary = parsed["boundary"] as Boundary;
  return result;
}

/** True when the row is the model's reading alone (R1). */
export function isCandidate(item: CheckItem): boolean {
  return item.source === "candidate";
}

/**
 * Items worth reporting: a verdict that states a result other than
 * `compatible`.
 *
 * A null verdict, a null result and `not_checked` all say the same thing in
 * different words: nothing was compared here. None of them is a problem to
 * publish. Both channels report the same set, so a diagnostic and a hook line
 * never disagree about what is wrong.
 */
export function problemItems(result: CheckResult): CheckItem[] {
  return (result.items ?? []).filter((item) => {
    const verdict = item.verdict;
    if (!verdict || verdict.state === "not_checked") return false;
    return verdict.result != null && verdict.result !== "compatible";
  });
}

/** Items that name a counterpart in another service, problem or not. */
export function connectedItems(result: CheckResult): CheckItem[] {
  return (result.items ?? []).filter((item) => (item.counterparts ?? []).length > 0);
}
