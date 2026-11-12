/**
 * carrick#1433: a member the CHECKER synthesised has no declaration of its own,
 * and `Symbol.getDeclaredType()` answers `any` for such a symbol — so the
 * printer published `{ id: any; createdAt: any }` for a type the compiler had
 * fully resolved, and every row carrying one was demoted for a top type that
 * is not in the contract.
 *
 * A mapped type is where these come from in real code: a query builder's
 * projection, a `Pick`, anything of the `{ [K in Keys]: … }` shape. The test
 * asserts the members really do carry no declaration before asserting the
 * print, because a mapped type that is NOT in that state would exercise the
 * declaration branch instead and pass whatever this one does.
 */
export {};
