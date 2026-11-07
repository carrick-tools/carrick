# `request-descriptor`

Fixture for carrick#3470: a request a function builds and returns, for a
caller to send, with nothing in its service calling the function. Read by
`tests/request_descriptor_test.rs`. Everything in it is invented.

Two services, root `carrick.json`:

- `orders-api` serves `GET /api/v2/orders/:orderId` and `POST /api/v2/orders`
  as flat file routes (`app/routes/`), so the producer is stated with no model
  in both the scan and the local check.
- `ops-console` holds the builders.

## Answer key

`ops-console/src/order-requests.ts` (nothing in the repo calls any of these):

| Function | Line | Row |
|---|---|---|
| `buildOrderLookup` | 7 | `GET /api/v2/orders/:orderId`, `request_descriptor`, matched to `orders-api` |
| `buildOrderCreate` | 15 | `POST /api/v2/orders`, `request_descriptor`, matched; request body typed `{ sku: string; quantity: number; }` |
| `buildStaleLookup` | 24 | `GET /api/v1/orders/:orderId`, `request_descriptor`, matches no route |
| `noticeInit` | - | none: a response's init, no `url` |
| `routeTable` | - | none as a call (keyed `path`; the existing route-descriptor pass states `GET /health` at line 36 as a route) |
| `buildWholePath` | - | none: no literal segment |
| `buildGlued` | - | none: the base is glued to the path |

`ops-console/src/invoice-client.ts`: `buildInvoiceLookup` is called by
`fetchInvoice`, which sends it. Its one row is the model's at the `fetch`
(line 15, `GET /api/v2/invoices/:invoiceId`, cassette
`__llm__/analyze-file/invoice-client.json`), as before the pass, and no
descriptor.

No descriptor carries a response type: the literal is no call.
