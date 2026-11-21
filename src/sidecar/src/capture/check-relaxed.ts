/**
 * The relaxed re-check (incompatible-rule T1): evidence for whether a pair the
 * judge called `incompatible` differs only by kinds of difference that do not
 * prove a break.
 *
 * It runs AFTER the judge, over the pairs the judge called incompatible and no
 * others, in its own probe files and its own compile. Its only output is
 * `break_evidence` on those verdicts: it never sets a bucket, a code, a
 * diagnostic or a note, and it never makes a pair compatible or incompatible.
 * The relabel that reads the evidence is the scanner's (T3).
 *
 * How the evidence is made. The relaxed probe is the judge's own probe text,
 * byte for byte, with a block appended. The block takes the exact two types
 * the deciding assignment compared (the sent form it assigned, the expected
 * type it assigned to) and assigns them once more for every subset of the
 * relaxations below:
 *
 *   - the sending side through `Firm`: an open `string` or `number` member is
 *     branded as an open value, a generic JSON value as an open JSON value, and
 *     (only under `listed_value`) a literal member as a listed value of its
 *     kind. Optional keys stay optional and `undefined` stays `undefined`;
 *   - the receiving side through `Relax`: every position also accepts the
 *     brands its enabled kinds name, `null` where its kind is enabled, and a
 *     type whose every field is optional also takes any other fields.
 *
 * Required keys stay required and kinds that do not overlap still fail, so a
 * field the receiver requires that the sender never sends, or sends as another
 * kind, still fails every subset.
 *
 * Fail closed. The evidence says "not proven" only when ALL of these hold:
 *   - the second compile ended normally, inside its time limit;
 *   - the judge's own failing assignment fails again in the relaxed file (the
 *     canary: the file was really checked, and checked the same way);
 *   - no line of the appended block raised a diagnostic other than the subset
 *     assignments themselves;
 *   - the empty subset FAILS (the helpers alone change nothing);
 *   - the passing subsets are closed under supersets (a relaxation that
 *     accepts more never accepts less);
 *   - the subset with every relaxation passes.
 * Anything else is `proven: true` with the cause named.
 */
import type { BreakEvidence, NotProvenKind } from './api.js';
import { decisiveAssignmentLine, type ProbePlan } from './check-probe.js';
import { ASSIGNMENT_CODES, type RawDiagnostic } from './check-classify.js';

/**
 * The kinds of difference the relaxed re-check can allow, each separately.
 * The relabel reads them as a table: which kinds it treats as not proven is
 * its own decision, and the evidence holds what each subset proved.
 *
 * - `open_value`: the sender's type is open where the receiver's names fixed
 *   values: a bare `string` or `number` against literals, a generic JSON value
 *   against an object or array shape (rule line R3).
 * - `null_where_optional`: `null` sent where the receiver allows absence
 *   (`string | null` against `x?: string`, R8).
 * - `null_where_required`: `null` sent where the receiver requires a value
 *   (`string | null` against `string`, R9).
 * - `weak_type`: the receiver's type has only optional fields and shares none
 *   with the sender's (an `{ error?: string }` cast, R12).
 * - `listed_value`: the sender's own type lists a value the receiver's does
 *   not (`'a' | 'b' | 'c'` against `'a' | 'b'`, R4).
 */
export const NOT_PROVEN_KINDS: readonly NotProvenKind[] = [
  'open_value',
  'null_where_optional',
  'null_where_required',
  'weak_type',
  'listed_value',
];

/**
 * The kinds the plan treats as not proven (rule lines R3, R8, R9, R12):
 * `proven` and `kinds` read these only. `listed_value` (R4) is in `passing`
 * alone, for a relabel that is ruled to allow it.
 */
export const PLAN_NOT_PROVEN_KINDS: readonly NotProvenKind[] = [
  'open_value',
  'null_where_optional',
  'null_where_required',
  'weak_type',
];

const P = '__rx_';

