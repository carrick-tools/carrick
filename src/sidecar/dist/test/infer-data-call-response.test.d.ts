/**
 * Regression for carrick#2055 (cause A): a route whose located response is a
 * call that PRODUCES data was published with what went into the call.
 *
 * The response read keeps a transitional drill for a located send
 * (`res.json(users)` -> `users`). Two gates decided when it ran, and both read
 * a data call as a send:
 *
 *  - a call handed an inline function was taken for a route registration, so
 *    `rows.map((r) => ({ ... }))` published the callback's return: the element,
 *    not the list;
 *  - a call whose element type is declared in a package was taken for a
 *    library describing itself, so `rows.map(toDto)` published the mapper's
 *    function type and `listItems(ownerId)` published `string`.
 *
 * A call whose result is plain data (an object with no callable member, or an
 * array of one, once promise levels and `null`/`undefined` are taken off) is
 * the payload, whatever its arguments and wherever its type is declared.
 * Everything else keeps its path: a send whose result is a library builder
 * with methods, a send whose result is `void`, a registration whose result
 * carries the handler, and a call whose result is a primitive (a serialiser).
 *
 * No library, framework or method name is matched.
 */
export {};
