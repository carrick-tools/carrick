# recheck-one-repo-two-services — one repo, three services, for the per-edit re-check

One repo (`shop/`) whose `carrick.json` lists three services, built so **every
row is deterministic**: the local index runs with no model. The only consumer
is `tests/local_mode_test.rs` (the tests named below).

- `catalog/` — the producer. A copy of `local-mode-workspace/catalog-web/`
  (a flat-route module deriving `GET /api/v1/widgets/:widgetId`).
- `inventory/` — the consumer. A copy of `local-mode-workspace/inventory-svc/`
  (a client member stating the same path and verb).
- `ledger/` — unrelated: one flat-route module, `GET /api/v1/entries`, that no
  call reaches and that reads nothing from the other two.

## Answer key

| edit | `recheck.scope.reread` | verdict on `catalog`'s widget route |
|---|---|---|
| none (index) | n/a | `compatible` against `inventory` |
| `activeCount: number` -> `string` in `inventory/src/client.ts`, re-check the client | `["inventory"]` | `type_mismatch` (the client now reads a string) |
| then the same edit in the route, re-check the route | `["catalog"]` | `compatible` (judged against the client as the last re-check left it) |
| a comment in `ledger/app/routes/api.v1.entries.ts` | `["ledger"]` | not asked |

Answer keys in `tests/fixtures/` are spec-of-record and are never edited to
make a run pass: a change to this tree changes this table in the same commit.
