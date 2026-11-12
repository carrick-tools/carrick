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
export type ResultLabel = {
    whole: true;
} | {
    member: [string];
} | {
    parse: string;
};
export type ResultRead = {
    type: ts.Type;
} | {
    abstain: string;
};
/** The constituents of a union, or the type alone. */
export declare function constituentsOf(type: ts.Type): readonly ts.Type[];
/**
 * Read `label` off a call's type. `raw` is the type as the call returns it and
 * `awaited` the type after the transport unwrap; `at` is the node properties
 * are instantiated at.
 */
export declare function readResultLabel(checker: ts.TypeChecker, raw: ts.Type, awaited: ts.Type, label: ResultLabel, at: ts.Node): ResultRead;