/** The relaxed re-check's helper types, every name under its own prefix. */
export function relaxedDeclarations(): string[] {
  return [
    `declare const ${P}openS: unique symbol;`,
    `type ${P}OpenS = { readonly [${P}openS]: 'open string' };`,
    `declare const ${P}openN: unique symbol;`,
    `type ${P}OpenN = { readonly [${P}openN]: 'open number' };`,
    `declare const ${P}openJ: unique symbol;`,
    `type ${P}OpenJ = { readonly [${P}openJ]: 'open JSON value' };`,
    `declare const ${P}listedS: unique symbol;`,
    `type ${P}ListedS = { readonly [${P}listedS]: 'listed string' };`,
    `declare const ${P}listedN: unique symbol;`,
    `type ${P}ListedN = { readonly [${P}listedN]: 'listed number' };`,
    // An array is rebuilt as an object of its elements, marked as a list, on
    // both sides: rebuilt arrays are all one type to the compiler, and three
    // of them nested on each side end its comparison unchecked (measured: a
    // field missing under three levels of arrays passed). A tuple is kept as
    // declared on both sides, unrelaxed.
    `declare const ${P}list: unique symbol;`,
    `declare const ${P}listedB: unique symbol;`,
    `type ${P}ListedB = { readonly [${P}listedB]: 'listed boolean' };`,
    // The generic JSON value in its two spellings: object values required (a
    // plain recursive alias) or optional (the mapped `{ [K in string]?: V }`
    // form). A type is a JSON value when it is mutually assignable with either.
    `type ${P}Json = string | number | boolean | null | ${P}Json[] | { [k: string]: ${P}Json };`,
    `type ${P}JsonOpt = string | number | boolean | null | ${P}JsonOpt[] | { [k: string]: ${P}JsonOpt | undefined };`,
    `type ${P}Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;`,
    `type ${P}IsJson<T> = 0 extends 1 & T ? false : [T] extends [never] ? false : ${P}Same<T, ${P}Json> extends true ? true : ${P}Same<T, ${P}JsonOpt>;`,
    ...levels(),
  ];
}

/**
 * How many levels of nesting (an object's fields, an array's elements, an
 * index signature's values) the re-check reads. Whatever the sending side
 * holds below it becomes a marker no receiving type accepts, so a pair that
 * nests deeper (a recursive type, say) reads proven: the re-check never
 * leaves a level to the compiler's own cut-off.
 */
export const RELAXED_DEPTH = 16;

/**
 * The two helpers, one copy per level of nesting. A recursive helper that
 * rebuilds BOTH sides makes the compiler see the same helper three times on
 * each side of its comparison, and past that it assumes the two related
 * without looking (measured: a field missing under three levels of arrays
 * passed). A copy per level is never on the stack twice, so every level is
 * compared for real.
 */
