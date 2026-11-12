/**
 * A response read from a function a route calls with its own arguments
 * (carrick#2372).
 *
 * Two routes often share one executor and tell it apart by an argument:
 * `execute(input, 'commit')` for `POST /x` and `execute(input, 'preview')`
 * for `POST /x/preview`, or `read({ id })` for an item route and `read({})`
 * for the list. The executor's sends, or its declared return, are the union
 * of every caller's answers. The compiler gives the same type for every call
 * of a signature that is neither generic nor overloaded, so the route's own
 * answer has to be read from the executor's paths: the tests on a body's
 * path that compare a parameter with a value the route's call states.
 *
 * - **What a call states.** A literal argument (a string, number, boolean,
 *   `null` or `undefined`) states its value; an argument left out states
 *   `undefined` (a parameter with a default states nothing); any other
 *   argument whose type holds neither `null` nor `undefined` states only that
 *   it is defined. A parameter the executor assigns to states nothing, nor
 *   does any parameter of a call with a spread argument. An object literal
 *   argument states each member the same way for a parameter read through a
 *   member access or a destructuring `const` (or a destructured parameter);
 *   a member it leaves out states `undefined` unless the literal spreads
 *   another object.
 * - **What a test decides.** `===`, `==`, `!==` and `!=` between a stated
 *   parameter and a literal, `!`, `&&` and `||`, and the truthiness of a
 *   stated literal. Anything else decides nothing, and a body under a test
 *   that decides nothing is kept.
 * - **Which bodies go.** A body one of whose path tests (an `if` around it,
 *   either arm of a conditional, or an earlier `if` that cannot complete)
 *   decides against the side the body is on. No other body is dropped.
 *
 * `tested` names every stated parameter some test in the executor reads as a
 * mode (a stated literal, or a value stated as defined under a test it
 * decides) and some other call of the executor states differently, whether
 * or not the test decided anything: a union read from such an executor
 * depends on which caller ran it, and where the bodies could not be told
 * apart the caller says so rather than judging the route against every
 * caller's answers. An executor only one call reaches, or whose callers all
 * pass the same value, has no such mode.
 */
import { Node, type CallExpression } from 'ts-morph';
export interface ArgumentNarrowing {
    /** The bodies no stated argument rules out, in the order given. */
    kept: Node[];
    /** Every stated parameter some test in the executor reads, sorted. */
    tested: string[];
}
/** The function a call runs, when it has one body in the program. */
export declare function calledFunction(call: CallExpression): Node | undefined;
/** The one call in `caller` that runs `callee`, or `undefined` for none or several. */
export declare function callInto(caller: Node, callee: Node): CallExpression | undefined;
/**
 * The bodies of `callee` the arguments `call` passes leave reachable. See the
 * module comment for what is read.
 */
export declare function narrowByArguments(callee: Node, call: CallExpression, bodies: Node[]): ArgumentNarrowing;
