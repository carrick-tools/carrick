/**
 * carrick#827, the producer side: a route whose handler reads nothing from
 * the incoming message published a request type.
 *
 * The scanner locates a producer's request body at the whole registration
 * call (`app.post(path, handler)`). Where the handler declares no request
 * contract and holds no typed read, the inferrer answered nothing. That
 * answer is not a decision, so the capture re-read the same span and
 * published the registration call's own type: `void` wherever the server
 * library's types resolve. The route then states a request type, and every
 * call to it is checked against it.
 *
 * Where the handler reads nothing from its parameters at all, the inferrer
 * now decides: this route takes no request body. Each use of a parameter is a
 * call made on it whose value is discarded or returned (`reply.json(result)`,
 * `return c.json(result)`), which is a send.
 *
 * A handler that reads anything from a parameter keeps the undecided answer
 * it had: a property read, a zero-argument call it uses the value of, or the
 * parameter handed whole to another function may each be the body read, and
 * this cannot tell. So may a call on a parameter that is handed something
 * able to receive (a callback, or an object with a method: `request.pipe(dest)`,
 * `request.on('data', cb)`, `request.json().then(cb)`). A handler with no
 * parameter proves nothing either. No method or property name is consulted.
 */
export {};
