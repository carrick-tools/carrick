/**
 * Which incoming-message field picks each member of a response union (carrick#2054).
 *
 * A handler that sends `{ x }` when `?mode=a` and `{ y }` otherwise publishes
 * `{ x } | { y }`. A call that states `mode=a` receives only `{ x }`, and a
 * call that states nothing may receive either. The union alone cannot say
 * which, so this reads it from the handler: for each success body the union
 * joined, the tests on its path that compare a value read from the incoming message
 * with a string literal.
 *
 * The reading, in order:
 *
 *  - A test counts only when it separates two of the joined bodies: one on
 *    each side of it, or, for an earlier `if` that cannot complete, one in
 *    its branch and one after it. A validation guard whose branch only sends
 *    an error separates nothing the join kept, and is ignored.
 *  - A counted test is a message read when a value it tests is read from a
 *    parameter of a function enclosing the send (`messageRead`). A test that
 *    reads nothing from the incoming message (server state, a local count) is ignored,
 *    so a body under it is under no test of the field and stays in every case.
 *  - `===`, `==`, `!==` and `!=` against a string literal, with `!` flipping
 *    the test, and `switch` labels, are read as values. Any other test of a
 *    message read leaves its field read with its values unread.
 *  - `reads` lists every message read among the counted tests. `cases` is
 *    read only for one placed field whose tests were all read: one case per
 *    literal compared, plus "any other value" (`value: null`) when a body
 *    admits one, each the union of the bodies it admits.
 *  - When every case is the same union, the route has no modes.
 *
 * The published union is not changed by any of this.
 */
import { Node, type CallExpression } from 'ts-morph';
/**
 * Where in the incoming message a field is read from. Only the HTTP sources
 * are read so far; another protocol (a socket message, a pub/sub payload)
 * adds its own source here.
 */
export type MessageSource = 'query' | 'body' | 'argument' | 'unplaced';
/** One incoming-message field a response union depends on. */
export interface ResponseModeRead {
    /**
     * `query`: read with `.get(...)` on the default lib's `URLSearchParams`.
     * `body`: a top-level property of the awaited result of a zero-argument
     * `json()` the default lib or an installed library declares. `unplaced`:
     * anything else (a header, a path parameter, the method, a framework's own
     * accessor, a `json()` the repo declares). `argument`: not the message,
     * what the route passes (an argument, or a member of one) to a function
     * other callers also call, which picks that function's body (carrick#2372,
     * `argument-modes.ts`).
     */
    location: MessageSource;
    field: string;
}
/** The union a stated value of the field receives. */
export interface ResponseModeCase {
    /** The value; `null` for any value no other case names. */
    value: string | null;
    type_string: string;
}
export interface ResponseModes {
    /** Every message field the union depends on, sorted by location, then field. */
    reads: ResponseModeRead[];
    /**
     * Present only when `reads` is one placed field and every test of it was
     * read. Sorted by value, the `null` case last.
     */
    cases?: ResponseModeCase[];
}
/** One success body the union joined, in source order. */
export interface JoinedBody {
    node: Node;
    type_string: string;
}
/**
 * The modes of the union joined from `bodies`, or `undefined` when no message
 * read separates them. `join` builds a union text from member texts with the
 * rules the published union is built with. `isBodyRead` says whether a call
 * is the platform's `json()` body read (`platform-body-read.ts`, blueprint
 * row N33).
 */
export declare function readResponseModes(bodies: JoinedBody[], join: (texts: string[]) => string, isBodyRead: (call: CallExpression) => boolean): ResponseModes | undefined;
/** The text of a string literal, `undefined` for anything else. */
export declare function stringLiteral(node: Node | undefined): string | undefined;
