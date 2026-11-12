/**
 * A declared `unknown` under a named member of the RECEIVING side is an open
 * member, as a declared `any` already is (carrick#2591).
 *
 * Any type is assignable to `unknown`, so the compiler's assignment judges only
 * the closed members and no mismatch can hide in the open one. On the SENDING
 * side `unknown` stays a gate: a sent `unknown` is rejected by a narrower
 * receiver, so the pair is not a comparison.
 *
 * Pins:
 *  1. receiving side declares `unknown` under a member -> probed, compatible, a fact;
 *  2. a renamed closed member beside it reads incompatible;
 *  3. sending side declares `unknown` -> unverifiable, gated at capture;
 *  4. only a declared member qualifies: root, unresolved import, budget
 *     sentinel, callable return and a list at the walk's cap stay gated;
 *  5. the rule keys on direction, not on the producer/consumer label (a
 *     request body is received by the producer).
 */
export {};
