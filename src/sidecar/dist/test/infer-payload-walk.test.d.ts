/**
 * carrick#707: the payload-argument walk for file-based route handlers.
 *
 * A file-based route module exports a handler and never annotates its return.
 * The payload either goes straight back or is handed to a serialiser, and the
 * serialiser's result is transport. carrick#631 already reads the serialiser's
 * ARGUMENT when the resolved return carries no contract, but only in two
 * situations: the callee resolves to known transport machinery, or the source
 * annotates the argument. On a bare checkout — what CI scans — neither holds
 * for the common shapes, so the whole endpoint reports `any`.
 *
 * Two gaps, measured on the trig-bench mirror before this change:
 *
 *  (A) `return serialise(payload)` where `serialise` has no installed
 *      declaration. The return is `any`, which says the callee was
 *      unresolvable, not that it was a serialiser, so the unannotated payload
 *      is left behind. A sibling body-plus-status call used to promote it;
 *      carrick#2472 C6d deleted that rule (the compiler resolving the callee,
 *      or a labelled send, replaces it), and the two sibling fixtures below
 *      now pin the honest `any`.
 *
 *  (B) The success payload sits inside a callback the handler hands to the
 *      returned call (`return settle(...).match(ok => serialise(body), err =>
 *      …)`). The walk only reads the handler's own `return` statements, so it
 *      never sees it.
 *
 * Everything here is structural: no framework, package, helper or method name
 * is matched anywhere, and the evidence for (A) comes from the handler's own
 * returned expressions rather than from anything the walk knows about HTTP.
 *
 * The negatives are the point of the design, not decoration:
 *  - a status-stating argument in FIRST position is a payload with a status
 *    field, not a body-plus-status call, and must not promote its callee;
 *  - evidence from a call the handler does not RETURN (a logger) must not
 *    promote its callee;
 *  - a callback return that is not itself a serialiser call must not be read
 *    as the payload, or `return rows.map(r => ({ … }))` reports a row as the
 *    endpoint's response;
 *  - an unresolvable callee with no evidence at all still reports `any`, and
 *    now says why.
 */
export {};
