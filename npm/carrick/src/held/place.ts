// Part 3, L1: place, map, arm and attribute. Pure functions, no engine and no disk.
//
// A producer response row stores where each reply is sent from (the value
// positions of its entry). To ask the compiler whether an edit breaks that, the
// value is wrapped in place, `__C.send<n>_<g>(<value>, (o) => o)`: the call
// returns the value, so the code around it types as before, and `o` is the
// value on the wire (the judge's own wire rule, pasted from the compiler
// engine) which the callback must return as `O<n>_<g>`, the stated type. `o`
// is no fresh literal, so there is no excess-property check. The check text is
// appended to the file's end (`declare namespace __C`), so no line moves.
//
// Four decisions live here:
//   map       (V5)  where a stored value is on the CURRENT text
//   place           the file with every value wrapped
//   arm       (D6)  which checks may speak: clean passes, its twin fails
//   attribute (R3)  which diagnostics are a check's own, and so may be a line
//
// A value is only ever placed by arithmetic. A value an edit crosses is never
// placed again by a guess at the node it became: its check is dropped
// (`value_edited`) until the text places it by arithmetic again. Nothing here
// parses a value.

import { lineStarts } from "../facts/match.ts";

/** One value's position: `[start, end)` in UTF-16 code units, never counting a leading BOM. */
export type Value = { file: string; start: number; end: number };

/** Why a value has no check. */
export type UnplacedReason = "javascript_file" | "top_type";

/** One changed stretch of text: the old and the new bounds (BOM not counted). */
export type Region = { oldStart: number; oldEnd: number; newStart: number; newEnd: number };

/** A compiler diagnostic: its start offset in the checked text, its code, its message. */
export type Diag = { start: number; code: number; message: string };

export type UnarmedReason = "twin_passes" | "decl_error" | `clean_check_fails:TS${number}`;

/**
 * A GraphQL send-form check (a resolver's returned value, the caller's expected
 * result held fixed). `leaf` is the root field's SDL leaf as JSON (the engine's
 * `write_shadow` answer), which selects the judge's scalar form; null selects
 * its JSON form. Which of the two a root takes is the engine's call, never ours.
 */
export type GraphqlForm = { kind: "graphql"; leaf: string | null };

/**
 * A socket send-form check (an emit's payload, the listener's parameter type
 * held fixed). The callback receives `__carrick_MsgSent<V, O<k>>`, the
 * engine's message rule (`write_shadow`'s `message`). An emit argument is a
 * value, never a promise, so nothing is awaited.
 */
export type MessageForm = { kind: "message" };

/** One (row, position) pair, numbered across the overlay, with one stated type per group. */
export type CheckInput = { n: number; value: Value; groups: Array<{ type: string }>; form?: GraphqlForm | MessageForm };

export type Check = {
  n: number;
  group: number;
  /** `<n>_<group>`; the check's alias is `O<k>`. */
  k: string;
  /** The whole check expression: `__C.send<k>(` through `, (o) => o)`. */
  span: [number, number];
  /** The check's declaration lines in the appended namespace. */
  declSpan: [number, number];
  /**
   * A GraphQL or message form check only: where the callback's body (the `o` of
   * `, (o) => o)`) starts. Its own diagnostics start exactly there (P8).
   */
  callback?: number;
};

export type Overlay = {
  text: string;
  checks: Check[];
  unplaced: Array<{ n: number; reason: UnplacedReason }>;
  /** Offset where the appended namespace starts. */
  bodyEnd: number;
};

const BOM = 0xfeff;
const stripBom = (text: string): string => (text.charCodeAt(0) === BOM ? text.slice(1) : text);

