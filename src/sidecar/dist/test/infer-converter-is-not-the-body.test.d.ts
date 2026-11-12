/**
 * carrick#2595: a consumer's untyped response handed to a function that
 * builds a new value from it was published as that function's return type.
 * The return type describes what the function built, not the body the call
 * answered with, so the row was concretely wrong where `unknown` was honest.
 *
 * Two readings took such a call as the body:
 *
 *  - a call around the body read whose result is bound to an object
 *    (`const pull = toPull(await response.json())`, carrick#1382);
 *  - the def-use walk over a call whose own value is untyped, which took any
 *    later declaration that used the value whole
 *    (`const raw = await readRaw(); const pull = toPull(raw)`, carrick#1375).
 *
 * Both now take a call only where it states its argument: a schema validates
 * it, or the callee returns it unchanged (through casts) or an empty default.
 * A function declared without a body (`declare`, a `.d.ts`) cannot be read,
 * and a parser and a converter share the signature `(value: unknown) => T`,
 * so its return type is not taken either. No library, method or framework
 * name is matched.
 */
export {};
