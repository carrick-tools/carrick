/**
 * carrick#3165: tier 2 of the early "does not reach" answer. A generated
 * client's method call states the response map as its first type argument and
 * its own free `Throw` type parameter as a companion, so the call's result is
 * `Awaited` of a conditional that stays deferred. Every member the retype's
 * reach test reads holds an `undefined` that the stated type never put there,
 * so the stated type reaches it only when it has a top-level `undefined`.
 *
 * The sidecar answers `undefined_free_reason` for such a call; the scanner
 * applies it only when the producer's text has no top-level `undefined`.
 *
 * The cases pin both sides. P1 and P1b are decided. Every other call looks
 * alike and must NOT be decided, each breaking one condition of the rule
 * (the full retype reaches a slot for N1, N2, N6 and N8, so those guards are
 * load-bearing). The equivalence tests pin that, for the producer types the
 * scanner applies the reason to, the full retype abstains with the same
 * sentence.
 *
 * The declarations are synthetic: a generated-client shape under neutral
 * names, run in a strict and in a loose program.
 */
export {};
