# wrapper-url-through-helper

A request wrapper hands its path parameter to a helper the text reader cannot
read before the request (carrick#4022). The hole that call reads as keeps the
parameter, so the callers that choose the path are saved, not the request
line. The test is `tests/wrapper_url_through_helper_test.rs`.

Answer key:

- `src/use-badges.ts` lines 6 and 10 (`signedFetch`, a helper with a `let`):
  one unread site each, `unread_expression`, depth 1. Nothing at
  `src/net/signed.ts` line 12.
- `src/use-badges.ts` line 14 (`twoReturnFetch`, a helper whose two returns
  state different texts): one unread site, `several_paths`, depth 1. Nothing
  at `src/net/two-way.ts` line 11.
- `src/net/uncalled.ts` line 12 (`stampedFetch`, no caller): one unread site,
  `name_never_bound`.
- `src/net/uncalled.ts` line 17 (`fixedStamp`, the helper handed no
  parameter): one unread site, `unread_expression`, at the request line.
- `src/use-badges.ts` lines 18, 22, 26, 30: rows (`GET /api/v1/badge`,
  `GET /api/v1/badge`, `GET /badges`, `GET /api/v1/badges/:id`). An unread
  helper call that is the base of a written path, or a whole path segment,
  still states the route.
