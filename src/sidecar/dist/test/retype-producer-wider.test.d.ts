/**
 * carrick#1516: a retyped consumer that fails against the producer's published
 * response but fits what the producer's handler returns is `wider`, not a
 * mismatch.
 *
 * The published type is the compiler's inference, with every literal the
 * handler returns widened (`scope: string`). The unwidened reading keeps
 * them (`scope: 'all' | 'specific'`). The consumer below declares the literal
 * union, and a second consumer narrows on a boolean discriminant; both are
 * correct for what the producer sends and both fail against the widened type.
 *
 * The milder class holds only when the unwidened type raises NOTHING: a
 * narrower type that still does not fit, or none at all, stays a mismatch.
 */
export {};
