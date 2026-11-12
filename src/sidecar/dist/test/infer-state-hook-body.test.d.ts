/**
 * A request body field fed from a UI state hook holding a literal union, which
 * a bench run reported as judged `string` against a producer declaring the
 * union (carrick-tools/carrick-cloud#1119, third finding).
 *
 * Two shapes, because they have different right answers and only one of them
 * would be a defect:
 *  - the state type STATED at the hook call must survive into the body, so a
 *    producer declaring the same union agrees;
 *  - the state type left to inference is `string` by the compiler's own rule
 *    (an unconstrained type parameter widens a literal argument), and serving
 *    that is correct, not a collapse.
 */
export {};
