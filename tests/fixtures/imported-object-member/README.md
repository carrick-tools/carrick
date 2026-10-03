# `imported-object-member`

Fixture for carrick#1733: a call through a client written as an object
constant in another module, where the model has only the call site to read.

## The shape

- `src/http.ts` is the transport. Its `fetch` sits in a callback handed to a
  package's span helper, so no pass reads a request through it.
- `src/orders.api.ts` is the client: `export const ordersApi = { ... }`, one
  property per endpoint. Each property calls the transport with its own path;
  `addNote` (arrow) and `setStatus` (method shorthand) carry an options bag
  with a `method`, while `list` and `downloadFile` carry none.
- `src/admin.api.ts` exports an object of the same name whose `addNote`
  posts somewhere else.
- `src/OrderActions.ts` calls `ordersApi` three times, one of them across a
  multi-line `.then().catch()` chain. Its cassette holds the paths a model
  makes up from the member names.
- `src/AdminActions.ts` calls `ordersApi.addNote` imported from `admin.api.ts`;
  its cassette holds the OTHER module's path.
- `src/viaParam.ts` calls `addNote` on a parameter typed as the object.

## The answer key

| site | truth |
|---|---|
| `OrderActions.ts:4` `ordersApi.downloadFile(...)` | the model's row, untouched: the property states no options bag or verb |
| `OrderActions.ts:7` `ordersApi.addNote(...)` | `POST /v1/orders/:orderId/notes` |
| `OrderActions.ts:12` `ordersApi.setStatus(...)` | `PUT /v1/orders/:orderId/status` |
| `AdminActions.ts:3` `ordersApi.addNote(...)` | `POST /admin/orders/:orderId/notes` |
| `viaParam.ts:3` `api.addNote(...)` | the model's row, untouched: the receiver is a parameter |
