/**
 * Which parts of a call's return could hold the response body the call
 * resolves to (carrick#2817). The compiler decides one case, that the
 * caller supplies the whole return as a type argument (`whole_decided`); for
 * every other return it only lists the candidates: the members that carry a
 * type parameter of the signature, and the parse methods. Choosing among them
 * is the store picker's job, and `holdsResult` judges the choice.
 *
 * Rule R, per signature `S` with type parameters `P`:
 * 1. `A` is the awaited return of `S`. While `A` is a type parameter of `S`
 *    whose default is not `unknown` or `any`, take the default (awaited), at
 *    most four hops: a default of `unknown`/`any` is the caller-supplies-it
 *    case.
 * 2. `whole_decided`: `A` is a type parameter of `S` with no constraint.
 * 3. `members`: across the branches of `A` (a conditional's true and false
 *    types, a union's constituents without `undefined`/`null`/`void`; depth at
 *    most four), every property with no call signature whose declared type
 *    mentions a type parameter in `P`.
 * 4. `parse`: properties of `S`'s raw return or of a branch of `A` that can
 *    be called with no required argument and whose awaited return is a bare,
 *    unconstrained type parameter of that method's own signature.
 * A signature that returns nothing (void, undefined, never) has no candidates.
 */
import { ts } from 'ts-morph';
import type { ResultShape, SurfaceResultCandidates } from './types.js';
export declare function resultCandidatesOf(checker: ts.TypeChecker, signature: ts.Signature): SurfaceResultCandidates | undefined;
/**
 * Rule R over a return type read against `parameters`: the type parameters
 * that stand for the caller's value. A handler a route takes returns a type
 * the route's own parameters decide (carrick#3047), so those are the ones
 * its return is read against.
 */
export declare function resultCandidatesOfReturn(checker: ts.TypeChecker, returned: ts.Type, parameters: readonly ts.Type[]): SurfaceResultCandidates | undefined;
/** Whether `shape` names one of `candidates`. `whole` is always one; a decided whole is the only one. */
export declare function holdsResult(candidates: SurfaceResultCandidates, shape: ResultShape): boolean;
