/**
 * carrick#1161: a route response is typed from its error or redirect branch
 * when the model's locator points at the first response call.
 *
 * The analyzer reports ONE response expression per row, and on a handler that
 * guards before it succeeds that expression is the guard's body:
 * `return ctx.json({ error: 'q required' }, 400)`. The located expression is
 * then a plain object literal, so the payload walk that drops a >= 400 branch
 * and unions the rest never ran, and the route's contract was published as its
 * 400 body. A redirect's location argument was published as a `string` body the
 * same way.
 *
 * Design: the locator anchors the HANDLER; the sidecar enumerates the handler's
 * response sites, classifies each by the status it states (a literal argument,
 * a literal-typed argument, or the send call's own status type), drops the
 * error and redirect branches, and unions what survives. A status that is not
 * a literal fails open into the union.
 *
 * The same sends are also the carrick#1163 wire rule: a JSON response carries
 * `toJSON()`'s return type, not the object that had one.
 *
 * Every framework name below is a synthetic stand-in; the send calls carry
 * their status as a TYPE, which is the only thing the classification reads.
 */
export {};
