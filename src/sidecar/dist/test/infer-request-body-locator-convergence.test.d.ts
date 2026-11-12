/**
 * Regression for `inferRequestBody` TEXT-LOCATOR convergence.
 *
 * The scanner sends the LLM's `expression_text` + `expression_line` locator
 * (not a byte span) for cross-repo consumer request bodies. Two locator
 * shapes used to resolve the WRONG node and surface the useless `string`
 * result of `JSON.stringify` instead of the payload:
 *
 *  1. A bare property-name locator (`text="body"`) on a `{ body:
 *     JSON.stringify(body) }` property line exact-matches the property NAME
 *     identifier — which types as the assigned value (`string`), not the
 *     payload.
 *  2. A whole-property locator (`text="body: JSON.stringify(body)"`)
 *     resolves the PropertyAssignment node itself, which also types as its
 *     value.
 *  3. A locator on a serialized IDENTIFIER one value-hop from the payload
 *     (`const body = JSON.stringify(payload); sendBeacon(url, body)`)
 *     resolves the argument identifier, whose own declared type is `string`.
 *
 * The fix adds two structural redirects in `inferRequestBody`: (1) a
 * PropertyAssignment / property-name-identifier locator redirects to the
 * property's value; (2) a serialized identifier follows ONE hop to its
 * declaration and, only when that declaration's initializer is itself a
 * `JSON.stringify(...)` call, resolves through to the serialized argument.
 * Cases 4 and 5 are controls: case 4 proves the pre-existing direct-call path
 * still works; case 5 proves the one-hop follow does not fire on a
 * non-stringify initializer.
 */
export {};
