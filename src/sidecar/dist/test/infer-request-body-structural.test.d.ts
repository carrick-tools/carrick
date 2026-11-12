/**
 * Targeted regression for the cross-repo `POST /payments` request-body edge.
 *
 * `inferRequestBody` used to render a resolved NAMED request type with
 * `typeText`, which keeps the bare name (`CreatePayment`). The engine wrote that
 * verbatim into the cross-repo `<repo>_types.d.ts` as
 * `export type <alias> = CreatePayment;` — but those bundle files carry only
 * alias lines, no source declarations. So the name was a dangling reference →
 * resolved to `any`/`unknown` → ts_check reported `unverifiable` → `compat = None`
 * on the request edge.
 *
 * The fix expands the resolved named request type STRUCTURALLY (shared
 * `type-structural-expander.ts`, the same recovery `inferResponseBody` and
 * `inferFunctionReturn` already apply) so the bundle carries the real members
 * `{ orderId: number; amountCents: number; }`. Two shapes are covered:
 *
 *  (a) Consumer — `fetch(url, { body: JSON.stringify(payload) })` with
 *      `payload: CreatePayment`. The inferrer drills into `JSON.stringify` to
 *      the argument, whose resolved type is the named interface. No wrapper rule
 *      and no cast/binding fire, so the resolved object is expanded structurally.
 *  (b) Producer — `req.body as CreatePayment`. The cast TARGET (recovered via
 *      `extractExplicitTypeFromAncestor`) is expanded structurally; the inner
 *      `req.body` (typed `unknown`) is never emitted as the shape.
 */
export {};
