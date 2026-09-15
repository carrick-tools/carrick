# `url-bases-through-bindings`

Fixture for carrick#1150, #1152 and #1153: a call whose base is read through a
binding, where the binding's declaration states the value outright.

## The shapes

| file | shape | ticket |
|---|---|---|
| `src/admin.ts:4`, `:9` | a module-level path literal interpolated as the prefix | #1150 |
| `src/admin.ts:14` | a query string appended by a trailing ternary | #1150 |
| `src/orders.ts:4` | an imported default-export config object whose property reads `import.meta.env` with a `\|\|` default | #1152 |
| `src/reports.ts:4` | an imported named config object whose property reads `Deno.env.get` with a `??` default | #1152 |
| `src/vendor.ts:4` | an imported string constant holding an absolute vendor origin | #1153 |

`carrick.json` declares one internal env var, the one `orders.ts` reaches,
because #1152's acceptance is that such a call keys on the declared service's
route. The `Deno.env.get` variable is left undeclared: an undeclared env-var
base is an expected find, and its row keeps the base.

## The answer key

| site | truth |
|---|---|
| `admin.ts:4` | `GET /admin/accounts/:accountId` |
| `admin.ts:9` | `GET /admin/accounts` (query string dropped from the route) |
| `admin.ts:14` | `GET /admin/jobs` (query string dropped from the route) |
| `orders.ts:4` | `GET /v1/orders/:orderId`, base env var `VITE_ORDERS_API_URL` |
| `reports.ts:4` | `GET ${process.env.REPORTS_URL}/v1/reports`, base env var `REPORTS_URL` |
| `vendor.ts:4` | `POST https://api.payments-vendor.example/v2/charges`, external host kept on the key |

## The cassettes

`__llm__/analyze-file/*.json` hold each target verbatim, interpolation and all,
which is what the analyzer emits for a base it cannot see the value of.
Resolving it is the scanner's job, so the cassettes make this a regression net
for that resolution rather than a measurement of the model.

Line numbers in the cassettes' `@line:` placeholders reference exact source
lines. Re-count them after any edit to a source file.
