# procedure-router-merged

Answer key for a verified `merges` scope (contract amendment 9 on carrick#2434):
a member that takes routers in a rest slot serves each argument's procedures at
the caller's level. The packages are invented (`fixture-procs`, `fixture-web`).

| Source | Served at | Expected rows |
|---|---|---|
| `merged.ts` | `/rpc-merged` | `GET /rpc-merged/a`, `POST /rpc-merged/b` |
| `merged-alias-use.ts`, `orders.ts` | `/rpc-merged-alias` | `GET /rpc-merged-alias/list`, `GET /rpc-merged-alias/get` (the merge member and the router member are `const` aliases; one argument is a const in another module) |
| `merged-nested.ts` | `/rpc-merged-nested` | `GET /rpc-merged-nested/admin.x`, `GET /rpc-merged-nested/admin.y` |
| `merged-spread.ts` | `/rpc-merged-spread` | none: a spread argument names no router |
