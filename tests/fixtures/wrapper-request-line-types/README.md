# wrapper-request-line-types

When a wrapper's request line gives up its model row to its callers' rows
(carrick#3961, see `../wrapper-request-line`), the types that row held move
to those callers' rows. Each caller row is then typed and judged against
the producer.

Two services:

- `api` serves `POST /v1/orders` and `POST /v1/refunds`, with typed request
  bodies and responses.
- `web` calls them through wrappers in `web/src/net/`. Each wrapper builds
  its body and leaves the path to its callers in `web/src/checkout.ts`.

The model's answer for each wrapper's request line is in
`model-cassette/analyze-file/<file stem>.json`. Each answer states a response
type and a payload.

| wrapper | callers | types at each caller | verdicts at each caller |
|---|---|---|---|
| `placeOrder` | `checkout.ts:5`, `:6` | the request line's payload and `Order` | request and response compatible |
| `sendRefund` | `checkout.ts:7` | the request line's payload and `Refund` | request and response compatible |
| `reorder` | `boot.ts:2` at module level (a fact row), `boot.ts:3` | the request line's payload at each (carrick#4121); `boot.ts:2` keeps its own response | request compatible at each |
| `orderDraft`, `OrderClient.place`, `orderInline`, `orderAsIs` | `widgetcaller.ts:2` to `:5` | `:2`, `:3` and `:5` take the body the wrapper builds (carrick#4260); `:4` sends its parameter and keeps the declared type | request compatible at `:2`, `:3`, `:5` |
| `openSession` (also sends a token request) | `checkout.ts:8`, two rows there | none moved: the request line keeps the model's row and its types | none |

A caller whose own call types a side keeps its own type for that side. A
unit test of the type requests covers that case.

Read by `tests/wrapper_request_line_test.rs`. It needs the built sidecar.

Protocols: HTTP only.
