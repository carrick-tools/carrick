/**
 * carrick#1491: an untyped consumer call is judged by retyping it with the
 * producer's response type and reading what the consumer file's own
 * type-check says.
 *
 * The client declared below is hand-written and synthetic: a generic
 * instance whose `post<T>` resolves to an envelope carrying `T` as `data`
 * beside a request-data parameter that defaults to `any`. That default is
 * what leaves both call shapes without a comparable type on a real scan:
 * the untyped call's payload is `any`, and the typed call's envelope carries
 * an `any` beside its payload.
 *
 * The two cases the ticket requires, (a) a typed call and (b) the same call
 * with no type argument, both read `response.data.x`. Against a producer
 * returning `{ y }` both must be flagged at the read; against one returning
 * `{ x }` neither may be. The rest pin the edges: a typed wrapper whose
 * declared return is the sink, a call whose result goes nowhere, an untyped
 * helper with no type parameter, results that escape the file's own
 * type-check, and a producer type the consumer cannot resolve.
 */
export {};
