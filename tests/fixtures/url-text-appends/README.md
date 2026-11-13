# url-text-appends

A string local extended with `+=` and literal text reads as the ways that
reach the `+=`, followed by the text. The test is
`tests/url_text_appends_test.rs`.

Answer key (rows per `fetch`, one per way, no duplicates; a query is not part
of the route):

- `src/appends.ts:6` `GET /api/notes` (the query hole is stripped).
- `:12` `GET /api/items/all` only: the append runs on every path.
- `:19` `GET /api/twice/a` and `GET /api/twice/a/b`.
- `:26` `GET /api/first` and `GET /api/second` (the append follows either write).
- `:34` no row: the append is in a loop.
- `:43` no row: the append is in a closure.
- `:49` `GET /api/self`: `u = u + "?q=1"` reads as `+=` does.
- `:55` `GET /api/pages/:page`: a number append is not text.
- `:61` no row: the right side is not literal text.
- `:68` `GET /api/both/x` and `GET /api/both/y`, never `/x/y`.
- `src/provider.ts:14` `GET /api/catalog/entries`.
