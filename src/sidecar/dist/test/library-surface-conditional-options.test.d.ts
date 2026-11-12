/**
 * An options parameter generic over what it serves, whose type is
 * an intersection with a conditional on the type parameter (the context
 * option is optional only when the served collection's context is empty).
 * Inside the generic signature that conditional stays deferred. The lister
 * listed no keys there, so a claim could not state `runs` at the key that
 * takes the collection; the verifier refused the same key. Both read one
 * predicate (`isObjectLike`), so a key listed is a key that verifies.
 *
 * The package is invented and its declarations are hand-written.
 */
export {};
