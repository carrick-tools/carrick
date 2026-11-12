/**
 * The text-level canonical union order (carrick#775, #760).
 *
 * `canonicalizeUnionsInText` is the half of the normal form that works on the
 * compiler's own print, where the type API can no longer reach the members.
 * Two properties matter and are asserted separately:
 *
 *  - unions come out in the same canonical order `orderMembers` produces
 *    (intrinsics first in printed order, then members sorted by text);
 *  - EVERYTHING ELSE comes back byte-identical. A type print is arbitrary
 *    text and this rewrite is not a parser, so the cases it does not
 *    understand — function types, conditionals, unbalanced prints — must be
 *    returned untouched rather than half-rewritten, and order-significant
 *    lists (tuples, generic arguments) must never be sorted.
 */
export {};
