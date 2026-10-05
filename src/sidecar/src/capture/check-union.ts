/**
 * A sent union whose members the expected type takes only in part, where the
 * rest fail by nothing but the compiler's weak-type check (carrick#1995).
 *
 * A handler that answers a different body per branch (an action that switches
 * on a field of the request) publishes the union of those bodies. A caller of
 * one branch reads the response with a type whose every field is optional. The
 * compiler then rejects each member that shares no field with that type
 * (TS2559, "has no properties in common"): a type whose every property is
 * optional is a "weak type", and the check exists to catch a misspelt property
 * bag. It is not a soundness rule. At runtime every member satisfies a type
 * whose reads all allow absence, and the call only ever receives the body of
 * the branch it asked for, which the types do not say.
 *
 * So the pair is neither compatible nor incompatible: the judge cannot say
 * which member the call receives. Kept narrow on purpose:
 * - the sent type must be a union with at least one member that assigns;
 * - the expected type must be one object type whose every property is
 *   optional, with no index or call signatures;
 * - every member that does not assign must be an object with properties, none
 *   of which the expected type names.
 * A sent type that is not a union and shares no field with a weak expected
 * type is the misspelling the check exists for, and stays a mismatch.
 *
 * Read in the probe program, over the two types the decisive assignment
 * compared, with the compiler's own relation. Seam: typescript + this bundle.
 */

import ts from 'typescript';
import type { ProbePlan } from './check-probe.js';
import type { ProbeProgram } from './check-deep.js';
import { comparedTypes } from './check-fields.js';

export interface DispatchUnion {
  /** Members of the sent union. */
  members: number;
  /** Members the expected type takes. */
  agreeing: number;
}

/** The finding for every plan whose compared types have this shape, by pair id. */
export function pairDispatchUnions(
  opened: ProbeProgram | undefined,
  plans: ProbePlan[]
): Map<string, DispatchUnion> {
  const results = new Map<string, DispatchUnion>();
  if (!opened) return results;
  for (const plan of plans) {
    const pair = comparedTypes(opened, plan);
    if (!pair) continue;
    const finding = dispatchUnion(
      pair.compared,
      pair.expected.type,
      pair.checker,
      pair.isAssignableTo
    );
    if (finding) results.set(plan.pairId, finding);
  }
  return results;
}

/** The shape above for one compared pair, or `undefined` when it does not hold. */
export function dispatchUnion(
  sent: ts.Type,
  expected: ts.Type,
  checker: ts.TypeChecker,
  isAssignableTo: (source: ts.Type, target: ts.Type) => boolean
): DispatchUnion | undefined {
  if (!sent.isUnion()) return undefined;
  const names = weakPropertyNames(expected, checker);
  if (!names) return undefined;
  let agreeing = 0;
  for (const member of sent.types) {
    if (isAssignableTo(member, expected)) {
      agreeing += 1;
      continue;
    }
    if (!sharesNoProperty(member, names, checker)) return undefined;
  }
  if (agreeing === 0 || agreeing === sent.types.length) return undefined;
  return { members: sent.types.length, agreeing };
}

/**
 * The property names of a weak object type: one object type, at least one
 * property, every property optional, no index signature and no call or
 * construct signature. `undefined` for any other type.
 */
function weakPropertyNames(type: ts.Type, checker: ts.TypeChecker): Set<string> | undefined {
  if (!(type.flags & ts.TypeFlags.Object)) return undefined;
  if (
    checker.getSignaturesOfType(type, ts.SignatureKind.Call).length > 0 ||
    checker.getSignaturesOfType(type, ts.SignatureKind.Construct).length > 0 ||
    checker.getIndexInfosOfType(type).length > 0
  ) {
    return undefined;
  }
  const properties = checker.getPropertiesOfType(type);
  if (properties.length === 0) return undefined;
  if (!properties.every((property) => (property.flags & ts.SymbolFlags.Optional) !== 0)) {
    return undefined;
  }
  return new Set(properties.map((property) => property.name));
}

/** Whether `member` is an object with properties, none of them in `names`. */
function sharesNoProperty(
  member: ts.Type,
  names: Set<string>,
  checker: ts.TypeChecker
): boolean {
  if (!(member.flags & ts.TypeFlags.Object)) return false;
  const properties = checker.getPropertiesOfType(member);
  return properties.length > 0 && properties.every((property) => !names.has(property.name));
}