/** One region, from the common prefix and suffix of the two texts. */
export function regionOfEdit(oldText: string, newText: string): Region {
  const a = stripBom(oldText);
  const b = stripBom(newText);
  const limit = Math.min(a.length, b.length);
  let prefix = 0;
  while (prefix < limit && a.charCodeAt(prefix) === b.charCodeAt(prefix)) prefix += 1;
  let suffix = 0;
  while (suffix < limit - prefix && a.charCodeAt(a.length - 1 - suffix) === b.charCodeAt(b.length - 1 - suffix)) suffix += 1;
  return { oldStart: prefix, oldEnd: a.length - suffix, newStart: prefix, newEnd: b.length - suffix };
}

/** The longest edit script, in lines, the line diff searches for; a larger change is one run from its first changed line to its last. */
const MAX_EDIT_LINES = 2000;

/** A run of changed lines: old lines `[a0, a1)` became new lines `[b0, b1)`. */
type Run = [a0: number, a1: number, b0: number, b1: number];

/**
 * The runs of lines that differ between `a` and `b`, in order, from the
 * shortest edit script (Myers' diff over whole lines, after the common first
 * and last lines are cut off).
 */
function changedRuns(a: string[], b: string[]): Run[] {
  let lo = 0;
  while (lo < a.length && lo < b.length && a[lo] === b[lo]) lo += 1;
  let endA = a.length;
  let endB = b.length;
  while (endA > lo && endB > lo && a[endA - 1] === b[endB - 1]) {
    endA -= 1;
    endB -= 1;
  }
  const n = endA - lo;
  const m = endB - lo;
  if (n === 0 && m === 0) return [];
  if (n === 0 || m === 0) return [[lo, endA, lo, endB]];
  const off = n + m + 1;
  const v = new Int32Array(2 * off + 1);
  // `trace[d]` is `v` before step d, for diagonals -d-1 .. d+1 (diagonal k at `k + d + 1`).
  const trace: Int32Array[] = [];
  let reached = false;
  for (let d = 0; d <= Math.min(n + m, MAX_EDIT_LINES) && !reached; d += 1) {
    trace.push(v.slice(off - d - 1, off + d + 2));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[off + k - 1]! < v[off + k + 1]!) ? v[off + k + 1]! : v[off + k - 1]! + 1;
      let y = x - k;
      while (x < n && y < m && a[lo + x] === b[lo + y]) {
        x += 1;
        y += 1;
      }
      v[off + k] = x;
      if (x >= n && y >= m) {
        reached = true;
        break;
      }
    }
  }
  if (!reached) return [[lo, endA, lo, endB]];
  // Walk back from the end: each step of d is one deleted or inserted line, after a run of equal ones.
  const ops: Array<"=" | "-" | "+"> = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d >= 0; d -= 1) {
    const before = trace[d]!;
    const at = (k: number): number => before[k + d + 1]!;
    const k = x - y;
    const prevK = k === -d || (k !== d && at(k - 1) < at(k + 1)) ? k + 1 : k - 1;
    const prevX = at(prevK);
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      ops.push("=");
      x -= 1;
      y -= 1;
    }
    if (d > 0) ops.push(x === prevX ? "+" : "-");
    x = prevX;
    y = prevY;
  }
  ops.reverse();
  const runs: Run[] = [];
  let i = lo;
  let j = lo;
  let open: [number, number] | null = null;
  for (const op of ops) {
    if (op === "=") {
      if (open !== null) runs.push([open[0], i, open[1], j]);
      open = null;
      i += 1;
      j += 1;
      continue;
    }
    if (open === null) open = [i, j];
    if (op === "-") i += 1;
    else j += 1;
  }
  if (open !== null) runs.push([open[0], i, open[1], j]);
  return runs;
}

/**
 * The regions between two texts: one per run of changed lines, each narrowed
 * by common prefix and suffix inside its run (a run replaces whole lines; the
 * change is usually a few characters of them). A value between two runs moves
 * by arithmetic, so an edit above it and an edit below it never cross it,
 * however many edits the two texts are apart.
 */
