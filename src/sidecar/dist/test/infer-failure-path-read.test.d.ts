/**
 * Regression for carrick#1796: a consumer's body read on its failure path was
 * published as the call's response type.
 *
 * The def-use walk behind a `call_result` row takes the LAST whole read of the
 * call's result as the payload. A client that reads the success body inside
 * `if (res.ok) { ...; return ... }` and then reads the error text after it
 * (`const errorText = await res.text()`) published `string` as the response
 * contract. That text is what the server sends when the call FAILED, so it is
 * not the contract, and against a route that answers an object it is a false
 * incompatible.
 *
 * A read the source reaches only when the response failed describes the
 * failure. The walk does not look at it. "Failed" is read off the source's own
 * tests of the response and nothing else: `ok` is false, or the status is
 * outside 200-299. A test is decided only when EVERY status it lets through
 * failed. `res.status === 204` lets 200 through on its false side, so the
 * json read after `if (res.status === 204) return null` stays the payload.
 *
 * The runtime is the compiler's own DOM library, so `Response` is the platform
 * type a client really gets back from `fetch`. No library name is matched.
 */
export {};
