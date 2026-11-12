/**
 * carrick#1493: a consumer that calls `fetch` and reads the body later is
 * judged by retyping the BODY READ with the producer's response type.
 *
 * `fetch` takes no type argument and returns a typed `Response`, so the call
 * the row locates cannot carry the producer's type. The payload is
 * `res.json()` on the binding of that call's result: the retype casts that
 * read to the producer's type (or replaces a cast the source wrote around
 * it) and diffs the file's diagnostics as it does for a typed call.
 *
 * The guards: every body read must be on THIS call's result, the response
 * object must not be handed anywhere its body could be read out of view, the
 * body read must not already state a type, and the body itself must not
 * escape the file's own type-check.
 */
export {};
