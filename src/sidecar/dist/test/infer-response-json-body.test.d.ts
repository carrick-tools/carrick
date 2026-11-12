/**
 * Regression for carrick#1017: the fetch `Response` wrapper was published as a
 * route's response type.
 *
 * `Response.json(entry)`, `new Response(JSON.stringify(entry))` and
 * `ctx.json(entry)` all evaluate to the platform `Response`. Reading the type
 * of the returned expression therefore answers the transport object —
 * `headers`, `ok`, `redirected`, `status`, `json(): Promise<any>` — and every
 * consumer that reads the real body is told its type is missing those members.
 * The body is in the ARGUMENT of the response call, so that is where it is
 * read from, per branch of a conditional, with a branch whose options object
 * states a >= 400 status dropped as the error body it is.
 *
 * Two halves of the same false mismatch:
 *
 *  (a) PRODUCER. The three body shapes above, plus a route that hands back a
 *      response it did not build — there the body cannot be determined, and
 *      the row must stay unresolved rather than publish the wrapper. A verdict
 *      is impossible either way; a WRONG verdict is not.
 *
 *  (b) CONSUMER. `const res = await fetch(url); if (res.status === 404) return
 *      null; return (await res.json()) as Entry` recorded the expected
 *      response type as `boolean` — the def-use walk ended on the status
 *      comparison beside the call, and the producer's real body was then
 *      reported as not assignable to it.
 *
 * The runtime declarations live where Carrick materialises them for a non-Node
 * runtime (`.carrick/deno/<hash>/runtime.d.ts`), which is the origin that made
 * this live: the machinery gate admits a `lib.*.d.ts` and `node_modules`, and
 * that file is neither, so nothing recognised the platform `Response` at all.
 * No framework name is matched anywhere — only the standard response
 * constructor, the json-body call shape, and Carrick's own artefact path.
 */
export {};
