/**
 * Regression for carrick#1377: one nested member that resolves to nothing
 * discarded the WHOLE type.
 *
 * A request body typed `{ documentId: string; items: { id: string; evidence:
 * Evidence }[] }`, where `Evidence` comes from a package the scanned checkout
 * does not have, printed an answer naming `Evidence` — an identifier nothing
 * declares. carrick#1165 records that as `undeclared_names`, and the scanner's
 * publish gate refuses the alias entirely, so the request type read null and
 * the documentId and the rest of the items went with it.
 *
 * A member that did not resolve is now written `unknown` at its own position
 * and everything around it survives. The substitution is recorded as an
 * unresolved placeholder, so the self-check labels it `unresolved_import` —
 * "install or generate the missing module" — rather than `declared`, which
 * would tell a reader the author wrote `unknown` there on purpose.
 *
 * `undeclared_names` is NOT retired by this. It stays as what it always was —
 * what the PRINTED answer names and nothing declares — and is now empty
 * because the print no longer names it. Anything substitution cannot reach
 * still fills it and still refuses publication.
 *
 * The pair is still not judged compatible: the capture deep-decay pre-gate
 * reads the `unknown` and abstains, naming the member. That is the soundness
 * rule (a partially-unresolved type would let an arbitrary shape read
 * compatible) and is deliberately untouched. What changed is that the type
 * reaches the index at all, with the one member it could not see marked.
 */
export {};
