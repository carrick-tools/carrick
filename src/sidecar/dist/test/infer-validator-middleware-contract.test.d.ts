/**
 * The request contract of a route whose body is declared by a VALIDATOR
 * MIDDLEWARE, and the guard that stops a callable context member being
 * published as a request contract (carrick#964).
 *
 * A context-object framework (Hono is the one this was found on) gives the
 * handler a single context that both reads the request and sends the response.
 * That context has a `body` MEMBER — the response sender — so the
 * handler-parameter anchor, which reads `body` off each parameter's type,
 * answered the sender's callable type for every route in the service and
 * published it as an explicit request contract.
 *
 * Two things are locked in here:
 *
 *  1. a type with call signatures is never a payload, on any anchor or on the
 *     located expression itself; and
 *  2. when a validator middleware on the registration binds a schema to the
 *     request body, the schema's input IS the request contract, required and
 *     optional members intact (input vs output is locked in by
 *     `infer-schema-direction.test.ts`, carrick#1101).
 *
 * A validator bound to a non-body part declares no request body, and a route
 * that declares nothing publishes no request type, never a framework internal.
 */
export {};
