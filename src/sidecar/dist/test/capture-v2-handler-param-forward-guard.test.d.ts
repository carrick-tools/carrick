/**
 * carrick#770: the capture side of the forward-match guard carrick#766 added
 * to the v1 inferrer.
 *
 * `functionsNearLine` accepts any function starting within two lines of a
 * line-only anchor, in either direction. Looking BACK is free — a function
 * starting before the anchor is one the anchor sits inside or just after.
 * Looking FORWARD is how an anchor reaches a handler that starts a line or two
 * into the registration it names, and it is also how an anchor landing on a
 * statement that declares no function (`export { handler };`) reaches the NEXT
 * declaration in the file.
 *
 * On the v1 side that published a private helper's return type as a route's
 * response contract. Here the exposure is narrower — the caller keeps the first
 * candidate whose PARAMETER NAME resolves, so a neighbour has to declare a
 * parameter of that name to be read at all — but `payload`, `body` and `filter`
 * are exactly the names route helpers share with route handlers. When it
 * happens, the helper's parameter type is published as the route's request
 * contract, silently.
 *
 * The anchor must resolve to the handler or to nothing. Never to the
 * neighbour.
 *
 * Fixtures are synthetic and generically named.
 */
export {};
