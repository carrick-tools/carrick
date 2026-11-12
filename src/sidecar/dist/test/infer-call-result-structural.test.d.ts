/**
 * Targeted regression for issue #257.
 *
 * A consumer like `return res.json() as Promise<OrderView>` routes through the
 * `call_result` inference path. The inferrer used to resolve the `as
 * Promise<OrderView>` annotation to its TEXT, then string-unwrap the Promise,
 * yielding the bare type NAME `"OrderView"`. The engine wrote that verbatim
 * into the cross-repo `<repo>_types.d.ts` as `export type <alias> = OrderView;`
 * — but those bundle files carry only alias lines, no source declarations. So
 * `OrderView` was a dangling reference → resolved to `any` → ts_check reported
 * `unverifiable` → `compat = None`. The consumer's real, recoverable shape was
 * discarded.
 *
 * The fix expands the annotation's named object type STRUCTURALLY (the same
 * inliner #246 shipped for the definition-resolver, now shared via
 * `type-structural-expander.ts`), so the inferred type carries the real
 * members `{ id: string; currency: string }` and lands in the bundle as a
 * self-contained shape the type checker can actually compare.
 */
export {};
