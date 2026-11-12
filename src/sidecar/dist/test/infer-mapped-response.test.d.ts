/**
 * Regression for carrick#1732: a route that sends a MAPPED object was
 * published with the type of the row it was mapped from.
 *
 * The handler loads a database row and sends `toPublicView(row, ...)`, a
 * function in the same repo that builds a new object: dates as ISO strings,
 * big integers as strings, private columns left out. The model located that
 * call as the response expression, which is right. The response-body path
 * then treated every located call as a send it had to look inside
 * (`res.json(users)` -> `users`) and published the call's FIRST ARGUMENT, the
 * raw row. A consumer that reads the mapped shape was then judged against
 * columns the route never sends.
 *
 * A call whose result is a payload-shaped value that is not a library's own
 * object IS the payload, whoever declares the callee. The look-inside
 * fallback stays for what it was for: a send the transport types do not
 * describe (its result is `void`, `any` or `unknown`), and a call that hands
 * back a library's own object (a reply builder, a codec's writer).
 *
 * No framework or library name is matched: the rule reads what the call
 * evaluates to and where that type is declared.
 */
export {};
