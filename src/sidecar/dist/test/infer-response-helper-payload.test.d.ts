/**
 * Regression for carrick#631 (producer half): a route handler that returns
 * through a `json()` / `Response`-style helper collapsed to `unknown` or `any`
 * in the type manifest, so the index reported that the endpoint had no
 * contract at all.
 *
 * Two shapes, one mechanism.
 *
 *  (a) The handler returns `wrap(request, reply({ items, total }))`. `wrap` is
 *      local and returns transport machinery (`Response`), so the #371
 *      fail-closed guard abstains and the manifest entry stays `unknown`. The
 *      contract is one level in, in the helper's ARGUMENT.
 *
 *  (b) The handler returns `reply({ … } satisfies ItemListBody)`. The helper
 *      comes from a package with no installed declaration (the scanner reads a
 *      bare checkout), so the helper's own return type is `any` and the whole
 *      response resolved to `any` — even though the contract is written out in
 *      source on the argument.
 *
 * The fix reads the helper's argument when the resolved return carries no
 * contract: it walks the handler's own `return` statements, descends through
 * wrapper calls to the argument that carries a payload, skips branches whose
 * sibling options object states a >= 400 status, and takes the `satisfies`/`as`
 * annotation when the source states one. It is structural throughout — no
 * framework, package or helper name is matched anywhere.
 *
 * How much of the argument is trusted depends on WHY the return carries
 * nothing. Machinery means the callee is known transport, so its argument is
 * the payload. `any` means the callee was merely unresolvable, which on a bare
 * checkout is true of most imported callees, so there only an argument the
 * source annotates counts.
 *
 * Two guards keep the recovery from over-reaching: a returned redirect-style
 * helper whose argument is a bare string must stay unresolved rather than
 * report the path literal, and an unresolvable callee handed an unannotated
 * object (a database query) must stay unresolved rather than report the query.
 */
export {};
