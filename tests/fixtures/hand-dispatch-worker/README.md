# hand-dispatch-worker

One service with no router and no dependency (carrick#2382). Every name in it
is invented.

`src/worker.ts` exports a default object whose `fetch(request)` reads
`pathname` off `new URL(request.url)` and dispatches by hand:

| Route | How the file states it |
|---|---|
| `GET /items` (line 23) | `pathname === "/items"` |
| `POST /items` (line 27) | `pathname === "/items"` |
| `DELETE /items/:id` (line 34) | `pathname.startsWith(ITEM_PREFIX)`, with `const ITEM_PREFIX = "/items/"` |
| `GET /health` (line 42) | `case "/health":` in a `switch (pathname)` |

The file also requests `fetch("/audit", { method: "POST", body })` on line 16:
a route literal used only as a request target, which states no route this
file serves.

The scanner lists no candidate at any of those comparisons, so the model has
no correct id to echo for the four routes. The listed candidates that state
no route path are calls the handler makes; the answers use the body read,
`request.json()` on line 28.

The model's answers live outside the fixture root, chosen with
`CARRICK_MOCK_FIXTURE_DIR`:

- `../hand-dispatch-worker.answers/listed/`: the four routes, each echoing the
  id of `request.json()` (`"@line:28"`). The join keeps each at its own line
  with no span, as it keeps a route whose id names a call the handler makes
  (carrick#1448).
The set states the `/audit` request as a data call at its listed site.

`expected.json` is the answer key: the four route rows.
`tests/unanchored_route_rows_test.rs` reads it.