export function regionsBetween(oldText: string, newText: string): Region[] {
  const a = stripBom(oldText);
  const b = stripBom(newText);
  if (a === b) return [];
  const oldAt = [...lineStarts(a), a.length];
  const newAt = [...lineStarts(b), b.length];
  const lines = (text: string, at: number[]): string[] => at.slice(0, -1).map((start, i) => text.slice(start, at[i + 1]));
  return changedRuns(lines(a, oldAt), lines(b, newAt)).map(([a0, a1, b0, b1]) => {
    const [oldStart, oldEnd, newStart, newEnd] = [oldAt[a0]!, oldAt[a1]!, newAt[b0]!, newAt[b1]!];
    const inner = regionOfEdit(a.slice(oldStart, oldEnd), b.slice(newStart, newEnd));
    return { oldStart: oldStart + inner.oldStart, oldEnd: oldStart + inner.oldEnd, newStart: newStart + inner.newStart, newEnd: newStart + inner.newEnd };
  });
}

/**
 * The text a check appends: the engine's wire declarations and its tolerance
 * helpers (K-a: a receiver declared void states no contract), composed into
 * the one type a check's callback receives. The judge's absent-body rule is
 * not applied: it reads a request, a socket or a pub/sub payload, never an HTTP
 * response (`check-probe.ts`, `absentBodyAllowed`), and this check reads only
 * responses. Nothing here restates a rule: it only names the order the judge
 * applies them in. The literal-widened tolerance reads an export
 * of a scan's stub, so it cannot apply here.
 */
export function checkWire(wire: string[], tolerances: string[]): string[] {
  return [
    ...wire,
    ...tolerances,
    "type __carrick_Sent<S, E> = __carrick_IsVoid<E> extends true ? E : __carrick_Wire<S, E>;",
  ];
}

/** What the judge's bytes gate reads (`IsByteBody`, `check-probe.ts`), by name. */
const BYTE_NAMES = "Blob|ArrayBuffer|SharedArrayBuffer|ArrayBufferView|Buffer|DataView|(?:Big)?(?:Int|Uint|Float)(?:8|16|32|64)(?:Clamped)?Array|ReadableStream(?:<[^>]*>)?";
const TOP_OR_BYTES = new RegExp(`^(?:any|unknown|never|${BYTE_NAMES})$`);

/**
 * A stated type that is `any`, `unknown`, `never` or bytes (alone, or with
 * `null` / `undefined`). The judge reads none of these as a contract
 * (`sent:` and `expected:` gates), and the engine's GraphQL form composes only
 * two of its gates, so a GraphQL check never arms on one: held silence is not
 * compatible, and a held check never speaks on a top type.
 */
export function isTopOrBytes(type: string): boolean {
  let t = type.trim();
  while (t.startsWith("(") && t.endsWith(")")) t = t.slice(1, -1).trim();
  const members = t.split("|").map((m) => m.trim()).filter((m) => m !== "null" && m !== "undefined" && m !== "");
  return members.length > 0 && members.every((m) => TOP_OR_BYTES.test(m));
}

const delta = (r: Region): number => r.newEnd - r.newStart - (r.oldEnd - r.oldStart);

/**
 * V5, the two arithmetic cases. A region meets the value when it touches
 * `[start, end]`. None does: the value shifts by the regions before it. Every
 * one that does lies strictly inside: the value keeps its bounds and grows or
 * shrinks with them. Any other edit (one across a bound) leaves no place a
 * check may be trusted at: `"crossed"`, and the value is not placed.
 */
export function mapValue(value: Value, regions: Region[]): Value | "crossed" {
  const meeting = regions.filter((r) => r.oldStart <= value.end && r.oldEnd >= value.start);
  const before = regions.filter((r) => r.oldEnd < value.start).reduce((sum, r) => sum + delta(r), 0);
  if (meeting.length === 0) return { file: value.file, start: value.start + before, end: value.end + before };
  if (meeting.some((r) => r.oldStart <= value.start || r.oldEnd >= value.end)) return "crossed";
  const within = meeting.reduce((sum, r) => sum + delta(r), 0);
  return { file: value.file, start: value.start + before, end: value.end + before + within };
}

