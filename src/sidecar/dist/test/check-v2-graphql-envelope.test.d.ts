/**
 * v2 check — GraphQL resolver-return envelope unwrap (end-to-end, real
 * vendored pnpm + tsc).
 *
 * A GraphQL producer's captured type is the resolver's RETURN type with
 * transport layers already peeled (`Promise<ApiResponse<Order>>` ->
 * `{ data: Order; errors: string[] }`), while the consumer's expectation is
 * the SDL field payload its selection set reads (`OrderView`). Comparing the
 * envelope raw was the corpus-1 `graphql|query|order` false-incompatible
 * ("missing the following properties from type 'OrderView': id, total"); the
 * probe now unwraps an unambiguous single-payload envelope for graphql pairs
 * (v1 ts_check `unwrapGraphqlPayload` parity, ported type-level).
 *
 * Pins the fix AND its fail-closed edges: a real field mismatch under the
 * same envelope stays incompatible, a bare payload with an optional-vs-
 * required widening (the corpus-1 subscription shape) stays incompatible
 * with its field-level diagnostic, an ambiguous envelope is never unwrapped,
 * and the unwrap is graphql-scoped (the same envelope under http keeps its
 * raw comparison). A bare payload with ONE object-shaped property is not
 * unwrapped either when the consumer names a field only the outer object has
 * (carrick#1764), so a mismatch there names the field that differs.
 *
 * The same pairs pin the `__typename` rule (carrick#1759). A consumer type
 * generated from a document that selects `__typename` declares it required,
 * and a resolver's return type never states it, because the GraphQL server
 * adds that meta-field to every object a selection asks for. So a GraphQL
 * consumer's `__typename` is read as optional, at any depth: a producer that
 * omits it satisfies the consumer, a producer that states a different one
 * still does not, a real field mismatch beside it stays incompatible and is
 * named without it, and an http pair keeps `__typename` as an ordinary field.
 */
export {};
