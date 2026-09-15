# `wrapper-chain-const-url`

Fixture for carrick#1151: a client whose endpoints are reached through a chain
of request wrappers declared in the same file, with the URL held in a `const`
and the method carried in an options object.

## The shape

`src/client.ts`:

- `send(url, options, label)` issues the only request, `fetch(url, { ...options, headers })`.
  It states no method of its own; the method is whatever `options` carries.
- `sendWithRetry(url, options, label)` calls `send` twice: once, then again
  with the options re-spread under a fresh token. That is one request shape.
- `callApi(method, endpoint, data)` holds `const url = `${config.ordersApiUrl}${endpoint}``
  and `const init = { method, … }`, and calls `sendWithRetry(url, init, endpoint)`.
- The sites: `ordersApi.list/get/cancel` call `callApi` with a verb and a
  path, and `ordersApi.download` holds its own `endpoint`, `url` and `options`
  in `const`s and calls `sendWithRetry` directly.

None of the sites raises a candidate (each callee is a local identifier), and
the only candidate the file raises, the inner `fetch(url, …)`, states no
route.

## The answer key

| site | truth |
|---|---|
| `client.ts:33` | `GET ${ORDERS_API_URL}/v1/orders` |
| `client.ts:34` | `GET ${ORDERS_API_URL}/v1/orders/:orderId` |
| `client.ts:35` | `PATCH ${ORDERS_API_URL}/v1/orders/:orderId/cancel` |
| `client.ts:40` | `GET ${ORDERS_API_URL}/v1/orders/:orderId/invoice` |

`ORDERS_API_URL` is undeclared (there is no `carrick.json`), so each row keeps
its env-var base: an expected find.

## The cassette

`__llm__/analyze-file/client.json` holds the inner `fetch(url, …)` with its
bare `url` target, which is what extraction can honestly say about that line.
It fails the route-shape gate and states no row, so every row the test pins is
the wrapper pass's own.