function levels(): string[] {
  const out: string[] = [
    `type ${P}Firm<T, F> = ${P}Firm0<T, F>;`,
    `type ${P}Relax<T, F> = ${P}Relax0<T, F, false>;`,
    // The receiving side's null: `O` says whether the position allows
    // absence (an optional key, or a type holding `undefined`).
    `type ${P}Null<F, O> = O extends true ? ('null_where_optional' extends F ? null : never) : ('null_where_required' extends F ? null : never);`,
    // An object is rebuilt as its index signature, written out, and its
    // named keys: mapping a `Record` rebuilds the `Record` itself, the same
    // type at every level, which ends the comparison unchecked three levels
    // down (measured).
    `type ${P}Known<K> = string extends K ? never : number extends K ? never : K;`,
    `type ${P}Weak<T, F> = 'weak_type' extends F ? ({} extends T ? { [k: string]: unknown } : unknown) : unknown;`,
    `declare const ${P}tooDeep: unique symbol;`,
    `type ${P}Firm${RELAXED_DEPTH}<T, F> = typeof ${P}tooDeep;`,
    `type ${P}Relax${RELAXED_DEPTH}<T, F, O> = T;`,
  ];
  for (let d = 0; d < RELAXED_DEPTH; d++) {
    const firm = `${P}Firm${d + 1}`;
    const relax = `${P}Relax${d + 1}`;
    out.push(
      // The sending side. A JSON value is tested before the union
      // distributes, with `undefined` set aside so an optional JSON field
      // keeps its `?`.
      `type ${P}Firm${d}<T, F> = 'open_value' extends F ? (${P}IsJson<Exclude<T, undefined>> extends true ? ${P}OpenJ | Extract<T, undefined> : ${P}FirmEach${d}<T, F>) : ${P}FirmEach${d}<T, F>;`,
      `type ${P}FirmEach${d}<T, F> = 0 extends 1 & T ? T : T extends string ? ([string] extends [T] ? ('open_value' extends F ? ${P}OpenS : T) : ('listed_value' extends F ? ${P}ListedS : T)) : T extends number ? ([number] extends [T] ? ('open_value' extends F ? ${P}OpenN : T) : ('listed_value' extends F ? ${P}ListedN : T)) : T extends boolean ? ('listed_value' extends F ? ${P}ListedB : T) : T extends (...args: any[]) => any ? T : T extends readonly unknown[] ? (number extends T['length'] ? { readonly [${P}list]: true; readonly [i: number]: ${firm}<T[number], F> } : T) : T extends object ? ([${P}Known<keyof T>] extends [never] ? ${P}Indexed${d}<T, F> : ${P}Indexed${d}<T, F> & { [K in keyof T as ${P}Known<K>]: ${firm}<T[K], F> }) : T;`,
      `type ${P}Indexed${d}<T, F> = string extends keyof T ? { [k: string]: ${firm}<T[string], F> } : number extends keyof T ? { [i: number]: ${firm}<T[number], F> } : unknown;`,
      `type ${P}RelaxIndexed${d}<T, F> = string extends keyof T ? { [k: string]: ${relax}<T[string], F, undefined extends T[string] ? true : false> } : number extends keyof T ? { [i: number]: ${relax}<T[number], F, undefined extends T[number] ? true : false> } : unknown;`,
      `type ${P}Relax${d}<T, F, O> = ${P}RelaxEach${d}<T, F> | ${P}Null<F, O>;`,
      `type ${P}RelaxEach${d}<T, F> = 0 extends 1 & T ? T : T extends string ? T | ('open_value' extends F ? ${P}OpenS : never) | ('listed_value' extends F ? ${P}ListedS : never) : T extends number ? T | ('open_value' extends F ? ${P}OpenN : never) | ('listed_value' extends F ? ${P}ListedN : never) : T extends boolean ? T | ('listed_value' extends F ? ${P}ListedB : never) : T extends (...args: any[]) => any ? T : T extends readonly unknown[] ? (number extends T['length'] ? { readonly [${P}list]: true; readonly [i: number]: ${relax}<T[number], F, undefined extends T[number] ? true : false> } : T) | ('open_value' extends F ? ${P}OpenJ : never) : T extends object ? (([${P}Known<keyof T>] extends [never] ? ${P}RelaxIndexed${d}<T, F> : ${P}RelaxIndexed${d}<T, F> & { [K in keyof T as ${P}Known<K>]: ${relax}<T[K], F, undefined extends T[K] ? true : false> }) & ${P}Weak<T, F>) | ('open_value' extends F ? ${P}OpenJ : never) : T;`
    );
  }
  return out;
}

/** The kinds a subset mask names, in `NOT_PROVEN_KINDS` order. */
export function kindsOfMask(mask: number): NotProvenKind[] {
  return NOT_PROVEN_KINDS.filter((_, i) => (mask & (1 << i)) !== 0);
}

function kindsUnion(mask: number): string {
  const kinds = kindsOfMask(mask);
  return kinds.length === 0 ? 'never' : kinds.map((k) => `'${k}'`).join(' | ');
}

/** One comparison the relaxed block re-runs: the line the judge failed on. */
interface RelaxedComparison {
  /** The judge's own failing line in its probe (and, verbatim, in this file). */
  canaryLine: number;
  sentType: string;
  expectedType: string;
}

export interface RelaxedPlan {
  pairId: string;
  /** Basename of the relaxed probe file. */
  fileName: string;
  source: string;
  /** The first line of the appended block (1-based). */
  fromLine: number;
  canaryLines: number[];
  /** 1-based line of each subset assignment -> its mask, per comparison. */
  subsetLines: Map<number, { comparison: number; mask: number }>;
  comparisons: number;
  /** The judge's own diagnostics on its probe, as `line:code`, sorted: the relaxed compile must raise exactly these above `fromLine`. */
  firstPass: string[];
}

const ASSIGNMENT_RE = /^const (\w+): (\w+) = (\w+);$/;

/** The two types an assignment line of the judge's probe compares. */
function comparisonAt(source: string[], line: number): RelaxedComparison | undefined {
  const m = ASSIGNMENT_RE.exec(source[line - 1] ?? '');
  if (!m) return undefined;
  const [, , expectedType, sentConst] = m;
  const declared = new RegExp(`^declare const ${sentConst}: (\\w+);$`);
  for (const text of source) {
    const d = declared.exec(text);
    if (d) return { canaryLine: line, sentType: d[1], expectedType };
  }
  return undefined;
}

