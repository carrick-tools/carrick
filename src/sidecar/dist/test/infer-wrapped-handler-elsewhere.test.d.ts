/**
 * carrick#2472 P3-2b: a route whose handler is declared in another file, as
 * a name bound to a wrapper call, behind middleware.
 *
 * `router.route('/items').post(auth, audit, createItem)` registers three
 * functions. The handler is the last of them; the others are middleware. The
 * inferrer read the first function argument, so the route's request and
 * response were the middleware's. And `createItem` is not a function
 * literal: it is `wrap<CreateItem>(async (req, res) => …)`, a call whose value
 * is callable and which is handed exactly one function. The inferrer did not
 * follow that call, so the handler resolved to nothing and the capture
 * re-read the registration line and served the router.
 *
 * Now the handler is the last argument that resolves to a function, and a
 * name bound to such a wrapper call resolves to the one function it is
 * handed, in whatever file it is declared. The wrapper's type argument types
 * that function's parameters, so the route's request is `CreateItem['Body']`
 * and a response it returns is read off it.
 */
export {};
