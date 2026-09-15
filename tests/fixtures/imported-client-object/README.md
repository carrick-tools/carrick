# `imported-client-object`

Fixture for carrick#1146, the residual of carrick#1151, and carrick#1155: call
sites that reach an endpoint through a client object another module declares,
and a same-file wrapper site whose path no structural read can state.

## The shape

- `src/lib/http.ts`: `sendJson(verb, path, body)` issues the only request,
  `fetch(`${settings.gatewayUrl}${path}`, …)`, with the base read from
  `src/lib/settings.ts`.
- `src/lib/shelves.ts`: about 4 KB of type declarations, then
  `export const shelves = { list, get, rename, archive }`, each member calling
  the imported `sendJson` with a verb and a path. The member bodies sit past
  the first 4 KB of the module once its imports and types are counted in, and
  the module raises no candidate of its own (none of its calls is awaited).
- `src/pages/shelf-page.tsx`: imports `shelves` through the tsconfig alias
  `@/lib/shelves` and calls `shelves.rename` and `shelves.archive`. Neither
  call matches a candidate name heuristic.
- `src/pages/stock.ts`: calls the imported `sendJson` directly.
- `src/lib/ledger.ts`: a same-file wrapper `ledgerRequest(path)`, called once
  with a literal (`/totals`, which the wrapper pass resolves) and once with a
  path a `switch` picks (`entriesPath(kind)`, which it cannot).

## The answer key

| site | truth |
|---|---|
| `shelf-page.tsx:5` | offered; handed `shelves.rename`, `sendJson`, the `settings` import and its declaration; `PATCH ${process.env.SHELF_API_URL}/v2/shelves/:shelfId/label` |
| `shelf-page.tsx:9` | offered; the cassette answers with an invented path (`/v2/shelf/:id/archived`), which is dropped |
| `stock.ts:4` | offered; the row is written through `sendJson`; the cassette misspells the base as `${gatewayUrl}`, and the row is served `sendJson`'s own base, `${process.env.SHELF_API_URL}` |
| `shelves.ts:163` | offered; the row is written through `sendJson` |
| `ledger.ts:16` | offered as a candidate; the row is written through `ledgerRequest` |
| `ledger.ts:20` | `GET /ledger/totals`, resolved by the wrapper pass, not offered |

The cassettes in `__llm__/analyze-file/` are the model's answers, invented row
included. They are not the thing under test: which sites are offered, what the
analyzer is handed, what survives the evidence gate and what each row is
written through are.
