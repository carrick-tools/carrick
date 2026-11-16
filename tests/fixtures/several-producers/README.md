# several-producers

Made-up code. Three services in one shared store, and one PR that regenerates
a client's path so a call that two services serve starts to pair.

- `api` serves `GET /api/v1/refunds/:refundId` (returns `Refund {id, amount,
  reason}`) and `GET /api/v1/returns/:returnId`.
- `legacy` serves the same refunds route and returns `LegacyRefund
  {refund_id, cents}`.
- `web` calls through a generated client (`src/gen/services/ShopService.ts`
  over `src/gen/core/request.ts`, an env base `API_URL` declared in
  `internalEnvVars`). The base commit calls `/api/v1/refund-old/${refundId}`.
  The PR edit changes it to `/api/v1/refunds/${refundId}`.
- `__llm__/` holds the model answers: all empty, every row is deterministic.

Run by `tests/several_producers_test.rs`: fill scans of each repo at its base
commit into one store, the PR edit committed on `web`, then a PR scan of `web`
with the event's base sha. `CARRICK_OUTPUT_JSON` must be unset for the fill
scans, or nothing is written to the store.

## Answer key

| Arm | Store holds | Posted rows for the two call sites (`ShopService.ts:7`, `views.ts:3`) |
|---|---|---|
| A: two producers | api, legacy, web | `type_mismatch`, `edge_source: candidate`, `break: candidate`, `break_reason: several_services_serve_this_call` |
| B: right producer only | api, web | no finding |
| C: other producer only | legacy, web | `type_mismatch`, `edge_source: fact`, `break: certain` (one producer serves it), plus a `missing_endpoint` for the returns route |

Before the fix, arm A read `fact` and `certain` on both rows: the call is
paired with each service and nothing says which one its base reaches.
