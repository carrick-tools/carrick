/**
 * carrick#1841: a response table keyed by HTTP status code is not a body.
 *
 * A generated client types each operation by two tables, `{ 200: Item }` for
 * what a success carries and `{ 404: Problem }` for what a failure carries,
 * and answers `({ data: Item; error: undefined } | { data: undefined; error:
 * Problem }) & { request; response }`. The union is the client's bookkeeping.
 * What crossed the wire on success is the success table's body, `Item`.
 *
 * Two places publish that bookkeeping today, and both are fixed by one rule:
 * an object type whose every key is a status code is a response table, and
 * its body is its 2xx values.
 *
 *  - The inferred call result. The union carries no type arguments of its
 *    own; they are on the call (`client.get<Table, Errors>(…)`) or on the
 *    alias the wrapping function's declared return type was written with.
 *  - A symbol the model named for a consumer response (`GetItemResponses`):
 *    the v1 bundle and the capture both publish the table's body, not the
 *    table (see bundle-status-table.test.ts and
 *    capture-v2-status-table-symbol.test.ts).
 *
 * The fixture is a hand-written client of that shape with neutral names.
 */
export {};
