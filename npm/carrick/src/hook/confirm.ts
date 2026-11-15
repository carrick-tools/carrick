// Confirm or correct (part 3, B1, pins P9, P10, P12): the one comparison
// between an entry's early answer (the held compiler's, `held/early.ts`) and
// the full re-check's answer for the same entry. Every surface that prints a
// full answer, and the runner and the daemon that write the record, read it
// here.
//
// The join (P9): a route row by (row file, `METHOD path{dispatch}`), never by
// line; a part-2 consumer call row by place identity, its (file, line) being
// one of the early row's group places. What follows from it (P10), per early
// break:
// - part 2 states the same break (same result kind): `same`, nothing more;
// - part 2 calls the row compatible: `differ`, one correction line;
// - part 2 states a break of another kind on the route: `differ`, and part 2's
//   item prints (it is a break the early answer lacked, in kind);
// - part 2 leaves it unresolved, did not judge it, or has no row for it:
//   `part3_only`, and the early break stands (R5a).
// A part-2 break with no early row is `part2_only` and prints as today.

import type { CheckItem, CheckResult, MovedRow } from "../contract.ts";
import { problemItems } from "../contract.ts";

/** The label a check item has as a contract row: `METHOD path{ dispatch}`. */
export function itemLabel(item: CheckItem): string | null {
  return item.method && item.path ? `${item.method} ${item.path}${item.dispatch_label ?? ""}` : null;
}

export type RowOutcome = "same" | "differ" | "part3_only";
/** Why part 2 left an early break standing. */
export type P2State = "unresolved" | "absent" | "not_judged";
export type HeldBucket = RowOutcome | "part2_only" | "none";

/** A consumer place of an early row: a counterpart's file (its own repo's) and line. */
type Place = { file: string; line: number };

/** One break of the early answer: an item in the edited file or a moved row in another. */
export type EarlyRow = {
  /** Workspace-relative row file and label: the same break under two entries has one key. */
  key: string;
  label: string;
  item?: CheckItem;
  moved?: MovedRow;
  places: Place[];
};

const keyOf = (file: string, label: string): string => `${file}\0${label}`;

function placeOf(text: string): Place | null {
  const match = /^\S+ (.+):(\d+)$/.exec(text);
  return match ? { file: match[1]!, line: Number(match[2]) } : null;
}

/** The breaks an early answer states, in order. */
export function earlyRows(early: CheckResult): EarlyRow[] {
  const file = early.file ?? "";
  const out: EarlyRow[] = [];
  for (const item of problemItems(early)) {
    const label = itemLabel(item);
    if (label === null) continue;
    const places = (item.counterparts ?? []).flatMap((c) => (c.file && c.line !== undefined ? [{ file: c.file, line: c.line }] : []));
    out.push({ key: keyOf(file, label), label, item, places });
  }
  for (const row of early.recheck?.moved ?? []) {
    const label = `${row.method} ${row.path}`;
    out.push({ key: keyOf(row.file, label), label, moved: row, places: row.counterparts.map(placeOf).filter((p): p is Place => p !== null) });
  }
  return out;
}

/** Whether an early answer states any break. */
export function hasEarlyBreak(early: CheckResult): boolean {
  return earlyRows(early).length > 0;
}

export type RowCompared = { row: EarlyRow; outcome: RowOutcome; p2_state?: P2State; corrected: boolean; match: CheckItem | MovedRow | null };

/** What part 2 says about each early row, and its own breaks the early answer lacked. */
export type Compared = {
  rows: RowCompared[];
  /** Part 2's problem items and moved rows with no early row (mirror call rows excluded). */
  part2Only: Array<CheckItem | MovedRow>;
  /** Part 2's call rows at a place of an early row: the consumer end of the same break. */
  mirrors: Array<CheckItem | MovedRow>;
};

const samePlace = (file: string, line: number | undefined, place: Place): boolean =>
  line === place.line && (file === place.file || file.endsWith(`/${place.file}`));

/**
 * Compare an entry's early answer with its full answer. `full` undefined (or an
 * error) is a full answer that judged nothing: every early row stands, as
 * `absent`.
 */
export function compareEarly(early: CheckResult, full: CheckResult | undefined): Compared {
  const rows = earlyRows(early);
  const usable = full && !full.error ? full : undefined;
  const items = usable?.items ?? [];
  const moved = usable?.recheck?.moved ?? [];
  const fullFile = usable?.file ?? early.file ?? "";
  const matched = new Set<CheckItem | MovedRow>();
  const compared: RowCompared[] = rows.map((row) => {
    if (row.item) {
      const match = items.find((item) => item.kind === "route" && itemLabel(item) === row.label) ?? null;
      if (match) matched.add(match);
      const verdict = match?.verdict ?? null;
      if (match === null) return { row, outcome: "part3_only", p2_state: "absent", corrected: false, match };
      if (verdict?.state === "unresolved") return { row, outcome: "part3_only", p2_state: "unresolved", corrected: false, match };
      if (verdict?.state === "resolved" && verdict.result === "compatible") return { row, outcome: "differ", corrected: true, match };
      if (verdict?.result != null && verdict.result !== "compatible") {
        return { row, outcome: verdict.result === row.item.verdict?.result ? "same" : "differ", corrected: false, match };
      }
      return { row, outcome: "part3_only", p2_state: "not_judged", corrected: false, match };
    }
    const own = row.moved!;
    const match = moved.find((m) => m.role === "route" && m.file === own.file && `${m.method} ${m.path}` === row.label) ?? null;
    if (match === null) return { row, outcome: "part3_only", p2_state: "absent", corrected: false, match };
    matched.add(match);
    return { row, outcome: match.after === own.after ? "same" : "differ", corrected: false, match };
  });
  const standing = compared.filter((c) => !c.corrected).flatMap((c) => c.row.places);
  const mirror = (file: string, line: number | undefined): boolean => standing.some((place) => samePlace(file, line, place));
  const mirrors: Array<CheckItem | MovedRow> = [];
  const part2Only: Array<CheckItem | MovedRow> = [];
  for (const item of usable ? problemItems(usable) : []) {
    if (matched.has(item)) continue;
    if (item.kind === "call" && mirror(fullFile, item.line)) mirrors.push(item);
    else part2Only.push(item);
  }
  for (const row of moved) {
    if (matched.has(row)) continue;
    if (row.role === "call" && mirror(row.file, row.line)) mirrors.push(row);
    else part2Only.push(row);
  }
  return { rows: compared, part2Only, mirrors };
}

