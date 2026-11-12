/**
 * The canonical union order must hold at the DEPTH BACKSTOP too (carrick#775).
 *
 * `expandTypeStructural` stops recursing past `MAX_EXPANSION_DEPTH` and hands
 * back `namedText(type)` — the compiler's own print. For a union that print is
 * in type-id creation order, which is the instability #735 removed from the
 * expanded path and left untouched here: on the trig-bench webapp blob 12 of
 * the 40 literal unions in the index sit at brace depth 12 to 17, and every one
 * of them still printed in creation order after #761.
 *
 * The perturbation is the same as `expander-union-order.test.ts`: one source,
 * two aliases spelling the same literal set in different orders, resolved in
 * the two possible orders over two fresh programs. Whichever declaration the
 * checker reaches first decides the interned literals' ids, so a print that
 * depends on those ids differs between the two runs.
 */
export {};