/**
 * Which assignment lines decided an incompatible verdict, mirroring the
 * classifier: the decisive line, or the GraphQL declared line after it when
 * the probe has one. A pair the classifier called incompatible on the
 * declared fallback (the wire form could not be computed) gets none: its
 * sent form is an error type, which would assign to anything.
 *
 * When the probe also reads the producer's unwidened form (carrick#1538) and
 * that block raised nothing but its assignment mismatch, its assignment is a
 * second comparison: both must pass for the pair to be not proven, since the
 * unwidened form states the literals the handler really sends.
 */
export function decidingLines(
  plan: ProbePlan,
  probeDiags: RawDiagnostic[]
): { lines: number[] } | { cause: string } {
  const isAssignment = (line: number) =>
    probeDiags.some((d) => d.line === line && ASSIGNMENT_CODES.has(d.code));
  const decisive = decisiveAssignmentLine(plan);
  if (!isAssignment(decisive)) {
    return {
      cause:
        'the judge compared the declared types because the serialised form could not be computed, so the relaxed form was not built',
    };
  }
  const lines = [plan.declaredAssignmentLine ?? decisive];
  if (plan.unwidenedFromLine !== undefined) {
    const total = plan.source.split('\n').length - 1;
    const unwidenedLine = total; // the unwidened assignment is the probe's last line
    const blockDiags = probeDiags.filter((d) => d.line >= plan.unwidenedFromLine!);
    const onlyItsMismatch =
      blockDiags.length > 0 &&
      blockDiags.every((d) => d.line === unwidenedLine && ASSIGNMENT_CODES.has(d.code));
    if (onlyItsMismatch) lines.push(unwidenedLine);
  }
  return { lines };
}

/**
 * The relaxed probe for one pair the judge called incompatible: its own probe
 * text, then the helpers, then one assignment per subset of the kinds for
 * each deciding comparison. Returns the cause instead when none can be built.
 */
export function buildRelaxedProbe(
  plan: ProbePlan,
  probeDiags: RawDiagnostic[]
): RelaxedPlan | { cause: string } {
  const deciding = decidingLines(plan, probeDiags);
  if ('cause' in deciding) return deciding;
  const original = plan.source.split('\n');
  if (original[original.length - 1] === '') original.pop();
  const comparisons: RelaxedComparison[] = [];
  for (const line of deciding.lines) {
    const c = comparisonAt(original, line);
    if (!c) return { cause: 'the compared types could not be read from the probe' };
    comparisons.push(c);
  }
  const lines = [...original];
  const push = (text: string): number => {
    lines.push(text);
    return lines.length;
  };
  const fromLine = push(`// The relaxed re-check (evidence only; it decides no verdict).`);
  for (const text of relaxedDeclarations()) push(text);
  const all = (1 << NOT_PROVEN_KINDS.length) - 1;
  const subsetLines = new Map<number, { comparison: number; mask: number }>();
  comparisons.forEach((c, i) => {
    push(`type ${P}S${i} = ${c.sentType};`);
    push(`type ${P}E${i} = ${c.expectedType};`);
    push(`type ${P}G${i}_any = Assert<Not<IsAny<${P}Relax<${P}E${i}, ${kindsUnion(all)}>>>>;`);
    push(`type ${P}G${i}_unknown = Assert<Not<IsUnknown<${P}Relax<${P}E${i}, ${kindsUnion(all)}>>>>;`);
    for (let mask = 0; mask <= all; mask++) {
      push(`declare const ${P}s${i}_${mask}: ${P}Firm<${P}S${i}, ${kindsUnion(mask)}>;`);
      const at = push(`const ${P}e${i}_${mask}: ${P}Relax<${P}E${i}, ${kindsUnion(mask)}> = ${P}s${i}_${mask};`);
      subsetLines.set(at, { comparison: i, mask });
    }
  });
  return {
    pairId: plan.pairId,
    fileName: plan.fileName,
    source: lines.join('\n') + '\n',
    fromLine,
    canaryLines: comparisons.map((c) => c.canaryLine),
    subsetLines,
    comparisons: comparisons.length,
    firstPass: diagnosticKeys(probeDiags),
  };
}

/** A pair the relaxed re-check could not decide: proven, with the cause. */
export function provenBecause(cause: string): BreakEvidence {
  return { proven: true, kinds: [], passing: [], cause };
}

