// Confirm or correct (part 3, B1, pins P9, P10, P12): the one comparison
// between an entry's early answer (the held compiler's, `held/early.ts`) and
// the full re-check's answer for the same entry. Every surface that prints a
// full answer, and the runner and the daemon that write the record, read it
// here.
//
// The join (P9): a route row by (row file, `METHOD path{dispatch}`), never by
// line; a part-2 consumer call row by place identity, its (file, line) being
// one of the early row's group places. An early CALL row (a consumer-side
// item, HM4 D2) joins the other way round: by the call's place (edited file,
// the call's current line), method, response side and label; and a part-2
// route row at one of its producer places is its mirror. What follows from it (P10), per early
// break:
// - part 2 states the same break (same result kind): `same`, nothing more;
// - part 2 calls the row compatible: `differ`, one correction line;
// - part 2 states a break of another kind on the route (a route item of
//   another result, or a consumer call row at one of its places of another
//   result): `differ`, and part 2's row prints (it is a break the early answer
//   lacked, in kind);
// - part 2 states a request-side break on the route: not the same break (the
//   early answer states the response side); it prints and counts as
//   `part2_only`, and the early break stands;
// - part 2 leaves it unresolved, did not judge it, or has no row for it:
//   `part3_only`, and the early break stands (R5a).
// A part-2 break with no early row is `part2_only` and prints as today.

import type { CheckItem, CheckResult, MovedRow } from "../contract.ts";
import { problemItems, statedMoved } from "../contract.ts";
import { engineOfRun } from "../held/early.ts";

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
type RowKind = "route" | "call";

/** One break of the early answer: an item in the edited file or a moved row in another. */
export type EarlyRow = {
  /** Workspace-relative row file and label: the same break under two entries has one key. */
  key: string;
  /** The break as a told key lists it (`breakListed`): row file, label, side and result. */
  listed: string;
  /** The run of the compiler's check it belongs to; empty on a row with none. */
  run: string;
  /** Said at the engine's previous edit answer: not this edit's break. */
  standing: boolean;
  label: string;
  item?: CheckItem;
  moved?: MovedRow;
  places: Place[];
};

/** A call's key carries its line: two calls of one event in one file are two rows. */
const keyOf = (file: string, label: string, line?: number): string => (line === undefined ? `${file}\0${label}` : `${file}\0${label}\0${line}`);

function placeOf(text: string): Place | null {
  const match = /^\S+ (.+):(\d+)$/.exec(text);
  return match ? { file: match[1]!, line: Number(match[2]) } : null;
}

/**
 * A break as the session's told store lists it (T1, D3): the row's
 * workspace-relative file, its label, the side and the result. Never the line
 * or the detail, which move with unrelated edits and differ between the
 * compiler's words and part 2's. A moved row carries no side: it is the
 * response side, as every early row is.
 */
export function breakListed(file: string, label: string, direction: string | null | undefined, result: string | null | undefined): string {
  return `break::${file}::${label}::${direction ?? "response"}::${result ?? ""}`;
}

/** The told key of a break: its listing and the run it belongs to, so a break that was fixed and broke again is told again. */
export const toldBreak = (listed: string, run: string): string => `${listed}::${run}`;

/** The breaks an early answer states, in order. */
export function earlyRows(early: CheckResult): EarlyRow[] {
  const file = early.file ?? "";
  const out: EarlyRow[] = [];
  for (const item of problemItems(early)) {
    const label = itemLabel(item);
    if (label === null) continue;
    const places = (item.counterparts ?? []).flatMap((c) => (c.file && c.line !== undefined ? [{ file: c.file, line: c.line }] : []));
    out.push({
      key: keyOf(file, label, item.kind === "call" ? (item.line ?? 0) : undefined),
      listed: breakListed(file, label, item.direction, item.verdict?.result),
      run: item.run ?? "",
      standing: item.standing === true,
      label,
      item,
      places,
    });
  }
  for (const row of statedMoved(early)) {
    const label = `${row.method} ${row.path}`;
    out.push({
      key: keyOf(row.file, label, row.role === "call" ? (row.line ?? 0) : undefined),
      listed: breakListed(row.file, label, null, row.after),
      run: row.run ?? "",
      standing: row.standing === true,
      label,
      moved: row,
      places: row.counterparts.map(placeOf).filter((p): p is Place => p !== null),
    });
  }
  return out;
}

