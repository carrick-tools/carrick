/**
 * carrick#807: a route its module's place in the project states.
 *
 * Such a module exports one handler per HTTP method, and nothing in it
 * registers the route: there is no call to anchor on, so the scan asks at the
 * line the export opens on. The handler takes the platform `Request` and
 * returns the platform `Response`, or a library's subclass of either.
 *
 * Three readings were wrong or missing for that shape, and none of them is
 * particular to one framework:
 *
 *  (a) A redirect whose location is a `URL` object published `string` as the
 *      route's response contract. The payload rule refuses a bare primitive
 *      (a location, a status, a body string) and admitted the same location
 *      handed over as an object, which the JSON wire print then rendered as
 *      the string its `toJSON()` returns.
 *
 *  (b) A handler passed through a wrapper (`export const POST = guarded(async
 *      (request) => ...)`) published the platform's byte stream as its REQUEST
 *      contract. The handler-parameter anchor reads the parameter's `body`
 *      member, which is a contract only where the parameter's type is written
 *      with one. The platform declares `body` as the same stream on every
 *      request, so it states nothing about this route, and it outranked the
 *      body read the model had located.
 *
 *  (c) The request side was asked only where a model located the body read. A
 *      line-only request found no registration and answered nothing, so a
 *      route no model described had no request type at all.
 *
 * The response sender and the request type come from an invented package under
 * `node_modules`, so the origin gate sees an installed dependency as it does in
 * a real tree. The platform types are TypeScript's own `dom` library.
 */
export {};
