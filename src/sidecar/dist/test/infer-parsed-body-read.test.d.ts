/**
 * Regression for carrick#1382: a json body read overrode the parse applied to
 * it, so a consumer that validates what it reads published `any`.
 *
 * `const parsed = parseEnvelope(await response.json())` states the payload
 * twice: once as the untyped read, once as the parser's declared result. The
 * def-use walk visits candidates in pre-order, so the `VariableDeclaration`
 * for `parsed` is reached BEFORE the identifier `response` inside its own
 * initializer — the body-read branch then overwrote the better answer with the
 * `any` of `response.json()`.
 *
 * The body read stays the floor it was built to be (carrick#1017): a cast
 * around it is still read off it, and a body read whose value goes nowhere
 * useful keeps its own answer. What changed is that a call which CONSUMES the
 * body read, whose own result the source binds, and which states its
 * argument (a schema, or a function that returns it unchanged) states the
 * payload more precisely, so that call's result wins. A call that builds a
 * new value from the body is a converter, and stays out (carrick#2595).
 *
 * Nothing here names a library or a validator: the shape is "the body read is
 * an argument of a call whose result is bound, whose type is an object, and
 * that returns its argument unchanged", which is a shape of the language.
 */
export {};