/** The cause on a pair the re-check ran on and found a difference that no relaxation clears. */
export const REMAINS = `a difference remains with every relaxation allowed, or the types nest deeper than ${RELAXED_DEPTH} levels`;

/**
 * The evidence for one relaxed probe, read from the diagnostics its compile
 * raised in that file. Fail closed: see the module comment.
 */
export function evidenceFromDiagnostics(relaxed: RelaxedPlan, diags: RawDiagnostic[]): BreakEvidence {
  const above = diagnosticKeys(diags.filter((d) => d.line < relaxed.fromLine));
  if (above.join(',') !== relaxed.firstPass.join(',')) {
    return provenBecause("the relaxed compile did not raise the judge's own diagnostics on the judge's lines");
  }
  for (const line of relaxed.canaryLines) {
    if (!diags.some((d) => d.line === line && ASSIGNMENT_CODES.has(d.code))) {
      return provenBecause("the judge's own mismatch did not recur in the relaxed compile");
    }
  }
  const stray = diags.find((d) => d.line >= relaxed.fromLine && !relaxed.subsetLines.has(d.line));
  if (stray) {
    return provenBecause(`the relaxed re-check raised TS${stray.code} outside its assignments`);
  }
  const all = (1 << NOT_PROVEN_KINDS.length) - 1;
  // A subset passes only when it passes for every comparison.
  const failed = new Set<number>();
  for (const d of diags) {
    const at = relaxed.subsetLines.get(d.line);
    if (at) failed.add(at.mask);
  }
  const passes = (mask: number) => !failed.has(mask);
  if (passes(0)) return provenBecause('the relaxed form changed the comparison with no relaxation allowed');
  for (let mask = 0; mask <= all; mask++) {
    if (!passes(mask)) continue;
    for (let more = mask; more <= all; more++) {
      if ((more & mask) === mask && !passes(more)) {
        return provenBecause('the relaxations disagreed: allowing more failed where allowing less passed');
      }
    }
  }
  if (!passes(all)) return { proven: true, kinds: [], passing: [], cause: REMAINS };
  // The minimal passing subsets: the smallest sets of kinds that, allowed
  // together, clear every comparison. The relabel demotes when one of them
  // lies inside the kinds it treats as not proven.
  const minimal: number[] = [];
  for (let mask = 1; mask <= all; mask++) {
    if (!passes(mask)) continue;
    if (minimal.some((m) => (m & mask) === m)) continue;
    minimal.push(mask);
  }
  // In order of size, then of the kinds' order, so `kinds` is the smallest.
  minimal.sort((a, b) => popcount(a) - popcount(b) || a - b);
  const passing = minimal.map(kindsOfMask);
  // `proven` and `kinds` read the plan's kinds only (incompatible-rule D7):
  // a pair that fits only with a kind outside them, such as a value the
  // producer's own type lists, stays proven, and `passing` keeps the sets.
  const inPlan = minimal.find((m) => (m & PLAN_MASK) === m);
  if (inPlan === undefined) {
    return {
      proven: true,
      kinds: [],
      passing,
      cause: `it fits only with ${kindsOfMask(minimal[0]).join(' and ')} allowed, outside the kinds that prove no break`,
    };
  }
  return { proven: false, kinds: kindsOfMask(inPlan), passing };
}

function diagnosticKeys(diags: RawDiagnostic[]): string[] {
  return [...new Set(diags.map((d) => `${d.line}:${d.code}`))].sort();
}

const PLAN_MASK = NOT_PROVEN_KINDS.reduce(
  (mask, kind, i) => (PLAN_NOT_PROVEN_KINDS.includes(kind) ? mask | (1 << i) : mask),
  0
);

function popcount(n: number): number {
  let c = 0;
  for (let x = n; x; x &= x - 1) c++;
  return c;
}

/**
 * Attach evidence to the judge's verdicts: on `incompatible` verdicts only,
 * and nothing else about any verdict. A verdict with no evidence entry that
 * the judge called incompatible gets `proven: true` with `missing`.
 */
export function withBreakEvidence<V extends { pair_id: string; bucket: string; break_evidence?: BreakEvidence }>(
  verdicts: V[],
  evidence: Map<string, BreakEvidence>,
  missing: string
): V[] {
  return verdicts.map((v) => {
    if (v.bucket !== 'incompatible') return v;
    return { ...v, break_evidence: evidence.get(v.pair_id) ?? provenBecause(missing) };
  });
}
