/**
 * carrick#2282: a call inside a function that sits inside a returned
 * expression was typed as that expression.
 *
 * The call-result walk asks first whether the call is returned, and it looked
 * for the nearest `return` above the call. That search does not stop at a
 * function boundary. A request made in a callback, where the callback is part
 * of what an OUTER function returns, was answered with the outer function's
 * returned expression: a command table, a client object, a UI element.
 *
 * A return statement answers only for a call of the function it returns from.
 *
 * Nothing here matches a name or a library: the rule is the language's own
 * scoping of `return`.
 */
export {};
