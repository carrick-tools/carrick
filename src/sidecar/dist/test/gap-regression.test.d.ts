/**
 * Gap-regression tests for the type inferrer.
 *
 * Each test replays a request shape the Rust orchestrator actually sends
 * (see file_orchestrator.rs `push_infer`: text locator from the LLM,
 * span locator from the SWC candidate, or line-only anchors for
 * file-based routes) against test/fixtures/sample-repo/src/gap-cases.ts
 * and asserts the *correct* behavior:
 *
 * 1. An identifier locator with a drifted line must fail (→ unknown
 *    downstream) rather than substring-bind to a different identifier
 *    and return a confidently wrong type.
 * 2. A span locator pointing at the endpoint *registration* call (the
 *    fallback when the LLM emits no payload expression) must not type
 *    the route path literal as the response payload.
 * 3. Line-anchored `function_return` requests — the only shape sent for
 *    file-based routes (Next.js app router etc.) — must be accepted, not
 *    rejected by the protocol validator.
 * 4. One invalid item in an infer batch must not poison the whole batch.
 * 5. A union of Promises must unwrap per member, not produce a mangled
 *    type string.
 */
export {};
