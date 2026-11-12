/**
 * carrick#2427: a response located at an error-status send is not the route's
 * response. A handler that sends through a parameter (`res.status(409).json(e)`
 * as a statement, nothing returned) has its error sends come first, and the
 * model's locator can name one. The contract is the success body: a 2xx send
 * or one that states no status. Only error sends, plus a 204 or an empty
 * success, is no body at all, never the error body.
 *
 * The status is read from the code (a literal in the send's own chain or
 * argument); a status the source does not fix keeps the located send, as it
 * did before. `res-kit` is a synthetic stand-in: no framework name is read.
 */
export {};
