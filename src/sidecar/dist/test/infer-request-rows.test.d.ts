/**
 * carrick#1166: request rows a route declares only inside its handler body.
 *
 *  1. `schema.safeParse(await c.req.json())` and `schema.parse(body)`: the read
 *     itself is untyped, so the row published `unknown`. The schema that
 *     consumes the read declares what a caller sends, its INPUT (the same
 *     direction every other request anchor reads, carrick#1101).
 *  2. A validated read of a NON-body part (`c.req.valid('param')`) published
 *     the path parameters as the request body. A route whose validator binds
 *     a non-body part and reads nothing else declares no body.
 *  3. A registration with no body read, located by its whole-registration span
 *     in a file without semicolons, published `any`: the statement and its call
 *     share one span, and the tie went to the statement, whose type is `any`.
 *
 * An untyped read that nothing validates keeps its own honest `unknown`.
 */
export {};
