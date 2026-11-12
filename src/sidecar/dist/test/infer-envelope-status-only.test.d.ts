/**
 * Regression for carrick#3741: a client method answers a one-parameter result
 * envelope, `{ success: true; data: T } | { success: false; error: string }`,
 * and the call site reads only the envelope's own status and error members.
 * The consumer row published the envelope as the response type, and judged
 * against a producer that answers `T` it read as a break nobody can prove.
 *
 * The envelope is the client's bookkeeping, not the wire body, and a site
 * that never reads the payload member states nothing about the payload. It
 * abstains with the decision carrick#1375 records for a site that reads only
 * parts of a value (`projected_value_only`), so a sibling site for the same
 * operation can answer.
 *
 * Shape-only: a union whose branches hold the envelope's single type argument
 * as a member. No name, library or package is matched.
 */
export {};
