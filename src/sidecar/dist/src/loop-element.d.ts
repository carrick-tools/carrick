/**
 * A route registered once per element of a constant list (carrick#1029).
 *
 * ```ts
 * for (const [path, key] of [['a', 'alpha'], ['b', 'beta']] as const) {
 *   app.get(`/items/${path}`, async () => ({ items: rows()[key] }));
 * }
 * ```
 *
 * One handler serves two operations, `/items/a` and `/items/b`. Typed as
 * written, `key` is `'alpha' | 'beta'`, so each operation's response is the
 * union of both payloads, and a consumer of `/items/a` reads as mismatched.
 *
 * The scanner sends the row's own path (`route_path`) on a route row's
 * request when its line serves more than one operation. This reads, from the
 * syntax and the compiler's symbols only, which element of the list the path
 * names, and returns the rewrite that makes the list that one element. The
 * inferrer then types the same handler in the rewritten file: the compiler
 * infers the response for that element, and no rule of ours decides it.
 *
 * Read only when every piece is stated:
 *
 * - the registration's path argument is a template whose every hole is a name
 *   one loop binds: a `for (const … of LIST)` or `LIST.forEach((…) => …)`
 *   around the registration, binding a name or an array pattern of names;
 * - `LIST` is an array literal (through parentheses, `as const` and
 *   `satisfies`), or a `const` bound to one that nothing in its file mutates,
 *   or that is `as const`;
 * - every element is a string, or an array of strings at the positions the
 *   holes read, with no spread;
 * - exactly one element's path is the row's path, segment by segment from
 *   the end, a parameter matching a parameter of any name.
 *
 * Anything else returns `undefined`, and the request is typed as before.
 */
import { Node, type SourceFile } from 'ts-morph';
/** The rewrite that leaves only the picked element in the list. */
export interface LoopPick {
    file: SourceFile;
    /** The list expression's span in the file's original text. */
    start: number;
    end: number;
    /** What replaces it. Holds no more newlines than the text it replaces. */
    text: string;
    /** Which element, of how many. */
    index: number;
    of: number;
}
/**
 * The element of the list `registration` is made once for that `routePath`
 * names, or `undefined` when the code does not state it.
 */
export declare function loopElementPick(registration: Node, routePath: string): LoopPick | undefined;
/**
 * Whether `full` ends with `own`, segment by segment: the row's path holds
 * the route's own path after whatever prefix its mounts add.
 */
export declare function endsWithPath(full: string, own: string): boolean;
