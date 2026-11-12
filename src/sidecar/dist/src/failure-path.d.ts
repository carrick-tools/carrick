/**
 * Whether the source reaches a node only when an HTTP response FAILED
 * (carrick#1796).
 *
 * A consumer reads the success body on one path and the error text or the
 * error body on another. Only the first is the call's response contract, so
 * the def-use walk behind a `call_result` row must not take a read from the
 * second. Which path a read is on is told by the source's own tests of the
 * response, read two ways:
 *
 *  - the read sits in a branch of a test: `if (!res.ok) { ... }`, the `else`
 *    of `if (res.ok)`, either arm of a conditional expression;
 *  - the read follows an `if` whose other branch cannot complete, so the rest
 *    of the block runs only on the side that did not leave:
 *    `if (res.ok) { return ... } const text = await res.text()`.
 *
 * A test is read as the set of statuses it lets through. `ok` is true for
 * 200-299 and false for every other status; a comparison of the status with a
 * number lets through what it admits; `!`, `&&` and `||` combine the sets, and
 * any other condition lets everything through. A node is on the failure path
 * only when NO status in 200-299 can reach it.
 *
 * The retype check (retype.ts) reads the same tests through `testsOnPath` and
 * keeps its own policy on top: it also reads the false side of
 * `res.status === 200` as the failure path, so it can find a success read to
 * judge. Here a decided failure REMOVES a read, so the reading has to be sound
 * the other way round: after `if (res.status === 204) return null`, 200 still
 * gets through, and the json read that follows is the payload.
 */
import { Node, type CaseOrDefaultClause, type SwitchStatement } from 'ts-morph';
/** A set of HTTP statuses, as the predicate that admits them. */
export type Statuses = (status: number) => boolean;
/** The statuses `ok` is true for. */
export declare const SUCCEEDED: Statuses;
/** The statuses from 100 to 599 that `statuses` admits, in order. */
export declare function statusesIn(statuses: Statuses): number[];
/** One test of the response on the path to a node, as the side the node is on. */
export interface SideTaken {
    /** Every status that can take this side, and maybe more (see `inexact`). */
    admits: Statuses;
    /**
     * The test reads the response's `ok` or `status` in a form this reading
     * cannot follow (`res.status === OK`, `codes.includes(res.status)`), or
     * combines it with a condition that is not about the response
     * (`res.ok && fresh`). `admits` then lets through more statuses than the
     * side does: still sound for "no success status reaches the node", but no
     * longer only the statuses the source singled out.
     */
    inexact: boolean;
}
/**
 * True when the source reaches `node` only after the response named by
 * `isResponse` failed. The walk climbs from `node` to `boundary` (the function
 * the call sits in) and never looks at a test outside it.
 */
export declare function reachedOnlyOnFailure(node: Node, boundary: Node, isResponse: (node: Node) => boolean): boolean;
/**
 * Each test of the response on the path from `node` up to `boundary` (the
 * whole file when it is `undefined`), as the side `node` is on: a branch of
 * an `if` or a conditional expression it sits in, or an earlier `if` in an
 * enclosing block one of whose branches cannot complete, which leaves the
 * rest of the block to the other side. A test that does not read the
 * response's `ok` or `status` is not listed. A `switch` is not read here:
 * `retype.ts` finds one by its own climb.
 */
export declare function testsOnPath(node: Node, boundary: Node | undefined, isResponse: (node: Node) => boolean): SideTaken[];
/**
 * One test on the path to a node, as the side the node is on. `test` is the
 * statement or expression that tests, so two nodes under the same test share
 * it.
 *
 *  - `branch`: an `if` or a conditional expression, and whether the node is
 *    on its true side: the branch the node sits in or, for an earlier `if` in
 *    an enclosing block one of whose branches cannot complete, the side that
 *    did not leave.
 *  - `clause`: the node sits in this clause of a `switch`.
 *  - `after-switch`: the node follows a `switch` in an enclosing block.
 */
export type PathStep = {
    kind: 'branch';
    test: Node;
    condition: Node;
    whenTrue: boolean;
} | {
    kind: 'clause';
    test: SwitchStatement;
    clause: CaseOrDefaultClause;
} | {
    kind: 'after-switch';
    test: SwitchStatement;
};
/**
 * Every test on the path from `node` up to `boundary` (the whole file when it
 * is `undefined`), whatever it reads, innermost first. `testsOnPath` reads
 * the branches as statuses; `response-modes.ts` reads branches and switches
 * as values of a request field (carrick#2054).
 */
export declare function stepsOnPath(node: Node, boundary: Node | undefined): PathStep[];
/** `node` reads the response's `ok` or `status`, itself or anywhere inside it. */
export declare function readsResponseStatus(node: Node, isResponse: (node: Node) => boolean): boolean;
/**
 * A statement that never runs on into the statement after it: it returns,
 * throws, breaks or continues, or every path through it does. A loop, a
 * `switch` or a `try` is read as one that can complete, which only ever
 * leaves a read on the path it was on.
 */
export declare function cannotComplete(statement: Node): boolean;
