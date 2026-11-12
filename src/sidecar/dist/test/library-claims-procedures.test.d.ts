/**
 * carrick#2474 R6a: procedure-router fields on `verify_library_claims`
 * (contract amendment 3 of 2026-10-23 on carrick#2434). Each field is checked
 * against the package's own declarations, one valid claim and one invalid
 * claim per field:
 * - `collects` on a scope: the router member takes an object whose keys
 *   name procedures and nested routers;
 * - `name: { bound: 'key' }` on a receive op: the procedure is named by the
 *   key it sits under, and the label states its method;
 * - `merges` on a scope (amendment 9): the member takes, as a rest, the
 *   routers a verified `collects` scope returns;
 * - an adapter, a maker whose `runs` slot takes the router a `collects`
 *   scope returns, with its endpoint at `prefix`.
 *
 * The packages are invented and hand-written from the general shape of a
 * procedure kit (a kit made by `init.create()`, a procedure builder whose
 * `query` and `mutation` members register a resolver, a `router` member
 * taking an object of procedures, a fetch-style adapter taking the router
 * and an endpoint) and of a second package that ships a middleware adapter
 * for it, never copied from a real one. Claims with none of these fields are
 * pinned unchanged by `library-claims.test.ts`, `library-claims-v2.test.ts`
 * and `library-claims-routes.test.ts`.
 */
export {};
