/**
 * carrick#3158: the retype's "the call's type argument does not reach its
 * result" is decided before the rewrite when the type argument the rewrite
 * fills appears in the result only as a conditional's check or extends type
 * (or not at all). The sidecar answers `fresh_type_reason`; the scanner
 * applies it only when the producer's type is one new object type.
 *
 * The rule is the proof's input, so the cases below pin both sides: a to c
 * are decided, d to j look alike and must NOT be (each breaks one condition),
 * and the equivalence tests pin that, for the producer types the scanner
 * applies the reason to, the full retype answers the same string.
 *
 * The declarations are synthetic: a generated-client shape whose only type
 * parameter is a throw-on-error switch, and plain generic helpers.
 */
export {};
