/**
 * Whether the retype's "the call's type argument does not reach its result"
 * can be answered before the rewrite for a call whose result stays deferred
 * (carrick#3165, tier 2 of carrick#3158), for every stated type that has no
 * top-level `undefined`.
 *
 * The shape is a generated client's method call: the first type argument is
 * the response map, a later one is the enclosing function's own free type
 * parameter, so the result is `Awaited` of a conditional that stays deferred.
 * The reach test reads the result, its alias argument and the members of its
 * apparent type. Each member is a union of the awaited arms' member types, and
 * holds an `undefined` the stated type never put there, so it is identical to
 * the stated type only when the stated type has a top-level `undefined`. The
 * scanner applies the answer only when the producer's text has none
 * (`states_no_top_level_undefined`).
 *
 * Reads only the consumer's code. Every rule fails closed: a shape this file
 * does not understand answers `false` and the call goes to the full retype,
 * which is unchanged.
 *
 *  T0  the program is strict (`strictNullChecks`): without it `X | undefined`
 *      is `X` and the witness below is empty.
 *  T1  one signature (tier 1's C0) and a written type argument.
 *  T2  the first type parameter is `D` (the stated type). A later one is
 *      `free` (a bare type parameter written at the call) or `closed` (a
 *      written argument, or a default, naming no type parameter).
 *  T3  walk the return type. `D` never sits at a structural position, only
 *      inside the type of a member, which is recorded.
 *  T4  the result is the default library's `Awaited` of a conditional.
 *  T5  every property the test reads has an arm whose declaration names no
 *      `D` and whose type holds `undefined`.
 */
import type { CallExpression } from 'ts-morph';
export declare function deferredResultNeedsUndefined(call: CallExpression): boolean;
