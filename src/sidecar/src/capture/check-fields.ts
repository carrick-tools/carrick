/**
 * Field-level report for a pair the judge called incompatible
 * (carrick-tools/carrick-cloud#1118).
 *
 * `tsc` decides. Its elaboration names ONE field and then elides the rest
 * ("Type 'A' is not assignable to type 'B'. Property 'x' is missing"), which is
 * not enough for a reader deciding what to change: a create endpoint that
 * requires `username` while the client sends `userName` reads as one missing
 * property with no hint that the sent object carries a near-neighbour.
 *
 * This walk enumerates the differing fields of the SAME two types the judge
 * compared, in the SAME program, using the compiler's own assignability
 * relation. It is not a second judge:
 *   - it runs only after the bucket is decided and never changes one;
 *   - it never contradicts: a difference is only named when the checker itself
 *     says the two member types do not assign, and when it finds nothing it
 *     adds nothing and the raw tsc text stands alone;
 *   - it refuses the shapes where a member list is not an account of the type
 *     (a union root, a receiver with an index signature), rather than guessing
 *     about them.
 *
 * It reports one thing the judge structurally cannot: an optionality gap in the
 * direction that still assigns (the sending side always provides a field the
 * receiving side declares optional). That is a real drift between two sources
 * — the receiver carries a branch that never runs — and no assignment error can
 * exist for it. It is stated as an observation beside the verdict, never as the
 * verdict.
 *
 * Seam: node builtins + `typescript` + this bundle only.
 */

import ts from 'typescript';
import type { ProbePlan, Side } from './check-probe.js';
import type { ProbeProgram } from './check-deep.js';

/** What kind of difference one field path carries. */
export type FieldDifferenceNature =
  /** The receiving side declares it; the sending side has no such member. */
  | 'missing_in_sent'
  /** The sending side provides it; the receiving side declares no such member. */
  | 'extra_in_sent'
  /** Optional where it is sent, required where it is read. */
  | 'optional_in_sent'
  /** Always sent, optional where it is read (no assignment error can exist). */
  | 'optional_in_expected'
  /** Both declare it and the member types do not assign. */
  | 'type_differs';

export interface FieldDifference {
  /** Dotted member path from the compared root (`''` is the root itself). */
  path: string;
  nature: FieldDifferenceNature;
  /** Printed member type on the sending side, for `type_differs`. */
  sentText?: string;
  /** Printed member type on the receiving side, for `type_differs`. */
  expectedText?: string;
}

export interface PairFieldReport {
  /** Named differences, capped and ordered deterministically. */
  differences: FieldDifference[];
  /** How many further differences were found beyond the cap. */
  truncated: number;
  /**
   * Whether the sent type's JSON wire form differs from its declared form, so
   * the comparison the reader is being shown is against the serialised shape
   * (a `Date` compared as the string it serialises to).
   */
  wireApplied: boolean;
}

/**
 * Cap on named fields. A mismatch with more differing members than this is
 * better described as two unrelated shapes than as a list, and the text says
 * how many more there are rather than pretending the list is complete.
 */
export const MAX_NAMED_FIELDS = 8;

/** Printed member types are for reading, not for re-parsing. */
const MAX_TYPE_TEXT = 80;

/** Depth cap on the structural descent. Deeper differences are reported at the
 * deepest ancestor the walk reached, never dropped. */
const MAX_FIELD_DEPTH = 4;

/**
 * The compiler's assignability relation — the same one the judge's assignment
 * statement is checked with, which is why this walk cannot disagree with it.
 * Not in the public `TypeChecker` surface (the same standing as the
 * `intrinsicName` and node-builder internals this bundle already depends on).
 * When a compiler build does not expose it the walk reports nothing at all,
 * because every statement it could make without the relation would be a guess.
 */
type CheckerWithRelation = ts.TypeChecker & {
  isTypeAssignableTo?: (source: ts.Type, target: ts.Type) => boolean;
};

function assignabilityOf(
  checker: ts.TypeChecker
): ((source: ts.Type, target: ts.Type) => boolean) | undefined {
  const fn = (checker as CheckerWithRelation).isTypeAssignableTo;
  return typeof fn === 'function' ? fn.bind(checker) : undefined;
}