/**
 * The early rows that say the break `listed` still stands (T1 P1.4): every row
 * the newest ready early answer of a service lists under it. `newest` holds
 * one early answer per service (`newestEarlies`). Two emits of one event in one
 * file share a listing and differ by run, so all are returned. None is no
 * evidence either way.
 */
export function stillStanding(listed: string, newest: Iterable<CheckResult>): EarlyRow[] {
  const out: EarlyRow[] = [];
  for (const early of newest) out.push(...earlyRows(early).filter((r) => r.listed === listed));
  return out;
}

/** The newest early answer of a service, as `standsAsRun` reads it: its rows, and the engine that answered. */
export type NewestEarly = { result: CheckResult; engine?: string | undefined };

/**
 * Whether early row `row` still stands as the same run (T1 P1.4): `newest`, the
 * newest early answer of the row's own service, lists its break with the run it
 * carries. An answer of another engine than the one that spoke the run (a
 * restarted engine re-gates, and a check that speaks is not armed) is no
 * evidence either way (`engineChanged`): nothing prints or counts the break.
 * No answer at all is no evidence it was fixed, and it stands.
 */
export function standsAsRun(row: EarlyRow, newest: NewestEarly | undefined): boolean {
  if (newest?.engine === undefined) return true;
  if (engineChanged(row, newest)) return false;
  return stillStanding(row.listed, [newest.result]).some((r) => r.run === row.run);
}

/** Whether the newest early answer of the row's service came from another engine than the one that spoke the row's run. */
const engineChanged = (row: EarlyRow, newest: NewestEarly | undefined): boolean => newest?.engine !== undefined && engineOfRun(row.run) !== newest.engine;

/**
 * An engine restart leaves an early break with no evidence either way (T4 P4.5):
 * it is not counted, and the route it names is not called unjudged or clean
 * either. `result` without the route items an unsettled break of `earlies`
 * names.
 */
export function withoutUnsettled(result: CheckResult, earlies: Iterable<NewestEarly & { service?: string | undefined }>, newest: ReadonlyMap<string, NewestEarly>): CheckResult {
  const labels = new Set<string>();
  for (const early of earlies) {
    const latest = early.service === undefined ? undefined : newest.get(early.service);
    for (const row of earlyRows(early.result)) if (engineChanged(row, latest)) labels.add(row.label);
  }
  if (labels.size === 0) return result;
  return { ...result, items: (result.items ?? []).filter((item) => !(item.kind === "route" && labels.has(itemLabel(item) ?? ""))) };
}

/**
 * The files (workspace-relative, as an early answer names them) holding an early
 * break that is not provably fixed (T4 P4.2, P4.5): listed by the newest early
 * answer of its service as the same run, or left by another engine's answer.
 * A clean line is never said for such a file.
 */
export function unsettledFiles(earlies: Iterable<NewestEarly & { service?: string | undefined; relative: string }>, newest: ReadonlyMap<string, NewestEarly>): Set<string> {
  const files = new Set<string>();
  for (const early of earlies) {
    const latest = early.service === undefined ? undefined : newest.get(early.service);
    for (const row of earlyRows(early.result)) {
      if (latest === undefined || engineChanged(row, latest) || standsAsRun(row, latest)) {
        files.add(early.relative);
        files.add(row.moved?.file ?? early.relative);
      }
    }
  }
  return files;
}

