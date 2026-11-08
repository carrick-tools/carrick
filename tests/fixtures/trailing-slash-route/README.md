# trailing-slash-route

Answer key for `tests/trailing_slash_route_test.rs` (carrick#3522).

One service. `src/server.ts` declares `app.get('/api/v1/environments/')`
(trailing slash) and `app.get('/')` (the root). `src/client.ts` calls
`fetch('/api/v1/environments')`.

- The route is stored as `/api/v1/environments`, the form every stored route key
  owes (`carrick_match::canonical_path`).
- The root route stays `/`.
- The call pairs with the route: one edge.