/**
 * Field reports for every plan whose probe the program could read, keyed by
 * pair id. A plan with no entry has no report, which is not a claim that its
 * types agree.
 */
export function pairFieldReports(
  opened: ProbeProgram | undefined,
  plans: ProbePlan[]
): Map<string, PairFieldReport> {
  const results = new Map<string, PairFieldReport>();
  if (!opened) return results;
  const { program, checker, probesDir } = opened;
  const isAssignableTo = assignabilityOf(checker);
  if (!isAssignableTo) return results;

  for (const plan of plans) {
    const file = program.getSourceFile(
      `${probesDir}/probes/${plan.fileName}`.split('\\').join('/')
    );
    const source =
      file ??
      program
        .getSourceFiles()
        .find((sf) => sf.fileName.endsWith(`/probes/${plan.fileName}`));
    if (!source) continue;
    // Whatever the judge's decisive assignment actually sent: the GraphQL
    // comparand where one exists, then the JSON wire form where one exists.
    // Reading `sent` there instead would describe a type the judge did not
    // compare, which is the one way this walk could contradict it.
    const declared =
      declaredConstType(source, checker, 'sentComparand') ??
      declaredConstType(source, checker, 'sent');
    const expected = declaredConstType(source, checker, 'expected');
    if (!declared || !expected) continue;
    const wire = declaredConstType(source, checker, 'sentWire');
    // Whether serialising changes anything observable about the sent type.
    const wireChanges =
      wire !== undefined &&
      !(
        isAssignableTo(wire.type, declared.type) && isAssignableTo(declared.type, wire.type)
      );
    // Walk the DECLARED type unless serialising really changed it.
    //
    // The probe declares its wire comparand through a conditional alias that
    // short-circuits back to the declared type whenever that already assigns,
    // and a conditional the checker has not had to resolve carries no members
    // to walk. Reading it unconditionally therefore emptied the report on
    // exactly the pairs that AGREE — the ones whose only statement is an
    // optionality gap or the wire note (carrick#1341). Where serialisation
    // did change the type the wire form is a mapped type with real members,
    // and it stays the thing compared, because that is what the judge judged.
    const compared = wireChanges ? wire.type : declared.type;
    const report = diffReport(
      compared,
      expected.type,
      checker,
      isAssignableTo,
      expected.node
    );
    report.wireApplied = wireChanges;
    results.set(plan.pairId, report);
  }
  return results;
}

/** The type of one of the probe's declared consts, read where it is declared. */
function declaredConstType(
  file: ts.SourceFile,
  checker: ts.TypeChecker,
  name: 'sent' | 'sentComparand' | 'sentWire' | 'expected'
): { type: ts.Type; node: ts.Node } | undefined {
  for (const statement of file.statements) {
    if (!ts.isVariableStatement(statement)) continue;
    for (const declaration of statement.declarationList.declarations) {
      if (!ts.isIdentifier(declaration.name) || declaration.name.text !== name) continue;
      const type = checker.getTypeAtLocation(declaration.name);
      if (!type) return undefined;
      return { type, node: declaration.name };
    }
  }
  return undefined;
}

function diffReport(
  sent: ts.Type,
  expected: ts.Type,
  checker: ts.TypeChecker,
  isAssignableTo: (a: ts.Type, b: ts.Type) => boolean,
  at: ts.Node
): PairFieldReport {
  const found: FieldDifference[] = [];
  walk(sent, expected, '', 0, { checker, isAssignableTo, at, found });
  found.sort((a, b) =>
    a.path === b.path ? compareText(a.nature, b.nature) : compareText(a.path, b.path)
  );
  return {
    differences: found.slice(0, MAX_NAMED_FIELDS),
    truncated: Math.max(0, found.length - MAX_NAMED_FIELDS),
    wireApplied: false,
  };
}

