# stated-route-response: a route the receiver trace writes types its response

A fastify plugin registers two routes on a typed receiver. The library
store's answer (`cassettes/library-claims/default.json`) states both from the
code, and the model's answer for `routes.ts` (`cassettes/analyze-file/`) names
no route. The trace therefore writes both rows itself, with no model row
behind them. `tests/stated_route_response_test.rs` is the only consumer
(carrick#4071).

## Answer key

| route | handler | response type |
|---|---|---|
| `GET /api/v1/items/:id` (`routes.ts:4`) | inline return | `{ id: string; connected: boolean; }` |
| `GET /api/v1/items` (`routes.ts:9`) | named, `listItems` | `{ id: string; connected: boolean; }[]` |

The `fastify` install under `node_modules/` is a minimal stand-in with the
declarations the routes use.