/**
 * Part 2's breaks with every row the session was already told withheld (T1
 * P1.3): a row is withheld only on evidence, when an early answer still lists
 * it (`stillStanding`) and the told key of each run it lists is in `told`. Every other row
 * prints as it stands. `keys` are the told keys of the rows that print with
 * that evidence, to mark once they are printed. `relative` is the edited
 * file, workspace-relative (an item's `result.file` is its repo's).
 */
export function withoutTold(full: CheckResult, relative: string, newest: Iterable<CheckResult>, told: (key: string) => boolean): { result: CheckResult; keys: string[] } {
  const keys: string[] = [];
  const drop = new Set<CheckItem | MovedRow>();
  const judge = (row: CheckItem | MovedRow, listed: string): void => {
    const runs = [...new Set(stillStanding(listed, newest).map((r) => toldBreak(listed, r.run)))];
    if (runs.length === 0) return;
    // Withheld only when every standing break it may be was told.
    const untold = runs.filter((key) => !told(key));
    if (untold.length === 0) drop.add(row);
    else keys.push(...untold);
  };
  for (const item of problemItems(full)) {
    const label = itemLabel(item);
    if (label !== null) judge(item, breakListed(relative, label, item.direction, item.verdict?.result));
  }
  for (const row of statedMoved(full)) judge(row, breakListed(row.file, `${row.method} ${row.path}`, null, row.after));
  if (drop.size === 0) return { result: full, keys };
  const moved = full.recheck?.moved?.filter((row) => !drop.has(row));
  return {
    result: { ...full, items: (full.items ?? []).filter((item) => !drop.has(item)), ...(full.recheck ? { recheck: { ...full.recheck, ...(moved ? { moved } : {}) } } : {}) },
    keys,
  };
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

/** Whether an early row is a consumer-side call (an item of kind `call`, or a moved row of role `call`). */
const earlyRowKind = (row: EarlyRow): RowKind => (row.item?.kind === "call" || row.moved?.role === "call" ? "call" : "route");

/** The kind of a part-2 row: a check item's `kind`, a moved row's `role`. */
const partKind = (row: CheckItem | MovedRow): RowKind => (("kind" in row ? row.kind : row.role) === "call" ? "call" : "route");

/** The label a correction names: a call's label gains its place, so two calls of one event are told apart. */
function correctionLabel(row: EarlyRow, early: CheckResult): string {
  if (earlyRowKind(row) !== "call") return row.label;
  const at = row.item ? `${early.file ?? ""}:${row.item.line ?? ""}` : `${row.moved!.file}:${row.moved!.line ?? ""}`;
  return `${row.label} at ${at}`;
}

/** The result kind an early row states: `type_mismatch` for an item, the row's `after` for a moved row. */
const earlyKind = (row: EarlyRow): string | null => row.item?.verdict?.result ?? row.moved?.after ?? null;

/** The result kind a part-2 call row states. */
const callKind = (row: CheckItem | MovedRow): string | null => ("kind" in row ? (row.verdict?.result ?? null) : row.after);

/** Part 2's route item is a response-side statement, the side the early answer states. */
const responseSide = (item: CheckItem): boolean => item.direction == null || item.direction === "response";

/** The places of the early rows `rows`, each with the result its row states and the kind of the row. */
const placesOf = (rows: readonly EarlyRow[]): Array<Place & { kind: string | null; rowKind: RowKind }> =>
  rows.flatMap((row) => row.places.map((place) => ({ ...place, kind: earlyKind(row), rowKind: earlyRowKind(row) })));

/**
 * Compare an entry's early answer with its full answer. `full` undefined (or an
 * error) is a full answer that judged nothing: every early row stands, as
 * `absent`.
 */
export function compareEarly(early: CheckResult, full: CheckResult | undefined): Compared {
  const rows = earlyRows(early);
  const usable = full && !full.error ? full : undefined;
  const items = usable?.items ?? [];
  // Part 2 says a break only where the CLI marked it `stated`: a mismatch it
  // holds but does not state (one it reads as model-only, or on main) leaves
  // the early break standing, as an unresolved row does (R5a).
  const moved = usable ? statedMoved(usable) : [];
  const fullFile = usable?.file ?? early.file ?? "";
  // A full answer that ran nothing (deadline, queued behind another check, read model rebuilding, edit not re-read)
  // holds the index's stored verdicts for an older file state: it judged no row, so it corrects none.
  const judged = usable?.recheck?.ran !== "none";
  const matched = new Set<CheckItem | MovedRow>();
  // An early call item's part-2 row: the same place, method and label on the response side.
  const callMatch = (own: CheckItem, label: string): CheckItem | null => {
    const at = items.filter((item) => item.kind === "call" && item.line === own.line && item.method === own.method && responseSide(item) && !matched.has(item) && itemLabel(item) === label);
    return at[0] ?? null;
  };
  let compared: RowCompared[] = rows.map((row) => {
    if (row.item) {
      const match = row.item.kind === "call" ? callMatch(row.item, row.label) : (items.find((item) => item.kind === "route" && itemLabel(item) === row.label && responseSide(item)) ?? null);
      if (match) matched.add(match);
      const verdict = match?.verdict ?? null;
      if (match === null) return { row, outcome: "part3_only", p2_state: "absent", corrected: false, match };
      if (verdict?.state === "unresolved") return { row, outcome: "part3_only", p2_state: "unresolved", corrected: false, match };
      if (judged && verdict?.state === "resolved" && verdict.result === "compatible") return { row, outcome: "differ", corrected: true, match };
      if (match.stated === true && verdict?.result != null && verdict.result !== "compatible") {
        return { row, outcome: verdict.result === row.item.verdict?.result ? "same" : "differ", corrected: false, match };
      }
      return { row, outcome: "part3_only", p2_state: "not_judged", corrected: false, match };
    }
    const own = row.moved!;
    const match =
      own.role === "call"
        ? (moved.find((m) => m.role === "call" && m.method === own.method && `${m.method} ${m.path}` === row.label && !matched.has(m) && samePlace(m.file, m.line, { file: own.file, line: own.line ?? -1 })) ?? null)
        : (moved.find((m) => m.role === "route" && m.file === own.file && `${m.method} ${m.path}` === row.label) ?? null);
    if (match === null) return { row, outcome: "part3_only", p2_state: "absent", corrected: false, match };
    matched.add(match);
    return { row, outcome: match.after === own.after ? "same" : "differ", corrected: false, match };
  });
  const standing = placesOf(compared.filter((c) => !c.corrected).map((c) => c.row));
  const mirrors: Array<CheckItem | MovedRow> = [];
  const part2Only: Array<CheckItem | MovedRow> = [];
  // A call row at a place of an early break is the consumer end of that break
  // only when it states the same result. One of another result is a break the
  // early answer lacked: it prints, and the early row it sits at is `differ`.
  const differing = new Set<EarlyRow>();
  // A part-2 row mirrors only the places of an early row of the opposite kind: a call row the producer's
  // consumers, a route row a call's producers.
  const sort = (row: CheckItem | MovedRow, file: string): void => {
    const opposite = (rowKind: RowKind): boolean => rowKind !== partKind(row);
    const at = standing.filter((place) => opposite(place.rowKind) && samePlace(file, row.line, place));
    if (at.length === 0) return void part2Only.push(row);
    if (at.some((place) => place.kind === callKind(row))) return void mirrors.push(row);
    const flipped = compared.filter((c) => c.outcome === "part3_only" && opposite(earlyRowKind(c.row)) && c.row.places.some((place) => samePlace(file, row.line, place)));
    for (const c of flipped) differing.add(c.row);
    // Counted once: as the early row's `differ`, or as `part2_only` when no early row was left standing at it.
    if (flipped.length === 0) part2Only.push(row);
  };
  for (const item of usable ? problemItems(usable) : []) {
    if (matched.has(item)) continue;
    sort(item, fullFile);
  }
  for (const row of moved) {
    if (matched.has(row)) continue;
    sort(row, row.file);
  }
  compared = compared.map((c) => (differing.has(c.row) ? { ...c, outcome: "differ", p2_state: undefined } : c));
  return { rows: compared, part2Only, mirrors };
}

/** The differ record (P12): one bucket per entry by precedence, the count per bucket, and why part 2 left a break standing. */
export type HeldRecord = { held: HeldBucket; held_rows: Record<Exclude<HeldBucket, "none">, number>; p2_state?: P2State };

const PRECEDENCE: Array<Exclude<HeldBucket, "none">> = ["differ", "part3_only", "part2_only", "same"];

export function heldRecord(compared: Compared): HeldRecord {
  const held_rows = { differ: 0, part3_only: 0, part2_only: compared.part2Only.length, same: 0 };
  // A standing row is an earlier edit's break, already recorded at that edit's entry (T1 P1.2).
  const own = compared.rows.filter((row) => !row.row.standing);
  for (const row of own) held_rows[row.outcome] += 1;
  const held = PRECEDENCE.find((bucket) => held_rows[bucket] > 0) ?? "none";
  const first = own.find((row) => row.outcome === "part3_only");
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
    if (c.corrected) corrected.push(correctionLabel(c.row, early));
  }
  const saidPlaces = placesOf(saidRows.filter((c) => !c.corrected).map((c) => c.row));
  for (const row of compared.mirrors) {
    const file = "kind" in row ? (full.file ?? "") : row.file;
    if (saidPlaces.some((place) => place.rowKind !== partKind(row) && place.kind === callKind(row) && samePlace(file, row.line, place))) drop.add(row);
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
  const standing = new Set(compareEarly(early, full).rows.filter((c) => c.outcome === "part3_only").map((c) => c.row.key));
  return keepRows(early, (row) => standing.has(row.key));
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

/** An early break on a route in another file, as the item Stop counts: it is never listed, only counted and named. */
function movedBreak(row: MovedRow): CheckItem {
  return { kind: row.role, method: row.method, path: row.path, ...(row.role === "call" && row.line !== undefined ? { line: row.line } : {}), direction: "response", verdict: { state: "resolved", result: row.after, detail: row.reason }, stated: true };
}

/**
 * The early breaks a full answer did not withdraw, as items of that answer:
 * what Stop's header counts and keeps out of its clean line (P13, H6). A row
 * part 2 states itself is already its item; a row it called compatible is
 * withdrawn; a row the newest early answer of its service no longer lists as
 * the same run (`newest`, T1 P1.4) was fixed since; every other row is added as
 * the early answer stated it.
 */
export function withEarlyBreaks(full: CheckResult, early: CheckResult, newest?: NewestEarly, counted?: Set<string>): CheckResult {
  const compared = compareEarly(early, full);
  const key = (c: RowCompared): string => toldBreak(c.row.listed, c.row.run);
  const stands = compared.rows.filter((c) => c.outcome === "part3_only" && standsAsRun(c.row, newest));
  // A break an earlier answer of the session already counted (#4900), or part 2 states itself, counts once.
  const kept = stands.filter((c) => !counted?.has(key(c)));
  for (const c of [...stands, ...compared.rows.filter((r) => r.outcome !== "part3_only" && !r.corrected)]) counted?.add(key(c));
  const added = kept.map((c) => c.row.item ?? movedBreak(c.row.moved!));
  // A route part 2 did not judge (or holds no row for) gives way to the early break.
  const replaced = new Set(stands.filter((c) => c.match && "kind" in c.match).map((c) => c.match));
  if (added.length === 0) return full;
  return { ...full, items: [...(full.items ?? []).filter((item) => !replaced.has(item)), ...added] };
}
