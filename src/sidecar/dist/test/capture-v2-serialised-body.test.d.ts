/**
 * carrick#1162: a consumer request sent as `body: JSON.stringify(payload)` was
 * captured as `string` and judged against the producer's object contract.
 *
 * The v1 inferrer already reads through `JSON.stringify` to the payload. When
 * its text is not usable (a member it could only type as `any`), the alias
 * falls to its capture infer anchor, whose raw locator matched the
 * `JSON.stringify(...)` call and printed the call's own `string` result. A
 * serialised body is the JSON of its argument, so the capture reads the
 * argument, on one line or several.
 *
 * carrick#2010: when the locator's text does not match the source (written on
 * one line, comments dropped), the anchor falls back to the line, and the first
 * node it prefers there is the `body: JSON.stringify(...)` property, whose type
 * is the call's `string`. A property whose value is the serialiser call reads
 * the call's argument too.
 */
export {};
