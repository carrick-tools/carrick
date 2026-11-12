/**
 * Contract amendment 8 (carrick#2434), seam step SB0-S: a GraphQL root field
 * written as a method decorator. The op's name is `{ bound: 'decorated' }`,
 * the decorated method's own name, or `{ first: [...] }`, the first entry the
 * call fills, read in order. The verifier holds strict D2 on EVERY overload
 * that returns a method decorator, so a no-argument overload never verifies
 * a name that would overwrite an explicit one.
 *
 * Every package below is invented and its declarations are hand-written: one
 * package per overload shape, and one that holds them all.
 */
export {};
