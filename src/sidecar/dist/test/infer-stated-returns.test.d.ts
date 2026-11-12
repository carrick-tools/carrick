/**
 * carrick#3054: the reply a route handler returns, read through the `returns`
 * field of the route's receive op (reply-by-return).
 *
 * Two invented server packages:
 * - `fixture-routes`: `get<R>(path, handler: (req) => R | Promise<R>)`, where
 *   the route's type parameter is whatever the handler returns, plus a
 *   labelled `reply.send` member for a handler that sends instead;
 * - `fixture-typed-routes`: `get<P, Out>(path, h: (c) => Reply<Out>)`, whose
 *   label names the `body` member of `Reply` as the response.
 */
export {};
