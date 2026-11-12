/**
 * Issue #433: producer response types declared as INLINE type literals decay
 * to `any` under bare-checkout capture.
 *
 * On a bare checkout the framework generic wrapping the literal
 * (`Response<{ ... }>`) is error-typed — its package is absent — so the
 * deterministic-infer path's checker lookup at the anchor site resolves to a
 * top type and the surface emits `export type ... = any;` (self_check
 * decayed_internal). But the literal type ARGUMENT is dependency-free source
 * syntax: only the outer generic needed the missing package.
 *
 * The fix recovers syntactically, with a purely structural trigger (no
 * framework names anywhere):
 *  - the located node's inferred type is a whole top type, AND
 *  - the governing declared annotation — found via the payload's own call
 *    (argument position or the send call's receiver chain), or via the
 *    located registration call's callback parameters — is a TypeReference
 *    whose resolution FAILED (error type) and which carries exactly one
 *    unambiguous type-literal type argument.
 * The literal node's own type (members resolve against local imports) is then
 * printed through the normal SymbolTracker-backed node-builder path, so local
 * named references ride the surface as import("...") types and the emitted
 * tree carries their declaration closure — the alias genuinely passes the
 * real self-check.
 *
 * Ambiguity refuses recovery: when more than one literal argument is in
 * scope and no payload locator disambiguates, the alias keeps decaying
 * honestly.
 */
export {};
