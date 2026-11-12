/**
 * carrick#1046: two captures of ONE tree must not disturb each other.
 *
 * The surface entry has to be written inside the scanned tree (an entry beside
 * a `rootDir` of `src` fails TS6059), and it used to carry one name per repo
 * root. Two captures of the same tree therefore shared a single file: whichever
 * finished first unlinked it while the other's program was still reading it,
 * and every alias whose node-builder print anchors in that destination demoted
 * to `structural_fallback` — honestly, for a reason that described the harness
 * and not the code. The sibling symptom one layer up is `decayed_internal`
 * where `ok` was expected.
 *
 * It surfaced as a flaky merge gate rather than as a bug, because it needs
 * concurrency to appear: each test file passes alone every time, and the four
 * files that capture `capture-v2-bare` only collide when `node --test` runs
 * them together, which is what CI does. Measured on `main` before the fix, that
 * combination went red on 5 of 20 invocations.
 *
 * The assertion here is not a tier by name. It is that a capture made while
 * others run over the same tree is IDENTICAL to one made alone — which is the
 * property the shared name broke, and which cannot rot as the fixture's
 * expected tiers change.
 */
export {};
