/**
 * Targeted regression for issues #257 (producer expansion) and #240
 * (deterministic anchor), the PRODUCER-side analogues of #259's consumer fix.
 *
 * A producer `res.json(payment)` where `payment: Payment` routes through the
 * `response_body` inference path. The inferrer used to render the resolved
 * payload type with `typeText`, which keeps the bare NAME `Payment`. The engine
 * wrote that verbatim into the cross-repo `<repo>_types.d.ts` as
 * `export type <alias> = Payment;` — but those bundle files carry only alias
 * lines, no source declarations. So `Payment` was a dangling reference →
 * resolved to `any` → ts_check reported `unverifiable` → `compat = None`,
 * masking the producer's real, recoverable shape (deterministically explaining
 * `payments-svc|POST|/payments -> web-frontend` reading `None`).
 *
 * The fix:
 *  (a) expands the resolved named object STRUCTURALLY (shared
 *      `type-structural-expander.ts`), so the bundle carries the real members
 *      `{ id: string; amountCents: number; currency: string }`; and
 *  (b) carries the deterministic source symbol on `primary_type_symbol`
 *      (`Payment`), derived from the ts-morph `Type` symbol with TS/lib globals
 *      filtered out, so the manifest anchor no longer depends on the LLM.
 */
export {};