function compareText(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

interface WalkContext {
  checker: ts.TypeChecker;
  isAssignableTo: (a: ts.Type, b: ts.Type) => boolean;
  /** Location the member types are read at (the probe's own declaration). */
  at: ts.Node;
  found: FieldDifference[];
}

/**
 * A shape whose members can be compared one by one without diverging from the
 * whole-type relation.
 *
 * The object flag is the first and widest of these: a union or an intersection
 * does not carry it, and neither does a primitive, so a root the judge compared
 * as a whole is never taken apart into members one of its constituents happens
 * to share.
 *
 * An index signature is excluded because it makes the member list an incomplete
 * account of the type: a field the sender provides that the receiver's index
 * signature accepts is not a field the receiver "declares no such field" for,
 * and saying so would be false. Arrays and tuples carry a numeric one, so the
 * same clause keeps `length` and `push` out of a field list; an element
 * difference is reported at the field that holds the array.
 */
function isComparableObject(type: ts.Type, checker: ts.TypeChecker): boolean {
  if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never)) {
    return false;
  }
  if ((type.flags & ts.TypeFlags.Object) === 0) return false;
  if (checker.getIndexInfosOfType(type).length > 0) return false;
  return true;
}

function walk(
  sent: ts.Type,
  expected: ts.Type,
  path: string,
  depth: number,
  ctx: WalkContext
): void {
  const { checker } = ctx;
  if (!isComparableObject(sent, checker) || !isComparableObject(expected, checker)) {
    return;
  }

  const sentProps = new Map(sent.getProperties().map((p) => [p.getName(), p]));
  const expectedProps = new Map(expected.getProperties().map((p) => [p.getName(), p]));
  /** Members the receiver declares and the sender has no member for, optional
   * ones included. Only the REQUIRED ones are a difference; the rest still
   * count here, because a receiver waiting on a member it never gets is what
   * makes a sender-only member worth naming beside it. */
  let absent = 0;

  for (const [name, expectedProp] of expectedProps) {
    const at = join(path, name);
    const sentProp = sentProps.get(name);
    if (!sentProp) {
      absent += 1;
      // An optional member the sender omits is what optional MEANS. Naming it
      // would state that the receiver requires it, which is false, and it is
      // not what the judge rejected the pair for.
      if (!isOptional(expectedProp)) {
        ctx.found.push({ path: at, nature: 'missing_in_sent' });
      }
      continue;
    }
    const sentOptional = isOptional(sentProp);
    const expectedOptional = isOptional(expectedProp);
    if (sentOptional && !expectedOptional) {
      ctx.found.push({ path: at, nature: 'optional_in_sent' });
    } else if (!sentOptional && expectedOptional) {
      // No assignment error exists for this direction, which is exactly why
      // the judge cannot report it and this walk must.
      ctx.found.push({ path: at, nature: 'optional_in_expected' });
    }
    const sentType = memberType(sentProp, ctx);
    const expectedType = memberType(expectedProp, ctx);
    if (ctx.isAssignableTo(sentType, expectedType)) continue;
    const sentInner = checker.getNonNullableType(sentType);
    const expectedInner = checker.getNonNullableType(expectedType);
    if (
      depth + 1 < MAX_FIELD_DEPTH &&
      isComparableObject(sentInner, checker) &&
      isComparableObject(expectedInner, checker)
    ) {
      walk(sentInner, expectedInner, at, depth + 1, ctx);
      continue;
    }
    ctx.found.push({
      path: at,
      nature: 'type_differs',
      sentText: printType(sentType, ctx),
      expectedText: printType(expectedType, ctx),
    });
  }

  // A field the sender provides that the receiver does not declare is normal
  // (a response carrying more than a call site reads), so it is only worth
  // naming beside a member the receiver is waiting on and does not get: that
  // pairing is what a renamed or relocated field looks like from the outside.
  // On the common subset case — a call site reading fewer fields than the
  // producer returns — nothing is absent and nothing is named.
  if (absent === 0) return;
  for (const [name] of sentProps) {
    if (expectedProps.has(name)) continue;
    ctx.found.push({ path: join(path, name), nature: 'extra_in_sent' });
  }
}

function join(path: string, name: string): string {
  return path === '' ? name : `${path}.${name}`;
}

function isOptional(symbol: ts.Symbol): boolean {
  return (symbol.flags & ts.SymbolFlags.Optional) !== 0;
}

