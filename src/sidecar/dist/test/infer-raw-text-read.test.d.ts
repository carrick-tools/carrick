/**
 * carrick#1842: a consumer that reads the body as raw text states no
 * structural contract, so the inferrer says so beside the `string` it
 * publishes, and the check phase reads that pair unverifiable, as it does a
 * body of bytes (carrick#1812).
 *
 * The published `string` stays: it is what the call site reads. What changes
 * is that the row now carries `raw_text_read`, because `string` alone cannot
 * tell a text read from a JSON body that is a string.
 *
 * Two shapes of a text read, and nothing else:
 *  - a zero-argument `.text()` read of the call's own response, the platform
 *    `Response` here, on its binding or in a `then` callback;
 *  - a call whose resolved signature, at this site, types one member of an
 *    object argument as exactly the literal `'text'` the source passes, which
 *    is how a request library lets a caller choose a text body
 *    (`type: 'text'`), through a wrapper rule or without one.
 *
 * A json read whose type is `string`, a call that returns `string` with no
 * text choice, and a `'text'` member a signature types as a wider union are
 * all left unmarked: none of them is a raw-text read.
 */
export {};
