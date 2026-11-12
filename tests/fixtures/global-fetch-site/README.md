# global-fetch-site

One Node-typed service with two files (carrick, the bare `fetch` name test).

- `src/a.ts` calls the global `fetch` twice, in a file nothing else raises. The
  cassette labels the runtime's `fetch` (`node:web-globals/fetch`), so each call
  is read through that label and states a row.
- `src/shadow.ts` declares its own function named `fetch` and calls it. It is
  not the global, has no label, and states no row. A rule that reads the call by
  its name alone sends the file to the model; the scan's request count shows it.

## Answer key

| File:line | Call | Row |
|---|---|---|
| `a.ts:2` | `fetch("/api/orders")` | `GET /api/orders` |
| `a.ts:6` | `fetch(...)` with `{ method: "POST" }` | `POST /api/items` |
| `shadow.ts:6` | local `fetch("/x")` | none, and the file is not sent to the model |
