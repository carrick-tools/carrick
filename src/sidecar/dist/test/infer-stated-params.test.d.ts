/**
 * carrick#2674: the request and reply bodies a route handler's own parameters
 * state, read through the members the service's server-framework labels name.
 *
 * The server package is invented, from the general shape of two styles of
 * server library:
 * - a keyed route generic: `delete<P extends Parts = Parts>(path, (req:
 *   Req<P>, rep: Rep<P>) => unknown)`, where the request's body member is
 *   `unknown` until the route's type argument fills it, and the reply's send
 *   method takes the route's reply part;
 * - positional request and reply type parameters: `Request2<Params, ResBody,
 *   ReqBody>` and `Response2<ResBody>`, whose send member is a property typed
 *   by a call-signature alias. Beside it sit a send member with a fixed
 *   `(path: string)` payload and one typed by its own type parameter.
 *
 * The labels reach `infer` as the store answers them: a receive op whose
 * `request.body` names the body member, and `send` ops with a payload slot.
 * A member is matched by its declaration, never by its name.
 */
export {};
