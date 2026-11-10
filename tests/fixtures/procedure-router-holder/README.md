# procedure-router-holder

Answer key for the router a procedure row names (`procedure.router.binding`):
the innermost named declaration that holds the property of the procedure's
first key. The packages are invented (`fixture-procs`, `fixture-web`).

| Source | Served at | Router binding |
|---|---|---|
| `direct.ts` | `/direct` | `directRouter` |
| `table.ts` | `/table` | `routes` (the table, not the router call) |
| `pair.ts` | `/first`, `/second` | `firstRouter`, `secondRouter` (same file, same first key) |

A router built inside a function, and an `export default` expression, are named
by the same rule (unit tests in `router_holder.rs`), but the procedure walk does
not follow either to its procedures, so no row is served from them here.
