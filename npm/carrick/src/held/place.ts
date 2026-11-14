// Part 3, T4: place, arm and attribute. Pure functions, no engine and no disk.
//
// A producer response row says "this route's 2xx reply has type T". To ask the
// compiler whether an edit breaks that, the registration call is wrapped in a
// check, `__C.reg<k>(<call>, (o) => o)`, where `o` is the route framework's own
// record of the handler's reply and the callback must return `O<k>`, the row's
// stated type. The check text is appended to the file's end (`declare
// namespace __C`), so no line moves.
//
// Three decisions live here:
//   place     (D7)  which call is the row's site, on the CURRENT text
//   arm       (D6)  which checks may speak: clean passes, its twin fails
//   attribute (R3)  which diagnostics are a check's own, and so may be a line

import ts from "typescript";
import type { ContractGroup, ContractRow } from "../facts/store.ts";

/** What the placer reads of a contract row (a subset of the fact store's row). */
export type PlaceRow = Pick<ContractRow, "label" | "locator"> & {
  id: number;
  site: string | null;
  line: number | null;
  groups: Array<Pick<ContractGroup, "type">>;
};

/** A compiler diagnostic: its start offset in the checked text, its code, its message. */
export type Diag = { start: number; code: number; message: string };

export type UnplacedReason =
  | "not_registration"
  | "dispatch_case"
  | "javascript_file"
  | "no_site_found"
  | "site_not_unique";

export type UnarmedReason = "twin_passes" | "decl_error" | "no_hono" | `clean_check_fails:TS${number}`;

/** `no_hono` is a row's own reason too (the clean check names a non-route object). */
export type RowReason = UnplacedReason | UnarmedReason;

export type Check = {
  id: number;
  group: number;
  /** `<id>_<group>`; the check's alias is `O<k>`. */
  k: string;
  /** The whole check expression: `__C.reg<k>(` through `, (o) => o)`. */
  span: [number, number];
  /** The check's two declaration lines in the appended namespace. */
  declSpan: [number, number];
};

export type Overlay = {
  text: string;
  checks: Check[];
  unplaced: Array<{ id: number; reason: UnplacedReason }>;
  /** Offset where the appended namespace starts. */
  bodyEnd: number;
};

const POST = ", (o) => o)";
const OK = "200 | 201 | 202 | 203 | 204 | 205 | 206 | 207 | 208 | 226";
const PREAMBLE = [
  `  type Ok = ${OK};`,
  "  type Pick2xx<E> = E extends { status: infer St; output: infer O } ? ([Extract<St, Ok>] extends [never] ? never : O) : never;",
  '  type Out<R, P extends string, M extends string> = R extends import("hono").Hono<any, infer S extends import("hono").Schema, any>',
  "    ? (P extends keyof S ? (`$${M}` extends keyof S[P] ? Pick2xx<S[P][`$${M}`]> : { readonly __carrick_no_method_in_route_schema: M })",
  "      : { readonly __carrick_no_site_in_route_schema: P }) : { readonly __carrick_not_a_route_object: R };",
];

const methodOf = (label: string): string => label.split(" ")[0]!.toLowerCase();

const scriptKind = (file: string): ts.ScriptKind => (file.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);

/** One member call: the call node, its method name, and its first argument when that is a plain string. */
type Call = { node: ts.CallExpression; name: string; literal: string | undefined };

function memberCalls(sf: ts.SourceFile): Array<Call & { line: number }> {
  const out: Array<Call & { line: number }> = [];
  const visit = (n: ts.Node): void => {
    if (ts.isCallExpression(n) && ts.isPropertyAccessExpression(n.expression)) {
      const a0 = n.arguments[0];
      const literal = a0 && (ts.isStringLiteral(a0) || ts.isNoSubstitutionTemplateLiteral(a0)) ? a0.text : undefined;
      out.push({ node: n, name: n.expression.name.text, literal, line: sf.getLineAndCharacterOfPosition(n.getStart(sf)).line + 1 });
    }
    ts.forEachChild(n, visit);
  };
  visit(sf);
  return out;
}

/**
 * D7. The call that starts on the row's line, whose callee is the label's
 * method and whose first argument is the row's site literal. With none on that
 * line, the unique such call in the file. Two or more is never a guess.
 */