/** The differ record (P12): one bucket per entry by precedence, the count per bucket, and why part 2 left a break standing. */
export type HeldRecord = { held: HeldBucket; held_rows: Record<Exclude<HeldBucket, "none">, number>; p2_state?: P2State };

const PRECEDENCE: Array<Exclude<HeldBucket, "none">> = ["differ", "part3_only", "part2_only", "same"];

export function heldRecord(compared: Compared): HeldRecord {
  const held_rows = { differ: 0, part3_only: 0, part2_only: compared.part2Only.length, same: 0 };
  for (const row of compared.rows) held_rows[row.outcome] += 1;
  const held = PRECEDENCE.find((bucket) => held_rows[bucket] > 0) ?? "none";
  const first = compared.rows.find((row) => row.outcome === "part3_only");
  return { held, held_rows, ...(held === "part3_only" && first?.p2_state ? { p2_state: first.p2_state } : {}) };
}

/**
 * The full answer as it prints once the early rows under `said` were printed:
 * without the breaks those rows stated (`same`) and their mirror call rows, and
 * the labels part 2 called compatible, for one correction line each. Rows not
 * said were never printed, so nothing about them is taken out or corrected.
 */
export function confirmedResult(full: CheckResult, early: CheckResult, said: ReadonlySet<string>): { result: CheckResult; corrected: string[] } {
  const compared = compareEarly(early, full);
  const drop = new Set<CheckItem | MovedRow>();
  const corrected: string[] = [];
  const saidRows = compared.rows.filter((c) => said.has(c.row.key));
  for (const c of saidRows) {
    if (c.outcome === "same" && c.match) drop.add(c.match);
    if (c.corrected) corrected.push(c.row.label);
  }
  const saidPlaces = saidRows.filter((c) => !c.corrected).flatMap((c) => c.row.places);
  for (const row of compared.mirrors) {
    const file = "kind" in row ? (full.file ?? "") : row.file;
    if (saidPlaces.some((place) => samePlace(file, row.line, place))) drop.add(row);
  }
  if (drop.size === 0) return { result: full, corrected };
  const items = (full.items ?? []).filter((item) => !drop.has(item));
  const moved = full.recheck?.moved?.filter((row) => !drop.has(row));
  return {
    result: { ...full, items, ...(full.recheck ? { recheck: { ...full.recheck, ...(moved ? { moved } : {}) } } : {}) },
    corrected,
  };
}

/**
 * The early answer as it prints when the full answer is already there: only
 * the rows part 2 did not judge (`part3_only`), which stand (R5a). A row part 2
 * judged is part 2's to say, and it has said it.
 */
export function standingEarly(early: CheckResult, full: CheckResult | undefined): CheckResult {
  if (full === undefined) return early;
  const standing = new Set(compareEarly(early, full).rows.filter((c) => c.outcome === "part3_only").map((c) => c.row));
  return keepRows(early, (row) => standing.has(row));
}

/** The early answer with only the rows `keep` accepts. */
export function keepRows(early: CheckResult, keep: (row: EarlyRow) => boolean): CheckResult {
  const rows = earlyRows(early).filter(keep);
  const items = new Set(rows.flatMap((row) => (row.item ? [row.item] : [])));
  const moved = rows.flatMap((row) => (row.moved ? [row.moved] : []));
  return {
    ...early,
    items: (early.items ?? []).filter((item) => items.has(item)),
    ...(early.recheck ? { recheck: { ...early.recheck, moved } } : {}),
  };
}

/**
 * The early breaks a full answer did not withdraw, as items of that answer:
 * what Stop's header counts and keeps out of its clean line (P13, H6). A row
 * part 2 states itself is already its item; a row it called compatible is
 * withdrawn; every other row is added as the early answer stated it.
 */
export function withEarlyBreaks(full: CheckResult, early: CheckResult): CheckResult {
  const compared = compareEarly(early, full);
  const added = compared.rows.filter((c) => c.outcome === "part3_only" && c.row.item).map((c) => c.row.item!);
  if (added.length === 0) return full;
  // A route part 2 did not judge (or holds no row for) gives way to the early break.
  const replaced = new Set(compared.rows.filter((c) => c.outcome === "part3_only" && c.match && "kind" in c.match).map((c) => c.match));
  return { ...full, items: [...(full.items ?? []).filter((item) => !replaced.has(item)), ...added] };
}
