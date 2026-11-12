/**
 * carrick#2011: a consumer's request body built from literals is published
 * widened (`provider: string`), because a literal widens in an object
 * property. The call only ever sends the literal, so a producer that accepts
 * a literal union for that field accepts every value the call sends.
 *
 * The request inference carries, beside the published type, the body read
 * again with its literals kept (`unwidened_type_string`), as a response
 * inference does (carrick#1516). The judge reads it only to clear a mismatch
 * the published type raised.
 *
 * What a request's reading marks differs from a handler's in one way: a
 * literal handed to some other call (`useState('open')`, the `method: 'POST'`
 * option) is not marked. Keeping it narrow would narrow that call's generic
 * (the state's setter would accept only `'open'`), and the code that calls
 * the setter elsewhere, in a file the guard never checks, could send another
 * value. A literal inside the sent expression, or in a `const` it reads, is
 * marked.
 *
 * Shapes:
 * - a literal written in the body passed to `JSON.stringify`;
 * - a literal in an object sent as the call's argument directly;
 * - a `const` chosen by a conditional of two literals, then sent;
 * - a parameter declared `string`: nothing to mark, no reading;
 * - a state whose initial value is a literal handed to a generic call, written
 *   in the call or through a `const`: no reading;
 * - a body and a response read in one batch, in one function and in another
 *   function of the same file: the responses' rewrite comes first and must not
 *   cost the body its reading (review of PR #2710);
 * - an object built with a literal and then assigned another value: no
 *   reading (the guard sees the assignment).
 */
export {};