export function findSite(sf: ts.SourceFile, row: Pick<PlaceRow, "label" | "site" | "line">): { node: ts.CallExpression } | { reason: UnplacedReason } {
  if (row.site === null) return { reason: "no_site_found" };
  const method = methodOf(row.label);
  const same = memberCalls(sf).filter((c) => c.name === method && c.literal === row.site);
  const onLine = same.filter((c) => c.line === row.line);
  if (onLine.length === 1) return { node: onLine[0]!.node };
  if (onLine.length > 1) return { reason: "site_not_unique" };
  if (same.length === 1) return { node: same[0]!.node };
  return { reason: same.length === 0 ? "no_site_found" : "site_not_unique" };
}

type Op = { at: number; s: string; side: "pre" | "post"; seq: number; start: number; end: number };

/**
 * The file with every placeable row's registration wrapped. `clean` states the
 * row's type; `twin` states it intersected with a property no reply has, so it
 * must fail where the check can see the reply at all.
 */
export function overlay(file: string, text: string, rows: PlaceRow[], mode: "clean" | "twin"): Overlay {
  const unplaced: Overlay["unplaced"] = [];
  const js = /\.(?:[mc]?js|jsx)$/.test(file);
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, scriptKind(file));
  const ops: Op[] = [];
  const checks: Array<Check & { span: [number, number] }> = [];
  const decls: string[] = [];
  let seq = 0;
  for (const row of rows) {
    if (row.locator !== "registration") { unplaced.push({ id: row.id, reason: "not_registration" }); continue; }
    if (row.label.includes("{")) { unplaced.push({ id: row.id, reason: "dispatch_case" }); continue; }
    if (js) { unplaced.push({ id: row.id, reason: "javascript_file" }); continue; }
    const found = findSite(sf, row);
    if (!("node" in found)) { unplaced.push({ id: row.id, reason: found.reason }); continue; }
    const start = found.node.getStart(sf);
    const end = found.node.end;
    const method = methodOf(row.label);
    row.groups.forEach((g, group) => {
      const k = `${row.id}_${group}`;
      const stated = mode === "twin" ? `(${g.type}) & { readonly __carrick_never: true }` : `(${g.type})`;
      decls.push(`  type O${k} = ${stated};`);
      decls.push(`  function reg${k}<R>(r: R, c: (o: Out<R, ${JSON.stringify(row.site)}, ${JSON.stringify(method)}>) => O${k}): R;`);
      ops.push({ at: start, s: `__C.reg${k}(`, side: "pre", seq, start, end });
      ops.push({ at: end, s: POST, side: "post", seq, start, end });
      seq += 1;
      checks.push({ id: row.id, group, k, span: [0, 0], declSpan: [0, 0] });
    });
  }
  // Wraps at one offset must nest. Opening: the wider call first, later group outside.
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
  out += "\n" + ["declare namespace __C {", ...PREAMBLE, ...decls, "}"].join("\n") + "\n";
  checks.forEach((c, i) => {
    c.span = [pos.get(`${i}:pre`)!, pos.get(`${i}:post`)! + POST.length];
    const declAt = out.indexOf(`  type O${c.k} = `, bodyEnd);
    c.declSpan = [declAt, out.indexOf("\n", out.indexOf(`function reg${c.k}<`, declAt))];
  });
  return { text: out, checks, unplaced, bodyEnd };
}

const inside = (p: number, s: [number, number]): boolean => p >= s[0] && p <= s[1];
const names = (message: string, k: string): boolean => new RegExp(`\\bO${k}\\b`).test(message);

/**
 * R3. A diagnostic is a check's own line only when its start lies in the
 * check's whole expression AND its message names the check's alias. Several
 * checks can share an expression (two groups of one row, a chain): the
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
      .filter((c) => inside(d.start, c.span) && names(d.message, c.k))
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
): { armed: string[]; unarmed: Array<{ k: string; id: number; group: number; reason: UnarmedReason }> } {
  const armed: string[] = [];
  const unarmed: Array<{ k: string; id: number; group: number; reason: UnarmedReason }> = [];
  for (const c of ov.checks) {
    const no = (reason: UnarmedReason): void => { unarmed.push({ k: c.k, id: c.id, group: c.group, reason }); };
    if (clean.some((d) => inside(d.start, c.declSpan))) { no("decl_error"); continue; }
    const bad = speaks(c, clean);
    if (bad) { no(bad.message.includes("__carrick_not_a_route_object") ? "no_hono" : `clean_check_fails:TS${bad.code}`); continue; }
    if (!speaks(c, twin)) { no("twin_passes"); continue; }
    armed.push(c.k);
  }
  return { armed, unarmed };
}
