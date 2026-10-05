# own-route-sibling-serves

Synthetic two-service monorepo for carrick#1944: the caller has a catch-all
route of its own, and a sibling defines the concrete route. `http-server` is a
hand-written declaration, not a real package.

- `api/src/routes.ts` answers `GET /api/orders/:orderId`.
- `web/src/proxy.ts` answers `GET /api/*` by passing the request on.
- `web/src/orders.ts` calls `/api/orders/${id}` (line 2) and `/api/status`
  (line 7).

`own_route` on a call row says that a route of the calling service matches the
call. It is what one service can know about itself, and both calls carry it:
`web`'s catch-all shares the literal segment `api` with each.

Who serves a call is a separate statement, decided over every service's routes
by the most literal segments in common:

| Call | `own_route` | Producer | Why |
|---|---|---|---|
| `GET /api/orders/:id` | true | `api` | `api`'s route agrees on two literal segments, `web`'s catch-all on one |
| `GET /api/status` | true | `web` | only the catch-all serves it |

So a count of marked rows and a count of same-service edges differ by exactly
the calls a sibling serves better. Nothing that matches reads the mark.

Before the mark, the scan deleted both rows, and `api` lost a real consumer.

A marked call has its type entries, and the type check pairs it like any other
call (carrick#1945).

`expected.json` is the answer key and `__llm__/` holds the mocked model answers
for a `CARRICK_MOCK_ALL` scan. Used by `tests/own_route_calls_test.rs`.
