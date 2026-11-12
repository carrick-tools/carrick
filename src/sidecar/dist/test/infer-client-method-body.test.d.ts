/**
 * carrick#2628: a call into a client method that builds the request inside
 * itself.
 *
 * A table of tools (closed by `as const`) holds handlers that call methods
 * of an in-repo client: `client.addTags(project, unique(tags))`. The method
 * wraps its argument before it sends it, `body: { tags }`. The consumer row
 * sits at the handler's call, so the request locator names the call's
 * argument and the response locator names the call.
 *
 * - The argument is not the body: the method's parameter is not a body slot,
 *   and the method sends `{ tags: string[] }`, not `string[]`. The request is
 *   the body the method builds, or unresolved when it cannot be told.
 * - A handler that returns its own object built around the call's result
 *   (`return { ok: true, count: count(await client.add(...)) }`) returns the
 *   handler's value, not the HTTP response. The response is the call's own
 *   result.
 */
export {};