type Op = { at: number; s: string; side: "pre" | "post"; seq: number; start: number; end: number };

const POST = ", (o) => o)";
/** Where the callback's body starts inside `POST`. */
const POST_BODY = POST.indexOf("=> o") + 3;

/**
 * The file with every placeable value wrapped. `wire` is `checkWire`'s text (it holds
 * `__carrick_Sent<S, E>`). `clean` states each
 * group's type; `twin` states it intersected with a property no value has, so
 * it must fail where the check can see the value at all. `wire` is `clean`
 * with a scalar-form GraphQL check's callback reading the judge's wire line
 * (`__carrick_GqlWire`) in place of its declared line: no field the record
 * lacks or converts is read in the type the schema declares. Its body, spans
 * and callbacks are `clean`'s; every other check keeps its `clean` text.
 */
export function overlay(file: string, text: string, checks: CheckInput[], wire: string[], mode: "clean" | "twin" | "wire"): Overlay {
  const unplaced: Overlay["unplaced"] = [];
  const js = /\.(?:[mc]?js|jsx)$/.test(file);
  const bom = text.charCodeAt(0) === BOM ? 1 : 0;
  const ops: Op[] = [];
  const made: Check[] = [];
  const decls: string[] = [];
  let seq = 0;
  for (const input of checks) {
    if (js) { unplaced.push({ n: input.n, reason: "javascript_file" }); continue; }
    const start = input.value.start + bom;
    const end = input.value.end + bom;
    const form = input.form;
    let placed = 0;
    input.groups.forEach((g, group) => {
      if (form && isTopOrBytes(g.type)) return;
      placed += 1;
      const k = `${input.n}_${group}`;
      const stated = mode === "twin" ? `(${g.type}) & { readonly __carrick_never: true }` : `(${g.type})`;
      decls.push(`  type O${k} = ${stated};`);
      if (form?.kind === "message") {
        decls.push(`  function send${k}<V>(v: V, c: (o: __carrick_MsgSent<V, O${k}>) => O${k}): V;`);
      } else if (form) {
        const leaf = form.leaf !== null;
        if (leaf) decls.push(`  type L${k} = ${form.leaf};`);
        const scalar = mode === "wire" ? "__carrick_GqlWire" : "__carrick_GqlSentS";
        const sent = leaf ? `${scalar}<Awaited<V>, O${k}, L${k}>` : `__carrick_GqlSent<Awaited<V>, O${k}>`;
        decls.push(`  function send${k}<V>(v: V, c: (o: ${sent}) => __carrick_GqlExpected<O${k}>): V;`);
      } else {
        decls.push(`  function send${k}<V>(v: V, c: (o: __carrick_Sent<V, O${k}>) => O${k}): V;`);
      }
      ops.push({ at: start, s: `__C.send${k}(`, side: "pre", seq, start, end });
      ops.push({ at: end, s: POST, side: "post", seq, start, end });
      seq += 1;
      made.push({ n: input.n, group, k, span: [0, 0], declSpan: [0, 0], ...(form ? { callback: 0 } : {}) });
    });
    if (form && placed === 0) unplaced.push({ n: input.n, reason: "top_type" });
  }
  // Wraps at one offset must nest. Opening: the wider value first, later group outside.
  // Closing: the mirror image. A close at an offset comes before an open there.
  ops.sort((a, b) => {
    if (a.at !== b.at) return a.at - b.at;
    if (a.side !== b.side) return a.side === "post" ? -1 : 1;
    if (a.side === "pre") return b.end - a.end || b.seq - a.seq;
    return b.start - a.start || a.seq - b.seq;
  });
  let out = "";
  let last = 0;
  const pos = new Map<string, number>();
  for (const o of ops) {
    out += text.slice(last, o.at);
    pos.set(`${o.seq}:${o.side}`, out.length);
    out += o.s;
    last = o.at;
  }
  out += text.slice(last);
  const bodyEnd = out.length;
  out += "\n" + ["declare namespace __C {", ...wire.map((l) => `  ${l}`), ...decls, "}"].join("\n") + "\n";
  made.forEach((c, i) => {
    c.span = [pos.get(`${i}:pre`)!, pos.get(`${i}:post`)! + POST.length];
    if (c.callback !== undefined) c.callback = pos.get(`${i}:post`)! + POST_BODY;
    const declAt = out.indexOf(`  type O${c.k} = `, bodyEnd);
    c.declSpan = [declAt, out.indexOf("\n", out.indexOf(`function send${c.k}<`, declAt))];
  });
  return { text: out, checks: made, unplaced, bodyEnd };
}

