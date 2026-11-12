/**
 * carrick#2372: a route whose response is read from an executor it shares
 * with other routes, which tell it apart by an argument.
 *
 * `POST /x` calls `execute(input, 'commit')` and `POST /x/preview` calls
 * `execute(input, 'preview')`; a list route calls `read({ name })` and an
 * item route `read({ name, id })`. The executor's returns (or sends) are the
 * union of every caller's answers, and the compiler types every call of a
 * signature that is neither generic nor overloaded the same way. Each route
 * gets the bodies its own arguments leave reachable. Where the arguments
 * cannot tell the bodies apart, the union stays and the row lists the
 * argument as a read no case can be picked by (`response_modes`, location
 * `argument`), so a mismatch against it is not reported as the route's.
 *
 * Every name is a placeholder. The server library is invented; its reply's
 * `send` is a labelled body send.
 */
export {};
