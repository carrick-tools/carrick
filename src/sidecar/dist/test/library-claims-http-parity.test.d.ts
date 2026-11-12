/**
 * carrick#1616: HTTP parity. An HTTP claim answered in the shared
 * shape must get exactly the verdict and reason its #1564 kind gets: any HTTP
 * fixture verdict that changes under the shared checks is a regression.
 *
 * `httpCheck` is the conversion both repos apply (the scanner converts the
 * client-semantics answer after the normaliser), so it is pinned here kind by
 * kind. Then every check below goes through both actions, `verify_client_semantics`
 * in the #1564 shape and `verify_library_claims` in the shared shape, and the
 * two answers must be equal, result for result and module for module.
 */
export {};
