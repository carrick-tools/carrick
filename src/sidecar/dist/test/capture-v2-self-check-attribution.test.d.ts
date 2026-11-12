/**
 * The capture self-check blamed every alias for one alias's dangling import
 * (carrick-cloud#1184, cause 4).
 *
 * `self-check.ts` collected failed module specifiers per FILE, and every
 * alias's closure starts at `surface.d.ts` — which holds EVERY alias. So a
 * single dangling specifier anywhere in the surface marked the whole service
 * `decayed_internal`: two real services reported `usable_rate: 0` and
 * `by_self_check: { ok: 0 }` on 52 and 12 aliases while only a handful of
 * surface lines carried a diagnostic.
 *
 * That is not only a wrong metric. `backfill_accepted` (Rust) requires
 * `self_check == "ok"`, so on any service whose surface has one dangling line
 * every anchor-backfill re-capture was rejected.
 *
 * `check-poison.ts` has always attributed a surface diagnostic to the alias
 * whose `export type` statement SPAN covers it. This pins the capture side to
 * the same rule: a failure on the surface file blames the alias whose own
 * statement names the specifier, and nobody else.
 */
export {};
