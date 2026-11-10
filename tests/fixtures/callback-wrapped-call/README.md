# Callback-wrapped call fixture (carrick#3712)

`repo/` has two services of one run, `app` and `client` (as `discovery-dispatcher`).
The client's `verify(id)` makes two requests, one after the other, through one
`send` helper that calls `fetch`: `POST /auth/token`, then `POST /reports/${id}`
from inside a callback that `retried` runs. The app's `connect()` hands
`() => verify(id)` to `guarded`, a function written inside `connect`, which runs it.

## Answer key

Decided sites of `app` with `client` as its sibling
(`decided_sites_with`, `src/request_summary/decided_sites.rs`):

| File:line | Call | Record |
|---|---|---|
| `main.ts:18` | `verify(id)` | A site, `/auth/token`: its own chain, the shortest walk to `fetch` |
| `main.ts:16` | `guarded(id, ...)` | No site and no lead: the call inside its callback (`main.ts:18`) decides the request |

No `several_operations` lead anywhere. Before carrick#3712 the walk of
`main.ts:16` (entered through `guarded` since carrick#2448 T3c) could not be
placed through its callback, so the pass walked up from `fetch` over every
caller, reached `main.ts:18` a second time through `report` with
`/reports/...`, and withdrew the site there as a call that chooses between
two requests.
