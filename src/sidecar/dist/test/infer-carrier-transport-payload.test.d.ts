/**
 * Regression for carrick#1841: a consumer call whose result carrier holds a
 * request library's own response object published that object as the
 * response contract.
 *
 * The client below returns `Task<Outcome<Reply<T>, WireError>>`, and the
 * service has rules for what a result holds (`Reply`, `Envelope`) and none
 * for the thenable or the outcome around it. The carrick#1376 carrier read
 * peels those two by shape and finds what the success side holds,
 * `Reply<T>`: the library's status, url and headers around the body, not the
 * body. Judged against a producer's body, every one of those members read as
 * one the producer does not send.
 *
 * The carrier's payload is offered to the service's rules before it is
 * published. A rule that verifies it as the library's transport object and
 * reads no payload out of it makes the site decide it states no contract
 * (`machinery_envelope` at the root, no anchor). A rule that does read a
 * payload out of it publishes that payload: a shape printed with its own
 * members, or the `string` of a body read as text, marked as raw text
 * (carrick#1842).
 *
 * `Reply` is a type alias, so its rule read no payload out of it until
 * carrick#1843, and the text read below abstained with the json one. A
 * service whose rules name all three wrappers never reaches the carrier
 * read: test/infer-alias-wrapper-rules.test.ts.
 */
export {};
