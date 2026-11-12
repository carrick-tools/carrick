/**
 * An `any` the compiler printed because an import did not resolve is not an
 * `any` the source declares (carrick#1164).
 *
 * The node builder prints TypeScript's unresolved-reference placeholder (the
 * `error` intrinsic) as the keyword `any`. Once printed, the emitted surface
 * holds the same text an author annotation would, and the self-check — which
 * reads emitted text — reported every such member as `declared`: "declared that
 * way in the source". A reader told that stops looking, where the truth is that
 * a dependency or a generated module was missing on the scanned checkout, and
 * the fix is to install or generate it.
 *
 * The two causes are only distinguishable on the SOURCE program, where the
 * member's type is still the placeholder. The capture reads them there, at
 * anchor time, and the self-check labels the matching findings.
 */
export {};