const inside = (p: number, s: [number, number]): boolean => p >= s[0] && p <= s[1];
const names = (message: string, k: string): boolean => new RegExp(`\\bO${k}\\b`).test(message);

/**
 * R3. A diagnostic is a check's own line only when its start lies in the
 * check's whole expression AND its message names the check's alias. Several
 * checks can share an expression (two groups of one value, a nest): the
 * narrowest that the message names wins. A diagnostic in a check's declaration
 * lines is that check's `decl_error`. Everything else is `outside`, and never
 * a line.
 */
export function attribute(ov: Overlay, diags: Diag[]): { lines: Map<string, Diag[]>; declErrors: Set<string>; outside: Diag[] } {
  const lines = new Map<string, Diag[]>();
  const declErrors = new Set<string>();
  const outside: Diag[] = [];
  for (const d of diags) {
    const decl = ov.checks.find((c) => inside(d.start, c.declSpan));
    if (decl) { declErrors.add(decl.k); continue; }
    const own = ov.checks
      .filter((c) => (c.callback === undefined ? inside(d.start, c.span) && names(d.message, c.k) : d.start === c.callback))
      .sort((a, b) => a.span[1] - a.span[0] - (b.span[1] - b.span[0]));
    if (own.length === 0) { outside.push(d); continue; }
    const k = own[0]!.k;
    lines.set(k, [...(lines.get(k) ?? []), d]);
  }
  return { lines, declErrors, outside };
}

/** A check "speaks" at the gate when anything but a syntax error (TS1xxx) starts in it. */
function speaks(c: Check, diags: Diag[]): Diag | undefined {
  return diags.find((d) => d.code >= 2000 && inside(d.start, c.span));
}

/**
 * D6. On the current text, a check is armed only when its clean form is
 * silent and its twin speaks. The gate reads the check's span, not the alias
 * name: the twin's message may print the expanded intersection instead of
 * `O<k>`, and the gate must not take that for silence. Only the gate is this
 * broad; a line (see `attribute`) never is.
 */
export function arm(
  ov: Overlay,
  clean: Diag[],
  twin: Diag[],
): { armed: string[]; unarmed: Array<{ k: string; n: number; group: number; reason: UnarmedReason }> } {
  const armed: string[] = [];
  const unarmed: Array<{ k: string; n: number; group: number; reason: UnarmedReason }> = [];
  for (const c of ov.checks) {
    const no = (reason: UnarmedReason): void => { unarmed.push({ k: c.k, n: c.n, group: c.group, reason }); };
    if (clean.some((d) => inside(d.start, c.declSpan))) { no("decl_error"); continue; }
    const bad = speaks(c, clean);
    if (bad) { no(`clean_check_fails:TS${bad.code}`); continue; }
    if (!speaks(c, twin)) { no("twin_passes"); continue; }
    armed.push(c.k);
  }
  return { armed, unarmed };
}
