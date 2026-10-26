/**
 * Reading a labelled call's result (carrick#2818).
 *
 * A request library's call rarely returns the response body itself: the body
 * sits in a member of what comes back (`data`), behind a parse method
 * (`json()`), or is the whole value. The library store labels which, per
 * operation, and the scanner sends the label on the consumer's infer anchor.
 * This reads the labelled part of the located call's type, so the contract the
 * consumer states is the body and never the library's envelope.
 *
 * The label is one of:
 *  - `{ whole: true }`: the awaited type;
 *  - `{ member: [m] }`: `m` on every non-nullish constituent of the awaited
 *    type, `undefined` and `null` removed from what is read;
 *  - `{ parse: p }`: the awaited return of calling `p()`, read off the type as
 *    the call returns it (a promise-like with a `json()` method is a thenable,
 *    and awaiting it first would drop the method).
 *
 * A label the type cannot satisfy abstains with `result_member_missing`.
 */

import ts from 'typescript';

/** The wire form of a result label. Exactly one key is set. */
export type ResultLabel =
  | { whole: true }
  | { member: [string] }
  | { parse: string };

export type ResultRead = { type: ts.Type } | { abstain: string };

const NULLISH = ts.TypeFlags.Undefined | ts.TypeFlags.Null | ts.TypeFlags.Void;

/** The constituents of a union, or the type alone. */
export function constituentsOf(type: ts.Type): readonly ts.Type[] {
  return type.isUnion() ? type.types : [type];
}

function missing(what: string): ResultRead {
  return { abstain: `result_member_missing: ${what}` };
}

/**
 * Read `label` off a call's type. `raw` is the type as the call returns it and
 * `awaited` the type after the transport unwrap; `at` is the node properties
 * are instantiated at.
 */
export function readResultLabel(
  checker: ts.TypeChecker,
  raw: ts.Type,
  awaited: ts.Type,
  label: ResultLabel,
  at: ts.Node
): ResultRead {
  if ('whole' in label) return { type: awaited };
  if ('member' in label) return readMember(checker, awaited, label.member[0], at);
  return readParse(checker, raw, awaited, label.parse, at);
}

function readMember(
  checker: ts.TypeChecker,
  awaited: ts.Type,
  name: string,
  at: ts.Node
): ResultRead {
  const read: ts.Type[] = [];
  for (const constituent of constituentsOf(awaited)) {
    if (constituent.flags & NULLISH) continue;
    const property = checker.getPropertyOfType(constituent, name);
    if (!property) return missing(`the labelled member '${name}' is not on every constituent`);
    const held = checker.getTypeOfSymbolAtLocation(property, at);
    for (const part of constituentsOf(held)) {
      if (!(part.flags & NULLISH)) read.push(part);
    }
  }
  if (read.length === 0) return missing(`the labelled member '${name}' holds no value`);
  if (read.length === 1) return { type: read[0] };
  const union = (
    checker as unknown as { getUnionType?: (types: ts.Type[], reduction?: number) => ts.Type }
  ).getUnionType;
  // Without the checker's own union the members cannot be joined honestly;
  // abstaining beats publishing the first of them.
  if (!union) return missing(`the labelled member '${name}' holds several types`);
  return { type: union.call(checker, read) };
}

function readParse(
  checker: ts.TypeChecker,
  raw: ts.Type,
  awaited: ts.Type,
  name: string,
  at: ts.Node
): ResultRead {
  // The method is looked up on the call's own type first: it is the thenable
  // that carries `json()`, and its awaited value does not.
  for (const holder of [raw, awaited]) {
    const method = checker.getPropertyOfType(holder, name);
    if (!method) continue;
    const signature = checker
      .getSignaturesOfType(checker.getTypeOfSymbolAtLocation(method, at), ts.SignatureKind.Call)
      .find((candidate) => candidate.parameters.every((p) => isOptional(p)));
    if (!signature) continue;
    const returned = checker.getReturnTypeOfSignature(signature);
    return { type: checker.getAwaitedType(returned) ?? returned };
  }
  return missing(`the labelled parse method '${name}' is not callable with no arguments`);
}

function isOptional(parameter: ts.Symbol): boolean {
  const declaration = parameter.valueDeclaration;
  return (
    !!declaration &&
    ts.isParameter(declaration) &&
    (declaration.questionToken !== undefined ||
      declaration.initializer !== undefined ||
      declaration.dotDotDotToken !== undefined)
  );
}
