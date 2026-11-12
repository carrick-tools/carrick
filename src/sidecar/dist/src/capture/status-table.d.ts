/**
 * Response tables keyed by HTTP status code (carrick#1841).
 *
 * An object type whose every key is a status code (`{ 200: Item; 404: Problem
 * }`, or a range key such as `2XX`) is not a body: it is a table of the bodies
 * each status carries. What a caller receives on success is the union of its
 * 2xx values. The producer side reads the same rule off a schema object
 * literal (`successStatusEntry` in the inferrer); this module reads it off a
 * TYPE, through the compiler's own API, so the inferrer, the v1 bundle and
 * the capture all decide a table the same way.
 *
 * Nothing here names a library or a generator. The shape is the HTTP status
 * vocabulary, which is the language of the wire, not of any one client.
 */
import ts from 'typescript';
/** `value` when it is an integer in the HTTP status range, else `undefined`. */
export declare function httpStatus(value: unknown): number | undefined;
/** One verdict for a set of status codes, or `mixed` when they disagree. */
export declare function classifyStatusCodes(codes: number[]): 'success' | 'error' | 'redirect' | 'mixed';
/**
 * The status code a response-table KEY names: `200`, or a range key (`2XX`,
 * `2xx`) read as the bottom of its range. Quotes around the key are ignored.
 * `undefined` for any other key.
 */
export declare function statusKeyCode(name: string): number | undefined;
/** One row of a response table: the key as written and the status it names. */
export interface StatusTableEntry {
    symbol: ts.Symbol;
    /** The key as the type declares it (`200`, `2XX`). */
    key: string;
    code: number;
}
/**
 * The rows of `type` when it is a response table: a plain object type with at
 * least one property, every key a status code, no index signature and no call
 * or construct signature. `undefined` for anything else, so a payload that
 * happens to hold one numeric key among others is never read as a table.
 */
export declare function statusTableOf(checker: ts.TypeChecker, type: ts.Type): StatusTableEntry[] | undefined;
/** The rows of a table that answer a success: 2xx, in the order declared. */
export declare function successEntries(entries: StatusTableEntry[]): StatusTableEntry[];
/**
 * Whether a body type states nothing: `void`, `undefined`, `never`, or a union
 * made only of them. A `204: void` row is such a body.
 */
export declare function statesNoBody(type: ts.Type): boolean;
/**
 * Whether a payload type states no shape (carrick#2116): at its root, or as
 * a member of its root union, it is
 *
 *  - the checker's own empty object type, which is what narrowing `unknown`
 *    yields (`if (cached != null) return cached`). It has no property, no
 *    signature, no index signature and, unlike every `{}` the source writes,
 *    no symbol: an empty object literal carries `__object`, a declared `{}`
 *    carries `__type`, an empty interface carries its own; or
 *  - the `object` keyword type.
 *
 * Printed, the first reads `{}` like an empty object a handler really sends,
 * so only the source program can tell them apart. A reader answers `unknown`
 * for such a type, as it does for one that is `unknown` at its root. A
 * member's own type is not looked at: `meta: {}` stays a member.
 */
export declare function statesNoShape(checker: ts.TypeChecker, type: ts.Type): boolean;
/**
 * What a response table's success rows carry, read for a consumer:
 *  - `body`: the 2xx rows' value types, at least one of which states a body;
 *  - `no_body`: the table has no 2xx row, or every 2xx row states nothing.
 * `undefined` when `type` is not a response table.
 */
export declare function statusTableBody(checker: ts.TypeChecker, type: ts.Type, at: ts.Node): {
    kind: 'body';
    entries: StatusTableEntry[];
    types: ts.Type[];
} | {
    kind: 'no_body';
    entries: StatusTableEntry[];
} | undefined;
