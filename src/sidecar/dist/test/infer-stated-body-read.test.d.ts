/**
 * Regression for carrick#1749: two consumer call sites recorded an expected
 * response type that is not the body the caller reads.
 *
 *  (a) The source casts the body read to a wrapper (`res.json() as
 *      Promise<{ members: Member[] }>`) and the model named the element
 *      (`Member`). The inference reads the cast correctly, but nothing told
 *      the scanner that the text was the source's own statement, so the
 *      model's symbol won and the pair read incompatible. The inference now
 *      says so (`stated_body`), with the name the statement is rooted at:
 *      none for an object literal type, `Member` for `Promise<Member>`.
 *
 *  (b) The call starts a chain, and a callback in it is handed the body
 *      unread and casts it (`.mapOk(response => { const data = response as
 *      SearchResponse; ... })`). The call sits inside a `return`, so the walk
 *      answered the value the chain ends in, which is what the caller
 *      computed from the body. The cast is the body read.
 *
 * Nothing here matches a library or a method name. The chain is "member calls
 * whose receiver is the previous call"; the body read is "a callback's first
 * parameter, typed `unknown` by the compiler, cast by the source", and it
 * answers only when exactly one exists in the chain.
 */
export {};
