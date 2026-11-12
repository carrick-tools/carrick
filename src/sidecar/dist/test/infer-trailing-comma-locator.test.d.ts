/**
 * Regression for #335: a trailing comma defeats matchByText and the producer
 * response type is captured as the string literal "active".
 *
 * The source `res.json({...})` payload is a multi-line object literal with a
 * trailing comma (`userCount: userCount,`). The LLM's `response_expression_text`
 * is the same expression printed single-line WITHOUT the trailing comma.
 * `normalizeWhitespace` only collapsed whitespace, so:
 *
 *  1. Exact match failed on the comma, for the object literal and for every
 *     enclosing node (`res.json(...)`, the statement, ...).
 *  2. Forward containment (node text includes target) failed the same way.
 *  3. The reverse-substring branch (target includes node text, min length 8)
 *     matched many tiny sub-expressions: `"active"` (8 chars), `userCount`,
 *     `status: "active"`, `new Date().toISOString()`, ...
 *  4. `pickBestMatch` prefers the smallest node, so `"active"` won,
 *     deterministically, and every consumer of the endpoint got a permanent
 *     CAUTION contract risk (`producer "active" vs consumer ...`).
 *
 * The fix strips trailing commas before `}` `)` `]` in the normalization step
 * (applied symmetrically to node text and target, so the exact match succeeds)
 * and requires a reverse-substring match to cover at least half of the target,
 * so a 100+ char locator can never bind to an 8-char fragment.
 */
export {};
