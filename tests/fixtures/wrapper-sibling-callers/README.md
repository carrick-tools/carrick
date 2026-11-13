# wrapper-sibling-callers

A decided HTTP site yields to a summary row for its request only where that
row was composed through the site's own call. A caller the summaries did not
compose keeps its row (C11 T1). The test is
`tests/wrapper_sibling_callers_test.rs`.

Answer key:

- `src/items.ts`: three GET rows, `/api/items/:id/one` (a caller of `send`
  with an options bag), `/api/items/:id/two` and `/api/items/three` (callers
  of `sendBytes`, which calls `send`). All three reach the same `fetch` in
  `src/conn.ts`.
- `src/notices.ts`: one GET row, `/api/notices`, at line 6. The site at the
  `fetch` itself yields to the row at the wrapper's caller.
- `src/listing.ts`: one GET row, `/api/listing`, at line 10. The site at the
  middle call (line 6) yields to the row one caller up, whose chain passes
  through it: no possible call and no unread site in the file.
