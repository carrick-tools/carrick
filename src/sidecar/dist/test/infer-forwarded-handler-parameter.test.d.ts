/**
 * Regression for carrick-cloud#2576: a route handler that hands its own
 * request and reply objects to a helper (`return finish(request, reply)`) was
 * published with the REQUEST object as its response body.
 *
 * The located response is the helper call. Its result is not data (the helper
 * sends through the reply and hands back what the send returns), so the
 * transitional send drill read the call's first argument as the payload: the
 * handler's request parameter, typed by the framework from the route's type
 * argument (`{ Querystring: ... }`). A compare then reported the request's
 * fields as fields the producer sends.
 *
 * A bare parameter of the handler the route registration hands in is an
 * object the framework passed it (its request, its reply, its context), never
 * a body: a call that forwards one sends through it, and states no body by its
 * own arguments. The row abstains. A parameter of any other function is still
 * a payload, wherever its type is declared: a callback's
 * (`.then((account) => reply.send(account))`) or a helper's
 * (`sendAccount(reply, account: Account)`), typed by the repo or by an
 * installed package. A direct send keeps the type of what it sends.
 *
 * No library, framework or member name is matched.
 */
export {};
