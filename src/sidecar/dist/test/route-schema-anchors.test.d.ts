/**
 * Route-contract anchors for schema-first services (carrick#528).
 *
 * A route registered as
 *
 *   server.post(path, { schema: { body: $ref('X'), response: { 200: $ref('Y') } } },
 *               async (request: TypedRequest, reply) => controller(request, reply))
 *
 * writes its contract down in two places, and evaluates NEITHER: the handler's
 * parameter annotation and the registration's schema object. The pre-existing
 * request path followed the handler and looked for a typed request READ in its
 * body; a forwarding arrow has none, so it returned null and the endpoint's
 * request type stayed `any`. The response path followed the handler's RETURN
 * type, which for a `reply.send(...)` forwarder is framework machinery, so the
 * response stayed `any` too.
 *
 * These lock the two anchors in, in order, and lock in that the pre-existing
 * typed-read anchor still wins on a route that declares neither.
 */
export {};
