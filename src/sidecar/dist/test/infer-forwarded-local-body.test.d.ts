/**
 * carrick#2671: a call into an in-repo function that copies its argument into
 * a local and forwards that local, unchanged, to a helper that sends it as the
 * body.
 *
 *   export async function createPrice(token, org, input: PriceInput) {
 *     const payload: PriceInput = { label: norm(input.label), amount: input.amount };
 *     return await post<Res>(token, `/v1/${org}/prices`, payload);
 *   }
 *
 * The body sent is the local, so the request is the local's type. It is not
 * a decided `unknown` (the argument goes into a literal), and it is not the
 * argument's own type (the function rewrote it). A local the function builds
 * a further body from, or never sends, leaves the abstain in place.
 */
export {};
