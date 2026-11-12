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
export interface DispatchUnion {
    /** Members of the sent union. */
    members: number;
    /** Members the expected type takes. */
    agreeing: number;
}
/** The finding for every plan whose compared types have this shape, by pair id. */
export declare function pairDispatchUnions(opened: ProbeProgram | undefined, plans: ProbePlan[]): Map<string, DispatchUnion>;
/** The shape above for one compared pair, or `undefined` when it does not hold. */
export declare function dispatchUnion(sent: ts.Type, expected: ts.Type, checker: ts.TypeChecker, isAssignableTo: (source: ts.Type, target: ts.Type) => boolean): DispatchUnion | undefined;
