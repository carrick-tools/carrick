/**
 * carrick#1851: a body read taken in place on the call's own value is the
 * call's payload, as the same read on a binding of that value is.
 *
 * `const res = await fetch(url); const body = await res.text()` has always
 * ended the def-use walk on the read. Written without the binding,
 * `const body = await (await fetch(url)).text()`, the declaration's
 * initializer is the read and not the call, so no binding was found, the walk
 * had nothing to follow, and the row published the transport response object.
 *
 * The read is now the terminal where its receiver is the call itself, through
 * the wrappers that leave a value as it is (parentheses, `await`, `!`). What
 * the binding walk does with a read then holds here too: a parser that takes
 * the read, validates it and returns it says more than the read (carrick#1382,
 * carrick#2595),
 * and a text read publishes `string` marked as raw text, which is what keeps
 * it from being judged against a JSON body (carrick#1842).
 *
 * The forms that were already right keep their answers: a cast around the
 * read, an annotated declaration, a returned read. A member that is not a
 * whole-body read is not taken.
 */
export {};