function memberType(symbol: ts.Symbol, ctx: WalkContext): ts.Type {
  return ctx.checker.getTypeOfSymbolAtLocation(symbol, ctx.at);
}

function printType(type: ts.Type, ctx: WalkContext): string {
  const text = ctx.checker.typeToString(
    type,
    undefined,
    ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias
  );
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length > MAX_TYPE_TEXT ? `${flat.slice(0, MAX_TYPE_TEXT - 1)}…` : flat;
}

/**
 * The one wording for "this comparison was made against the serialised form",
 * shared by the mismatch diagnostic and the `notes` channel so the two can
 * never drift apart (carrick#1341).
 */
export function wireFormNote(sentSide: Side): string {
  return `The ${sentSide}'s type is compared in the form JSON puts on the wire: a value with a toJSON() method (a Date, for example) travels as what it serialises to.`;
}

/**
 * The statements this report makes that are NOT a mismatch: what belongs on
 * `CheckVerdict.notes` (carrick#1341).
 *
 * Two of the things the walk can find are true of a pair the judge called
 * COMPATIBLE, and so have no mismatch diagnostic to ride on:
 *
 *  - the wire note. Since carrick#1340 a producer `Date` read as a `string` is
 *    not a drift, because that is what arrives. A reader comparing the two
 *    declared shapes by hand sees `Date` against `string` and concludes the
 *    check missed it, so the verdict has to say the comparison was made
 *    against the serialised form.
 *  - an optionality gap (`optional_in_expected`): the sending side always
 *    provides a field the receiving side declares optional. That assigns, so
 *    no diagnostic can exist for it, and the two sources still disagree — the
 *    receiver carries a branch that never runs.
 *
 * Both are OBSERVATIONS. Nothing here is a verdict, nothing here may move one,
 * and this is never a substitute for a mismatch reason: a caller that finds
 * notes on an incompatible row has one statement made twice, not two
 * statements. Returned in the report's own deterministic order.
 */
export function fieldReportNotes(
  report: PairFieldReport,
  sentSide: Side,
  expectedSide: Side
): string[] {
  const notes: string[] = [];
  if (report.wireApplied) notes.push(wireFormNote(sentSide));
  for (const difference of report.differences) {
    if (difference.nature !== 'optional_in_expected') continue;
    notes.push(`${describeDifference(difference, sentSide, expectedSide)}.`);
  }
  return notes;
}

/**
 * The sentence appended to a mismatch diagnostic. Names the two sides as
 * producer and consumer (never the probe's internal sent/expected), so the
 * reader knows which repo to change.
 */
export function describeFieldReport(
  report: PairFieldReport,
  sentSide: Side,
  expectedSide: Side
): string {
  const parts: string[] = [];
  if (report.wireApplied) {
    parts.push(wireFormNote(sentSide));
  }
  if (report.differences.length > 0) {
    const named = report.differences
      .map((d) => describeDifference(d, sentSide, expectedSide))
      .join('; ');
    const more =
      report.truncated > 0
        ? `; and ${report.truncated} further field${report.truncated === 1 ? '' : 's'} differ${report.truncated === 1 ? 's' : ''} (${MAX_NAMED_FIELDS} named here)`
        : '';
    parts.push(`Fields that differ: ${named}${more}.`);
  }
  return parts.length === 0 ? '' : ` ${parts.join(' ')}`;
}

function describeDifference(
  difference: FieldDifference,
  sentSide: Side,
  expectedSide: Side
): string {
  const at = `'${difference.path}'`;
  switch (difference.nature) {
    case 'missing_in_sent':
      return `${at} is required by the ${expectedSide} and the ${sentSide} does not send it`;
    case 'extra_in_sent':
      return `${at} is sent by the ${sentSide} and the ${expectedSide} declares no such field`;
    case 'optional_in_sent':
      return `${at} is optional on the ${sentSide} and required by the ${expectedSide}`;
    case 'optional_in_expected':
      return `${at} is always sent by the ${sentSide} and optional on the ${expectedSide}`;
    case 'type_differs':
      return `${at} is ${difference.sentText} on the ${sentSide} and ${difference.expectedText} on the ${expectedSide}`;
  }
}
