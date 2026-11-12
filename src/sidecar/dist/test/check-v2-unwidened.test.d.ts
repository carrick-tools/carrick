/**
 * carrick#1538: a pair is judged again with the producer's unwidened response
 * type when its published type mismatches.
 *
 * A handler that returns `cond ? 'a' : 'b'` publishes `string`, because a
 * literal widens where it is inferred. A consumer that DECLARES `'a' | 'b'`
 * then mismatches a response it reads correctly. The retype check clears that
 * for a consumer whose type was inferred (carrick#1516). A consumer that
 * states its type is never retyped, so the check itself reads the unwidened
 * alias the producer's stub ships (`<alias>_Unwidened`).
 *
 * `producer_wider` is filed only where the published assignment mismatches and
 * the second reading raises nothing. Every other outcome is the published
 * verdict, unchanged: a reading that still mismatches (a different literal, or
 * a member the handler may leave `undefined`), a reading that is or holds a
 * top type, a pair with no reading, a pair that agreed already, and a GraphQL
 * pair.
 *
 * carrick#2011: a request body is read again the same way, where the consumer
 * sends. A consumer has no class of its own on the wire: the pair it clears is
 * `compatible`, with a note that the published request is wider.
 */
export {};
