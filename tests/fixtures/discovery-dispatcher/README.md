# Dispatcher fixture (carrick#2816)

`repo/` has two services of one run, `app` and `client` (as `discovery-siblings`).
The client's `connect(params)` sends one of two requests by `params.type` (or none),
all through one `send` helper that calls `fetch`.

## Answer key

Decided sites of `app` with `client` as its sibling
(`decided_sites_with`, `src/request_summary/decided_sites.rs`):

| File:line | Call | Record |
|---|---|---|
| `main.ts:4`, `:8`, `:12` | `connect({ ... })` | Lead (`several_operations`), no site: the branches reach different requests |
| `main.ts:16` | `fetchOne("42")` | A site, `/items/42` |
| `local.ts:16` | `sync({ ... })` | Lead (`several_operations`), no site: the same shape in this service's file |

The walk reaches each call from two branches with different names, so the
operation is chosen when the program runs. Picking the branch the argument
selects is the next slice.
