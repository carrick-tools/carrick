/**
 * carrick#3047 (H3 of the HTTP slice), label restructure amendment 7
 * (carrick#2434), part 3: the verifier judges `form: set` (a send), `new`
 * (a send, a request or a receive: carrick-cloud#3139), `decorate` (a
 * receive, and a scope) and the
 * `returns` field of a receive op. `override`, and `set` on a receive, stay
 * `part_unbuilt`. Absorbs carrick#3022's verifier half (`set`).
 *
 * Every package below is invented and its declarations are hand-written.
 */
export {};
