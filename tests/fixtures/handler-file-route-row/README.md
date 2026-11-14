# handler-file-route-row

Synthetic one-service repo for carrick#4238. `http-server` is a hand-written
declaration, not a real package; `__llm__/` holds the mocked model answers for
a `CARRICK_MOCK_ALL` scan.

The model sometimes writes a route row at a handler's own definition, in a
file that registers nothing, for a handler another file registers. That row is
a second reading of the registration. The scan withdraws it when the code
places it: the row's line lies inside the body of the function it names as its
handler, and exactly one registration in another file, of the same method,
hands in that function through an import.

- `src/routes.ts` registers `allV1` (`ALL /v1/*`, line 6), `getItem`
  (`GET /items/:id`, line 7) and `h` (`GET /b`, line 8), all imported. The
  router is mounted at `/api` in `src/server.ts`.
- `src/controllers/v1.ts` defines `allV1`; its answer states `ALL /v1/*` at
  line 4, inside `allV1`. Withdrawn.
- `src/controllers/items.ts` defines `getItem`; its answer states
  `GET /items/:id` with no mount prefix at line 4, inside `getItem`. Withdrawn.
- Both withdrawn answers carry a response anchor (`reply.send(...)`, line
  5), so each would be asked a type: withdrawn, neither leaves one.
- `src/controllers/self.ts` defines `h` and registers it itself at `/a`
  (line 8, outside `h`'s body), while `src/routes.ts` registers it at `/b`.
  Both rows stay.
- `src/graphql.ts` and `src/test-helper.ts` both register `gql`, which
  `src/gql-handler.ts` defines: two real registrations of one handler. Both
  rows stay.

`expected.json` is the answer key. Used by
`tests/handler_file_route_row_test.rs`.
