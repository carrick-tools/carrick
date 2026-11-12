/**
 * Phase 3 step 3 (carrick#2472, P3-4): a consumer reads the body of a `fetch`
 * Response where it is bound by a promise chain or by a `Promise.all`
 * destructuring, not by a `const` beside the call.
 *
 *   fetch(u).then((r) => r.json()).then((d) => use(d.x))
 *   const [a, b] = await Promise.all([fetch(u1), fetch(u2)])
 *
 * The body read is `r.json()` on the callback's first parameter, or `a.json()`
 * on the element the call sits at. The retype casts that read to the
 * producer's type, and the success-path rules of carrick#1505 apply unchanged.
 */
export {};
